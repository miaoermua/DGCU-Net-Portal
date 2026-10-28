use crate::{
    cmcc::Phase,
    network,
    settings::{CredentialStore, ReconnectMode, Settings},
    traffic::{AccountingRates, Rate},
    AccountInfo, AppError, CmccContext, Credential, OnlineSession, PortalClient,
    PortalLoginOutcome,
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, serde::Deserialize, Debug)]
pub struct Snapshot {
    pub status: String,
    pub message: String,
    pub sessions: Vec<OnlineSession>,
    pub rates: HashMap<String, Rate>,
    pub selected_id: Option<String>,
    pub background_paused: bool,
    pub authenticated: bool,
    pub one_session: bool,
    /// 后台报告的套餐与到期时间；未登录或无此接口时为 None。
    pub account: Option<AccountInfo>,
}
/// 原目标会话消失后的分级结论。
///
/// 后台没有 MAC 或可靠的设备标识，`framedipaddress` 只是本机网卡的地址，
/// 因此这里最多只能给出"疑似"被挤下线，不能当成服务器明确报告的事实。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LossKind {
    /// 原会话消失、本机 IP 不在列表、在线会话超过 2 条，且最近有连接异常。
    SuspectedEviction,
    /// 会话确实消失，但缺少多设备抢占的证据。
    Unclear,
}
#[derive(Clone, Debug)]
struct SessionLoss {
    /// 消失的原目标会话。保留它，避免 `selected` 被清理后丢掉判断依据。
    id: String,
    kind: LossKind,
}
pub struct Controller {
    pub settings: Settings,
    logs: crate::logging::LogBuffer,
    api: Option<PortalClient>,
    credential: Option<Credential>,
    account: Option<AccountInfo>,
    rows: Vec<OnlineSession>,
    selected: Option<String>,
    accounting: AccountingRates,
    latest_rates: HashMap<String, Rate>,
    pub paused: bool,
    status: String,
    message: String,
    missing: u32,
    attempts: u32,
    /// 网络层恢复的独立退避计数；与 `attempts`（会话掉线重拨）互不干扰。
    network_attempts: u32,
    last_attempt: Option<Instant>,
    awaiting_session: Option<HashSet<String>>,
    reconnecting: bool,
    /// 本轮监测到的原目标会话丢失及其分级；普通刷新不能覆盖它。
    loss: Option<SessionLoss>,
    /// 疑似被挤下线时暂停自动重拨。与 `paused` 分开：`paused` 不该停止会话监测。
    redial_hold: bool,
    /// 最近一次成功解析的网卡上下文（解析失败为 None），用于判断本机 IP 是否还在列表里。
    current_network: Option<network::NetworkContext>,
    /// 最近一次轮询失败的时刻，作为"连接异常"的证据；成功后清空。
    last_poll_failure: Option<Instant>,
    /// 当前 client 绑定的网卡上下文。与实时解析结果不一致时重建 client。
    bound_network: Option<network::NetworkContext>,
    /// 网卡解析入口。默认是真实的 `network::resolve`；测试里替换成受控实现，
    /// 就能在不改动本机网络的前提下验证"地址变化 → 重建 client"。
    resolve_network: NetworkResolver,
}
type NetworkResolver =
    Arc<dyn Fn(&str) -> Result<Option<network::NetworkContext>, String> + Send + Sync>;
