use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::str::FromStr;
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("invalid server URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("network request failed: {0}")]
    Network(#[from] reqwest::Error),
    #[error("remote rejected request: {0}")]
    RemoteRejected(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("session {0} is not present")]
    SessionNotFound(String),
    #[error("dial flow has not been captured yet")]
    DialFlowNotCaptured,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiStatus {
    pub success: i32,
    #[serde(default)]
    pub msg: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub wait_time: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiEnvelope<T> {
    pub v: ApiStatus,
    pub d: T,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OnlineSession {
    pub radacctid: String,
    pub username: String,
    pub acctstarttime: String,
    #[serde(default, deserialize_with = "de_u64")]
    pub acctsessiontime: u64,
    #[serde(default, deserialize_with = "de_ip")]
    pub framedipaddress: Option<IpAddr>,
    #[serde(default, deserialize_with = "de_u64")]
    pub acctinputoctets: u64,
    #[serde(default, deserialize_with = "de_u64")]
    pub acctoutputoctets: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OnlineLog {
    pub data: Vec<OnlineSession>,
    #[serde(default)]
    pub total: usize,
}

#[derive(Clone, Debug)]
pub struct PortalClient {
    client: Client,
    base_url: Url,
}

impl PortalClient {
    pub fn new(base_url: &str) -> Result<Self, AppError> {
        Self::with_options(base_url, false)
    }

    pub fn with_options(base_url: &str, bypass_proxy: bool) -> Result<Self, AppError> {
        let mut url = Url::parse(base_url)?;
        if !url.path().ends_with('/') {
            let path = format!("{}/", url.path());
            url.set_path(&path);
        }
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .timeout(std::time::Duration::from_secs(10))
            .cookie_store(true);
        if bypass_proxy {
            builder = builder.no_proxy();
        }
        let client = builder.build()?;
        Ok(Self {
            client,
            base_url: url,
        })
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    fn endpoint(&self, path: &str) -> Result<Url, AppError> {
        Ok(self.base_url.join(path.trim_start_matches('/'))?)
    }

    pub async fn login(&self, username: &str, password: &str) -> Result<ApiStatus, AppError> {
        let url = self.endpoint("home.php?c=user&a=user_login")?;
        let response = self
            .client
            .post(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[("username", username), ("password", password)])
            .send()
            .await?;
        let result: ApiEnvelope<Vec<serde_json::Value>> = response.json().await?;
        ensure_success(&result.v)?;
        Ok(result.v)
    }

    pub async fn sessions(&self) -> Result<Vec<OnlineSession>, AppError> {
        let url = self.endpoint("home.php?c=user&a=onlinelog&page=1&pagesize=50")?;
        let result: ApiEnvelope<OnlineLog> = self.client.get(url).send().await?.json().await?;
        ensure_success(&result.v)?;
        Ok(result.d.data)
    }

    pub async fn disconnect(&self, session: &OnlineSession) -> Result<ApiStatus, AppError> {
        let url = self.endpoint("home.php?c=user&a=offline&r=person")?;
        let form = [
            ("radacctid", session.radacctid.clone()),
            ("username", session.username.clone()),
            ("acctstarttime", session.acctstarttime.clone()),
            ("acctsessiontime", session.acctsessiontime.to_string()),
            (
                "framedipaddress",
                session
                    .framedipaddress
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            ),
            ("acctinputoctets", session.acctinputoctets.to_string()),
            ("acctoutputoctets", session.acctoutputoctets.to_string()),
            ("key", session.radacctid.clone()),
            ("num", "1".to_string()),
        ];
        let result: ApiEnvelope<Vec<serde_json::Value>> = self
            .client
            .post(url)
            .form(&form)
            .send()
            .await?
            .json()
            .await?;
        ensure_success(&result.v)?;
        Ok(result.v)
    }

    pub async fn disconnect_and_confirm(
        &self,
        session: &OnlineSession,
    ) -> Result<ApiStatus, AppError> {
        let status = self.disconnect(session).await?;
        for delay in [1, 2, 4] {
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            if !self
                .sessions()
                .await?
                .iter()
                .any(|s| s.radacctid == session.radacctid)
            {
                return Ok(status);
            }
        }
        Err(AppError::RemoteRejected(
            "disconnect requested but session remains active".into(),
        ))
    }
}

fn ensure_success(status: &ApiStatus) -> Result<(), AppError> {
    if status.success == 1 {
        Ok(())
    } else {
        Err(AppError::RemoteRejected(if status.msg.is_empty() {
            "unknown server error".into()
        } else {
            status.msg.clone()
        }))
    }
}

fn de_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Number(n) => n
            .as_u64()
            .ok_or_else(|| serde::de::Error::custom("expected unsigned integer")),
        serde_json::Value::String(s) => s.parse::<u64>().map_err(serde::de::Error::custom),
        serde_json::Value::Null => Ok(0),
        _ => Err(serde::de::Error::custom(
            "expected number or numeric string",
        )),
    }
}

fn de_ip<'de, D>(deserializer: D) -> Result<Option<IpAddr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    match value.as_deref() {
        None | Some("") => Ok(None),
        Some(s) => IpAddr::from_str(s)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

pub fn redact(value: &str) -> String {
    if value.len() <= 4 {
        return "****".into();
    }
    format!("{}***{}", &value[..2], &value[value.len() - 2..])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redaction_hides_middle() {
        assert_eq!(redact("password"), "pa***rd");
    }

    #[test]
    fn parses_har_numeric_strings() {
        let value = serde_json::json!({"radacctid":"1","username":"u","acctstarttime":"now","acctsessiontime":"660","framedipaddress":"10.0.0.2","acctinputoctets":"10","acctoutputoctets":20});
        let session: OnlineSession = serde_json::from_value(value).unwrap();
        assert_eq!(session.acctsessiontime, 660);
        assert_eq!(session.framedipaddress.unwrap().to_string(), "10.0.0.2");
        assert_eq!(session.acctinputoctets, 10);
    }
}
