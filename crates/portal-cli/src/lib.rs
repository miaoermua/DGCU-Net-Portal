//! LFRadius API clients. Portal login and self-service login are separate sessions.
pub mod cmcc;
pub mod controller;
pub mod daemon;
pub mod diagnose;
mod discovery;
pub mod ipc;
pub mod logging;
pub mod network;
pub mod service;
pub mod settings;
pub mod traffic;
pub use cmcc::{CmccContext, PortalLoginOutcome};

use reqwest::{Client, Response};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{net::IpAddr, sync::Arc, time::Duration};
use url::Url;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

pub const DEFAULT_SERVER: &str = "http://172.18.100.65/lfradius/";

/// 单次请求的整体上限，包含连接、发送和读取。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// 连接阶段单独设限：源地址失效时应该立刻失败，而不是拖到整体超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// 空闲连接保活，让睡眠唤醒后的死连接能被内核探活剔除。
const TCP_KEEPALIVE: Duration = Duration::from_secs(30);
/// 空闲连接最多留 15 秒；合盖前后残留的半开连接不会一直被复用。
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("地址无效，只支持不含凭据的 HTTP(S) URL")]
    InvalidUrl,
    /// 传输层失败。分类只进本地日志，不含 URL、响应体或系统错误原文。
    #[error("请求失败，请检查网络、服务器地址和代理")]
    Network(NetworkFault),
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

