#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use portal_cli::{
    controller::Snapshot,
    ipc::{self, Request},
    logging::LogEntry,
    settings::{self, CredentialStore, RunMode, Settings, UiPreferences},
    validate_url, Credential,
};
use tauri::{AppHandle, Emitter, Manager, State};
use zeroize::Zeroizing;

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
async fn daemon_request(request: Request) -> Result<portal_cli::ipc::Response, String> {
    match ipc::request(request.clone()).await {
        Ok(response) => Ok(response),
        Err(_) => {
            let binary = daemon_binary()?;
            let mut command = std::process::Command::new(binary);
            command.arg("run").arg("--daemon");
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;
                // 不加这个标志，GUI 拉起 daemon 时会闪一个控制台黑框。
                command.creation_flags(0x0800_0000);
            }
            command.spawn().map_err(|_| "无法启动 portal-cli daemon")?;
            // daemon 首次启动要创建 IPC 端点，Windows 上明显比 Unix 慢，
            // 单次 150ms 等待经常还没就绪就连过去。
            let mut last = "无法连接 portal-cli daemon".to_string();
            for _ in 0..10 {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                match ipc::request(request.clone()).await {
                    Ok(response) => return Ok(response),
                    Err(error) => last = error.to_string(),
                }
            }
            Err(last)
        }
    }
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
    state: State<'_, AppState>,
    mut value: Settings,
    password: String,
) -> Result<Settings, String> {
    let password = Zeroizing::new(password);
    if state.demo {
        return Err("Demo 不写入系统设置".into());
    }
    value.normalize()?;
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
fn show(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
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
            use tauri::{
                menu::{Menu, MenuItem},
                tray::TrayIconBuilder,
            };
            let cfg = Settings::load();
            let show_item = MenuItem::with_id(app, "show", "打开主窗口", true, None::<&str>)?;
            let exit_item =
                MenuItem::with_id(app, "exit", "退出界面（保留后台）", true, None::<&str>)?;
            let stop_item =
                MenuItem::with_id(app, "stop", "退出并停止后台服务", true, None::<&str>)?;
            // 轻量模式下关窗口就是退界面，"退出界面"这一项与它重复，去掉一个易混的出口。
            let menu = if cfg.run_mode == RunMode::Lightweight {
                Menu::with_items(app, &[&show_item, &stop_item])?
            } else {
                Menu::with_items(app, &[&show_item, &exit_item, &stop_item])?
            };
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
            TrayIconBuilder::new()
                .icon(tray_icon)
                .icon_as_template(cfg!(target_os = "macos"))
                .tooltip("DGCU-Net-Portal")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    // 只退界面，daemon 照旧在跑。
                    "exit" => app.exit(0),
                    // 停服务会断掉认证，交给前端确认后再退。
                    "stop" => {
                        let _ = app.emit("quit-request", ());
                    }
                    _ => show(app),
                })
                .build(app)?;
            // 轻量模式不留托盘，隐藏启动没有意义；只有"托盘启动"才静默起窗口。
            if !demo && cfg.run_mode == RunMode::TrayStartup {
                if let Some(w) = app.get_webview_window("main") {
                    w.hide()?;
                }
            }
            Ok(())
        })
        // 关窗口的含义由运行方式决定：轻量模式退出界面（daemon 是独立进程，认证不受影响），
        // 其余模式收进托盘常驻。每次都重读设置，保存后即时生效、无需重启。
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if Settings::load().run_mode == RunMode::Lightweight {
                    window.app_handle().exit(0);
                } else {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            initial,
            connect,
            refresh,
            snapshot,
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
            exit_app
        ])
        .run(tauri::generate_context!())
        .expect("DGCU-Net-Portal startup failed");
}
