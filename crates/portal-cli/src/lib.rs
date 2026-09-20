//! LFRadius API clients. Portal login and self-service login are separate sessions.
pub mod cmcc;
pub mod controller;
pub mod daemon;
mod discovery;
pub mod ipc;
pub mod logging;
pub mod network;
pub mod settings;
pub mod traffic;
pub use cmcc::{CmccContext, PortalLoginOutcome};

use reqwest::{Client, Response};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{net::IpAddr, time::Duration};
use url::Url;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

pub const DEFAULT_SERVER: &str = "http://172.18.100.65/lfradius/";

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("地址无效，只支持不含凭据的 HTTP(S) URL")]
    InvalidUrl,
    #[error("请求失败，请检查网络、服务器地址和代理")]
    Network,
    #[error("网卡配置错误：{0}")]
    NetworkInterface(String),
    #[error("认证页探测请求超时。后台地址可达不代表外部探测站点可达；可粘贴浏览器弹出的完整 Portal URL 后再试")]
    DiscoveryTimeout,
    #[error("探测未发现校园网认证页：可能已经联网或探测域名被放行。可使用“仅登录后台”管理会话，或粘贴当前 Portal URL")]
    DiscoveryNotFound,
    #[error(
        "发现了认证跳转，但地址不属于配置的认证服务器；请检查服务器地址，不会向该地址发送账号密码"
    )]
    DiscoveryUntrusted,
    #[error("认证页发生循环跳转或跳转次数过多；请使用浏览器获取当前完整 Portal URL")]
    DiscoveryLoop,
    #[error("HTTP 状态异常：{0}")]
    Http(u16),
    #[error("后台拒绝请求或登录状态已过期")]
    Rejected,
    #[error("响应不符合预期：{0}")]
    InvalidResponse(&'static str),
    #[error("收到非预期跳转，请重新获取认证页面")]
    Redirect,
    #[error("{0}等待超时")]
    Timeout(&'static str),
    #[error("未找到指定会话，请刷新后重新选择")]
    SessionNotFound,
    #[error("下线请求已发送，但仍未确认目标会话消失")]
    DisconnectPending,
    #[error("无法唯一确定本次会话，请手动选择会话 ID")]
    AmbiguousSession,
}
impl From<reqwest::Error> for AppError {
    fn from(_: reqwest::Error) -> Self {
        Self::Network
    }
}
impl From<url::ParseError> for AppError {
    fn from(_: url::ParseError) -> Self {
        Self::InvalidUrl
    }
}

// RAII covers success, errors and cancellation. No Debug/Serialize for secrets.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct Credential {
    pub username: String,
    pub password: String,
}
impl Credential {
    pub fn new(username: String, password: String) -> Self {
        Self { username, password }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ApiStatus {
    #[serde(deserialize_with = "de_u64")]
    pub success: u64,
    #[serde(default)]
    pub msg: String,
    #[serde(default)]
    pub url: String,
    #[serde(default, deserialize_with = "de_u64")]
    pub wait_time: u64,
}
#[derive(Deserialize)]
struct Envelope {
    v: ApiStatus,
    d: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OnlineSession {
    pub radacctid: String,
    pub username: String,
    pub acctstarttime: String,
    #[serde(default, deserialize_with = "de_u64")]
    pub acctsessiontime: u64,
    #[serde(default, deserialize_with = "de_ip")]
    pub framedipaddress: Option<IpAddr>,
    // Only LFRadius accounting counters are used; no OS/NIC statistics.
    #[serde(default, deserialize_with = "de_u64")]
    pub acctinputoctets: u64,
    #[serde(default, deserialize_with = "de_u64")]
    pub acctoutputoctets: u64,
}
impl Drop for OnlineSession {
    fn drop(&mut self) {
        self.username.zeroize();
        self.radacctid.zeroize();
        self.acctstarttime.zeroize();
        self.framedipaddress = None;
    }
}
#[derive(Deserialize)]
struct OnlineLog {
    data: Vec<OnlineSession>,
    #[serde(default, deserialize_with = "de_u64")]
    total: u64,
}

pub fn validate_url(value: &str) -> Result<Url, AppError> {
    let url = Url::parse(value)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(AppError::InvalidUrl);
    }
    Ok(url)
}

/// One client per account/context. Cookies are memory-only. Clones share this jar.
/// Dropping all clients releases the jar; third-party HTTP buffers are not zeroize-guaranteed.
#[derive(Clone)]
pub struct PortalClient {
    client: Client,
    base_url: Url,
    logs: logging::LogBuffer,
}
impl PortalClient {
    pub fn new(base: &str) -> Result<Self, AppError> {
        Self::with_options(base, true)
    }
    pub fn with_options(base: &str, bypass: bool) -> Result<Self, AppError> {
        Self::with_options_and_local(base, bypass, None)
    }
    pub fn with_options_and_local(
        base: &str,
        bypass: bool,
        local_address: Option<IpAddr>,
    ) -> Result<Self, AppError> {
        let mut base_url = validate_url(base)?;
        if base_url.query().is_some() || base_url.fragment().is_some() {
            return Err(AppError::InvalidUrl);
        }
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        let mut builder = Client::builder()
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10));
        if bypass {
            builder = builder.no_proxy();
        }
        if let Some(address) = local_address {
            builder = builder.local_address(address);
        }
        Ok(Self {
            client: builder.build()?,
            base_url,
            logs: logging::LogBuffer::default(),
        })
    }
    pub fn with_logs(mut self, logs: logging::LogBuffer) -> Self {
        self.logs = logs;
        self
    }
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }
    fn endpoint(&self, path: &str) -> Result<Url, AppError> {
        Ok(self.base_url.join(path)?)
    }
    fn trusted_url(&self, value: &str) -> Result<Url, AppError> {
        let u = validate_url(value)?;
        if u.origin() != self.base_url.origin() || !u.path().starts_with(self.base_url.path()) {
            return Err(AppError::Redirect);
        }
        Ok(u)
    }
    async fn text(response: Response) -> Result<Zeroizing<String>, AppError> {
        if response.status().is_redirection() {
            return Err(AppError::Redirect);
        }
        if !response.status().is_success() {
            return Err(AppError::Http(response.status().as_u16()));
        }
        let mut bytes = Zeroizing::new(Vec::new());
        let mut response = response;
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
                return Err(AppError::InvalidResponse("响应过大"));
            }
            bytes.extend_from_slice(&chunk);
        }
        String::from_utf8(bytes.to_vec())
            .map(Zeroizing::new)
            .map_err(|_| AppError::InvalidResponse("编码"))
    }
    async fn envelope(response: Response) -> Result<Envelope, AppError> {
        // Real server labels JSON as text/html.
        let body = Self::text(response).await?;
        let env: Envelope =
            serde_json::from_str(&body).map_err(|_| AppError::InvalidResponse("JSON"))?;
        if env.v.success != 1 {
            return Err(AppError::Rejected);
        }
        Ok(env)
    }
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, AppError> {
        let env = Self::envelope(self.client.get(self.endpoint(path)?).send().await?).await?;
        serde_json::from_value(env.d).map_err(|_| AppError::InvalidResponse("数据字段"))
    }
    pub async fn login(&self, username: &str, password: &str) -> Result<(), AppError> {
        self.logs.record(logging::Event::BackendLogin);
        Self::envelope(
            self.client
                .post(self.endpoint("home.php?c=user&a=user_login")?)
                .form(&[("username", username), ("password", password)])
                .send()
                .await?,
        )
        .await?;
        self.logs.record(logging::Event::BackendAccepted);
        Ok(())
    }
    pub async fn sessions(&self) -> Result<Vec<OnlineSession>, AppError> {
        let mut rows = Vec::new();
        for page in 1..=100 {
            let log: OnlineLog = self
                .get(&format!(
                    "home.php?c=user&a=onlinelog&page={page}&pagesize=15"
                ))
                .await?;
            if log.data.is_empty() && (rows.len() as u64) < log.total {
                return Err(AppError::InvalidResponse("分页不完整"));
            }
            rows.extend(log.data);
            if rows.len() as u64 >= log.total {
                self.logs.record(logging::Event::ReadSessions);
                return Ok(rows);
            }
        }
        Err(AppError::InvalidResponse("会话数量超出范围"))
    }
    pub async fn disconnect_id(&self, id: &str) -> Result<(), AppError> {
        let rows = self.sessions().await?;
        let s = rows
            .iter()
            .find(|s| s.radacctid == id)
            .ok_or(AppError::SessionNotFound)?;
        self.disconnect_and_confirm(s).await
    }
    pub async fn disconnect_and_confirm(&self, s: &OnlineSession) -> Result<(), AppError> {
        self.logs.record(logging::Event::Disconnect);
        let form = [
            ("radacctid", s.radacctid.clone()),
            ("username", s.username.clone()),
            ("acctstarttime", s.acctstarttime.clone()),
            ("acctsessiontime", s.acctsessiontime.to_string()),
            (
                "framedipaddress",
                s.framedipaddress.map(|v| v.to_string()).unwrap_or_default(),
            ),
            ("acctinputoctets", s.acctinputoctets.to_string()),
            ("acctoutputoctets", s.acctoutputoctets.to_string()),
            ("key", s.radacctid.clone()),
            ("num", "1".into()),
        ];
        Self::envelope(
            self.client
                .post(self.endpoint("home.php?c=user&a=offline&r=person")?)
                .form(&form)
                .send()
                .await?,
        )
        .await?;
        for delay in [1, 2, 4] {
            tokio::time::sleep(Duration::from_secs(delay)).await;
            if !self
                .sessions()
                .await?
                .iter()
                .any(|r| r.radacctid == s.radacctid)
            {
                self.logs.record(logging::Event::Disconnected);
                return Ok(());
            }
        }
        Err(AppError::DisconnectPending)
    }
}