/// 传输层失败的分类。只保留对排查有用的三档，供日志与文案选择。
///
/// 之所以要分开：`Connect` 几乎总是本机绑定的源地址已经失效（合盖唤醒换网段
/// 后最典型），而 `Timeout` 更像链路慢或被拦。两者原先压成同一句，日志里完全
/// 看不出区别。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NetworkFault {
    /// 连接阶段失败，含源地址绑定失败。
    Connect,
    Timeout,
    Other,
}
impl From<reqwest::Error> for AppError {
    fn from(error: reqwest::Error) -> Self {
        // 只取 reqwest 自己暴露的判定，不拼接 `source()` 链或系统错误原文：
        // 那些可能带上本地地址和 URL，日志里不允许出现。
        let fault = if error.is_timeout() {
            NetworkFault::Timeout
        } else if error.is_connect() {
            NetworkFault::Connect
        } else {
            NetworkFault::Other
        };
        Self::Network(fault)
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

/// 用户自服务首页 `myinfo` 报告的套餐与到期时间。只提取界面要用的两项：
/// 账号名和凭据不进入这个结构，也就不会随快照进入前端。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountInfo {
    /// 原始套餐名，例如“电信100M包年”。
    pub plan: String,
    /// 由套餐名解析出的带宽档位，例如“100Mbps”；认不出写法时为 None。
    pub bandwidth: Option<String>,
    /// 到期日，只保留“年-月-日”。
    pub expires_on: Option<String>,
    /// 后台把未订购账号的套餐名写成“学生”：界面显示 Free，并说明只能在公共区域使用（例如：图书馆）。
    pub unpurchased: bool,
}

#[derive(Deserialize)]
struct MyInfo {
    #[serde(default)]
    myinfo: Vec<MyInfoField>,
}
#[derive(Deserialize)]
struct MyInfoField {
    name: String,
    #[serde(default)]
    value: String,
}
impl MyInfo {
    fn value(&self, name: &str) -> Option<&str> {
        self.myinfo
            .iter()
            .find(|field| field.name == name)
            .map(|field| field.value.trim())
            .filter(|value| !value.is_empty())
    }
    fn into_account(self) -> AccountInfo {
        let plan = self.value("servername").unwrap_or_default().to_owned();
        let bandwidth = bandwidth_tier(&plan);
        AccountInfo {
            unpurchased: unpurchased_plan(&plan, bandwidth.as_deref()),
            bandwidth,
            expires_on: self.value("expiretime").and_then(expires_on),
            plan,
        }
    }
}

/// 取套餐名里第一个紧跟 `M`/`m`/`兆` 的数字作为带宽档位，输出 `{n}Mbps`。
///
/// 这里刻意不维护 20M/100M/300M 的档位白名单：日后后台新增 500M、1000M 之类
/// 的档位无需改代码就能显示。认不出的写法（例如中文“千兆”）返回 None，
/// 界面回退到原始套餐名，不会出现空白。
fn bandwidth_tier(plan: &str) -> Option<String> {
    let mut digits = String::new();
    for character in plan.chars() {
        if character.is_ascii_digit() {
            digits.push(character);
            continue;
        }
        if matches!(character, 'M' | 'm' | '兆') {
            // 超出 u64 的数字串会解析失败，直接当作认不出。
            if let Ok(mbps) = digits.parse::<u64>() {
                if mbps > 0 {
                    return Some(format!("{mbps}Mbps"));
                }
            }
        }
        digits.clear();
    }
    None
}

/// 未订购账号的套餐名被后台写成“学生”：既没有带宽档位，也没有订购关系。
///
/// 要求带宽同时认不出来才判定：日后后台若出现“学生100M”这类带档位的写法，
/// 会按已订购处理，不会被这句文案冤枉。
fn unpurchased_plan(plan: &str, bandwidth: Option<&str>) -> bool {
    bandwidth.is_none() && plan.contains("学生")
}

/// `expiretime` 形如“2027-10-01 00:00:00”，界面只要日期部分。
fn expires_on(value: &str) -> Option<String> {
    let date = value
        .split(|c: char| c.is_whitespace() || c == 'T')
        .next()?;
    (!date.is_empty()).then(|| date.to_owned())
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

/// 校验后台地址并补上结尾斜杠，让 `Url::join` 始终相对该路径拼接。
fn normalize_base(value: &str) -> Result<Url, AppError> {
    let mut url = validate_url(value)?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(AppError::InvalidUrl);
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

/// One client per account/context. Cookies live in an explicit jar that survives
/// a rebuilt HTTP client — that is what lets `rebind` follow an address change
/// without losing the login. Dropping all clients releases the jar; third-party
/// HTTP buffers are not zeroize-guaranteed.
#[derive(Clone)]
pub struct PortalClient {
    client: Client,
    base_url: Url,
    /// Cookie 放在显式的 jar 里，重建 HTTP 客户端时不丢登录态。
    jar: Arc<reqwest::cookie::Jar>,
    /// 保留构建参数，供 `rebind` 用新源地址重建。
    bypass: bool,
    local_address: Option<IpAddr>,
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
        let base_url = normalize_base(base)?;
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let client = Self::build_client(bypass, local_address, jar.clone())?;
        Ok(Self {
            client,
            base_url,
            jar,
            bypass,
            local_address,
            logs: logging::LogBuffer::default(),
        })
    }
    /// 后台地址变更后重新指向新地址。
    ///
    /// 客户端会连同连接池一起重建（旧池连的是旧主机）；Cookie 按域匹配，旧域的
    /// Cookie 不会发往新后台，不需要手动清空。
    pub fn rebase(&mut self, base: &str) -> Result<(), AppError> {
        self.base_url = normalize_base(base)?;
        self.client = Self::build_client(self.bypass, self.local_address, self.jar.clone())?;
        Ok(())
    }
    fn build_client(
        bypass: bool,
        local_address: Option<IpAddr>,
        jar: Arc<reqwest::cookie::Jar>,
    ) -> Result<Client, AppError> {
        let mut builder = Client::builder()
            .cookie_provider(jar)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .tcp_keepalive(TCP_KEEPALIVE)
            .pool_idle_timeout(POOL_IDLE_TIMEOUT);
        if bypass {
            builder = builder.no_proxy();
        }
        if let Some(address) = local_address {
            builder = builder.local_address(address);
        }
        Ok(builder.build()?)
    }
    /// 用新的本地源地址重建 HTTP 客户端，Cookie 留在共享 jar 里。
    ///
    /// 合盖唤醒、Wi-Fi 漫游后网卡地址会变化，而 `local_address` 是绑死在
    /// 客户端上的：不重建就会对每个请求立刻失败。
    pub fn rebind(&mut self, local_address: Option<IpAddr>) -> Result<(), AppError> {
        self.client = Self::build_client(self.bypass, local_address, self.jar.clone())?;
        self.local_address = local_address;
        Ok(())
    }
    /// 当前绑定的源地址；未绑定时为 None（跟随系统路由）。
    pub fn bound_address(&self) -> Option<IpAddr> {
        self.local_address
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
    /// 读取套餐与到期时间。后台登录后调用，失败不影响会话查询。
    pub async fn account(&self) -> Result<AccountInfo, AppError> {
        let info: MyInfo = self.get("home.php?c=user&a=myinfo").await?;
        self.logs.record(logging::Event::ReadAccount);
        Ok(info.into_account())
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
