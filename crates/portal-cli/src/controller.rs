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
            message: "输入账号后上线，或仅登录后台查询会话".into(),
            missing: 0,
            attempts: 0,
            network_attempts: 0,
            last_attempt: None,
            awaiting_session: None,
            reconnecting: false,
            bound_network: None,
            resolve_network: Arc::new(network::resolve),
        }
    }
    pub fn clear(&mut self) {
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
    pub fn forget(&mut self) {
        self.clear();
        self.logs.record(crate::logging::Event::LocalReleased);
        self.status = "idle".into();
        self.message = "已清除本地会话；这不代表远端已经下线".into();
    }
    fn fail(&mut self, e: AppError) -> AppError {
        self.logs.error(&e);
        if self.settings.credential_store == CredentialStore::Memory {
            self.clear();
        }
        self.status = "error".into();
        self.message = e.to_string();
        e
    }
    pub fn snapshot(&mut self) -> Snapshot {
        Snapshot {
            status: self.status.clone(),
            message: self.message.clone(),
            sessions: self.rows.clone(),
            rates: self.latest_rates.clone(),
            selected_id: self.selected.clone(),
            background_paused: self.paused,
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
                self.paused = self.settings.credential_store == CredentialStore::Memory
                    || self.settings.reconnect_mode == ReconnectMode::Disabled;
                if self.settings.credential_store != CredentialStore::Memory
                    && self.settings.reconnect_mode != ReconnectMode::Disabled
                {
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
        self.message = match outcome {
            Some(PortalLoginOutcome::Dialed) => "Portal 与代拨均已确认成功",
            Some(_) => "Portal 已确认成功",
            None if self.selected.is_some() => "已登录后台并恢复本机对应的在线会话",
            None => "已登录用户后台；请选择要管理的会话",
        }
        .into();
        if query_failed {
            self.message
                .push_str("；后台会话查询暂不可用，稍后自动重试");
        }
        self.paused = self.settings.credential_store == CredentialStore::Memory
            || self.settings.reconnect_mode == ReconnectMode::Disabled
            || outcome.is_none();
        if self.settings.credential_store != CredentialStore::Memory
            && self.settings.reconnect_mode != ReconnectMode::Disabled
        {
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
            return false;
        };
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
        self.rows = api.sessions().await?;
        self.bind_new_session();
        self.update_rates();
        if self.settings.reconnect_mode == ReconnectMode::Disabled {
            self.update_session_status();
        }
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
    fn update_session_status(&mut self) {
        if let Some(id) = &self.selected {
            if !self.rows.iter().any(|r| r.radacctid == *id) {
                self.missing += 1;
                self.status = "offline".into();
                self.message = "所选会话已从后台在线列表消失".into();
                if self.settings.credential_store == CredentialStore::Memory {
                    self.clear();
                }
            } else {
                self.missing = 0;
                self.status = "session_online".into();
                self.message = "所选会话在后台在线列表中".into();
            }
        }
    }
    pub fn select(&mut self, id: &str) -> Result<Snapshot, AppError> {
        if !self.rows.iter().any(|r| r.radacctid == id) {
            return Err(AppError::SessionNotFound);
        }
        self.selected = Some(id.into());
        self.awaiting_session = None;
        self.logs.record(crate::logging::Event::SelectSession);
        self.missing = 0;
        Ok(self.snapshot())
    }
    fn bind_new_session(&mut self) {
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
            self.message = "远端下线未确认；请在认证后台检查。临时模式本地凭据已释放".into();
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
            self.message = e.to_string();
        }
    }
    /// Check the selected session on the short drop-detection cadence. This is
    /// intentionally separate from `tick`: it reads session presence but does
    /// not update accounting-rate samples.
    pub async fn redial_tick<F: Fn(Phase)>(&mut self, progress: F) {
        if self.api.is_none()
            || self.settings.credential_store == CredentialStore::Memory
            || self.settings.reconnect_mode == ReconnectMode::Disabled
            || self.paused
        {
            return;
        }
        // 每次轮询前先确认 client 绑的还是当前网卡地址；换过地址就重建。
        self.rebind_changed_network();
        let rows = match self.api.as_ref().ok_or(AppError::Rejected) {
            Ok(api) => api.sessions().await,
            Err(error) => Err(error),
        };
        match rows {
            Ok(rows) => {
                self.rows = rows;
                self.bind_new_session();
                self.update_session_status();
                self.network_attempts = 0;
            }
            Err(error) => {
                self.logs.error(&error);
                self.message = error.to_string();
                // 原实现到这里直接 return：missing 永远不会增加，掉线重拨的
                // 阈值也就永远达不到，于是网络恢复不了、重拨也永不触发。
                if Self::recoverable_poll_error(&error) {
                    self.retry_after_failure(progress).await;
                }
                return;
            }
        }
        if self.paused
            || self.settings.credential_store == CredentialStore::Memory
            || self.settings.reconnect_mode == ReconnectMode::Disabled
            || self.missing < 3
            || self.attempts >= 3
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
                }
                self.status = old_status;
                self.message = format!(
                    "{}；网络暂时不可用，保留当前会话并等待下一次掉线检测",
                    old_message
                );
            }
            Err(_) => {
                self.logs.record(crate::logging::Event::RetryPaused);
                self.paused = true;
                self.message = "自动重拨被认证系统拒绝，已暂停；请手动检查".into();
            }
        }
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
        if self.paused
            || self.settings.credential_store == CredentialStore::Memory
            || self.settings.reconnect_mode == ReconnectMode::Disabled
        {
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
}
