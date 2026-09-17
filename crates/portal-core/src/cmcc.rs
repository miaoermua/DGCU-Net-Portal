use crate::{validate_url, AppError, PortalClient};
use dom_query::Document;
use serde::Serialize;
use std::{collections::BTreeMap, time::Duration};
use tokio::time::{sleep, timeout, Instant};
use url::Url;
use zeroize::Zeroizing;

const LOGIN: &str = "libs/portal/unify/portal.php/login/";

/// Contains a trusted entry URL, never assumes `paip` equals the hidden `basip`.
pub struct CmccContext {
    pub portal_url: String,
}
impl CmccContext {
    pub fn from_portal_url(value: &str) -> Result<Self, AppError> {
        let u = validate_url(value)?;
        if !u.path().contains("/portal.php/login/main/nasid/") {
            return Err(AppError::InvalidResponse("不是 Portal 入口 URL"));
        }
        Ok(Self {
            portal_url: u.to_string(),
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PortalLoginOutcome {
    Direct,
    Dialed,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Discovering,
    ReadingForm,
    Authenticating,
    WaitingPortal,
    WaitingDial,
    Accepted,
}

struct Form {
    action: String,
    fields: BTreeMap<String, String>,
}
fn read_form(html: &str, required: &str) -> Result<Form, AppError> {
    // Parse attributes independent of quoting and attribute order. Never run page JavaScript.
    let doc = Document::from(html);
    for node in doc.select("form").nodes() {
        let form = Document::from(node.html().as_ref());
        if form.select(&format!("input[name='{required}']")).length() != 1 {
            continue;
        }
        if !node
            .attr("method")
            .is_some_and(|m| m.eq_ignore_ascii_case("post"))
        {
            return Err(AppError::InvalidResponse("表单不是 POST"));
        }
        let mut fields = BTreeMap::new();
        for input in form.select("input[name]").nodes() {
            let name = input.attr("name").unwrap().to_string();
            if fields
                .insert(
                    name,
                    input
                        .attr("value")
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                )
                .is_some()
            {
                return Err(AppError::InvalidResponse("重复的表单字段"));
            }
        }
        return Ok(Form {
            action: node
                .attr("action")
                .map(|v| v.to_string())
                .unwrap_or_default(),
            fields,
        });
    }
    Err(AppError::InvalidResponse("缺少认证表单"))
}
fn poll_token(html: &str) -> Result<Zeroizing<String>, AppError> {
    // Exact literal from captured template; no eval, no decoding of credential-bearing payload.
    let re = regex::Regex::new(r#"xhr\.send\(\s*["']l=([A-Za-z0-9+/=_%-]+)["']\s*\)"#).unwrap();
    re.captures(html)
        .map(|c| Zeroizing::new(c[1].to_owned()))
        .ok_or(AppError::InvalidResponse("缺少结果查询载荷"))
}
fn success_page(html: &str) -> bool {
    Document::from(html)
        .select(".mbody")
        .text()
        .contains("登录成功")
}

impl PortalClient {
    /// HTTP-only discovery. Follows a bounded redirect chain until the configured origin's
    /// Portal URL is found. No interface enumeration or NIC counters.
    pub async fn discover(&self, probe_url: &str) -> Result<CmccContext, AppError> {
        let mut current = validate_url(probe_url)?;
        for _ in 0..6 {
            if current.origin() == self.base_url().origin()
                && current.path().contains("/portal.php/login/main/nasid/")
            {
                self.trusted_url(current.as_str())?;
                return CmccContext::from_portal_url(current.as_str());
            }
            let response = self.client.get(current.clone()).send().await?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|s| s.to_str().ok())
                    .ok_or(AppError::Redirect)?;
                current = validate_url(current.join(location)?.as_str())?;
            } else {
                return Err(AppError::InvalidResponse(
                    "未发现 Portal 跳转，请粘贴最新认证网址",
                ));
            }
        }
        Err(AppError::Redirect)
    }
    pub async fn cmcc_login(
        &self,
        context: &CmccContext,
        username: &str,
        password: &str,
    ) -> Result<PortalLoginOutcome, AppError> {
        self.cmcc_login_progress(context, username, password, |_| {})
            .await
    }
    pub async fn cmcc_login_progress<F: Fn(Phase)>(
        &self,
        context: &CmccContext,
        username: &str,
        password: &str,
        progress: F,
    ) -> Result<PortalLoginOutcome, AppError> {
        let entry = self.trusted_url(&context.portal_url)?;
        progress(Phase::ReadingForm);
        let html = Self::text(self.client.get(entry.clone()).send().await?).await?;
        let mut form = read_form(&html, "usrname")?;
        let submit = self.checked_action(&entry, &form.action, "cmcc_login/")?;
        // Explicitly use server hidden values, including basip. paip is NOT its fallback.
        for name in [
            "nasid",
            "usrmac",
            "usrip",
            "basip",
            "portal_version",
            "portal_papchap",
        ] {
            if form.fields.get(name).is_none_or(|v| v.is_empty()) {
                return Err(AppError::InvalidResponse("认证页缺少网络上下文"));
            }
        }
        if form.fields["portal_version"] != "1" || form.fields["portal_papchap"] != "pap" {
            return Err(AppError::InvalidResponse("目前仅验证 Portal 1.0/PAP"));
        }
        for name in ["success", "fail"] {
            let callback = form
                .fields
                .get(name)
                .ok_or(AppError::InvalidResponse("缺少回调地址"))?;
            let suffix = if name == "success" {
                "success/"
            } else {
                "fail/"
            };
            self.checked_action(&entry, callback, suffix)?;
        }
        form.fields.insert("usrname".into(), username.into());
        form.fields.insert("passwd".into(), password.into());
        form.fields.insert("treaty".into(), "on".into());
        // Owned form values are dropped after encoding; password/token are not logged.
        let fields: Vec<(String, String)> = form.fields.into_iter().collect();
        let fields = Zeroizing::new(fields);
        progress(Phase::Authenticating);
        let html = Self::text(
            self.client
                .post(submit.clone())
                .form(&*fields)
                .send()
                .await?,
        )
        .await?;
        let hidden = read_form(&html, "cmcc_login_value")?;
        let submit = self.checked_action(&submit, &hidden.action, "cmcc_login/")?;
        let token = Zeroizing::new(
            hidden
                .fields
                .get("cmcc_login_value")
                .cloned()
                .ok_or(AppError::InvalidResponse("隐藏载荷"))?,
        );
        let html = Self::text(
            self.client
                .post(submit)
                .form(&[("cmcc_login_value", token.as_str())])
                .send()
                .await?,
        )
        .await?;
        let result_token = poll_token(&html)?;
        progress(Phase::WaitingPortal);
        self.poll(
            &format!("{LOGIN}cmcc_login_result/"),
            Some(&result_token),
            Duration::from_secs(1),
        )
        .await?;

        let response = self
            .client
            .get(self.endpoint(&format!("{LOGIN}success/"))?)
            .send()
            .await?;
        let outcome = if response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|h| h.to_str().ok())
                .ok_or(AppError::Redirect)?;
            let wait_url = self.checked_action(self.base_url(), location, "__coa_search_page/")?;
            // This GET can establish state/cookies required by the body-less poll.
            let wait_page = Self::text(self.client.get(wait_url).send().await?).await?;
            if !wait_page.contains("[代拨]") || !wait_page.contains("__coa_search/") {
                return Err(AppError::InvalidResponse("代拨等待页"));
            }
            progress(Phase::WaitingDial);
            self.poll(
                &format!("{LOGIN}__coa_search/"),
                None,
                Duration::from_secs(3),
            )
            .await?;
            let html = Self::text(
                self.client
                    .get(self.endpoint(&format!("{LOGIN}success/success/1"))?)
                    .send()
                    .await?,
            )
            .await?;
            if !success_page(&html) {
                return Err(AppError::InvalidResponse("最终成功页"));
            }
            PortalLoginOutcome::Dialed
        } else {
            let html = Self::text(response).await?;
            if !success_page(&html) {
                return Err(AppError::InvalidResponse("成功页"));
            }
            PortalLoginOutcome::Direct
        };
        progress(Phase::Accepted);
        Ok(outcome)
    }
    fn checked_action(&self, base: &Url, action: &str, suffix: &str) -> Result<Url, AppError> {
        let target = self.trusted_url(base.join(action)?.as_str())?;
        if target != self.endpoint(&format!("{LOGIN}{suffix}"))? {
            return Err(AppError::Redirect);
        }
        Ok(target)
    }
    async fn poll(
        &self,
        path: &str,
        token: Option<&str>,
        interval: Duration,
    ) -> Result<(), AppError> {
        let label = if token.is_some() {
            "Portal 认证"
        } else {
            "代拨"
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if Instant::now() + interval >= deadline {
                return Err(AppError::Timeout(label));
            }
            sleep(interval).await;
            let remaining = deadline.saturating_duration_since(Instant::now());
            let mut request = self
                .client
                .post(self.endpoint(path)?)
                .header("Content-Type", "application/x-www-form-urlencoded");
            if let Some(token) = token {
                request = request.form(&[("l", token)]);
            }
            let result = timeout(remaining, async { Self::text(request.send().await?).await })
                .await
                .map_err(|_| AppError::Timeout(label))??;
            if result.as_str() == "success" {
                return Ok(());
            }
            // The captured page continues displaying any non-success body until timeout.
            if result.len() > 8192 {
                return Err(AppError::InvalidResponse("状态查询返回页面"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn html_attribute_order_and_entities() {
        let form = read_form("<form method='POST' action='/next'><input value='a&amp;b' name='usrname'><input name='basip' value='192.0.2.5'></form>", "usrname").unwrap();
        assert_eq!(form.fields["usrname"], "a&b");
        assert_eq!(form.fields["basip"], "192.0.2.5");
    }
    #[test]
    fn token_is_opaque() {
        assert_eq!(
            poll_token("xhr.send(\"l=YWJj+/==\");").unwrap().as_str(),
            "YWJj+/=="
        );
        assert!(poll_token("xhr.send(variable);").is_err());
    }
}
