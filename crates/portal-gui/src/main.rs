#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod service;
use portal_core::{
    cmcc::Phase,
    controller::{Controller, Snapshot},
    settings::{self, Settings},
    validate_url, Credential,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

struct AppState {
    inner: Mutex<Controller>,
    demo: bool,
}
#[tauri::command]
async fn initial(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let c = state.inner.lock().await;
    Ok(serde_json::json!({"settings":c.settings,"demo":state.demo}))
}
#[tauri::command]
async fn connect(
    app: AppHandle,
    state: State<'_, AppState>,
    username: String,
    password: String,
    portal_url: String,
    backend_only: bool,
) -> Result<Snapshot, String> {
    let mut c = state.inner.lock().await;
    if state.demo {
        return Err("Demo 模式不连接网络".into());
    }
    let mut credential = Credential::new(username, password);
    if credential.password.is_empty() && c.settings.remember_account && !c.settings.one_session {
        credential.username = c.settings.username.clone();
        credential.password = settings::password()?.to_string();
    }
    if credential.username.is_empty() || credential.password.is_empty() {
        return Err("请输入账号和密码".into());
    }
    let url = Zeroizing::new(portal_url);
    c.connect(credential, &url, backend_only, |phase| {
        let _ = app.emit("auth-phase", phase);
    })
    .await
    .map_err(|e| e.to_string())
}
#[tauri::command]
async fn refresh(state: State<'_, AppState>) -> Result<Snapshot, String> {
    state
        .inner
        .lock()
        .await
        .refresh()
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn snapshot(state: State<'_, AppState>) -> Result<Snapshot, String> {
    Ok(state.inner.lock().await.snapshot())
}
#[tauri::command]
async fn select_session(state: State<'_, AppState>, id: String) -> Result<Snapshot, String> {
    state
        .inner
        .lock()
        .await
        .select(&id)
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn disconnect(state: State<'_, AppState>, id: String) -> Result<Snapshot, String> {
    state
        .inner
        .lock()
        .await
        .disconnect(&id)
        .await
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn forget(state: State<'_, AppState>) -> Result<Snapshot, String> {
    let mut c = state.inner.lock().await;
    c.forget();
    Ok(c.snapshot())
}
#[tauri::command]
async fn save_settings(
    state: State<'_, AppState>,
    mut value: Settings,
    password: String,
) -> Result<Settings, String> {
    let password = Zeroizing::new(password);
    let mut c = state.inner.lock().await;
    if state.demo {
        return Err("Demo 不写入系统设置".into());
    }
    value.normalize()?;
    if c.snapshot().authenticated {
        return Err("请先结束本地会话，再修改连接与账号设置".into());
    }
    if value.one_session || !value.remember_account {
        // Remove saved identity as well as memory, before claiming transient mode is enabled.
        settings::forget_password()?;
    } else if !password.is_empty() {
        settings::save_password(&password)?;
    } else if c.settings.username != value.username || c.settings.server != value.server {
        return Err("切换账号或服务器时请重新输入密码".into());
    }
    if value.service_enabled != c.settings.service_enabled {
        if value.service_enabled {
            service::enable(&std::env::current_exe().map_err(|_| "无法获取程序路径")?)?;
        } else {
            service::disable()?;
        }
    }
    value.save()?;
    c.settings = value.clone();
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
async fn exit_app(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let mut c = state.inner.lock().await;
    if c.settings.one_session {
        let s = c.snapshot();
        if let Some(id) = s.selected_id {
            if let Err(e) = c.disconnect(&id).await {
                return Err(format!("{}；本地凭据已释放，再次退出可关闭窗口。", e));
            }
        }
    }
    c.forget();
    drop(c);
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
    let demo = std::env::args().any(|v| v == "--demo") || std::env::current_exe().ok().and_then(|p|p.file_name().map(|v|v=="dgcu-portal-demo")).unwrap_or(false);
    let background = std::env::args().any(|v| v == "--background");
    let settings = if demo {
        Settings::default()
    } else {
        Settings::load()
    };
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show(app)))
        .manage(AppState {
            inner: Mutex::new(Controller::new(settings)),
            demo,
        })
        .setup(move |app| {
            use tauri::{
                menu::{Menu, MenuItem},
                tray::TrayIconBuilder,
            };
            let show_item = MenuItem::with_id(app, "show", "打开主窗口", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &quit_item])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("DGCU Portal")
                .menu(&menu)
                .on_menu_event(|app, event| {
                    show(app);
                    if event.id().as_ref() == "quit" {
                        let _ = app.emit("close-request", ());
                    }
                })
                .build(app)?;
            let state = app.state::<AppState>();
            let cfg = state.inner.blocking_lock().settings.clone();
            if !demo && (background || cfg.tray_startup) {
                if let Some(w) = app.get_webview_window("main") {
                    w.hide()?;
                }
            }
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if !demo
                    && background
                    && !cfg.one_session
                    && cfg.remember_account
                    && cfg.auto_redial
                {
                    if let Ok(password) = settings::password() {
                        let credential =
                            Credential::new(cfg.username.clone(), password.to_string());
                        let _ = handle
                            .state::<AppState>()
                            .inner
                            .lock()
                            .await
                            .connect(credential, "", false, |p| {
                                let _ = handle.emit("auth-phase", p);
                            })
                            .await;
                    }
                }
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    if demo {
                        continue;
                    }
                    let state = handle.state::<AppState>();
                    if let Ok(mut c) = state.inner.try_lock() {
                        c.tick(|p: Phase| {
                            let _ = handle.emit("auth-phase", p);
                        })
                        .await;
                        let _ = handle.emit("snapshot", c.snapshot());
                    };
                }
            });
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
            save_settings,
            open_auth_site,
            exit_app
        ])
        .run(tauri::generate_context!())
        .expect("DGCU Portal startup failed");
}
