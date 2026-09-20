#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use portal_cli::{
    controller::Snapshot,
    ipc::{self, Request},
    logging::LogEntry,
    settings::{self, CredentialStore, Settings, UiPreferences},
    validate_url, Credential,
};
use tauri::{AppHandle, Emitter, Manager, State};
use zeroize::Zeroizing;

struct AppState {
    demo: bool,
}
fn daemon_binary() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|_| "无法获取 GUI 路径")?;
    for parent in exe.ancestors().skip(1) {
        let candidate = parent.join("portal-cli");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err("找不到同包内的 portal-cli daemon".into())
}
async fn daemon_request(request: Request) -> Result<portal_cli::ipc::Response, String> {
    match ipc::request(request.clone()).await {
        Ok(response) => Ok(response),
        Err(_) => {
            let binary = daemon_binary()?;
            std::process::Command::new(binary)
                .arg("run")
                .arg("--daemon")
                .spawn()
                .map_err(|_| "无法启动 portal-cli daemon")?;
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            ipc::request(request)
                .await
                .map_err(|error| error.to_string())
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
    webbrowser::open("https://github.com/miaoermua/dgcu-portal")
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
#[tauri::command]
async fn exit_app(app: AppHandle, _state: State<'_, AppState>) -> Result<(), String> {
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
    let background = std::env::args().any(|v| v == "--background");
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show(app)))
        .manage(AppState { demo })
        .setup(move |app| {
            use tauri::{
                menu::{Menu, MenuItem},
                tray::TrayIconBuilder,
            };
            let show_item = MenuItem::with_id(app, "show", "打开主窗口", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &quit_item])?;
            #[cfg(target_os = "macos")]
            let tray_icon = tauri::image::Image::new_owned(
                include_bytes!("../icons/tray-template.rgba").to_vec(),
                32,
                32,
            );
            #[cfg(not(target_os = "macos"))]
            let tray_icon = app.default_window_icon().unwrap().clone();
            TrayIconBuilder::new()
                .icon(tray_icon)
                .icon_as_template(cfg!(target_os = "macos"))
                .tooltip("DGCU-Net-Portal")
                .menu(&menu)
                .on_menu_event(|app, event| {
                    show(app);
                    if event.id().as_ref() == "quit" {
                        let _ = app.emit("close-request", ());
                    }
                })
                .build(app)?;
            let cfg = Settings::load();
            if !demo && (background || cfg.tray_startup) {
                if let Some(w) = app.get_webview_window("main") {
                    w.hide()?;
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.emit("close-request", ());
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
