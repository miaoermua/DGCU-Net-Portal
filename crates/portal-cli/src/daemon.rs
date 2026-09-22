use crate::{
    controller::Controller,
    ipc::{Request, Response, SOCKET_NAME},
    logging::LogBuffer,
    settings::Settings,
    Credential,
};
use interprocess::local_socket::{
    tokio::{prelude::*, Stream},
    GenericNamespaced, ListenerOptions,
};
use std::sync::Arc;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::Mutex,
};

type Shared = Arc<Mutex<Controller>>;

pub async fn serve() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let logs = LogBuffer::default();
    let settings = Settings::load();
    logs.set_enabled(settings.log_enabled);
    let auto_settings = settings.clone();
    let state: Shared = Arc::new(Mutex::new(Controller::with_logs(settings, logs.clone())));
    let name = SOCKET_NAME.to_ns_name::<GenericNamespaced>()?;
    let listener = ListenerOptions::new()
        .name(name)
        .try_overwrite(true)
        .create_tokio()?;
    let worker_state = state.clone();
    if auto_settings.credential_store != crate::settings::CredentialStore::Memory {
        let password = match auto_settings.credential_store {
            crate::settings::CredentialStore::System => crate::settings::password(),
            crate::settings::CredentialStore::File => crate::settings::file_password(),
            crate::settings::CredentialStore::Memory => unreachable!(),
        };
        if let Ok(password) = password {
            let startup_state = state.clone();
            let username = auto_settings.username.clone();
            tokio::spawn(async move {
                let credential = Credential::new(username, password.to_string());
                let _ = startup_state
                    .lock()
                    .await
                    .connect(credential, "", false, |_| {})
                    .await;
            });
        }
    }
    tokio::spawn(async move {
        loop {
            let settings = worker_state.lock().await.settings.clone();
            let delay = settings
                .refresh_policy
                .next_delay(settings.poll_jitter)
                .unwrap_or_else(|| std::time::Duration::from_secs(1));
            tokio::time::sleep(delay).await;
            worker_state.lock().await.tick(|_| {}).await;
        }
    });
    eprintln!("portal-cli daemon ready");
    loop {
        let stream = listener.accept().await?;
        let state = state.clone();
        let logs = logs.clone();
        tokio::spawn(async move {
            let _ = handle(stream, state, logs).await;
        });
    }
}

async fn handle(
    stream: Stream,
    state: Shared,
    logs: LogBuffer,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let request: Request = match serde_json::from_str(&line) {
        Ok(value) => value,
        Err(error) => {
            let response =
                serde_json::to_string(&Response::error(format!("请求格式错误：{error}")))?;
            writer.write_all(response.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            return Ok(());
        }
    };
    let response = match request {
        Request::Status => Response {
            ok: true,
            message: "daemon 正在运行".into(),
            snapshot: Some(state.lock().await.snapshot()),
            logs: None,
        },
        Request::Logs => Response {
            ok: true,
            message: "日志读取成功".into(),
            snapshot: None,
            logs: Some(logs.entries()),
        },
        Request::Sessions => {
            let mut controller = state.lock().await;
            match controller.refresh().await {
                Ok(snapshot) => Response {
                    ok: true,
                    message: "会话刷新成功".into(),
                    snapshot: Some(snapshot),
                    logs: None,
                },
                Err(error) => Response::error(error.to_string()),
            }
        }
        Request::Up {
            username,
            password,
            portal_url,
            backend_only,
        } => {
            let mut controller = state.lock().await;
            match controller
                .connect(
                    Credential::new(username, password),
                    &portal_url,
                    backend_only,
                    |_| {},
                )
                .await
            {
                Ok(snapshot) => Response {
                    ok: true,
                    message: snapshot.message.clone(),
                    snapshot: Some(snapshot),
                    logs: None,
                },
                Err(error) => Response::error(error.to_string()),
            }
        }
        Request::Down { session_id } => {
            let mut controller = state.lock().await;
            match controller.disconnect(&session_id).await {
                Ok(snapshot) => Response {
                    ok: true,
                    message: snapshot.message.clone(),
                    snapshot: Some(snapshot),
                    logs: None,
                },
                Err(error) => Response::error(error.to_string()),
            }
        }
        Request::Select { session_id } => {
            let mut controller = state.lock().await;
            match controller.select(&session_id) {
                Ok(snapshot) => Response {
                    ok: true,
                    message: "会话已选择".into(),
                    snapshot: Some(snapshot),
                    logs: None,
                },
                Err(error) => Response::error(error.to_string()),
            }
        }
        Request::ClearLogs => {
            logs.clear();
            Response {
                ok: true,
                message: "日志已清空".into(),
                snapshot: None,
                logs: None,
            }
        }
        Request::Forget => {
            let mut controller = state.lock().await;
            controller.forget();
            Response {
                ok: true,
                message: "本地会话已清除".into(),
                snapshot: Some(controller.snapshot()),
                logs: None,
            }
        }
        Request::Reload => {
            let settings = Settings::load();
            logs.set_enabled(settings.log_enabled);
            logs.record(crate::logging::Event::refresh_policy(
                settings.refresh_policy,
            ));
            logs.record(crate::logging::Event::poll_jitter(settings.poll_jitter));
            let mut controller = state.lock().await;
            controller.settings = settings;
            Response {
                ok: true,
                message: "配置已重新加载".into(),
                snapshot: Some(controller.snapshot()),
                logs: None,
            }
        }
    };
    let encoded = serde_json::to_string(&response)?;
    writer.write_all(encoded.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    Ok(())
}
