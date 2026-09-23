use crate::{
    cmcc::Phase,
    network,
    settings::{CredentialStore, ReconnectMode, Settings},
    traffic::{AccountingRates, Rate},
    AppError, CmccContext, Credential, OnlineSession, PortalClient, PortalLoginOutcome,
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
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
}
pub struct Controller {
    pub settings: Settings,
    logs: crate::logging::LogBuffer,
    api: Option<PortalClient>,
    credential: Option<Credential>,
    rows: Vec<OnlineSession>,
    selected: Option<String>,
    accounting: AccountingRates,
    latest_rates: HashMap<String, Rate>,
    pub paused: bool,
    status: String,
    message: String,
    missing: u32,
    attempts: u32,
    last_attempt: Option<Instant>,
    awaiting_session: Option<HashSet<String>>,
    reconnecting: bool,
}
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
            rows: Vec::new(),
            selected: None,
            accounting: AccountingRates::default(),
            latest_rates: HashMap::new(),
            paused: true,
            status: "idle".into(),
            message: "输入账号后上线，或仅登录后台查询会话".into(),
            missing: 0,
            attempts: 0,
            last_attempt: None,
            awaiting_session: None,
            reconnecting: false,
        }
    }
    pub fn clear(&mut self) {
        self.api = None;
        self.credential = None;
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
        let network = if backend_only {
            network::resolve(&self.settings.interface_name)
                .ok()
                .flatten()
        } else {
            Some(
                network::resolve(&self.settings.interface_name)
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
        self.status = "authenticating".into();
        // Authenticate the self-service client before Portal login so we can record a
        // baseline. The same cookie jar is reused after Portal login; a second
        // user_login request is only needed when this first attempt failed.
        let mut backend_authenticated = false;
        let baseline = match api.login(&credential.username, &credential.password).await {
            Ok(()) => {
                backend_authenticated = true;
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
    pub async fn refresh(&mut self) -> Result<Snapshot, AppError> {
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
        let rows = match self.api.as_ref().ok_or(AppError::Rejected) {
            Ok(api) => api.sessions().await,
            Err(error) => Err(error),
        };
        match rows {
            Ok(rows) => {
                self.rows = rows;
                self.bind_new_session();
                self.update_session_status();
            }
            Err(error) => {
                self.logs.error(&error);
                self.message = error.to_string();
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
        if self.last_attempt.is_some_and(|t| {
            t.elapsed()
                < self
                    .settings
                    .poll_jitter
                    .apply(Duration::from_secs(30 * (1 << self.attempts)))
        }) {
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
            AppError::Network
                | AppError::Timeout(_)
                | AppError::DiscoveryTimeout
                | AppError::DiscoveryNotFound
                | AppError::Http(408 | 425 | 429 | 500..=599)
        )
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
}