/// Prefer a unique newly-created record, never fall back to an arbitrary existing account session.
pub fn new_session_id(before: &[OnlineSession], after: &[OnlineSession]) -> Option<String> {
    let mut added = after
        .iter()
        .filter(|s| !before.iter().any(|b| b.radacctid == s.radacctid));
    match (added.next(), added.next()) {
        (Some(s), None) => Some(s.radacctid.clone()),
        _ => None,
    }
}

fn de_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    match value {
        serde_json::Value::Number(n) => n
            .as_u64()
            .ok_or_else(|| serde::de::Error::custom("unsigned integer required")),
        serde_json::Value::String(s) => s.parse().map_err(serde::de::Error::custom),
        serde_json::Value::Null => Ok(0),
        _ => Err(serde::de::Error::custom(
            "number or numeric string required",
        )),
    }
}
fn de_ip<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<IpAddr>, D::Error> {
    let value = Option::<String>::deserialize(d)?;
    match value.as_deref() {
        None | Some("") => Ok(None),
        Some(s) => s.parse().map(Some).map_err(serde::de::Error::custom),
    }
}
pub fn redact(s: &str) -> String {
    let chars: Vec<_> = s.chars().collect();
    if chars.len() <= 4 {
        return "****".into();
    }
    format!(
        "{}***{}",
        chars[..2].iter().collect::<String>(),
        chars[chars.len() - 2..].iter().collect::<String>()
    )
}

#[cfg(test)]
mod tests;
