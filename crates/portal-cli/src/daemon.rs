use crate::{
    controller::{CollectClock, Controller, ManualRefresh, Snapshot},
    ipc::{RefreshState, Request, Response, SOCKET_NAME},
    logging::LogBuffer,
    settings::Settings,
    AppError, Credential,
};
use interprocess::local_socket::{
    tokio::{prelude::*, Listener, Stream},
    GenericNamespaced, ListenerOptions,
};
use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{watch, Mutex, MutexGuard},
};

/// daemon 的共享状态：权威控制器，加上它最近一次完整投影的快照。
///
/// `Controller` 仍是唯一可写的状态源，快照只是它的只读副本，不参与任何业务判断。
/// 所有 IPC 命令与后台 worker 共用同一把异步锁：`Controller` 的每次操作都在整个
/// `await` 期间独占 `&mut Controller`，因此 `connect()` 期间不会插入 `select`、
/// `disconnect` 等用户操作；`ReconnectCheckpoint` 的回滚不会覆盖并发状态变化。
/// 一次手动刷新的结果，决定 IPC 回什么样的提示。
///
/// 三种情况都带快照：被取代时快照来自更新的操作，数据不比这次差；被闸门挡住时
/// 快照就是当前最新状态。区别只在提示语，不在数据可用性，所以 `ok` 一律为真。
pub(crate) enum RefreshOutcome {
    /// 本次结果已提交。
    Applied(Snapshot),
    /// 结果被更新的操作取代（连接换了、下线了，或有更新的采集先提交）。
    Superseded(Snapshot),
    /// 已有一个手动刷新在执行，本次没有发请求。
    Busy(Snapshot),
}

pub(crate) struct State {
    controller: Mutex<Controller>,
    latest: watch::Sender<Snapshot>,
    latest_rx: watch::Receiver<Snapshot>,
    /// 采集心跳的共享句柄。放在 `State` 上是为了让 `Status` 不取控制器锁也能
    /// 读出实时年龄——只靠"发布时的旧值"会让停滞提示晚一个发布周期。
    collect_clock: CollectClock,
    /// 手动刷新的 single-flight 闸门：一个 permit，拿不到说明已有一次手动刷新
    /// 在跑，这一次就不再向后台发请求。
    ///
    /// 用信号量而不是布尔标志：permit 随 future 一起析构，HTTP 失败、结果被
    /// 取代、任务被取消都会自动释放，不需要额外的清理分支。闸门只在手动刷新
    /// 之间互斥，5 秒采集走自己的路径，不受它影响。
    refresh_gate: Arc<tokio::sync::Semaphore>,
}

impl State {
    /// 用刚构造好的控制器初始化快照槽。初始快照在做任何操作之前发布，`Status`
    /// 不必等第一次解锁就能读到完整状态。
    pub(crate) fn new(mut controller: Controller) -> Self {
        let collect_clock = controller.clock();
        let (latest, latest_rx) = watch::channel(controller.snapshot());
        Self {
            controller: Mutex::new(controller),
            latest,
            latest_rx,
            collect_clock,
            refresh_gate: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    /// 取控制器锁。返回的 [`Locked`] 在释放锁之前发布一次完整快照，调用方不需要
    /// （也不能）自己记得发布。
    pub(crate) async fn lock(&self) -> Locked<'_> {
        Locked {
            controller: self.controller.lock().await,
            latest: &self.latest,
        }
    }

    /// 读最近一次发布的快照，并把新鲜度换成**读取时**的值。
    ///
    /// `collect_age_ms` / `collector_stalled` 是派生字段，不参与任何业务判断，
    /// 所以在读路径上现算既不会有多余的锁竞争，也不会让陈旧提示延迟。
    pub(crate) fn snapshot(&self) -> Snapshot {
        let mut snapshot = self.latest_rx.borrow().clone();
        let (age, stalled) = self.collect_clock.health(snapshot.authenticated);
        snapshot.collect_age_ms = age;
        snapshot.collector_stalled = stalled;
        snapshot
    }

    /// 常规 5 秒采集：锁内领票 → 锁外请求 → 锁内提交。
    pub(crate) async fn session_tick(&self) {
        let Some(ticket) = self.lock().await.begin_collect() else {
            return;
        };
        let result = Controller::collect(&ticket).await;
        self.lock()
            .await
            .finish_session_collect(ticket, result, |_| {})
            .await;
    }

    /// 手动刷新：与常规采集走同一条锁外通路，只是提交时的收尾不同。
    ///
    /// 并发的手动刷新被 single-flight 挡住：拿不到闸门就直接回当前快照，不再
    /// 向校园网多发一次 `/sessions`。闸门不等待、不占控制器锁，所以定时采集
    /// 完全不受影响。
    pub(crate) async fn refresh(&self) -> Result<RefreshOutcome, AppError> {
        let _permit = match self.refresh_gate.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => return Ok(RefreshOutcome::Busy(self.snapshot())),
        };
        let Some(ticket) = self.lock().await.begin_collect() else {
            return Err(AppError::Rejected);
        };
        let result = Controller::collect(&ticket).await;
        let refreshed = self.lock().await.finish_manual_refresh(ticket, result)?;
        Ok(match refreshed {
            ManualRefresh::Applied(snapshot) => RefreshOutcome::Applied(snapshot),
            ManualRefresh::Superseded(snapshot) => RefreshOutcome::Superseded(snapshot),
        })
    }

