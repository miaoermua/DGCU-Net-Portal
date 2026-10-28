#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use portal_cli::{
    controller::Snapshot,
    diagnose::Diagnostic,
    ipc::{self, Request},
    logging::LogEntry,
    settings::{self, CredentialStore, RunMode, Settings, UiPreferences},
    validate_url, Credential,
};
use tauri::{
    menu::{Menu, MenuBuilder, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State,
};
use zeroize::Zeroizing;

mod update;

/// 托盘图标的固定 id：设置保存后据此取回托盘，重建菜单。
const TRAY_ID: &str = "main";
/// 托盘菜单项（id，文案），顺序即菜单顺序。
const TRAY_SHOW: (&str, &str) = ("show", "打开主窗口");
const TRAY_LIGHTWEIGHT: (&str, &str) = ("lightweight", "进入轻量模式");
const TRAY_STOP: (&str, &str) = ("stop", "退出服务");

/// 轻量模式下关闭窗口本来就会退出 GUI，"进入轻量模式"只会重复同一件事，
/// 因此该模式下不展示这一项；常规与托盘启动模式才需要它作为退界面的入口。
fn tray_items(run_mode: RunMode) -> Vec<(&'static str, &'static str)> {
    let mut items = vec![TRAY_SHOW];
    if run_mode != RunMode::Lightweight {
        items.push(TRAY_LIGHTWEIGHT);
    }
    items.push(TRAY_STOP);
    items
}
fn build_tray_menu(app: &AppHandle, run_mode: RunMode) -> tauri::Result<Menu<tauri::Wry>> {
    let mut builder = MenuBuilder::new(app);
    for (id, label) in tray_items(run_mode) {
        builder = builder.item(&MenuItem::with_id(app, id, label, true, None::<&str>)?);
    }
    builder.build()
}

struct AppState {
    demo: bool,
}
fn daemon_binary() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|_| "无法获取 GUI 路径")?;
    let name = format!("portal-cli{}", std::env::consts::EXE_SUFFIX);
    for parent in exe.ancestors().skip(1) {
        let candidate = parent.join(&name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!("找不到同包内的 {name} daemon"))
}
/// 版本已经核对过的 daemon 不需要每次请求都再握手一次。
static DAEMON_VERIFIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 核对 IPC 端点上的 daemon 版本；不一致就请它退出，让调用方拉起新进程。
///
/// 升级后旧的常驻 daemon 仍占着端点，直接连过去会出现两种坏结果：
/// 新界面发出的命令（例如 diagnose）对它是未知命令名，以及它返回的快照
/// 缺少新加的字段（例如 account），界面只能显示成“—”。
async fn verify_or_replace_daemon() {
    let Ok(status) = ipc::request(Request::Status).await else {
        return;
    };
    if status.version.as_deref() == Some(env!("CARGO_PKG_VERSION")) {
        DAEMON_VERIFIED.store(true, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    let _ = ipc::request(Request::Shutdown).await;
    // 旧 daemon 收到退出后约 80ms 结束进程，这里留出端点释放的时间。
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
}
async fn daemon_request(request: Request) -> Result<portal_cli::ipc::Response, String> {
    if !DAEMON_VERIFIED.load(std::sync::atomic::Ordering::Relaxed) {
        verify_or_replace_daemon().await;
    }
    if let Ok(response) = ipc::request(request.clone()).await {
        DAEMON_VERIFIED.store(true, std::sync::atomic::Ordering::Relaxed);
        return Ok(response);
    }
    // 有后台登录服务时优先请服务管理器拉起，避免不必要地启动第二个进程。
    let service_started = Settings::load().service_enabled && portal_cli::service::start().is_ok();
    let mut last = "无法连接 portal-cli daemon".to_string();
    // 服务管理器接受启动命令不代表 daemon 已经起来；短暂等待后直接启动同包程序兜底。
    if service_started {
        for _ in 0..5 {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            match ipc::request(request.clone()).await {
                Ok(response) => {
                    DAEMON_VERIFIED.store(true, std::sync::atomic::Ordering::Relaxed);
                    return Ok(response);
                }
                Err(error) => last = error.to_string(),
            }
        }
    }

    let binary = daemon_binary()?;
    let mut command = std::process::Command::new(&binary);
    command.arg("run").arg("--daemon");
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // 不加这个标志，GUI 拉起 daemon 时会闪一个控制台黑框。
        command.creation_flags(0x0800_0000);
    }
    command.spawn().map_err(|error| {
        format!(
            "无法启动 portal-cli daemon（{}）：{error}",
            binary.display()
        )
    })?;

    // daemon 首次启动要创建 IPC 端点，Windows 上比 Unix 慢一档；端点上有旧进程留下的
    // 僵尸文件时还要先探测再清理，所以等待给到 5 秒。
    for _ in 0..25 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        match ipc::request(request.clone()).await {
            Ok(response) => {
                DAEMON_VERIFIED.store(true, std::sync::atomic::Ordering::Relaxed);
                return Ok(response);
            }
            Err(error) => last = error.to_string(),
        }
    }
    Err(format!("连接 portal-cli daemon 失败：{last}"))
}
#[tauri::command]
async fn initial(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    Ok(
        serde_json::json!({"settings":Settings::load(),"demo":state.demo,"version":env!("CARGO_PKG_VERSION")}),
    )
}
#[tauri::command]
async fn connect(
    _app: AppHandle,
    state: State<'_, AppState>,
    username: String,
    password: String,
    portal_url: String,
    backend_only: bool,
) -> Result<Snapshot, String> {
    if state.demo {
        return Err("Demo 模式不连接网络".into());
    }
    let mut credential = Credential::new(username, password);
    if credential.password.is_empty() {
        let settings = Settings::load();
        if settings.credential_store != CredentialStore::Memory {
            credential.username = settings.username;
            credential.password = match settings.credential_store {
                CredentialStore::System => settings::password()?,
                CredentialStore::File => settings::file_password()?,
                CredentialStore::Memory => unreachable!(),
            }
            .to_string();
        }
    }
    if credential.username.is_empty() || credential.password.is_empty() {
        return Err("请输入账号和密码".into());
    }
    let response = daemon_request(Request::Up {
        username: credential.username.clone(),
        password: credential.password.clone(),
        portal_url: Zeroizing::new(portal_url).to_string(),
        backend_only,
    })
    .await?;
    if !response.ok {
        return Err(response.message);
    }
    Ok(response
        .snapshot
        .unwrap_or_else(|| portal_cli::controller::Snapshot {
            status: "accepted".into(),
            message: response.message,
            sessions: vec![],
            rates: std::collections::HashMap::new(),
            selected_id: None,
            background_paused: false,
            authenticated: true,
            one_session: Settings::load().credential_store == CredentialStore::Memory,
            account: None,
        }))
}
#[tauri::command]
async fn refresh(_state: State<'_, AppState>) -> Result<Snapshot, String> {
    let response = daemon_request(Request::Sessions).await?;
    response.snapshot.ok_or(response.message)
}
#[tauri::command]
async fn snapshot(_state: State<'_, AppState>) -> Result<Snapshot, String> {
    let response = daemon_request(Request::Status).await?;
    response.snapshot.ok_or(response.message)
}
#[tauri::command]
async fn diagnose(_state: State<'_, AppState>) -> Result<Diagnostic, String> {
    let response = daemon_request(Request::Diagnose).await?;
    response.diagnostic.ok_or(response.message)
}
#[tauri::command]
async fn select_session(_state: State<'_, AppState>, id: String) -> Result<Snapshot, String> {
    let response = daemon_request(Request::Select { session_id: id }).await?;
    response.snapshot.ok_or(response.message)
}
#[tauri::command]
async fn disconnect(_state: State<'_, AppState>, id: String) -> Result<Snapshot, String> {
    let response = daemon_request(Request::Down { session_id: id }).await?;
    response.snapshot.ok_or(response.message)
}
#[tauri::command]
async fn forget(_state: State<'_, AppState>) -> Result<Snapshot, String> {
    let response = daemon_request(Request::Forget).await?;
    response.snapshot.ok_or(response.message)
}
#[tauri::command]
async fn read_logs(_state: State<'_, AppState>) -> Result<Vec<LogEntry>, String> {
    let response = daemon_request(Request::Logs).await?;
    response.logs.ok_or(response.message)
}
#[tauri::command]
async fn clear_logs(_state: State<'_, AppState>) -> Result<(), String> {
    let response = daemon_request(Request::ClearLogs).await?;
    if response.ok {
        Ok(())
    } else {
        Err(response.message)
    }
}
#[tauri::command]
fn list_interfaces() -> Result<Vec<portal_cli::network::InterfaceInfo>, String> {
    portal_cli::network::list()
}
// UI preferences remain adjustable while an account is signed in, without touching credentials.
#[tauri::command]
async fn save_preferences(
    state: State<'_, AppState>,
    value: UiPreferences,
) -> Result<UiPreferences, String> {
    if state.demo {
        return Err("Demo 不写入系统设置".into());
    }
    let mut next = Settings::load();
    next.set_ui_preferences(&value);
    next.save()?;
    daemon_request(Request::Reload).await?;
    Ok(value)
}
#[tauri::command]
async fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    mut value: Settings,
    password: String,
) -> Result<Settings, String> {
    let password = Zeroizing::new(password);
    if state.demo {
        return Err("Demo 不写入系统设置".into());
    }
    value.normalize()?;
    let previous_run_mode = Settings::load().run_mode;
    if value.credential_store == CredentialStore::Memory {
        settings::forget_password()?;
    } else if !password.is_empty() {
        match value.credential_store {
            CredentialStore::System => settings::save_password(&password)?,
            CredentialStore::File => settings::save_file_password(&password)?,
            CredentialStore::Memory => unreachable!(),
        }
    } else if Settings::load().username != value.username || Settings::load().server != value.server
    {
        return Err("切换账号或服务器时请重新输入密码".into());
    }
    if value.service_enabled != Settings::load().service_enabled {
        if value.service_enabled {
            portal_cli::service::enable(&daemon_binary()?)?;
        } else {
            portal_cli::service::disable()?;
        }
    }
    value.save()?;
    // 运行方式变了就重建托盘菜单，免得切到轻量模式后仍留着重复的"进入轻量模式"。
    if value.run_mode != previous_run_mode {
        if let Some(tray) = app.tray_by_id(TRAY_ID) {
            if let Ok(menu) = build_tray_menu(&app, value.run_mode) {
                let _ = tray.set_menu(Some(menu));
            }
        }
    }
    daemon_request(Request::Reload).await?;
    Ok(value)
}
#[tauri::command]
fn open_auth_site(url: String) -> Result<(), String> {
    let url = validate_url(&url).map_err(|e| e.to_string())?;
    webbrowser::open(url.as_str())
        .map(|_| ())
        .map_err(|_| "无法打开系统浏览器".into())
}
#[tauri::command]
fn open_repository() -> Result<(), String> {
    webbrowser::open("https://github.com/miaoermua/DGCU-Net-Portal")
        .map(|_| ())
        .map_err(|_| "无法打开 GitHub 仓库".into())
}
#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    let url = validate_url(&url).map_err(|e| e.to_string())?;
    webbrowser::open(url.as_str())
        .map(|_| ())
        .map_err(|_| "无法打开系统浏览器".into())
}
/// 只比对 GitHub Releases 上的版本号；下载与安装留给用户自己的渠道，不在这里代劳。
#[tauri::command]
async fn check_update() -> Result<update::UpdateStatus, String> {
    update::check().await
}
/// 彻底退出：停下 daemon，必要时结束登录启动任务。只退界面走托盘菜单，不经过这里。
#[tauri::command]
async fn exit_app(app: AppHandle, _state: State<'_, AppState>) -> Result<(), String> {
    let _ = ipc::request(Request::Shutdown).await;
    if Settings::load().service_enabled {
        let _ = portal_cli::service::stop();
    }
    app.exit(0);
    Ok(())
}
/// 关掉窗口后程序还剩什么界面。
///
/// 决定 Dock 图标去留：窗口收进托盘后没有可见界面，Dock 里就不该再挂图标；
/// 轻量模式是直接退进程，图标随进程一起消失，不归这里管。
#[derive(Debug, PartialEq, Eq)]
enum OnClose {
    /// 收进托盘常驻，窗口隐藏。
    HideToTray,
    /// 退出 GUI 进程。
    Exit,
}
fn close_action(run_mode: RunMode) -> OnClose {
    match run_mode {
        RunMode::Lightweight => OnClose::Exit,
        RunMode::Standard | RunMode::TrayStartup => OnClose::HideToTray,
    }
}