impl Default for Controller {
    fn default() -> Self {
        Self::new(Settings::default())
    }
}
impl Controller {
    pub fn new(settings: Settings) -> Self {
        let logs = crate::logging::LogBuffer::default();
        logs.set_enabled(settings.log_enabled);
        Self::with_logs(settings, logs)
    }
    pub fn with_logs(settings: Settings, logs: crate::logging::LogBuffer) -> Self {
        logs.record(crate::logging::Event::refresh_policy(
            settings.refresh_policy,
        ));
        logs.record(crate::logging::Event::poll_jitter(settings.poll_jitter));
        Self {
            settings,
            logs,
            api: None,
            credential: None,
            account: None,
            rows: Vec::new(),
            selected: None,
            accounting: AccountingRates::default(),
            latest_rates: HashMap::new(),
            paused: true,
            status: "idle".into(),
            message: "输入账号后上线，或登录后台或重新绑定会话来查询会话".into(),
            missing: 0,
            attempts: 0,
            network_attempts: 0,
            last_attempt: None,
            awaiting_session: None,
            reconnecting: false,
            loss: None,
            redial_hold: false,
            current_network: None,
            last_poll_failure: None,
            bound_network: None,
            resolve_network: Arc::new(network::resolve),
        }
    }
    /// 丢掉连接与会话状态，但保留"原会话丢失"的结论。
    ///
    /// Memory 模式且未开重拨时，会话消失会立即释放本地登录态；判定必须在这之前
    /// 完成，`loss` 也要留着，否则用户什么都看不到。
    fn release(&mut self) {
        self.api = None;
        self.credential = None;
        self.account = None;
        self.rows.clear();
        self.selected = None;
        self.accounting.clear();
        self.latest_rates.clear();
        self.paused = true;
        self.missing = 0;
        self.attempts = 0;
        self.awaiting_session = None;
    }
    pub fn clear(&mut self) {
        self.release();
        // 显式清理（手动下线、忘记会话、重新登录）没有"谁的会话丢了"可言。
        self.loss = None;
        self.lift_eviction_protection();
    }
    /// 解除「疑似被挤下线」期间的重拨保护。
    ///
    /// 只有用户显式恢复（手动连接、手动下线、忘记会话）才允许调用。普通轮询
    /// 成功、切换选中会话、自动重试都只说明"后台这一轮还有响应"，不能证明本机
    /// 已安全上线：保护若被这些事件解除，新选中的会话一消失就会立刻自动认证，
    /// 可能把对方再挤下去。
    fn lift_eviction_protection(&mut self) {
        self.redial_hold = false;
    }
    pub fn forget(&mut self) {
        self.clear();
        self.logs.record(crate::logging::Event::LocalReleased);
        self.status = "idle".into();
        self.message = "已清除本地会话；这不代表远端已经下线".into();
    }
    /// 会话消失或连接失败时，Memory 模式是否该立刻丢弃本地登录态。
    ///
    /// 只有"重连也关着"时这份进程内凭据才没有后续用途；开启重连后它必须
    /// 留到下一次重拨，否则掉线后没有任何可用的账号，重连永远无法成功。
    fn drops_state_on_loss(&self) -> bool {
        self.settings.credential_store == CredentialStore::Memory
            && self.settings.reconnect_mode == ReconnectMode::Disabled
    }
    fn loss_message(&self, loss: &SessionLoss) -> String {
        match loss.kind {
            LossKind::SuspectedEviction => {
                if self.settings.reconnect_mode == ReconnectMode::Disabled {
                    "检测到在线设备数超过 2 台，本机连接异常，且后台在线会话中未发现本机 IP，疑似已被其他设备挤下线。自动重拨未开启，请检查在线设备后手动重新连接。"
                } else {
                    "检测到在线设备数超过 2 台，本机连接异常，且后台在线会话中未发现本机 IP，疑似已被其他设备挤下线。为避免反复抢占，自动重拨已暂停，请检查在线设备后手动重新连接。"
                }
                .into()
            }
            LossKind::Unclear => {
                let base = "所选会话已从后台在线列表消失，当前无法确认是否被其他设备挤下线，请检查网络或重新绑定会话。";
                if self.redial_hold {
                    // 保护仍生效时必须说出来：否则界面只显示"会话消失"，
                    // 用户无从知道自动重拨其实还压着。
                    format!("{base}疑似被挤下线期间暂停的自动重拨仍未恢复，需要时请手动重新连接。")
                } else {
                    base.into()
                }
            }
        }
    }
    /// 「连接异常」只认最近一次成功轮询之前、两分钟内的失败。
    ///
    /// 本轮查询失败根本不进这里：拿不到列表就不能断言任何会话消失。
    fn recent_poll_failure(&self) -> bool {
        self.last_poll_failure
            .is_some_and(|at| at.elapsed() <= Duration::from_secs(120))
    }
    /// 轮询失败是否属于本机连接异常（传输层失败或超时）。
    ///
    /// 只有这一类才能当"本机连接异常"的证据：HTTP 5xx、Cookie 失效/业务拒绝、
    /// 响应解析失败分别说明后台、登录态或协议出了问题，把它们算进来会让一次
    /// 后台维护或接口变更把一个仍然在线的会话误判成被挤下线。
    fn poll_failure_is_network(error: &AppError) -> bool {
        matches!(error, AppError::Network(_) | AppError::Timeout(_))
    }
    /// 本机 IPv4 是否还在这次成功返回的在线列表里。
    ///
    /// 三态，宁可"不知道"也不猜：
    /// - 任一已知地址与本机匹配 → `Some(true)`；
    /// - 所有会话地址都已知且都不匹配 → `Some(false)`；
    /// - 没有匹配项，但至少一条会话缺地址 → `None`（证据不完整）。
    ///
    /// 解析不到网卡地址同样是 `None`。把缺失字段当成"不在列表"，会让
    /// 「后台数据不全 + 恰有一次请求失败」误报成抢占。
    fn local_ip_in(&self, rows: &[OnlineSession]) -> Option<bool> {
        let ip = self.current_network.as_ref()?.ipv4.parse::<IpAddr>().ok()?;
        let mut unknown = false;
        for row in rows {
            match row.framedipaddress {
                Some(address) if address == ip => return Some(true),
                Some(_) => {}
                None => unknown = true,
            }
        }
        if unknown {
            None
        } else {
            // 空列表同样落在这里：没有任何会话，也就谈不上"本机地址还在"。
            Some(false)
        }
    }
    /// 记录/更新会话丢失结论。同一个目标只写一次日志，等级变化时补一条。
    fn record_loss(&mut self, id: &str, kind: LossKind) {
        // 同一目标的结论只升级不降级：刚出现过连接异常、随后几轮查询又成功，
        // 不该把"疑似被挤下线"退回成"原因不明"，更不该借机恢复自动重拨。
        let kind = match &self.loss {
            Some(previous) if previous.id == id && previous.kind == LossKind::SuspectedEviction => {
                LossKind::SuspectedEviction
            }
            _ => kind,
        };
        let changed = match &self.loss {
            Some(previous) => previous.id != id || previous.kind != kind,
            None => true,
        };
        if kind == LossKind::SuspectedEviction {
            // 暂停自动抢线，也放弃"等一条新会话出现再自动绑定"：新增的那条
            // 很可能是把本机挤下去的设备，不是本机。
            self.redial_hold = true;
            self.awaiting_session = None;
        }
        self.loss = Some(SessionLoss {
            id: id.to_owned(),
            kind,
        });
        if changed {
            self.logs.record(match kind {
                LossKind::SuspectedEviction => crate::logging::Event::SessionEvictedSuspected,
                LossKind::Unclear => crate::logging::Event::SessionLost,
            });
        }
    }
    fn fail(&mut self, e: AppError) -> AppError {
        self.logs.error(&e);
        if self.drops_state_on_loss() {
            self.clear();
        }
        self.status = "error".into();
        self.message = e.to_string();
        e
    }
    pub fn snapshot(&mut self) -> Snapshot {
        // 会话丢失的结论优先于普通轮询文案：否则下一轮刷新会把提示冲掉。
        let message = match &self.loss {
            Some(loss) => self.loss_message(loss),
            None => self.message.clone(),
        };
        Snapshot {
            status: self.status.clone(),
            message,
            sessions: self.rows.clone(),
            rates: self.latest_rates.clone(),
            selected_id: self.selected.clone(),
            background_paused: self.paused || self.redial_hold,
            authenticated: self.api.is_some(),
            one_session: self.settings.credential_store == CredentialStore::Memory,
            account: self.account.clone(),
        }
    }
    pub async fn connect<F: Fn(Phase)>(
        &mut self,
        credential: Credential,
        portal_url: &str,
        backend_only: bool,
        progress: F,
    ) -> Result<Snapshot, AppError> {
        // A manual connection starts a fresh local session. Automatic redial is
        // transactional: keep the current API/Cookie and session rows until the
        // replacement connection has completed successfully.
        if !self.reconnecting {
            self.clear();
        }
        let resolved = (self.resolve_network)(&self.settings.interface_name);
        let network = if backend_only {
            resolved.ok().flatten()
        } else {
            Some(
                resolved
                    .map_err(AppError::NetworkInterface)
                    .and_then(|value| {
                        value.ok_or_else(|| {
                            AppError::NetworkInterface(
                                "没有找到带 IPv4 和 MAC 的活动网卡，请在设置中选择网卡".into(),
                            )
                        })
                    })
                    .map_err(|e| self.fail(e))?,
            )
        };
        let local_address = network.as_ref().map(|value| {
            value
                .ipv4
                .parse::<IpAddr>()
                .map_err(|_| self.fail(AppError::NetworkInterface("网卡 IPv4 地址无效".into())))
        });
        let local_address = match local_address {
            Some(Ok(value)) => Some(value),
            Some(Err(error)) => return Err(error),
            None => None,
        };
        let api = PortalClient::with_options_and_local(
            &self.settings.server,
            self.settings.bypass_proxy,
            local_address,
        )?
        .with_logs(self.logs.clone());
        let portal = PortalClient::with_options_and_local(
            &self.settings.server,
            self.settings.bypass_proxy,
            local_address,
        )?
        .with_logs(self.logs.clone());
        // 记下这次 client 绑定的网卡上下文；后续轮询据此判断是否需要重建。
        self.bound_network = network.clone();
        self.status = "authenticating".into();
        // Authenticate the self-service client before Portal login so we can record a
        // baseline. The same cookie jar is reused after Portal login; a second
        // user_login request is only needed when this first attempt failed.
        let mut backend_authenticated = false;
        let baseline = match api.login(&credential.username, &credential.password).await {
            Ok(()) => {
                backend_authenticated = true;
                // 套餐与到期时间只在连接时读一次：它随订购变更，不随会话变化。
                self.account = api.account().await.ok();
                api.sessions().await.ok()
            }
            Err(e) => {
                self.logs.error(&e);
                None
            }
        };
        if let Some(rows) = baseline.as_ref() {
            if let Some(id) = Self::unique_local_session(rows, network.as_ref()) {
                self.api = Some(api);
                self.rows = rows.clone();
                self.selected = Some(id);
                self.awaiting_session = None;
                self.missing = 0;
                self.status = "session_online".into();
                self.message = "已恢复本机对应的在线会话".into();
                // 自动重拨走的也是 connect，但不会先 `clear()`；这里是成功恢复
                // 的分支，旧的"会话丢失"结论必须失效，否则 `snapshot()` 会继续
                // 把旧告警压在"已恢复"之上。抢占保护则只由显式恢复解除：
                // 自动路径在保护期间根本不会走到这里。
                self.loss = None;
                if !self.reconnecting {
                    self.lift_eviction_protection();
                }
                self.paused = self.settings.reconnect_mode == ReconnectMode::Disabled;
                // 凭据只存在控制器内存里；只要开了重连就保留，与密码是否落盘无关。
                if self.settings.reconnect_mode != ReconnectMode::Disabled {
                    self.credential = Some(credential);
                }
                return Ok(self.snapshot());
            }
        }
        let outcome = if backend_only {
            None
        } else {
            let ctx = if portal_url.is_empty() {
                progress(Phase::ReadingForm);
                // The DGCU gateway accepts the standard CMCC entry with the
                // current interface context. This avoids depending on public
                // captive-check hosts, which often time out before login.
                let template = CmccContext::from_server_context_options(
                    &self.settings.server,
                    network.as_ref().unwrap(),
                    &self.settings.paip,
                    (!self.settings.basip.is_empty()).then_some(self.settings.basip.as_str()),
                )?;
                if self.settings.probe_enabled {
                    match portal.portal_form_available(&template).await {
                        Ok(()) => Ok(template),
                        Err(_) => {
                            progress(Phase::Discovering);
                            let discovered = portal.discover(&self.settings.probe_url).await?;
                            CmccContext::with_network_context_options(
                                &discovered.portal_url,
                                network.as_ref().unwrap(),
                                &self.settings.paip,
                                (!self.settings.basip.is_empty())
                                    .then_some(self.settings.basip.as_str()),
                            )
                        }
                    }
                } else {
                    Ok(template)
                }
            } else {
                progress(Phase::ReadingForm);
                CmccContext::from_portal_url(portal_url).and_then(|ctx| {
                    CmccContext::with_network_context_options(
                        &ctx.portal_url,
                        network.as_ref().unwrap(),
                        &self.settings.paip,
                        (!self.settings.basip.is_empty()).then_some(self.settings.basip.as_str()),
                    )
                })
            };
            let ctx = ctx.map_err(|e| self.fail(e))?;
            Some(
                portal
                    .cmcc_login_progress(&ctx, &credential.username, &credential.password, progress)
                    .await
                    .map_err(|e| self.fail(e))?,
            )
        };
        // Portal cookies do not authorize the separate user self-service endpoints.
        // If the baseline login succeeded, retain that authenticated API client
        // instead of issuing a duplicate login request.
        if !backend_authenticated {
            match api.login(&credential.username, &credential.password).await {
                Ok(()) => backend_authenticated = true,
                Err(e) if outcome.is_some() => {
                    self.status = "accepted".into();
                    self.message =
                        "Portal 已确认成功，但后台登录失败；请重新登录后台查询会话".into();
                    self.logs.error(&e);
                    return Ok(self.snapshot());
                }
                Err(e) => return Err(self.fail(e)),
            }
        }
        if backend_authenticated {
            if self.account.is_none() {
                self.account = api.account().await.ok();
            }
            self.api = Some(api);
        }
        let rows = self.api.as_ref().unwrap().sessions().await;
        let query_failed = rows.is_err();
        self.rows = match rows {
            Ok(rows) => rows,
            Err(error) => {
                self.logs.error(&error);
                Vec::new()
            }
        };
        let restored_local = Self::unique_local_session(&self.rows, network.as_ref());
        if let Some(id) = restored_local {
            self.selected = Some(id);
            self.awaiting_session = None;
            self.missing = 0;
        }
        // Retain the pre-login baseline until accounting reports the new session.
        if outcome.is_some() {
            self.awaiting_session =
                baseline.map(|rows| rows.iter().map(|row| row.radacctid.clone()).collect());
            self.bind_new_session();
        }
        self.status = if self.selected.is_some() && outcome.is_none() {
            "session_online"
        } else if outcome.is_some() {
            "accepted"
        } else {
            "backend"
        }
        .into();
        // 认证成功和"绑定到哪一条会话"是两件独立的事：多会话时 bind_new_session
        // 会放弃自动绑定以免挑错，但认证本身仍然成功。此时提示必须同时说清这两点，
        // 否则用户看到"已确认成功"、流量却是空的，没有任何线索知道要自己选。
        let bound = self.selected.is_some();
        self.message = match outcome {
            Some(PortalLoginOutcome::Dialed) if bound => "Portal 与代拨均已确认成功",
            Some(PortalLoginOutcome::Dialed) => {
                "Portal 与代拨均已确认成功，但尚未绑定管理会话；请在会话列表中选择"
            }
            Some(_) if bound => "Portal 已确认成功",
            Some(_) => "Portal 已确认成功，但尚未绑定管理会话；请在会话列表中选择",
            None if bound => "已登录后台并恢复本机对应的在线会话",
            None => "已登录用户后台；请选择要管理的会话",
        }
        .into();
        if query_failed {
            self.message
                .push_str("；后台会话查询暂不可用，稍后自动重试");
        }
        self.paused = self.settings.reconnect_mode == ReconnectMode::Disabled || outcome.is_none();
        // 连接成功是新开始：旧的"会话已消失"事实结论失效，不再压住后续提示。
        self.loss = None;
        // 抢占保护仍只由用户显式恢复解除。自动恢复（`reconnecting`）成功只说明
        // 后台这次登录通过，不足以证明本机不会再被挤，不能顺手放开自动重拨。
        if !self.reconnecting {
            self.lift_eviction_protection();
        }
        if self.settings.reconnect_mode != ReconnectMode::Disabled {
            self.credential = Some(credential);
        }
        // Otherwise credential drops here, before the online session ends.
        Ok(self.snapshot())
    }
    /// 重新解析网卡；地址变了就用新地址重建 client。返回是否发生了重建。
    ///
    /// 合盖唤醒、Wi-Fi 漫游、插拔网线都会换掉 IPv4，而 `local_address` 是钉在
    /// client 上的：不重建就会对每个请求立刻失败。Cookie 在共享 jar 里，重建
    /// 不影响登录态，也不需要重新走认证流程。
    fn rebind_changed_network(&mut self) -> bool {
        // 解析失败（例如短暂读不到 MAC）时不动现有绑定，等下一次轮询再判断。
        let Ok(current) = (self.resolve_network)(&self.settings.interface_name) else {
            // 地址未知：本地 IP 不能作为"被挤下线"的证据。
            self.current_network = None;
            return false;
        };
        // 每次成功解析都更新，即使地址没变：判定要用的是"现在的"本机 IP。
        self.current_network = current.clone();
        if current == self.bound_network {
            return false;
        }
        let Some(api) = self.api.as_mut() else {
            return false;
        };
        let local = current
            .as_ref()
            .and_then(|context| context.ipv4.parse::<IpAddr>().ok());
        match api.rebind(local) {
            Ok(()) => {
                self.bound_network = current;
                self.logs.record(crate::logging::Event::NetworkRebind);
                true
            }
            Err(_) => {
                self.logs.record(crate::logging::Event::NetworkRebindFailed);
                false
            }
        }
    }
    /// 应用新设置，并让连接层跟着设置走。
    ///
    /// 原实现只替换 `settings`，于是"保存设置"对已经建立的 client 完全无效：
    /// 改了网卡或后台地址却还在用旧连接，用户只会看到"改了没用"。这里至少在
    /// 重建代价很低的情况下立刻生效，其余交给下一次轮询的自动重建。
    pub fn apply_settings(&mut self, settings: Settings) {
        let server = settings.server.clone();
        let server_changed = server != self.settings.server;
        let network_changed = settings.interface_name != self.settings.interface_name
            || settings.bypass_proxy != self.settings.bypass_proxy;
        self.settings = settings;
        // 关掉重连后，进程内凭据就没有后续用途了，不留到用户看不见的地方。
        if self.settings.reconnect_mode == ReconnectMode::Disabled {
            self.credential = None;
        }
        if !server_changed && !network_changed {
            return;
        }
        if self.api.is_none() {
            self.bound_network = None;
            return;
        }
        if server_changed {
            let result = match self.api.as_mut() {
                Some(api) => api.rebase(&server),
                None => Ok(()),
            };
            match result {
                Ok(()) => self.logs.record(crate::logging::Event::NetworkRebind),
                Err(_) => self.logs.record(crate::logging::Event::NetworkRebindFailed),
            }
        }
        if network_changed {
            // 清掉记录，下一次轮询按新网卡重新解析地址并重建 client。
            self.bound_network = None;
        }
    }
    /// 自动重拨与网络恢复共用的退避：30 秒 × 2^attempts，并叠加抖动。
    fn retry_backoff(&self) -> Duration {
        self.settings
            .poll_jitter
            .apply(Duration::from_secs(30 * (1 << self.attempts)))
    }
    pub async fn refresh(&mut self) -> Result<Snapshot, AppError> {
        self.rebind_changed_network();
        let api = self.api.as_ref().ok_or(AppError::Rejected)?;
        let rows = match api.sessions().await {
            Ok(rows) => rows,
            Err(error) => {
                // 拿不到列表就不动会话状态：请求失败不等于会话消失。
                // 只有传输层失败/超时才算"本机连接异常"的证据，后台 5xx、
                // 登录态失效或解析失败不能反推本机网络有问题。
                if Self::poll_failure_is_network(&error) {
                    self.last_poll_failure = Some(Instant::now());
                }
                return Err(error);
            }
        };
        self.rows = rows;
        self.bind_new_session();
        // 手动或低频刷新也更新状态，但不推进缺失计数：计数只由 5 秒 worker
        // 负责，否则两个 worker 会让 `missing >= 3` 提前到达。
        self.observe_session(false);
        self.last_poll_failure = None;
        self.update_rates();
        Ok(self.snapshot())
    }
    fn update_rates(&mut self) {
        self.latest_rates = if self.settings.traffic_enabled {
            self.accounting.update(&self.rows)
        } else {
            self.accounting.clear();
            HashMap::new()
        };
    }
    /// 判定并记录选中会话的在线状态。`count` 为 true 时顺带推进掉线计数。
    ///
    /// 判定必须在 `drops_state_on_loss()` 清理之前完成：清理会丢掉 `selected`，
    /// 之后就无从知道是哪条会话消失、原因是什么。
    fn observe_session(&mut self, count: bool) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        if self.rows.iter().any(|row| row.radacctid == id) {
            self.missing = 0;
            self.status = "session_online".into();
            // 原会话回来了（或用户已重新绑定）：旧目标丢失的告警不能永久残留。
            // 但「疑似被挤下线」的重拨保护是策略，不是"这一轮列表里有没有它"
            // 的事实——普通轮询成功、切换会话都不能解除它，只有显式恢复才行。
            self.loss = None;
            self.message = if self.redial_hold {
                "所选会话在后台在线列表中；疑似被挤下线期间暂停的自动重拨仍未恢复，需要时请手动重新连接".into()
            } else {
                "所选会话在后台在线列表中".into()
            };
            return;
        }
        if count {
            self.missing += 1;
        }
        let kind = if self.local_ip_in(&self.rows) == Some(false)
            && self.rows.len() > 2
            && self.recent_poll_failure()
        {
            LossKind::SuspectedEviction
        } else {
            LossKind::Unclear
        };
        // 先把结论与提示落定，再执行可能清空 `selected` 的释放。
        self.record_loss(&id, kind);
        self.status = "offline".into();
        self.message = self
            .loss
            .as_ref()
            .map(|loss| self.loss_message(loss))
            .unwrap_or_default();
        if self.drops_state_on_loss() {
            self.release();
        }
    }
    /// 旧的按次调用入口：保留 `missing` 的原有语义，供 5 秒 worker 与测试使用。
    fn update_session_status(&mut self) {
        self.observe_session(true);
    }
    pub fn select(&mut self, id: &str) -> Result<Snapshot, AppError> {
        if !self.rows.iter().any(|r| r.radacctid == id) {
            return Err(AppError::SessionNotFound);
        }
        self.selected = Some(id.into());
        self.awaiting_session = None;
        self.logs.record(crate::logging::Event::SelectSession);
        self.missing = 0;
        // 用户显式选了一条，旧目标丢失的告警到此为止。
        self.loss = None;
        // 但「疑似被挤下线」期间的重拨保护不跟着解除：选择只换了管理目标，
        // 不代表本机已经安全上线。随手点中的那条很可能是挤掉本机的设备，
        // 此时恢复自动重拨会用本机账号再认证一次，反而可能把对方挤下去。
        // 想恢复自动重拨必须走一次显式 connect（它会先 clear）。
        self.message = if self.redial_hold {
            "已切换管理会话；疑似被挤下线期间暂停的自动重拨仍未恢复，需要时请手动重新连接".into()
        } else {
            "已切换管理会话".into()
        };
        Ok(self.snapshot())
    }
    fn bind_new_session(&mut self) {
        // 已经判定原会话丢失时，列表里新出现的会话更可能属于挤掉本机的设备：
        // 不自动认领，交给用户显式选择。
        if self.loss.is_some() {
            return;
        }
        let Some(before) = &self.awaiting_session else {
            return;
        };
        let mut added = self
            .rows
            .iter()
            .filter(|row| !before.contains(&row.radacctid));
        match (added.next(), added.next()) {
            (Some(row), None) => {
                self.selected = Some(row.radacctid.clone());
                self.awaiting_session = None;
            }
            (Some(_), Some(_)) => {
                self.awaiting_session = None;
            } // An explicit selection is required.
            _ => {}
        }
    }
    fn unique_local_session(
        rows: &[OnlineSession],
        network: Option<&network::NetworkContext>,
    ) -> Option<String> {
        let ip = network?.ipv4.parse::<IpAddr>().ok()?;
        let mut matches = rows.iter().filter(|row| row.framedipaddress == Some(ip));
        let first = matches.next()?;
        if matches.next().is_some() {
            None
        } else {
            Some(first.radacctid.clone())
        }
    }
    pub async fn disconnect(&mut self, id: &str) -> Result<Snapshot, AppError> {
        self.paused = true;
        self.loss = None;
        // 用户显式下线：本地已不再持有会话，保护没有继续压着的理由。
        self.lift_eviction_protection();
        // 手动下线是显式释放路径：重连凭据在这里必须作废，任何保存方式都一样。
        self.credential = None;
        let result = match &self.api {
            Some(api) => api.disconnect_id(id).await,
            None => Err(AppError::Rejected),
        };
        if let Err(e) = result {
            self.logs.error(&e);
            if self.settings.credential_store == CredentialStore::Memory {
                self.clear();
            }
            self.status = "unknown".into();
            self.message = "远端下线未确认；请在认证后台检查。本地重连凭据已释放".into();
            return Err(e);
        }
        self.rows.retain(|r| r.radacctid != id);
        if self.selected.as_deref() == Some(id) {
            self.selected = None;
        }
        if self.settings.credential_store == CredentialStore::Memory {
            self.clear();
        }
        self.status = "offline".into();
        self.message = "目标会话已确认下线".into();
        Ok(self.snapshot())
    }
    /// Invoked by the app's worker, not by the WebView timer (works with window hidden).
    pub async fn tick(&mut self) {
        if self.api.is_none()
            || self.settings.refresh_policy == crate::settings::RefreshPolicy::Disabled
        {
            return;
        }
        if let Err(e) = self.refresh().await {
            self.logs.error(&e);
            // 已判定的会话丢失提示不能被一次网络抖动冲掉。
            if self.loss.is_none() {
                self.message = e.to_string();
            }
        }
    }
    /// Check the selected session on the short drop-detection cadence. This is
    /// intentionally separate from `tick`: it reads session presence but does
    /// not update accounting-rate samples.
    pub async fn redial_tick<F: Fn(Phase)>(&mut self, progress: F) {
        // 监测与重拨解耦：只要登录了后台就轮询会话，不因 `paused` 或
        // `reconnect_mode` 关闭而停；是否重拨由下面的条件单独决定。
        // backend-only 登录会置 `paused = true`，原实现因此永远看不到会话消失。
        if self.api.is_none() {
            return;
        }
        // 每次轮询前先确认 client 绑的还是当前网卡地址；换过地址就重建。
        self.rebind_changed_network();
        let rows = match self.api.as_ref() {
            Some(api) => api.sessions().await,
            None => return,
        };
        match rows {
            Ok(rows) => {
                self.rows = rows;
                self.bind_new_session();
                self.update_session_status();
                self.network_attempts = 0;
                self.last_poll_failure = None;
            }
            Err(error) => {
                // 请求失败只算"连接异常"的证据，不能据此断言任何会话消失；
                // 而且只有传输层失败/超时算本机异常，后台错误与解析失败不算。
                if Self::poll_failure_is_network(&error) {
                    self.last_poll_failure = Some(Instant::now());
                }
                self.logs.error(&error);
                if self.loss.is_none() {
                    self.message = error.to_string();
                }
                // 原实现到这里直接 return：missing 永远不会增加，掉线重拨的
                // 阈值也就永远达不到，于是网络恢复不了、重拨也永不触发。
                if Self::recoverable_poll_error(&error) {
                    self.retry_after_failure(progress).await;
                }
                return;
            }
        }
        if !self.auto_auth_allowed()
            || self.missing < 3
            || self.attempts >= 3
            || self.selected.is_none()
        {
            return;
        }
        if self
            .last_attempt
            .is_some_and(|t| t.elapsed() < self.retry_backoff())
        {
            return;
        }
        let Some(credential) = self.credential.take() else {
            return;
        };
        let retry_credential =
            Credential::new(credential.username.clone(), credential.password.clone());
        let terminate_first = self.settings.reconnect_mode == ReconnectMode::TerminateAndReconnect;
        let old_status = self.status.clone();
        let old_message = self.message.clone();
        let old_api = self.api.clone();
        let old_account = self.account.clone();
        let old_rows = self.rows.clone();
        let old_selected = self.selected.clone();
        let old_accounting = self.accounting.clone();
        let old_rates = self.latest_rates.clone();
        let old_paused = self.paused;
        let old_missing = self.missing;
        let old_loss = self.loss.clone();
        let old_hold = self.redial_hold;
        if terminate_first {
            self.clear();
        }
        self.last_attempt = Some(Instant::now());
        self.logs.record(crate::logging::Event::AutoRetry);
        self.attempts += 1;
        let attempts = self.attempts;
        let last = self.last_attempt;
        self.reconnecting = !terminate_first;
        let result = self.connect(credential, "", false, progress).await;
        self.reconnecting = false;
        self.attempts = attempts;
        self.last_attempt = last;
        match result {
            Ok(_) if self.selected.is_some() => {}
            Ok(_) => {
                self.logs.record(crate::logging::Event::RetryPaused);
                self.paused = true;
                self.message = "自动重拨未完成或无法绑定新会话，已暂停；请手动检查".into();
            }
            Err(error) if Self::transient_reconnect_error(&error) => {
                self.credential = Some(retry_credential);
                self.attempts = 0;
                if terminate_first {
                    self.api = old_api;
                    self.account = old_account;
                    self.rows = old_rows;
                    self.selected = old_selected;
                    self.accounting = old_accounting;
                    self.latest_rates = old_rates;
                    self.paused = old_paused;
                    self.missing = old_missing;
                    self.loss = old_loss;
                    self.redial_hold = old_hold;
                }
                self.status = old_status;
                self.message = format!(
                    "{}；网络暂时不可用，保留当前会话并等待下一次掉线检测",
                    old_message
                );
            }
            Err(_) => {
                // 暂停只是停止自动尝试；凭据仍留在进程内，供用户手动重试。
                self.credential = Some(retry_credential);
                self.logs.record(crate::logging::Event::RetryPaused);
                self.paused = true;
                self.message = "自动重拨被认证系统拒绝，已暂停；请手动检查".into();
            }
        }
    }

    /// 后台自动认证的统一准入：自动重拨与轮询失败后的自愈都必须先过这里。
    ///
    /// "可以继续查询"不等于"可以重新认证"：`redial_hold` 期间只允许轮询、
    /// 状态判定和日志，任何自动 Portal 登录都要挡住，否则一次可恢复的轮询
    /// 错误（网络错误或 `Rejected`）就能绕过「疑似被挤下线」的保护。
    /// 手动连接不走这里——那是用户显式恢复，允许解除保护。
    fn auto_auth_allowed(&self) -> bool {
        !self.redial_hold && !self.paused && self.settings.reconnect_mode != ReconnectMode::Disabled
    }

    fn transient_reconnect_error(error: &AppError) -> bool {
        matches!(
            error,
            AppError::Network(_)
                | AppError::Timeout(_)
                | AppError::DiscoveryTimeout
                | AppError::DiscoveryNotFound
                | AppError::Http(408 | 425 | 429 | 500..=599)
        )
    }

    /// 轮询失败是否值得起一次重建+重连。
    ///
    /// `Rejected` 也算：合盖一夜后后台 Cookie 过期同样会让每次轮询都失败，
    /// 而重连会用保存的凭据重新登录后台。真正被拒绝（密码错）时重连会失败，
    /// 由 `retry_after_failure` 收尾暂停。
    fn recoverable_poll_error(error: &AppError) -> bool {
        Self::transient_reconnect_error(error) || matches!(error, AppError::Rejected)
    }

    /// 重连尝试本身的失败是否属于“环境暂时不可用”。
    ///
    /// 比 `transient_reconnect_error` 多一类 `NetworkInterface`：网卡暂时消失
    /// （关 Wi-Fi、刚唤醒）是环境问题，不该像认证被拒那样停下来等人。
    fn retryable_connection_error(error: &AppError) -> bool {
        Self::transient_reconnect_error(error) || matches!(error, AppError::NetworkInterface(_))
    }

    /// 网络恢复的退避：30 秒起步，每次翻倍，最长 8 分钟。
    fn network_retry_backoff(&self) -> Duration {
        const MAX_SHIFT: u32 = 4;
        self.settings.poll_jitter.apply(Duration::from_secs(
            30 << self.network_attempts.min(MAX_SHIFT),
        ))
    }

    /// 轮询因网络/网卡失败后的自愈路径：用当前网卡上下文重建 client 并重连一次。
    ///
    /// 事务性：只有 `connect` 成功才替换现有会话与 client；失败时回滚到调用前
    /// 的状态，保留凭据等待下一个退避窗口，直到网络恢复为止。
    async fn retry_after_failure<F: Fn(Phase)>(&mut self, progress: F) {
        // 保护期间监测照常，但失败自愈不得顺手重新认证。这里挡的是全部
        // 可恢复错误（网络、超时、5xx、`Rejected`），不做错误类型特判。
        if !self.auto_auth_allowed() {
            return;
        }
        if self
            .last_attempt
            .is_some_and(|t| t.elapsed() < self.network_retry_backoff())
        {
            return;
        }
        let Some(credential) = self.credential.take() else {
            return;
        };
        let retry_credential =
            Credential::new(credential.username.clone(), credential.password.clone());
        let old_status = self.status.clone();
        let old_message = self.message.clone();
        let old_api = self.api.clone();
        let old_account = self.account.clone();
        let old_rows = self.rows.clone();
        let old_selected = self.selected.clone();
        let old_accounting = self.accounting.clone();
        let old_rates = self.latest_rates.clone();
        let old_paused = self.paused;
        let old_missing = self.missing;
        let old_bound = self.bound_network.clone();
        self.last_attempt = Some(Instant::now());
        self.logs.record(crate::logging::Event::NetworkRetry);
        self.reconnecting = true;
        let result = self.connect(credential, "", false, progress).await;
        self.reconnecting = false;
        match result {
            Ok(_) => {
                self.network_attempts = 0;
            }
            Err(error) if Self::retryable_connection_error(&error) => {
                self.network_attempts = self.network_attempts.saturating_add(1);
                self.credential = Some(retry_credential);
                self.api = old_api;
                self.account = old_account;
                self.rows = old_rows;
                self.selected = old_selected;
                self.accounting = old_accounting;
                self.latest_rates = old_rates;
                self.paused = old_paused;
                self.missing = old_missing;
                self.bound_network = old_bound;
                self.status = old_status;
                self.message = format!("{}；网络或网卡暂不可用，正在按退避自动重连", old_message);
            }
            Err(_) => {
                // 保留凭据：暂停只是"不再自动尝试"，不该顺手把用户保存的账号丢掉。
                self.credential = Some(retry_credential);
                self.logs.record(crate::logging::Event::RetryPaused);
                self.paused = true;
                self.message = "自动重连被认证系统拒绝，已暂停；请手动检查账号和网络".into();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::row;
    #[test]
    fn binds_unique_session_after_accounting_delay() {
        let mut c = Controller {
            awaiting_session: Some(["old".to_owned()].into_iter().collect()),
            rows: vec![row("old", 60, 0, 0)],
            ..Default::default()
        };
        c.bind_new_session();
        assert!(c.selected.is_none());
        assert!(c.awaiting_session.is_some());
        c.rows.push(row("new", 1, 0, 0));
        c.bind_new_session();
        assert_eq!(c.selected.as_deref(), Some("new"));
        assert!(c.awaiting_session.is_none());
    }
    #[test]
    fn multiple_new_sessions_require_explicit_selection() {
        let mut c = Controller {
            awaiting_session: Some(HashSet::new()),
            rows: vec![row("a", 1, 0, 0), row("b", 1, 0, 0)],
            ..Default::default()
        };
        c.bind_new_session();
        assert!(c.selected.is_none());
        assert!(c.awaiting_session.is_none());
    }
    fn context(ipv4: &str) -> network::NetworkContext {
        network::NetworkContext {
            interface_name: "en0".into(),
            ipv4: ipv4.into(),
            mac: "02:00:00:00:00:01".into(),
        }
    }
    /// 合盖唤醒后网卡地址变了：client 必须按新地址重建，且不丢后台 Cookie。
    /// 这是这次"日志疯狂刷 network.error"故障的核心回归测试。
    #[tokio::test]
    async fn changed_interface_address_rebuilds_client_without_relogin() {
        let mock = crate::tests::Mock::new("direct");
        let mut c = Controller::default();
        let client = PortalClient::new(&mock.base).unwrap();
        client.login("test-user", "test-password").await.unwrap();
        c.api = Some(client);
        c.bound_network = Some(context("192.0.2.1"));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        c.resolve_network = Arc::new(move |_| {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Some(context("127.0.0.1")))
        });
        assert!(c.rebind_changed_network());
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        let api = c.api.as_ref().unwrap();
        assert_eq!(
            api.bound_address(),
            Some(IpAddr::from([127, 0, 0, 1])),
            "应按新地址重新绑定"
        );
        // Cookie 还在，所以不需要重新登录就能继续查询。
        assert!(api.sessions().await.is_ok());
        // 地址没再变化就不该反复重建。
        assert!(!c.rebind_changed_network());
    }
    /// 网卡地址与后台会话的对应关系决定「自动选中本机会话」。
    ///
    /// 同一 IP 上有两条会话时代码不能替用户挑，必须留空等手动选：挑错就等于
    /// 把别人的设备下线或流量算到自己头上。
    #[test]
    fn local_session_binding_needs_an_unambiguous_ip_match() {
        // `tests::row` 造出的会话都带 IP 192.0.2.2，网卡上下文要对上才能匹配。
        let network = context("192.0.2.2");
        let rows = vec![crate::tests::row("a", 1, 0, 0)];
        assert_eq!(
            Controller::unique_local_session(&rows, Some(&network)),
            Some("a".into())
        );
        // 没有网卡上下文时无从判断，一律不自动绑定。
        assert_eq!(Controller::unique_local_session(&rows, None), None);
        // IP 对不上时不绑定。
        assert_eq!(
            Controller::unique_local_session(&rows, Some(&context("192.0.2.99"))),
            None
        );
        // 同一 IP 出现两条：不猜。
        let ambiguous = vec![
            crate::tests::row("a", 1, 0, 0),
            crate::tests::row("b", 1, 0, 0),
        ];
        assert_eq!(
            Controller::unique_local_session(&ambiguous, Some(&network)),
            None
        );
    }
    /// 连不上 portal 服务器时，文案要指出「不在校园网」和「服务器离线」这两个方向。
    ///
    /// 后台与 Portal 都在校园内网地址上，这两种情况占绝大多数；只说「请检查网络」
    /// 会让用户无从下手，所以这句是防呆设计的一部分，锁住以免被改回含糊的说法。
    #[test]
    fn unreachable_portal_names_the_two_likely_causes() {
        use crate::NetworkFault;
        for fault in [
            NetworkFault::Connect,
            NetworkFault::Timeout,
            NetworkFault::Other,
        ] {
            let message = AppError::Network(fault).to_string();
            assert!(message.contains("校园网"), "{message}");
            assert!(message.contains("离线"), "{message}");
        }
    }
    /// 轮询失败要能区分"环境暂时不可用"和"认证被拒"。
    #[test]
    fn classifies_poll_failures_for_automatic_recovery() {
        use crate::NetworkFault;
        assert!(Controller::recoverable_poll_error(&AppError::Network(
            NetworkFault::Connect
        )));
        assert!(Controller::recoverable_poll_error(&AppError::Rejected));
        assert!(!Controller::recoverable_poll_error(&AppError::InvalidUrl));
        assert!(Controller::retryable_connection_error(
            &AppError::NetworkInterface("no interface".into())
        ));
        assert!(!Controller::retryable_connection_error(&AppError::Rejected));
    }
    /// 网络/网卡不可用时的重连尝试必须可回滚：保住凭据和现有会话，只推进退避。
    #[tokio::test]
    async fn network_failure_keeps_credential_and_backs_off() {
        let mut c = Controller {
            settings: Settings {
                credential_store: CredentialStore::System,
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            // 已连线状态下后台轮询才会跑；默认构造是暂停态。
            paused: false,
            ..Default::default()
        };
        c.credential = Some(Credential::new("test-user".into(), "test-password".into()));
        c.resolve_network = Arc::new(|_| Err("no interface".into()));
        c.retry_after_failure(|_| {}).await;
        assert!(c.credential.is_some(), "凭据必须保留，否则永远无法自愈");
        assert!(!c.paused, "网卡暂时消失不该像认证被拒那样停下来");
        assert_eq!(c.network_attempts, 1);
        assert!(c.last_attempt.is_some());
        // 退避窗口内不再重复尝试。
        c.retry_after_failure(|_| {}).await;
        assert_eq!(c.network_attempts, 1);
    }
    /// 后台地址或网卡设置变了，已建立的连接要跟着变。
    #[test]
    fn settings_change_invalidates_existing_binding() {
        let mut c = Controller {
            bound_network: Some(context("192.0.2.1")),
            ..Default::default()
        };
        let settings = Settings {
            interface_name: "en1".into(),
            ..Default::default()
        };
        c.apply_settings(settings);
        assert!(c.bound_network.is_none(), "换网卡后应强制重新解析并重建");
    }
    /// 仅内存模式只是不落盘，不是"不能重连"：开了重连就必须在进程内保留凭据。
    #[tokio::test]
    async fn memory_login_keeps_credential_only_when_redial_is_enabled() {
        for (mode, retained) in [
            (ReconnectMode::NewSession, true),
            (ReconnectMode::TerminateAndReconnect, true),
            (ReconnectMode::Disabled, false),
        ] {
            let mock = crate::tests::Mock::new("direct");
            let mut c = Controller {
                settings: Settings {
                    server: mock.base.clone(),
                    credential_store: CredentialStore::Memory,
                    reconnect_mode: mode,
                    ..Default::default()
                },
                ..Default::default()
            };
            c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
            c.connect(
                Credential::new("test-user".into(), "test-password".into()),
                "",
                false,
                |_| {},
            )
            .await
            .unwrap();
            assert_eq!(c.credential.is_some(), retained, "{mode:?}");
            assert_eq!(c.paused, mode == ReconnectMode::Disabled, "{mode:?}");
            if retained {
                assert_eq!(c.credential.as_ref().unwrap().password, "test-password");
            }
        }
    }
    /// 会话消失只该累计缺失次数；重连开着时 API 与凭据要留到重拨真正发生。
    #[test]
    fn memory_session_loss_keeps_reconnect_state_when_redial_is_enabled() {
        let mut c = Controller {
            settings: Settings {
                credential_store: CredentialStore::Memory,
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            api: Some(PortalClient::new("http://127.0.0.1:9/lfradius/").unwrap()),
            credential: Some(Credential::new("test-user".into(), "test-password".into())),
            selected: Some("gone".into()),
            paused: false,
            ..Default::default()
        };
        for _ in 0..3 {
            c.update_session_status();
        }
        assert_eq!(c.missing, 3);
        assert!(c.api.is_some(), "重拨还需要这个 client");
        assert!(c.credential.is_some(), "重拨还需要这份进程内凭据");
        assert_eq!(c.status, "offline");
    }
    /// 重连关着时这份凭据没有任何后续用途，会话消失就该立刻释放。
    #[test]
    fn memory_session_loss_releases_state_when_redial_is_disabled() {
        let mut c = Controller {
            settings: Settings {
                credential_store: CredentialStore::Memory,
                reconnect_mode: ReconnectMode::Disabled,
                ..Default::default()
            },
            api: Some(PortalClient::new("http://127.0.0.1:9/lfradius/").unwrap()),
            credential: Some(Credential::new("test-user".into(), "test-password".into())),
            selected: Some("gone".into()),
            ..Default::default()
        };
        c.update_session_status();
        assert!(c.api.is_none());
        assert!(c.credential.is_none());
        assert!(c.paused);
        assert_eq!(c.missing, 0);
    }
    /// 三次缺失检测后必须真的发起重拨，并且用的是内存里留下的凭据。
    #[tokio::test]
    async fn memory_redial_fires_after_three_missing_checks() {
        let mock = crate::tests::Mock::new("direct");
        let settings = Settings {
            server: mock.base.clone(),
            credential_store: CredentialStore::Memory,
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        };
        let api = PortalClient::new(&settings.server).unwrap();
        api.login("test-user", "test-password").await.unwrap();
        let mut c = Controller {
            settings,
            api: Some(api),
            credential: Some(Credential::new("test-user".into(), "test-password".into())),
            selected: Some("gone".into()),
            paused: false,
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
        c.redial_tick(|_| {}).await;
        c.redial_tick(|_| {}).await;
        assert_eq!(c.missing, 2);
        assert_eq!(c.attempts, 0, "未到三次缺失不应重拨");
        assert!(c.credential.is_some());
        c.redial_tick(|_| {}).await;
        assert_eq!(c.attempts, 1);
        assert!(
            mock.requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.contains("cmcc_login_value")),
            "第三次缺失后应重新走一遍 Portal 登录"
        );
        assert!(c.credential.is_some(), "重拨后凭据仍在进程内");
        // 短时间内重复轮询会落在退避窗口里，不会追加尝试。
        c.redial_tick(|_| {}).await;
        assert_eq!(c.attempts, 1, "退避窗口内不应重复拨号");
    }
    /// 认证被拒后暂停自动重拨：不再自动尝试，但凭据留着手动重试。
    #[tokio::test]
    async fn rejected_redial_pauses_and_keeps_credential() {
        let mock = crate::tests::Mock::new("fake");
        let settings = Settings {
            server: mock.base.clone(),
            credential_store: CredentialStore::Memory,
            reconnect_mode: ReconnectMode::NewSession,
            ..Default::default()
        };
        let api = PortalClient::new(&settings.server).unwrap();
        api.login("test-user", "test-password").await.unwrap();
        let mut c = Controller {
            settings,
            api: Some(api),
            credential: Some(Credential::new("test-user".into(), "test-password".into())),
            selected: Some("gone".into()),
            paused: false,
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
        for _ in 0..3 {
            c.redial_tick(|_| {}).await;
        }
        assert_eq!(c.attempts, 1);
        assert!(c.paused, "认证被拒应暂停自动重拨");
        assert!(c.message.contains("已暂停"), "{}", c.message);
        assert!(c.credential.is_some(), "暂停不等于释放，凭据留给手动重试");
        c.redial_tick(|_| {}).await;
        assert_eq!(c.attempts, 1);
    }
    /// 手动下线与 Forget 都是显式释放：不论凭据存在哪里都必须作废。
    #[tokio::test]
    async fn manual_release_paths_clear_credentials() {
        let mut c = Controller {
            settings: Settings {
                credential_store: CredentialStore::Memory,
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            credential: Some(Credential::new("test-user".into(), "test-password".into())),
            ..Default::default()
        };
        // 没有可用 client 时下线会失败，但手动下线本身就该释放凭据。
        assert!(c.disconnect("gone").await.is_err());
        assert!(c.credential.is_none());
        assert!(c.paused);
        c.credential = Some(Credential::new("test-user".into(), "test-password".into()));
        c.forget();
        assert!(c.credential.is_none());
        assert!(c.api.is_none());
    }
    /// 关掉重连后，进程内不再保留没有用途的凭据。
    #[test]
    fn disabling_redial_drops_retained_credential() {
        let mut c = Controller {
            settings: Settings {
                credential_store: CredentialStore::Memory,
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            ..Default::default()
        };
        c.credential = Some(Credential::new("test-user".into(), "test-password".into()));
        c.apply_settings(Settings {
            credential_store: CredentialStore::Memory,
            reconnect_mode: ReconnectMode::Disabled,
            ..Default::default()
        });
        assert!(c.credential.is_none());
    }
    /// 认证成功、恰好新增一条会话：自动绑定，并给出不带附加条件的成功提示。
    #[tokio::test]
    async fn portal_success_adopts_a_single_new_session() {
        let mock = crate::tests::Mock::new("direct");
        mock.script_sessions(vec![
            vec![],
            vec![crate::tests::session_json("new", "192.0.2.2")],
        ]);
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                ..Default::default()
            },
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
        let snap = c
            .connect(
                Credential::new("test-user".into(), "test-password".into()),
                "",
                false,
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(snap.selected_id.as_deref(), Some("new"));
        assert!(snap.authenticated);
        assert_eq!(snap.message, "Portal 已确认成功");
        assert!(snap.rates.is_empty() || snap.rates.contains_key("new"));
    }
    /// 认证成功、但新增两条会话：认证状态保留，绝不替用户挑一条。
    ///
    /// 挑错的代价是流量显示成别人的，更糟的是 `update_session_status` 会把
    /// 别人的掉线当成自己掉线，进而触发自愈重连去动其他设备。
    #[tokio::test]
    async fn portal_success_with_multiple_new_sessions_asks_for_a_choice() {
        let mock = crate::tests::Mock::new("direct");
        mock.script_sessions(vec![
            vec![],
            vec![
                crate::tests::session_json("a", "192.0.2.2"),
                crate::tests::session_json("b", "192.0.2.3"),
            ],
        ]);
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
        let snap = c
            .connect(
                Credential::new("test-user".into(), "test-password".into()),
                "",
                false,
                |_| {},
            )
            .await
            .unwrap();
        assert!(snap.authenticated, "认证本身是成功的，不能被报成失败");
        assert_eq!(snap.selected_id, None, "多条新增时不得自动选第一条");
        assert!(
            snap.message.contains("尚未绑定管理会话"),
            "提示要说清没绑定：{}",
            snap.message
        );
        assert!(!snap.message.contains("失败"), "{}", snap.message);
        // 没有绑定就没有"本机会话消失"可言，缺失计数必须留在 0，
        // 否则会跨过 redial_tick 的 missing >= 3 阈值去重连。
        assert_eq!(c.missing, 0);
    }
    /// 认证成功、会计系统还没上报任何会话：保留成功状态，等后续轮询绑定。
    #[tokio::test]
    async fn portal_success_without_new_sessions_keeps_success() {
        let mock = crate::tests::Mock::new("direct");
        mock.script_sessions(vec![vec![], vec![]]);
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                ..Default::default()
            },
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
        let snap = c
            .connect(
                Credential::new("test-user".into(), "test-password".into()),
                "",
                false,
                |_| {},
            )
            .await
            .unwrap();
        assert!(snap.authenticated, "没有新会话不等于认证失败");
        assert_eq!(snap.selected_id, None, "也不能伪造已绑定状态");
        assert!(
            snap.message.contains("尚未绑定管理会话"),
            "{}",
            snap.message
        );
        // 保持等待态：后续轮询一旦看到唯一新增会话就能自动绑定。
        assert!(c.awaiting_session.is_some());
    }
    /// 仅登录后台（不认证）：沿用原有提示，要求用户自己选会话。
    #[tokio::test]
    async fn backend_only_login_asks_for_a_session_choice() {
        let mock = crate::tests::Mock::new("direct");
        mock.script_sessions(vec![
            vec![],
            vec![crate::tests::session_json("a", "192.0.2.2")],
        ]);
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                ..Default::default()
            },
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
        let snap = c
            .connect(
                Credential::new("test-user".into(), "test-password".into()),
                "",
                true,
                |_| {},
            )
            .await
            .unwrap();
        assert!(snap.authenticated);
        assert_eq!(snap.selected_id, None);
        assert_eq!(snap.message, "已登录用户后台；请选择要管理的会话");
    }
    /// 记录现有优先级：登录后台时就能按本机地址认出会话，会直接进
    /// `session_online` 并返回，不进入 Portal 认证与后续绑定流程。
    ///
    /// 因此"先按本机 IP 认领、再被新增会话覆盖"这条路径在到达 `bind_new_session`
    /// 之前就短路了；这里用测试固定住这个行为，避免以后修改提示时顺手改了绑定次序。
    #[tokio::test]
    async fn restored_local_session_short_circuits_before_binding() {
        let mock = crate::tests::Mock::new("direct");
        mock.script_sessions(vec![vec![crate::tests::session_json("local", "127.0.0.1")]]);
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
        let snap = c
            .connect(
                Credential::new("test-user".into(), "test-password".into()),
                "",
                false,
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(snap.selected_id.as_deref(), Some("local"));
        assert_eq!(snap.status, "session_online");
        assert_eq!(snap.message, "已恢复本机对应的在线会话");
    }
    /// 自动重拨（`reconnecting = true`）不会走 `clear()`；成功恢复本机会话时
    /// 上一轮的丢失结论必须一起失效，不能让旧告警盖住"已恢复"。
    #[tokio::test]
    async fn restored_local_session_clears_the_previous_loss() {
        let mock = crate::tests::Mock::new("direct");
        mock.script_sessions(vec![vec![crate::tests::session_json("local", "127.0.0.1")]]);
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            reconnecting: true,
            loss: Some(SessionLoss {
                id: "local".into(),
                kind: LossKind::Unclear,
            }),
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
        let snap = c
            .connect(
                Credential::new("test-user".into(), "test-password".into()),
                "",
                false,
                |_| {},
            )
            .await
            .unwrap();
        assert!(c.loss.is_none(), "成功恢复后旧告警不能残留");
        assert!(!c.redial_hold, "恢复连接后不应继续压着重拨");
        assert_eq!(
            snap.message, "已恢复本机对应的在线会话",
            "快照不能被上一轮的丢失文案盖住"
        );
    }
    /// 认证后按本机 IP 认出了会话，同时后台又多出一条别的会话：
    /// 新增的不止一条，不能把"本次认证产生的会话"猜成新增的那条。
    #[tokio::test]
    async fn restored_local_session_survives_an_extra_new_session() {
        let mock = crate::tests::Mock::new("direct");
        mock.script_sessions(vec![
            vec![],
            vec![
                crate::tests::session_json("local", "127.0.0.1"),
                crate::tests::session_json("other", "192.0.2.9"),
            ],
        ]);
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
        let snap = c
            .connect(
                Credential::new("test-user".into(), "test-password".into()),
                "",
                false,
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(snap.selected_id.as_deref(), Some("local"));
        assert_eq!(snap.status, "accepted");
        assert_eq!(snap.message, "Portal 已确认成功");
        assert_eq!(c.missing, 0);
    }
    /// 没有选中会话时，即使 `missing` 已被其他路径推高，也不能自动重拨：
    /// 空值不能被解释成"任选一条会话"。
    #[tokio::test]
    async fn unbound_controller_never_redials() {
        let mock = crate::tests::Mock::new("direct");
        let api = PortalClient::new(&mock.base).unwrap();
        api.login("test-user", "test-password").await.unwrap();
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            api: Some(api),
            credential: Some(Credential::new("test-user".into(), "test-password".into())),
            selected: None,
            missing: 3,
            paused: false,
            bound_network: Some(context("127.0.0.1")),
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
        c.redial_tick(|_| {}).await;
        assert_eq!(c.attempts, 0, "没有选中会话就不该发起重拨");
        assert!(c.credential.is_some(), "更不能消费凭据去重连");
        assert!(
            mock.requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| !request.contains("cmcc_login_value")),
            "未绑定会话时不得走 Portal 认证"
        );
    }
    /// 没有绑定会话时，后台列表怎么变都不算"本机会话掉线"。
    #[test]
    fn unbound_controller_never_reports_a_drop() {
        let mut c = Controller {
            api: Some(PortalClient::new("http://127.0.0.1:9/lfradius/").unwrap()),
            rows: vec![row("a", 1, 0, 0), row("b", 1, 0, 0)],
            selected: None,
            status: "accepted".into(),
            ..Default::default()
        };
        for _ in 0..5 {
            c.update_session_status();
        }
        assert_eq!(c.missing, 0, "没有选中会话就不该累计缺失次数");
        assert_eq!(c.status, "accepted", "状态不能被改写成 offline");
        assert!(c.selected.is_none(), "更不能顺手绑定其中一条");
    }
    fn session(id: &str, ip: &str) -> OnlineSession {
        OnlineSession {
            radacctid: id.into(),
            username: "test-user".into(),
            acctstarttime: String::new(),
            acctsessiontime: 60,
            framedipaddress: Some(ip.parse().unwrap()),
            acctinputoctets: 0,
            acctoutputoctets: 0,
        }
    }
    /// 本机地址固定为 192.0.2.1，后台会话用别的地址，模拟"本机 IP 不在列表"。
    fn observing(rows: Vec<OnlineSession>, selected: &str) -> Controller {
        Controller {
            rows,
            selected: Some(selected.into()),
            current_network: Some(context("192.0.2.1")),
            ..Default::default()
        }
    }
    fn crowded_elsewhere() -> Vec<OnlineSession> {
        vec![
            session("other-1", "203.0.113.11"),
            session("other-2", "203.0.113.12"),
            session("other-3", "203.0.113.13"),
        ]
    }
    /// 原会话还在，只是本机换了地址：不是丢失，绝不能提示被挤下线。
    #[test]
    fn changed_local_ip_with_session_present_is_not_a_loss() {
        let mut c = observing(vec![session("mine", "203.0.113.11")], "mine");
        c.update_session_status();
        assert_eq!(c.missing, 0);
        assert_eq!(c.status, "session_online");
        assert!(c.loss.is_none());
        assert!(!c.redial_hold);
    }
    /// 会话消失 + 本机 IP 不在列表 + 在线会话超过 2 条 + 连接异常 → 高疑似。
    #[test]
    fn suspected_eviction_needs_crowding_and_a_connection_fault() {
        let mut c = observing(crowded_elsewhere(), "mine");
        c.settings.reconnect_mode = ReconnectMode::NewSession;
        c.last_poll_failure = Some(Instant::now());
        c.update_session_status();
        assert_eq!(
            c.loss.as_ref().map(|loss| loss.kind),
            Some(LossKind::SuspectedEviction)
        );
        assert_eq!(c.loss.as_ref().unwrap().id, "mine");
        assert!(c.redial_hold, "高疑似时必须暂停自动抢线");
        assert!(
            c.loss_message(c.loss.as_ref().unwrap())
                .contains("疑似已被其他设备挤下线"),
            "{}",
            c.message
        );
        assert!(c.message.contains("自动重拨已暂停"), "{}", c.message);
    }
    /// 「连接异常」的证据只有两分钟时效：更早的失败不能再当作抢线依据。
    ///
    /// 这一项用 `Instant` 而非墙上时钟，所以窗口判断不会被系统时间调整影响。
    #[test]
    fn poll_failure_evidence_expires_after_two_minutes() {
        let mut stale = observing(crowded_elsewhere(), "mine");
        stale.settings.reconnect_mode = ReconnectMode::NewSession;
        stale.last_poll_failure = Some(Instant::now() - Duration::from_secs(121));
        stale.update_session_status();
        assert_eq!(
            stale.loss.as_ref().map(|loss| loss.kind),
            Some(LossKind::Unclear),
            "超过两分钟的历史失败不能升级成疑似被挤下线"
        );
        assert!(!stale.redial_hold, "证据过期就不该停掉重拨");

        let mut fresh = observing(crowded_elsewhere(), "mine");
        fresh.settings.reconnect_mode = ReconnectMode::NewSession;
        fresh.last_poll_failure = Some(Instant::now() - Duration::from_secs(119));
        fresh.update_session_status();
        assert_eq!(
            fresh.loss.as_ref().map(|loss| loss.kind),
            Some(LossKind::SuspectedEviction),
            "窗口内的失败仍算证据"
        );
        assert!(fresh.redial_hold);
    }
    /// 后台一条会话都没上报地址时，不能断言"本机 IP 不在列表里"。
    ///
    /// 那只是数据不全（`framedipaddress` 可以缺失），不是被挤下线的证据；
    /// 把它当成 `Some(false)` 会让「地址缺失 + 后台抖动」这种无关组合误报抢占。
    #[test]
    fn sessions_without_reported_addresses_are_not_eviction_evidence() {
        let rows: Vec<OnlineSession> = ["other-1", "other-2", "other-3"]
            .iter()
            .map(|id| OnlineSession {
                radacctid: (*id).into(),
                username: "test-user".into(),
                acctstarttime: String::new(),
                acctsessiontime: 60,
                framedipaddress: None,
                acctinputoctets: 0,
                acctoutputoctets: 0,
            })
            .collect();
        let mut c = observing(rows, "mine");
        c.settings.reconnect_mode = ReconnectMode::NewSession;
        c.last_poll_failure = Some(Instant::now());
        c.update_session_status();
        assert_eq!(
            c.loss.as_ref().map(|loss| loss.kind),
            Some(LossKind::Unclear),
            "地址缺失只能算证据不足，不能升级成疑似被挤下线"
        );
        assert!(!c.redial_hold, "证据不足时不该停掉自动重拨");
    }
    /// 同样的丢失，但只剩两条会话：不能升级成多设备抢占。
    #[test]
    fn session_loss_with_two_sessions_stays_unclear() {
        let mut c = observing(
            vec![
                session("other-1", "203.0.113.11"),
                session("other-2", "203.0.113.12"),
            ],
            "mine",
        );
        c.last_poll_failure = Some(Instant::now());
        c.update_session_status();
        assert_eq!(
            c.loss.as_ref().map(|loss| loss.kind),
            Some(LossKind::Unclear)
        );
        assert!(!c.redial_hold, "证据不足时不应停掉重拨");
        assert!(c.message.contains("无法确认"), "{}", c.message);
        assert!(!c.message.contains("挤下线。"), "{}", c.message);
    }
    /// 只剩一条别人的会话，同样只是"原因不明"。
    #[test]
    fn session_loss_with_one_session_stays_unclear() {
        let mut c = observing(vec![session("other-1", "203.0.113.11")], "mine");
        c.last_poll_failure = Some(Instant::now());
        c.update_session_status();
        assert_eq!(
            c.loss.as_ref().map(|loss| loss.kind),
            Some(LossKind::Unclear)
        );
        assert!(!c.redial_hold);
    }
    /// 三条会话里包含本机 IP：抢占证据不成立，哪怕列表很热闹。
    #[test]
    fn crowded_list_with_matching_ip_is_not_eviction() {
        let mut c = observing(
            vec![
                session("other-1", "203.0.113.11"),
                session("other-2", "203.0.113.12"),
                session("same-ip-other-id", "192.0.2.1"),
            ],
            "mine",
        );
        c.last_poll_failure = Some(Instant::now());
        c.update_session_status();
        assert_eq!(
            c.loss.as_ref().map(|loss| loss.kind),
            Some(LossKind::Unclear)
        );
        assert!(!c.redial_hold);
    }
    /// 原会话还在、列表又超过 2 条：只是"设备多"，不是被挤。
    #[test]
    fn crowded_list_with_selected_session_present_never_alerts() {
        let mut rows = crowded_elsewhere();
        rows.push(session("mine", "192.0.2.1"));
        let mut c = observing(rows, "mine");
        c.last_poll_failure = Some(Instant::now());
        c.update_session_status();
        assert!(c.loss.is_none());
        assert_eq!(c.status, "session_online");
    }
    /// 解析不到本机 IP 时证据不足：只能给"原因不明"。
    #[test]
    fn unknown_local_ip_never_counts_as_eviction() {
        let mut c = observing(crowded_elsewhere(), "mine");
        c.current_network = None;
        c.last_poll_failure = Some(Instant::now());
        c.update_session_status();
        assert_eq!(
            c.loss.as_ref().map(|loss| loss.kind),
            Some(LossKind::Unclear)
        );
        assert!(!c.redial_hold);
    }
    /// 查询失败不能被当成"所有会话都消失了"。
    #[tokio::test]
    async fn failed_poll_does_not_claim_a_loss() {
        let mut c = Controller {
            settings: Settings {
                server: "http://127.0.0.1:9/lfradius/".into(),
                ..Default::default()
            },
            api: Some(PortalClient::new("http://127.0.0.1:9/lfradius/").unwrap()),
            selected: Some("mine".into()),
            bound_network: Some(context("192.0.2.1")),
            current_network: Some(context("192.0.2.1")),
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
        c.redial_tick(|_| {}).await;
        assert!(c.loss.is_none(), "请求失败时没有任何会话消失的证据");
        assert_eq!(c.missing, 0, "缺失计数只在成功列表上推进");
        assert!(c.last_poll_failure.is_some(), "失败要留作后续连接异常证据");
        assert!(!c.message.contains("消失"), "{}", c.message);
    }
    /// backend-only 登录会置 `paused = true`；监测不能再被它挡住。
    #[tokio::test]
    async fn paused_backend_only_session_still_monitors() {
        let mock = crate::tests::Mock::new("direct");
        let api = PortalClient::new(&mock.base).unwrap();
        api.login("test-user", "test-password").await.unwrap();
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            api: Some(api),
            selected: Some("mine".into()),
            paused: true,
            bound_network: Some(context("192.0.2.1")),
            current_network: Some(context("192.0.2.1")),
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
        c.redial_tick(|_| {}).await;
        assert_eq!(c.missing, 1, "暂停重拨不等于停止监测");
        assert!(c.loss.is_some(), "会话消失必须被发现");
        assert_eq!(c.attempts, 0, "暂停状态下不能自动重拨");
    }
    /// 重连关闭时仍要监测；否则用户看不到会话已经掉了。
    #[tokio::test]
    async fn reconnect_disabled_still_monitors() {
        let mock = crate::tests::Mock::new("direct");
        let api = PortalClient::new(&mock.base).unwrap();
        api.login("test-user", "test-password").await.unwrap();
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                ..Default::default()
            },
            api: Some(api),
            selected: Some("mine".into()),
            paused: true,
            bound_network: Some(context("192.0.2.1")),
            current_network: Some(context("192.0.2.1")),
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
        c.redial_tick(|_| {}).await;
        assert_eq!(c.missing, 1);
        assert!(c.loss.is_some());
        assert_eq!(c.attempts, 0);
    }
    /// 疑似被挤后即使连续缺失三次，也不能自动重拨去抢别人的线。
    #[tokio::test]
    async fn eviction_hold_blocks_repeated_redial() {
        let mock = crate::tests::Mock::new("direct");
        mock.script_sessions(vec![crowded_elsewhere()
            .iter()
            .map(|row| crate::tests::session_json(&row.radacctid, "203.0.113.11"))
            .collect()]);
        let api = PortalClient::new(&mock.base).unwrap();
        api.login("test-user", "test-password").await.unwrap();
        let mut c = Controller {
            settings: Settings {
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            api: Some(api),
            credential: Some(Credential::new("test-user".into(), "test-password".into())),
            selected: Some("mine".into()),
            paused: false,
            bound_network: Some(context("192.0.2.1")),
            current_network: Some(context("192.0.2.1")),
            last_poll_failure: Some(Instant::now()),
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
        for _ in 0..5 {
            c.redial_tick(|_| {}).await;
        }
        assert!(c.redial_hold);
        assert_eq!(c.attempts, 0, "疑似被挤下线后不得自动抢线");
        assert!(
            !mock
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.contains("cmcc_login_value")),
            "不应重新走 Portal 认证"
        );
    }
    /// 同一告警只在状态变化时写一次日志，不随每轮轮询刷屏。
    #[test]
    fn suspected_eviction_is_logged_once() {
        let logs = crate::logging::LogBuffer::default();
        logs.set_enabled(true);
        let mut c = Controller {
            settings: Settings {
                log_enabled: true,
                ..Default::default()
            },
            logs: logs.clone(),
            rows: crowded_elsewhere(),
            selected: Some("mine".into()),
            current_network: Some(context("192.0.2.1")),
            last_poll_failure: Some(Instant::now()),
            ..Default::default()
        };
        for _ in 0..5 {
            c.update_session_status();
        }
        let hits = logs
            .entries()
            .iter()
            .filter(|entry| entry.code == "session.evicted_suspected")
            .count();
        assert_eq!(hits, 1, "同一目标丢失只该留下一条告警");
        assert!(
            logs.entries()
                .iter()
                .all(|entry| !entry.message.contains("已经确认")),
            "日志不能把推测写成事实"
        );
    }
    /// Memory + 未开重拨会立刻释放本地态，但告警文案与目标 ID 必须留下。
    #[test]
    fn memory_release_keeps_the_loss_notice() {
        let mut c = Controller {
            settings: Settings {
                credential_store: CredentialStore::Memory,
                reconnect_mode: ReconnectMode::Disabled,
                ..Default::default()
            },
            api: Some(PortalClient::new("http://127.0.0.1:9/lfradius/").unwrap()),
            credential: Some(Credential::new("test-user".into(), "test-password".into())),
            rows: crowded_elsewhere(),
            selected: Some("mine".into()),
            current_network: Some(context("192.0.2.1")),
            last_poll_failure: Some(Instant::now()),
            ..Default::default()
        };
        c.update_session_status();
        assert!(c.api.is_none() && c.selected.is_none(), "本地态应被释放");
        let snap = c.snapshot();
        assert!(
            snap.message.contains("疑似已被其他设备挤下线"),
            "{}",
            snap.message
        );
        assert_eq!(c.loss.as_ref().unwrap().id, "mine", "判断依据不能被清理掉");
    }
    /// 丢失后后台又冒出一条新会话：不能自动认领，它更可能是挤掉本机的设备。
    #[test]
    fn new_session_after_loss_is_not_auto_bound() {
        let mut c = Controller {
            rows: vec![session("someone-else", "203.0.113.11")],
            selected: None,
            awaiting_session: Some(HashSet::new()),
            loss: Some(SessionLoss {
                id: "mine".into(),
                kind: LossKind::Unclear,
            }),
            current_network: Some(context("192.0.2.1")),
            ..Default::default()
        };
        c.bind_new_session();
        assert!(c.selected.is_none(), "不能把别人的会话当成本机");
        assert!(c.awaiting_session.is_some(), "等待态交给用户显式处理");
    }
    /// 手动重新连接是唯一的恢复入口：旧告警与抢占暂停都要失效。
    #[tokio::test]
    async fn manual_reconnect_clears_the_eviction_hold() {
        let mock = crate::tests::Mock::new("direct");
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            loss: Some(SessionLoss {
                id: "mine".into(),
                kind: LossKind::SuspectedEviction,
            }),
            redial_hold: true,
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("127.0.0.1"))));
        c.connect(
            Credential::new("test-user".into(), "test-password".into()),
            "",
            false,
            |_| {},
        )
        .await
        .unwrap();
        assert!(c.loss.is_none(), "旧结论不能残留到新连接");
        assert!(!c.redial_hold, "手动重连后应恢复正常的重拨逻辑");
    }
    /// 选择会话只是换了管理目标，不等于本机已经安全上线。
    ///
    /// 「疑似被挤下线」期间的自动重拨保护不能被一次普通 UI 选择解除：用户
    /// 随手点中的那条很可能是挤掉本机的设备，恢复重拨会用本机账号再认证一次，
    /// 反而可能把对方挤下去。要恢复只能显式重新连接。
    #[test]
    fn selecting_a_session_does_not_lift_the_eviction_hold() {
        let mut c = Controller {
            settings: Settings {
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            rows: crowded_elsewhere(),
            selected: Some("mine".into()),
            current_network: Some(context("192.0.2.1")),
            last_poll_failure: Some(Instant::now()),
            ..Default::default()
        };
        c.update_session_status();
        assert_eq!(
            c.loss.as_ref().map(|loss| loss.kind),
            Some(LossKind::SuspectedEviction),
            "前置条件：应当已判定为疑似被挤下线"
        );
        assert!(c.redial_hold);
        c.select("other-1").unwrap();
        assert!(c.loss.is_none(), "换目标后旧目标的告警应当消失");
        assert!(c.redial_hold, "选择会话不得解除疑似被挤下线期间的重拨保护");
    }
    /// 低频刷新也要判定，但不能替代 5 秒 worker 的缺失计数。
    #[tokio::test]
    async fn refresh_observes_without_advancing_missing() {
        let mock = crate::tests::Mock::new("direct");
        mock.script_sessions(vec![vec![
            crate::tests::session_json("other-1", "203.0.113.11"),
            crate::tests::session_json("other-2", "203.0.113.12"),
            crate::tests::session_json("other-3", "203.0.113.13"),
        ]]);
        let api = PortalClient::new(&mock.base).unwrap();
        api.login("test-user", "test-password").await.unwrap();
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            api: Some(api),
            selected: Some("mine".into()),
            bound_network: Some(context("192.0.2.1")),
            current_network: Some(context("192.0.2.1")),
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
        c.refresh().await.unwrap();
        assert_eq!(c.missing, 0, "计数只由 5 秒 worker 推进");
        assert!(c.loss.is_some(), "刷新仍要更新丢失状态");
        assert_eq!(c.status, "offline");
    }
    /// 三次以上监测都不应把多条会话按顺序绑定给本机。
    #[test]
    fn multiple_new_sessions_are_never_bound_in_list_order() {
        let mut c = Controller {
            rows: vec![
                session("first", "203.0.113.11"),
                session("second", "203.0.113.12"),
                session("third", "203.0.113.13"),
            ],
            awaiting_session: Some(HashSet::new()),
            current_network: Some(context("203.0.113.11")),
            ..Default::default()
        };
        c.bind_new_session();
        assert!(c.selected.is_none(), "多条新增时必须让用户自己选");
    }
    fn portal_auth_requests(mock: &crate::tests::Mock) -> usize {
        mock.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| {
                request.contains("user_login") || request.contains("cmcc_login_value")
            })
            .count()
    }
    /// 保护不是"这一轮列表里还有选中会话"就能解除的。
    ///
    /// 从原会话丢失 → 改选另一条在线会话 → 成功轮询若干次 → 新目标也消失，
    /// 全程都不允许自动重拨，也不允许出现一次 Portal 认证请求。
    #[tokio::test]
    async fn eviction_hold_survives_polls_and_a_second_loss() {
        let mock = crate::tests::Mock::new("direct");
        let crowded = vec![
            crate::tests::session_json("other-1", "203.0.113.11"),
            crate::tests::session_json("other-2", "203.0.113.12"),
        ];
        mock.script_sessions(vec![
            crowded.clone(),
            crowded,
            vec![crate::tests::session_json("other-2", "203.0.113.12")],
        ]);
        let api = PortalClient::new(&mock.base).unwrap();
        api.login("test-user", "test-password").await.unwrap();
        let auth_before = portal_auth_requests(&mock);
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            api: Some(api),
            credential: Some(Credential::new("test-user".into(), "test-password".into())),
            selected: Some("mine".into()),
            loss: Some(SessionLoss {
                id: "mine".into(),
                kind: LossKind::SuspectedEviction,
            }),
            redial_hold: true,
            paused: false,
            bound_network: Some(context("192.0.2.1")),
            current_network: Some(context("192.0.2.1")),
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
        c.redial_tick(|_| {}).await;
        assert!(c.redial_hold, "原会话仍缺失时保护不能被轮询清掉");
        c.select("other-1").unwrap();
        assert!(c.redial_hold, "切换会话不能解除保护");
        c.redial_tick(|_| {}).await;
        assert_eq!(c.status, "session_online");
        assert!(c.redial_hold, "成功轮询看到选中会话在线也不能解除保护");
        let snap = c.snapshot();
        assert!(snap.background_paused, "保护生效时后台必须保持暂停");
        assert!(
            snap.message.contains("自动重拨仍未恢复"),
            "提示必须说明保护仍在：{}",
            snap.message
        );
        for _ in 0..4 {
            c.redial_tick(|_| {}).await;
        }
        assert!(c.missing >= 3, "前置条件：新目标也应累计到重拨阈值");
        assert!(c.redial_hold);
        assert_eq!(c.attempts, 0, "保护期间不得自动重拨");
        assert!(c.credential.is_some(), "凭据不能被消费");
        assert!(c.api.is_some(), "监测不能停");
        assert_eq!(
            portal_auth_requests(&mock),
            auth_before,
            "保护期间不得发起任何 Portal 认证"
        );
    }
    /// 保护期间遇到真实传输层失败：监测继续，失败自愈不得重新认证。
    #[tokio::test]
    async fn eviction_hold_blocks_recovery_after_network_poll_failure() {
        let mock = crate::tests::Mock::new("direct");
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            // 打不通的本地端口：模拟传输层失败，不依赖外部 Portal。
            api: Some(PortalClient::new("http://127.0.0.1:9/lfradius/").unwrap()),
            credential: Some(Credential::new("test-user".into(), "test-password".into())),
            selected: Some("mine".into()),
            loss: Some(SessionLoss {
                id: "mine".into(),
                kind: LossKind::SuspectedEviction,
            }),
            redial_hold: true,
            paused: false,
            bound_network: Some(context("192.0.2.1")),
            current_network: Some(context("192.0.2.1")),
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
        c.redial_tick(|_| {}).await;
        assert!(c.api.is_some(), "保护期间监测不能停");
        assert!(!c.paused, "保护期间不能把后台监测关掉");
        assert_eq!(c.attempts, 0, "失败自愈不得发起重拨");
        assert_eq!(c.network_attempts, 0, "失败自愈根本不应启动");
        assert!(c.credential.is_some(), "凭据不能被消费");
        assert!(c.redial_hold, "错误不应解除保护");
        assert!(
            c.last_poll_failure.is_some(),
            "传输层失败仍应作为本机连接异常的证据"
        );
        assert_eq!(portal_auth_requests(&mock), 0, "不得发起 Portal 认证");
        assert!(
            mock.requests.lock().unwrap().is_empty(),
            "保护期间不应向后台发送任何请求"
        );
    }
    /// 保护期间遇到后台拒绝（Cookie 失效）同样不能借自愈重新认证；
    /// 这类错误也不能记成本机连接异常。
    #[tokio::test]
    async fn eviction_hold_blocks_recovery_after_rejected_poll() {
        let mock = crate::tests::Mock::new("direct");
        // 不登录：onlinelog 返回 success:0，客户端映射为 Rejected。
        let api = PortalClient::new(&mock.base).unwrap();
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            api: Some(api),
            credential: Some(Credential::new("test-user".into(), "test-password".into())),
            selected: Some("mine".into()),
            loss: None,
            redial_hold: true,
            paused: false,
            bound_network: Some(context("192.0.2.1")),
            current_network: Some(context("192.0.2.1")),
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
        c.redial_tick(|_| {}).await;
        assert!(c.api.is_some(), "保护期间监测不能停");
        assert!(!c.paused, "保护期间不能把后台监测关掉");
        assert_eq!(c.attempts, 0);
        assert!(c.credential.is_some(), "凭据不能被消费");
        assert!(c.redial_hold);
        assert!(
            c.last_poll_failure.is_none(),
            "业务拒绝不能当成本机网络异常的证据"
        );
        assert_eq!(portal_auth_requests(&mock), 0, "不得发起 Portal 认证");
        assert!(
            mock.requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.contains("onlinelog")),
            "只允许继续查询会话"
        );
    }
    /// 本机地址判定必须是保守三态：未知不等于不匹配。
    #[test]
    fn local_ip_lookup_is_tri_state() {
        let c = Controller {
            current_network: Some(context("192.0.2.1")),
            ..Default::default()
        };
        let unknown = |id: &str| {
            let mut row = session(id, "203.0.113.99");
            row.framedipaddress = None;
            row
        };
        assert_eq!(c.local_ip_in(&[session("a", "192.0.2.1")]), Some(true));
        assert_eq!(c.local_ip_in(&crowded_elsewhere()), Some(false));
        assert_eq!(c.local_ip_in(&[unknown("a"), unknown("b")]), None);
        assert_eq!(
            c.local_ip_in(&[session("a", "203.0.113.11"), unknown("b")]),
            None,
            "存在未知地址时不能断言本机不在列表"
        );
        assert_eq!(
            c.local_ip_in(&[unknown("a"), session("b", "192.0.2.1")]),
            Some(true),
            "命中优先于未知"
        );
        assert_eq!(c.local_ip_in(&[]), Some(false), "空列表没有任何未知项");
    }
    /// 部分会话缺地址时，「本机 IP 不在列表」不成立，只能降级成原因不明。
    #[test]
    fn partial_unknown_addresses_downgrade_eviction_to_unclear() {
        let mut c = observing(
            vec![
                session("other-1", "203.0.113.11"),
                session("other-2", "203.0.113.12"),
                {
                    let mut row = session("other-3", "203.0.113.13");
                    row.framedipaddress = None;
                    row
                },
            ],
            "mine",
        );
        c.settings.reconnect_mode = ReconnectMode::NewSession;
        c.last_poll_failure = Some(Instant::now());
        c.update_session_status();
        assert_eq!(
            c.loss.as_ref().map(|loss| loss.kind),
            Some(LossKind::Unclear),
            "证据不完整时不能判成疑似被挤下线"
        );
        assert!(!c.redial_hold, "原因不明时不应停掉自动重拨");
    }
    /// 只有传输层失败/超时能当作本机连接异常；后台、登录态、解析错误都不算。
    #[test]
    fn only_transport_failures_are_connection_evidence() {
        assert!(Controller::poll_failure_is_network(&AppError::Network(
            crate::NetworkFault::Connect
        )));
        assert!(Controller::poll_failure_is_network(&AppError::Network(
            crate::NetworkFault::Timeout
        )));
        assert!(Controller::poll_failure_is_network(&AppError::Timeout(
            "下线确认"
        )));
        for error in [
            AppError::Http(500),
            AppError::Http(503),
            AppError::Rejected,
            AppError::InvalidResponse("后台会话列表缺少字段"),
            AppError::Redirect,
        ] {
            assert!(
                !Controller::poll_failure_is_network(&error),
                "{error:?} 不能当作本机连接异常"
            );
        }
    }
    /// 成功轮询后，之前的失败证据要清掉，不能继续留着升级下一次判定。
    #[tokio::test]
    async fn successful_poll_clears_the_failure_evidence() {
        let mock = crate::tests::Mock::new("direct");
        mock.script_sessions(vec![vec![crate::tests::session_json("mine", "192.0.2.1")]]);
        let api = PortalClient::new(&mock.base).unwrap();
        api.login("test-user", "test-password").await.unwrap();
        let mut c = Controller {
            settings: Settings {
                server: mock.base.clone(),
                reconnect_mode: ReconnectMode::NewSession,
                ..Default::default()
            },
            api: Some(api),
            selected: Some("mine".into()),
            last_poll_failure: Some(Instant::now()),
            paused: false,
            bound_network: Some(context("192.0.2.1")),
            current_network: Some(context("192.0.2.1")),
            ..Default::default()
        };
        c.resolve_network = Arc::new(|_| Ok(Some(context("192.0.2.1"))));
        c.redial_tick(|_| {}).await;
        assert!(c.last_poll_failure.is_none(), "成功轮询后旧证据必须失效");
        assert_eq!(c.missing, 0);
        assert_eq!(c.status, "session_online");
    }
}
