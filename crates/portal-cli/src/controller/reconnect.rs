//! 重拨调度与失败回滚。
//!
//! `redial_tick` 处理会话掉线后的自动重拨，`retry_after_failure` 处理轮询
//! 因网络/网卡失败后的自愈；两条路径共用 `ReconnectCheckpoint` 与
//! `retryable_connection_error` 的错误分类。
use super::{Controller, SessionLoss};
use crate::{
    cmcc::Phase,
    network,
    settings::ReconnectMode,
    traffic::{AccountingRates, Rate},
    AccountInfo, AppError, Credential, OnlineSession, PortalClient,
};
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

/// 一次自动重连尝试开始前的会话可见状态，用于失败时整体退回。
///
/// 只保存 `connect()` 在返回 `Err` 之前可能改写、且失败后必须退回的字段。刻意不保存：
///
/// - `credential` / `attempts` / `last_attempt` / `network_attempts`：重试调度由调用方
///   在 `connect()` 返回后决定（是否归还凭据、是否清零计数），快照恢复会覆盖这些决策；
/// - `reconnecting`：调用方在 `connect()` 前后自行置位与清理；
/// - `current_network` / `last_poll_failure`：失败后必须保留的新证据。回滚它们会把
///   “刚解析到的网卡地址”“刚刚发生过传输层失败”抹掉；
/// - `settings` / `logs` / `resolve_network`：配置、审计与外部环境，不属于一次连接事务。
///
/// 恢复只改内存，不能撤销已经发出的 HTTP 请求、已完成的 Portal 认证或服务端会话变化。
///
/// 并发约束：`daemon` 用 `Arc<tokio::sync::Mutex<Controller>>` 串行化所有入口，且
/// `connect()` 在整个 `await` 期间持有 `&mut Controller`，因此快照存续期间不会插入
/// `select` / `disconnect` 等用户操作，`restore` 不会覆盖异步期间的新状态。
pub(super) struct ReconnectCheckpoint {
    api: Option<PortalClient>,
    account: Option<AccountInfo>,
    rows: Vec<OnlineSession>,
    selected: Option<String>,
    awaiting_session: Option<HashSet<String>>,
    accounting: AccountingRates,
    latest_rates: HashMap<String, Rate>,
    paused: bool,
    status: String,
    message: String,
    missing: u32,
    loss: Option<SessionLoss>,
    redial_hold: bool,
    bound_network: Option<network::NetworkContext>,
}
impl ReconnectCheckpoint {
    /// 对 `Controller` 做穷尽解构：新增字段时必须在这里显式选择“恢复/保留”，
    /// 不会因为用了 `..` 而悄悄漏掉。
    pub(super) fn capture(c: &Controller) -> Self {
        let Controller {
            settings: _,
            logs: _,
            api,
            credential: _,
            account,
            rows,
            selected,
            accounting,
            latest_rates,
            paused,
            status,
            message,
            missing,
            attempts: _,
            network_attempts: _,
            last_attempt: _,
            awaiting_session,
            reconnecting: _,
            loss,
            redial_hold,
            current_network: _,
            last_poll_failure: _,
            bound_network,
            resolve_network: _,
        } = c;
        Self {
            api: api.clone(),
            account: account.clone(),
            rows: rows.clone(),
            selected: selected.clone(),
            awaiting_session: awaiting_session.clone(),
            accounting: accounting.clone(),
            latest_rates: latest_rates.clone(),
            paused: *paused,
            status: status.clone(),
            message: message.clone(),
            missing: *missing,
            loss: loss.clone(),
            redial_hold: *redial_hold,
            bound_network: bound_network.clone(),
        }
    }
    /// 瞬时失败（网络/超时）后把本地连接状态退回尝试前。只改内存：已经发出的
    /// HTTP 请求或服务端认证副作用撤销不了；调度字段与失败提示由调用方另写。
    pub(super) fn restore(self, c: &mut Controller) {
        c.api = self.api;
        c.account = self.account;
        c.rows = self.rows;
        c.selected = self.selected;
        c.awaiting_session = self.awaiting_session;
        c.accounting = self.accounting;
        c.latest_rates = self.latest_rates;
        c.paused = self.paused;
        c.status = self.status;
        c.message = self.message;
        c.missing = self.missing;
        c.loss = self.loss;
        c.redial_hold = self.redial_hold;
        c.bound_network = self.bound_network;
    }
    /// 认证被拒后的部分回滚：调用方要把 `status` / `message` / `paused` 改成
    /// “已停止自动尝试”，这些新决策必须保留；但旧连接仍在手上，`api`、会话列表
    /// 与 `bound_network`、`account` 不能停留在半途换过、随后又被丢弃的新连接上。
    pub(super) fn restore_connection(self, c: &mut Controller) {
        let Self {
            api,
            account,
            rows,
            selected,
            awaiting_session,
            accounting,
            latest_rates,
            missing,
            bound_network,
            paused: _,
            status: _,
            message: _,
            loss: _,
            redial_hold: _,
        } = self;
        c.api = api;
        c.account = account;
        c.rows = rows;
        c.selected = selected;
        c.awaiting_session = awaiting_session;
        c.accounting = accounting;
        c.latest_rates = latest_rates;
        c.missing = missing;
        c.bound_network = bound_network;
    }
}
impl Controller {
    /// 自动重拨的退避：30 秒 × 2^attempts（连续失败次数），并叠加抖动。
    fn retry_backoff(&self) -> Duration {
        self.settings
            .poll_jitter
            .apply(Duration::from_secs(30 * (1 << self.attempts)))
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
        // 先拍快照再 `clear()`：TerminateAndReconnect 会立刻丢掉旧连接，网络
        // 抖动时要把这份状态整体退回去；NewSession 不 `clear()`，但 `connect()`
        // 仍可能改写 `bound_network` / `account`，失败后同样不能留在半途。
        let checkpoint = ReconnectCheckpoint::capture(self);
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
            Ok(_) if self.selected.is_some() => {
                // `selected` 在 NewSession 下可能只是重拨前遗留的目标：只有它
                // 真的出现在本轮会话列表里，才算"已绑定有效会话"。这时清零
                // 连续失败计数，下一次掉线才能重新从 30 秒退避开始，而不是被
                // 历次成功重拨累加到 3 次上限后永久挡住。
                if self
                    .selected
                    .as_ref()
                    .is_some_and(|id| self.rows.iter().any(|row| &row.radacctid == id))
                {
                    self.attempts = 0;
                }
            }
            Ok(_) => {
                self.logs.record(crate::logging::Event::RetryPaused);
                self.paused = true;
                self.message = "自动重拨未完成或无法绑定新会话，已暂停；请手动检查".into();
            }
            // 与 `retry_after_failure` 使用同一分类：网卡暂时消失
            // （`NetworkInterface`）是环境问题，不能落进下面的"认证被拒"分支。
            Err(error) if Self::retryable_connection_error(&error) => {
                self.credential = Some(retry_credential);
                self.attempts = 0;
                // 恢复快照里的本地连接状态；它只改内存，不会撤销已经发出的
                // HTTP 请求或服务端认证副作用。`terminate_first` 与 `NewSession`
                // 在这里语义相同：失败都不该改变已有会话。
                checkpoint.restore(self);
                self.message = format!(
                    "{}；网络暂时不可用，保留当前会话并等待下一次掉线检测",
                    self.message
                );
            }
            Err(_) => {
                // 暂停只是停止自动尝试；凭据仍留在进程内，供用户手动重试。
                self.credential = Some(retry_credential);
                self.logs.record(crate::logging::Event::RetryPaused);
                // 认证被拒：`paused` / `status` / `message` 是新的决策，不能回滚；
                // 但 `TerminateAndReconnect` 已经 `clear()`，保留清空后的状态。
                // `NewSession` 的旧连接还在，`api`/`account`/`bound_network` 必须
                // 退回去，否则它们会指向一个认证失败后被丢弃的新连接。
                if !terminate_first {
                    checkpoint.restore_connection(self);
                }
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
    pub(super) fn recoverable_poll_error(error: &AppError) -> bool {
        Self::transient_reconnect_error(error) || matches!(error, AppError::Rejected)
    }

    /// 重连尝试本身的失败是否属于“环境暂时不可用”。
    ///
    /// 比 `transient_reconnect_error` 多一类 `NetworkInterface`：网卡暂时消失
    /// （关 Wi-Fi、刚唤醒）是环境问题，不该像认证被拒那样停下来等人。
    pub(super) fn retryable_connection_error(error: &AppError) -> bool {
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
    pub(super) async fn retry_after_failure<F: Fn(Phase)>(&mut self, progress: F) {
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
        // 失败自愈不 `clear()`，但 `connect()` 仍可能改写 `bound_network` / `account`
        // 等字段；失败时用同一份快照退回。`last_poll_failure`（刚发生的失败证据）
        // 不在快照里，会原样保留。
        let checkpoint = ReconnectCheckpoint::capture(self);
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
                checkpoint.restore(self);
                self.message = format!("{}；网络或网卡暂不可用，正在按退避自动重连", self.message);
            }
            Err(_) => {
                // 保留凭据：暂停只是"不再自动尝试"，不该顺手把用户保存的账号丢掉。
                self.credential = Some(retry_credential);
                self.logs.record(crate::logging::Event::RetryPaused);
                // 与 `redial_tick` 相同：认证被拒的新决策（paused/status/message）
                // 保留；旧连接仍有效，`api`/`account`/`bound_network` 退回尝试前。
                checkpoint.restore_connection(self);
                self.paused = true;
                self.message = "自动重连被认证系统拒绝，已暂停；请手动检查账号和网络".into();
            }
        }
    }
}