/// macOS 上窗口收起后不留 Dock 图标，唤回时再放回去。
///
/// Dock 图标跟着窗口走：窗口收进托盘后，程序仍在运行但没有可见界面，
/// 这时 Dock 里挂一个点不动的图标只会让人以为程序卡住；而用户想唤回窗口时
/// 又需要通过 Dock 找回应用。托盘图标不受影响，两者是两套机制。
///
/// 非 macOS 平台没有 Dock，这个调用是空操作。
fn set_dock_visible(app: &AppHandle, visible: bool) {
    #[cfg(target_os = "macos")]
    {
        let _ = app.set_dock_visibility(visible);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, visible);
    }
}

/// 唤回主窗口。Dock 图标要先恢复再显示窗口：顺序反了的话，
/// macOS 会把刚显示的窗口当成后台应用的一部分，点了图标窗口却起不来。
fn show(app: &AppHandle) {
    set_dock_visible(app, true);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        // 最小化时 show() 只恢复可见性，窗口仍缩在程序坞/任务栏里，
        // 不 unminimize 的话点了「打开主窗口」看起来还是没反应。
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
fn main() {
    let demo = std::env::args().any(|v| v == "--demo")
        || std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|v| v == "dgcu-portal-demo"))
            .unwrap_or(false);
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show(app)))
        .manage(AppState { demo })
        .setup(move |app| {
            let cfg = Settings::load();
            #[cfg(target_os = "macos")]
            let tray_icon = tauri::image::Image::new_owned(
                include_bytes!("../icons/tray-template.rgba").to_vec(),
                32,
                32,
            );
            #[cfg(target_os = "windows")]
            let tray_icon = tauri::image::Image::new_owned(
                include_bytes!("../icons/tray-win.rgba").to_vec(),
                32,
                32,
            );
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            let tray_icon = app.default_window_icon().unwrap().clone();
            TrayIconBuilder::with_id(TRAY_ID)
                .icon(tray_icon)
                .icon_as_template(cfg!(target_os = "macos"))
                .tooltip("DGCU-Net-Portal")
                .menu(&build_tray_menu(app.handle(), cfg.run_mode)?)
                // 左键唤回主窗口、右键弹菜单；Linux 的 appindicator 不支持这个区分，
                // 仍按原生行为由系统处理点击。
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show(tray.app_handle());
                    }
                })
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show(app),
                    // 只退界面，daemon 是独立进程、照旧在跑；不写回 run_mode，
                    // 托盘的一次点击不改变用户保存的运行方式。
                    "lightweight" => app.exit(0),
                    // 停服务会断掉认证，交给前端确认后再退。确认框在前端，
                    // 所以窗口必须先可见：收在托盘里（或托盘启动模式）时
                    // 窗口是隐藏的，只发事件会让用户点了菜单却什么也看不到。
                    "stop" => {
                        show(app);
                        let _ = app.emit("quit-request", ());
                    }
                    _ => {}
                })
                .build(app)?;
            // 轻量模式不留托盘，隐藏启动没有意义；只有"托盘启动"才静默起窗口。
            if !demo && cfg.run_mode == RunMode::TrayStartup {
                if let Some(w) = app.get_webview_window("main") {
                    w.hide()?;
                }
                // 启动就没有可见界面，Dock 图标同样不该出现；托盘点击唤回时再放回。
                set_dock_visible(app.handle(), false);
            }
            Ok(())
        })
        // 关窗口的含义由运行方式决定：轻量模式退出界面（daemon 是独立进程，认证不受影响），
        // 其余模式收进托盘常驻。每次都重读设置，保存后即时生效、无需重启。
        // 收进托盘时一并撤掉 Dock 图标：程序还在跑但没有可见界面，Dock 里留个
        // 点不动的图标会让人以为卡住；托盘图标不受影响，照常唤回。
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                match close_action(Settings::load().run_mode) {
                    OnClose::Exit => window.app_handle().exit(0),
                    OnClose::HideToTray => {
                        api.prevent_close();
                        let _ = window.hide();
                        set_dock_visible(window.app_handle(), false);
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            initial,
            connect,
            refresh,
            snapshot,
            diagnose,
            select_session,
            disconnect,
            forget,
            read_logs,
            clear_logs,
            list_interfaces,
            save_preferences,
            save_settings,
            open_auth_site,
            open_repository,
            open_external,
            check_update,
            exit_app
        ])
        .run(tauri::generate_context!())
        .expect("DGCU-Net-Portal startup failed");
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lightweight_mode_hides_the_enter_lightweight_item() {
        assert_eq!(tray_items(RunMode::Lightweight), vec![TRAY_SHOW, TRAY_STOP]);
    }
    #[test]
    fn other_modes_keep_all_three_items_in_order() {
        for mode in [RunMode::Standard, RunMode::TrayStartup] {
            assert_eq!(
                tray_items(mode),
                vec![TRAY_SHOW, TRAY_LIGHTWEIGHT, TRAY_STOP]
            );
        }
    }
    #[test]
    fn tray_ids_and_labels_are_stable() {
        assert_eq!(TRAY_SHOW, ("show", "打开主窗口"));
        assert_eq!(TRAY_LIGHTWEIGHT, ("lightweight", "进入轻量模式"));
        assert_eq!(TRAY_STOP, ("stop", "退出服务"));
    }
    #[test]
    fn closing_the_window_exits_only_in_lightweight_mode() {
        assert_eq!(close_action(RunMode::Lightweight), OnClose::Exit);
        assert_eq!(close_action(RunMode::Standard), OnClose::HideToTray);
        assert_eq!(close_action(RunMode::TrayStartup), OnClose::HideToTray);
    }
}