    /// 测试用：拿到共享心跳，模拟"采集停摆"而不必真的等 30 秒。
    #[cfg(test)]
    pub(crate) fn clock(&self) -> CollectClock {
        self.collect_clock.clone()
    }
}

/// 持有控制器锁的守卫，析构时把当前状态的完整投影写进快照槽。
///
/// 字段声明顺序有意义：`Drop::drop` 在字段析构之前运行，所以发布时控制器锁仍然
/// 被持有，发布出去的一定是同一次持锁区间的完整状态；随后字段析构才释放锁。
pub(crate) struct Locked<'a> {
    controller: MutexGuard<'a, Controller>,
    latest: &'a watch::Sender<Snapshot>,
}

impl Deref for Locked<'_> {
    type Target = Controller;

    fn deref(&self) -> &Controller {
        &self.controller
    }
}

impl DerefMut for Locked<'_> {
    fn deref_mut(&mut self) -> &mut Controller {
        &mut self.controller
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        let _ = self.latest.send_replace(self.controller.snapshot());
    }
}

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
    let state = Arc::new(State::new(Controller::with_logs(settings, logs.clone())));
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
    serve_on(listener, state, logs).await
}

/// 在已经绑定的端点上服务：先起两个后台 worker，再进入 accept 循环。
///
/// 两个 worker 共享同一次采集结果，节奏各自保留：
///
/// - 采集 worker 约每 5 秒跑一次 `redial_tick`，是**唯一**周期性发起 `/sessions`
///   的地方，结果写入控制器后供会话观察、绑定和流量结算共用。
/// - 结算 worker 约每 60 秒跑一次 `rate_tick`，只读时钟和控制器里的最新会话数据，
///   不发任何 HTTP 请求，因此不会产生第二次采集。
///
/// `serve()` 只负责绑定端点、读取设置和启动自动登录；测试用临时端点直接进这里，
/// 不去读本机设置。两个入口走的是同一份服务逻辑，没有测试专用分支。
pub(crate) async fn serve_on(
    listener: Listener,
    state: Arc<State>,
    logs: LogBuffer,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let collect_state = state.clone();
    tokio::spawn(async move {
        loop {
            let settings = collect_state.lock().await.settings.clone();
            let delay = settings
                .poll_jitter
                .apply(std::time::Duration::from_secs(5));
            tokio::time::sleep(delay).await;
            // 锁内领票 → 锁外请求 → 锁内提交：请求期间控制器锁是空的，
            // `Sessions`/`Up`/`Down` 不必排在一个 10 秒超时后面。
            collect_state.session_tick().await;
        }
    });
    let rate_state = state.clone();
    tokio::spawn(async move {
        loop {
            let settings = rate_state.lock().await.settings.clone();
            // 关闭后台刷新时维持 1 秒的空转节奏：`rate_tick` 进去就返回，不发请求。
            let delay = settings
                .refresh_policy
                .next_delay(settings.poll_jitter)
                .unwrap_or_else(|| std::time::Duration::from_secs(1));
            tokio::time::sleep(delay).await;
            rate_state.lock().await.rate_tick();
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

/// 把一次手动刷新的结果翻译成 IPC 响应。
///
/// 只有真正报错才回 `ok: false`：结果被取代、或已有刷新在跑，都不是失败——
/// 用户拿到的仍是当前最新会话数据，只是这次没有重新采集。把这两种情况说成
/// HTTP 失败会误导用户去查网络。
pub(crate) fn sessions_response(result: Result<RefreshOutcome, AppError>) -> Response {
    let (state, message, snapshot) = match result {
        Ok(RefreshOutcome::Applied(snapshot)) => (RefreshState::Applied, "会话刷新成功", snapshot),
        Ok(RefreshOutcome::Superseded(snapshot)) => (
            RefreshState::Superseded,
            "会话刷新已被更新的操作取代，当前显示最新状态",
            snapshot,
        ),
        Ok(RefreshOutcome::Busy(snapshot)) => (
            RefreshState::Busy,
            "已有刷新正在进行，当前显示最新状态",
            snapshot,
        ),
        Err(error) => return Response::error(error.to_string()),
    };
    Response {
        ok: true,
        message: message.into(),
        snapshot: Some(snapshot),
        refresh: Some(state),
        ..Default::default()
    }
}

async fn handle(
    stream: Stream,
    state: Arc<State>,
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
        // 读最近一次发布的快照，不取控制器锁：`Up`/`Down`/自动重拨持锁期间
        // 查询照常返回，代价是可能比控制器落后一个进行中的操作。
        Request::Status => Response {
            ok: true,
            message: "daemon 正在运行".into(),
            snapshot: Some(state.snapshot()),
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
        Request::Sessions => sessions_response(state.refresh().await),
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
