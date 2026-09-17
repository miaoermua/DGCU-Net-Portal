use crate::{
    cmcc::Phase,
    settings::Settings,
    traffic::{AccountingRates, Rate},
    AppError, CmccContext, Credential, OnlineSession, PortalClient, PortalLoginOutcome,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[derive(Clone, Serialize)]
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
}
impl Default for Controller {
    fn default() -> Self {
        Self::new(Settings::default())
    }
}
impl Controller {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
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
    }
    pub fn forget(&mut self) {
        self.clear();
        self.status = "idle".into();
        self.message = "已清除本地会话；这不代表远端已经下线".into();
    }
    fn fail(&mut self, e: AppError) -> AppError {
        if self.settings.one_session {
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
            one_session: self.settings.one_session,
        }
    }
    pub async fn connect<F: Fn(Phase)>(
        &mut self,
        credential: Credential,
        portal_url: &str,
        backend_only: bool,
        progress: F,
    ) -> Result<Snapshot, AppError> {
        self.clear();
        let api = PortalClient::with_options(&self.settings.server, self.settings.bypass_proxy)?;
        let portal = PortalClient::with_options(&self.settings.server, self.settings.bypass_proxy)?;
        self.status = "authenticating".into();
        // Obtain a baseline only after authenticating to the self-service system.
        let baseline = match api.login(&credential.username, &credential.password).await {
            Ok(()) => api.sessions().await.ok(),
            Err(_) => None,
        };
        let outcome = if backend_only {
            None
        } else {
            progress(Phase::Discovering);
            let ctx = if portal_url.is_empty() {
                portal.discover(&self.settings.probe_url).await
            } else {
                CmccContext::from_portal_url(portal_url)
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
        match api.login(&credential.username, &credential.password).await {
            Ok(()) => {
                self.api = Some(api);
            }
            Err(e) if outcome.is_some() => {
                self.status = "accepted".into();
                self.message = "Portal 已确认成功，但后台登录失败；请重新登录后台查询会话".into();
                let _ = e;
                return Ok(self.snapshot());
            }
            Err(e) => return Err(self.fail(e)),
        }
        self.rows = self
            .api
            .as_ref()
            .unwrap()
            .sessions()
            .await
            .unwrap_or_default();
        self.selected = baseline
            .as_ref()
            .and_then(|before| crate::new_session_id(before, &self.rows));
        self.status = if outcome.is_some() {
            "accepted"
        } else {
            "backend"
        }
        .into();
        self.message = match outcome {
            Some(PortalLoginOutcome::Dialed) => "Portal 与代拨均已确认成功",
            Some(_) => "Portal 已确认成功",
            None => "已登录用户后台；请选择要管理的会话",
        }
        .into();
        self.paused = self.settings.one_session || !self.settings.auto_redial || outcome.is_none();
        if !self.settings.one_session && self.settings.auto_redial {
            self.credential = Some(credential);
        }
        // Otherwise credential drops here, before the online session ends.
        Ok(self.snapshot())
    }
    pub async fn refresh(&mut self) -> Result<Snapshot, AppError> {
        let api = self.api.as_ref().ok_or(AppError::Rejected)?;
        self.rows = api.sessions().await?;
        self.latest_rates = self.accounting.update(&self.rows);
        if let Some(id) = &self.selected {
            if !self.rows.iter().any(|r| r.radacctid == *id) {
                self.missing += 1;
                self.status = "offline".into();
                self.message = "所选会话已从后台在线列表消失".into();
                if self.settings.one_session {
                    self.clear();
                }
            } else {
                self.missing = 0;
                self.status = "session_online".into();
                self.message = "所选会话在后台在线列表中".into();
            }
        }
        Ok(self.snapshot())
    }
    pub fn select(&mut self, id: &str) -> Result<Snapshot, AppError> {
        if !self.rows.iter().any(|r| r.radacctid == id) {
            return Err(AppError::SessionNotFound);
        }
        self.selected = Some(id.into());
        self.missing = 0;
        Ok(self.snapshot())
    }
    pub async fn disconnect(&mut self, id: &str) -> Result<Snapshot, AppError> {
        self.paused = true;
        let result = match &self.api {
            Some(api) => api.disconnect_id(id).await,
            None => Err(AppError::Rejected),
        };
        if let Err(e) = result {
            if self.settings.one_session {
                self.clear();
            }
            self.status = "unknown".into();
            self.message = "远端下线未确认；请在认证网站检查。临时模式本地凭据已释放".into();
            return Err(e);
        }
        self.rows.retain(|r| r.radacctid != id);
        if self.selected.as_deref() == Some(id) {
            self.selected = None;
        }
        if self.settings.one_session {
            self.clear();
        }
        self.status = "offline".into();
        self.message = "目标会话已确认下线".into();
        Ok(self.snapshot())
    }
    /// Invoked by the app's worker, not by the WebView timer (works with window hidden).
    pub async fn tick<F: Fn(Phase)>(&mut self, progress: F) {
        if self.api.is_none() {
            return;
        }
        if let Err(e) = self.refresh().await {
            self.message = e.to_string();
            return;
        }
        if self.paused
            || self.settings.one_session
            || !self.settings.auto_redial
            || self.missing < 3
            || self.attempts >= 3
        {
            return;
        }
        if self
            .last_attempt
            .is_some_and(|t| t.elapsed() < Duration::from_secs(30 * (1 << self.attempts)))
        {
            return;
        }
        let Some(credential) = self.credential.take() else {
            return;
        };
        self.last_attempt = Some(Instant::now());
        self.attempts += 1;
        let attempts = self.attempts;
        let last = self.last_attempt;
        let result = self.connect(credential, "", false, progress).await;
        self.attempts = attempts;
        self.last_attempt = last;
        if result.is_err() || self.selected.is_none() {
            self.paused = true;
            self.message = "自动重拨未完成或无法绑定新会话，已暂停；请手动检查".into();
        }
    }
}
