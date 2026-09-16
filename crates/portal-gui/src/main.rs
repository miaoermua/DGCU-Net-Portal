use portal_core::{OnlineSession, PortalClient};
use serde::Serialize;
use std::sync::Mutex;
use tauri::State;

#[derive(Default)]
struct AppState {
    client: Mutex<Option<PortalClient>>,
}

#[derive(Serialize)]
struct DemoSnapshot {
    online: bool,
    username: String,
    download_total: String,
    upload_total: String,
    download_rate: String,
    upload_rate: String,
    sessions: Vec<DemoSession>,
}

#[derive(Serialize)]
struct DemoSession {
    id: String,
    ip: String,
    duration: String,
}

#[tauri::command]
async fn demo_snapshot() -> DemoSnapshot {
    DemoSnapshot {
        online: true,
        username: "demo-user".into(),
        download_total: "5.52 GB".into(),
        upload_total: "312.4 MB".into(),
        download_rate: "等待后台计费更新".into(),
        upload_rate: "等待后台计费更新".into(),
        sessions: vec![DemoSession {
            id: "682516164".into(),
            ip: "10.90.156.148".into(),
            duration: "00:12:42".into(),
        }],
    }
}

#[tauri::command]
async fn login(
    state: State<'_, AppState>,
    server: String,
    username: String,
    password: String,
    bypass_proxy: bool,
) -> Result<String, String> {
    let client = PortalClient::with_options(&server, bypass_proxy).map_err(|e| e.to_string())?;
    let status = client
        .login(&username, &password)
        .await
        .map_err(|e| e.to_string())?;
    *state
        .client
        .lock()
        .map_err(|_| "客户端状态锁定".to_string())? = Some(client);
    Ok(format!("后台登录成功，等待 {} 秒", status.wait_time))
}

#[tauri::command]
async fn online_sessions(state: State<'_, AppState>) -> Result<Vec<OnlineSession>, String> {
    let client = state
        .client
        .lock()
        .map_err(|_| "客户端状态锁定".to_string())?
        .clone();
    let client = client.ok_or_else(|| "请先登录后台".to_string())?;
    client.sessions().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn disconnect(state: State<'_, AppState>, session_id: String) -> Result<String, String> {
    let client = state
        .client
        .lock()
        .map_err(|_| "客户端状态锁定".to_string())?
        .clone();
    let client = client.ok_or_else(|| "请先登录后台".to_string())?;
    client
        .disconnect_id(&session_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("会话 {session_id} 已确认下线"))
}

fn main() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            demo_snapshot,
            login,
            online_sessions,
            disconnect
        ])
        .run(tauri::generate_context!())
        .expect("error while running DGCU Portal");
}
