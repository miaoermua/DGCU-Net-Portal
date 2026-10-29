//! 控制器：状态模型与共享原语。
//!
//! 这里只保留 `Controller` 的字段、`Snapshot`、会话丢失告警、凭据与连接清理
//! 这些被多条流程共用的部分。连接生命周期在 `connection`，轮询与会话观察在
//! `session`，重拨调度与失败回滚在 `reconnect`。
use crate::{
    network,
    settings::{CredentialStore, ReconnectMode, Settings},
    traffic::{AccountingRates, Rate},
    AccountInfo, AppError, Credential, OnlineSession, PortalClient,
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};

mod connection;
mod reconnect;
mod session;

#[cfg(test)]
mod tests;

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
    /// 会话掉线重拨的连续失败次数；成功绑定会话或显式释放后归零。
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

    /// 重新解析网卡；地址变了就用新地址重建 client。返回是否发生了重建。
    ///
    /// 合盖唤醒、Wi-Fi 漫游、插拔网线都会换掉 IPv4，而 `local_address` 是钉在
    /// client 上的：不重建就会对每个请求立刻失败。Cookie 在共享 jar 里，重建
    /// 不影响登录态，也不需要重新走认证流程。
    ///
    /// 轮询（`session::refresh`）与重拨（`reconnect::redial_tick`）都要用它，
    /// 所以放在共享原语层，避免两个兄弟模块互相依赖。
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
}

impl Controller {
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
}
