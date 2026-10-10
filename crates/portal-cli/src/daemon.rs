use crate::{
    controller::Controller,
    ipc::{Request, Response, SOCKET_NAME},
    logging::LogBuffer,
    settings::Settings,
    Credential,
};
use interprocess::local_socket::{
    tokio::{prelude::*, Listener, Stream},
    GenericNamespaced, ListenerOptions,
};
use std::sync::Arc;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::Mutex,
};

type Shared = Arc<Mutex<Controller>>;

/// 探测端点时的等待上限。连接成功就说明确实有进程占着端点；它可能只是暂时忙
/// （例如控制器锁正被一次登录持有），所以这个上限只用来判定“活着但不回话”。
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// 独立占用 IPC 端点；端点已有活着的实例时返回 `None`，调用方应直接退出。
///
/// `try_overwrite` 分不清“崩溃残留的 socket 文件”和“另一个 daemon 正在服务的
/// 端点”：只要它开着，后启动的进程就会静默删掉对方的端点文件重新绑定，两个进程
/// 都活着，但先启动的那个再也收不到请求、也不会自己退出——GUI 与 launchd 同时
/// 拉起 daemon 时就是这么双开的。这里先尝试独占绑定，只有确认端点已经没人服务
/// （连接被拒）才清理残留文件重试。
pub(crate) async fn bind_endpoint(
    socket_name: &str,
) -> Result<Option<Listener>, Box<dyn std::error::Error + Send + Sync>> {
    match ListenerOptions::new()
        .name(socket_name.to_ns_name::<GenericNamespaced>()?)
        .try_overwrite(false)
        .create_tokio()
    {
        Ok(listener) => return Ok(Some(listener)),
        Err(error) if error.kind() != std::io::ErrorKind::AddrInUse => return Err(error.into()),
        Err(_) => {}
    }
    // 端点文件已存在，先看它还有没有活着的主人。
    match tokio::time::timeout(
        PROBE_TIMEOUT,
        crate::ipc::request_on(socket_name, Request::Status),
    )
    .await
    {
        // 同版本的实例在服务：让位，绝不抢端点。
        Ok(Ok(status)) if status.version.as_deref() == Some(env!("CARGO_PKG_VERSION")) => Ok(None),
        // 旧版本还在服务：请它退出，等端点释放后接管（界面也是这么换掉旧 daemon 的）。
        Ok(Ok(_)) => {
            let _ = crate::ipc::request_on(socket_name, Request::Shutdown).await;
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            Ok(Some(reclaim(socket_name).await?))
        }
        // 连得上但不回话：确实有进程占用端点，宁可等它也不抢。
        Err(_) => Ok(None),
        // 连接被拒：没人服务，是崩溃或退出时留下的僵尸文件，清掉重绑。
        Ok(Err(_)) => Ok(Some(reclaim(socket_name).await?)),
    }
}

/// 删掉端点残留文件后重新绑定。只能用在确认过端点无人服务之后。
async fn reclaim(socket_name: &str) -> Result<Listener, Box<dyn std::error::Error + Send + Sync>> {
    Ok(ListenerOptions::new()
        .name(socket_name.to_ns_name::<GenericNamespaced>()?)
        .try_overwrite(true)
        .create_tokio()?)
}

pub async fn serve() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 先抢端点：抢不到说明已有实例在服务，下面这套初始化都不必做。
    let Some(listener) = bind_endpoint(SOCKET_NAME).await? else {
        eprintln!("portal-cli daemon 已在运行，本进程退出");
        return Ok(());
    };
    let logs = LogBuffer::default();
    let settings = Settings::load();
    logs.set_enabled(settings.log_enabled);
    let auto_settings = settings.clone();
    let state: Shared = Arc::new(Mutex::new(Controller::with_logs(settings, logs.clone())));
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
    let redial_state = state.clone();
    tokio::spawn(async move {
        loop {
            let settings = worker_state.lock().await.settings.clone();
            let delay = settings
                .refresh_policy
                .next_delay(settings.poll_jitter)
                .unwrap_or_else(|| std::time::Duration::from_secs(1));
            tokio::time::sleep(delay).await;
            worker_state.lock().await.tick().await;
        }
    });
    tokio::spawn(async move {
        loop {
            let settings = redial_state.lock().await.settings.clone();
            let delay = settings
                .poll_jitter
                .apply(std::time::Duration::from_secs(5));
            tokio::time::sleep(delay).await;
            redial_state.lock().await.redial_tick(|_| {}).await;
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
    let should_shutdown = matches!(&request, Request::Shutdown);
    let mut response = match request {
        Request::Status => Response {
            ok: true,
            message: "daemon 正在运行".into(),
            snapshot: Some(state.lock().await.snapshot()),
            ..Default::default()
        },
        Request::Shutdown => Response {
            ok: true,
            message: "daemon 即将退出".into(),
            snapshot: None,
            ..Default::default()
        },
        Request::Logs => Response {
            ok: true,
            message: "日志读取成功".into(),
            snapshot: None,
            logs: Some(logs.entries()),
            ..Default::default()
        },
        Request::Sessions => {
            let mut controller = state.lock().await;
            match controller.refresh().await {
                Ok(snapshot) => Response {
                    ok: true,
                    message: "会话刷新成功".into(),
                    snapshot: Some(snapshot),
                    ..Default::default()
                },
                Err(error) => Response::error(error.to_string()),
            }
        }
        Request::Diagnose => {
            // 只取一份设置副本，检测期间不持有控制器锁：否则数秒的探测会
            // 把界面每 2 秒一次的 Status 轮询全部堵在锁上。
            let settings = state.lock().await.settings.clone();
            match crate::diagnose::run(&settings).await {
                Ok(diagnostic) => Response {
                    ok: true,
                    message: "连通性检测完成".into(),
                    diagnostic: Some(diagnostic),
                    ..Default::default()
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
                    ..Default::default()
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
                    ..Default::default()
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
                    ..Default::default()
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
                ..Default::default()
            }
        }
        Request::Forget => {
            let mut controller = state.lock().await;
            controller.forget();
            Response {
                ok: true,
                message: "本地会话已清除".into(),
                snapshot: Some(controller.snapshot()),
                ..Default::default()
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
            // 走 apply_settings 而不是直接赋值：后台地址或网卡设置变了要让
            // 已建立的连接跟着变，否则用户会以为“改了设置没用”。
            controller.apply_settings(settings);
            Response {
                ok: true,
                message: "配置已重新加载".into(),
                snapshot: Some(controller.snapshot()),
                ..Default::default()
            }
        }
    };
    // 统一在这里打上版本号：GUI 升级后据此识别并替换仍在驻留的旧 daemon。
    response.version = Some(env!("CARGO_PKG_VERSION").to_string());
    let encoded = serde_json::to_string(&response)?;
    writer.write_all(encoded.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    if should_shutdown {
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            std::process::exit(0);
        });
    }
    Ok(())
}
