use crate::{network::NetworkContext, validate_url, AppError, PortalClient};
use dom_query::Document;
use serde::Serialize;
use std::{collections::BTreeMap, time::Duration};
use tokio::time::{sleep, timeout, Instant};
use url::Url;
use zeroize::Zeroizing;

const LOGIN: &str = "libs/portal/unify/portal.php/login/";
/// The LFRadius Portal backend address used as the standard `paip` value.
pub const PORTAL_PAIP: &str = "172.18.100.65";

/// Contains a trusted entry URL, never assumes `paip` equals the hidden `basip`.
pub struct CmccContext {
    pub portal_url: String,
    local_ipv4: Option<String>,
    local_mac: Option<String>,
}
impl CmccContext {
    pub fn from_portal_url(value: &str) -> Result<Self, AppError> {
        let u = validate_url(value)?;
        if !u.path().contains("/portal.php/login/main/nasid/") {
            return Err(AppError::InvalidResponse("不是 Portal 入口 URL"));
        }
        Ok(Self {
            portal_url: u.to_string(),
            local_ipv4: None,
            local_mac: None,
        })
    }

    /// Apply the selected local interface to the gateway context. The gateway
    /// supplies the authoritative hidden `basip`; the client only rewrites the
    /// standard Portal query values and the fixed Portal server address (`paip`).
    pub fn with_network_context(value: &str, network: &NetworkContext) -> Result<Self, AppError> {
        let mut portal = validate_url(value)?;
        let mut pairs = portal.query_pairs().into_owned().collect::<Vec<_>>();
        fn set(pairs: &mut Vec<(String, String)>, key: &str, value: &str) {
            pairs.retain(|(name, _)| name != key);
            pairs.push((key.to_owned(), value.to_owned()));
        }
        set(&mut pairs, "wlanuserip", &network.ipv4);
        set(&mut pairs, "clientip", &network.ipv4);
        set(&mut pairs, "clientmac", &network.mac);
        set(&mut pairs, "paip", PORTAL_PAIP);
        portal.set_query(None);
        {
            let mut query = portal.query_pairs_mut();
            query.extend_pairs(
                pairs
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            );
        }
        let mut context = Self::from_portal_url(portal.as_str())?;
        context.local_ipv4 = Some(network.ipv4.clone());
        context.local_mac = Some(network.mac.clone());
        Ok(context)
    }

    /// Build the DGCU CMCC Portal entry when the captive-network probe cannot
    /// return a redirect. The gateway accepts the standard context as query
    /// values, so this path still avoids reading any other interface.
    pub fn from_server_context(server: &str, network: &NetworkContext) -> Result<Self, AppError> {
        let base = validate_url(server)?;
        let entry = base.join("libs/portal/unify/portal.php/login/main/nasid/4/")?;
        let mut context = Self::with_network_context(entry.as_str(), network)?;
        let mut url = Url::parse(&context.portal_url)?;
        url.query_pairs_mut()
            .append_pair("wlanacname", "route1")
            .append_pair("vlan", "0.0")
            .append_pair("iarmdst", "captive.apple.com/hotspot-detect.html");
        context.portal_url = url.to_string();
        Ok(context)
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
    /// Check that a Portal entry returns the expected login form without
    /// submitting credentials. Used before the optional public probe fallback.
    pub async fn portal_form_available(&self, context: &CmccContext) -> Result<(), AppError> {
        let entry = self.trusted_url(&context.portal_url)?;
        let html = Self::text(self.client.get(entry).send().await?).await?;
        read_form(&html, "usrname").map(|_| ())
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
        self.logs.record(crate::logging::Event::ReadForm);
        progress(Phase::ReadingForm);
        let html = Self::text(self.client.get(entry.clone()).send().await?).await?;
        let mut form = read_form(&html, "usrname")?;
        let submit = self.checked_action(&entry, &form.action, "cmcc_login/")?;
        // Use the selected interface for the client identity. Keep basip from the
        // server-generated form; paip is the fixed backend address in the URL.
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
        if let Some(ipv4) = &context.local_ipv4 {
            form.fields.insert("usrip".into(), ipv4.clone());
        }
        if let Some(mac) = &context.local_mac {
            form.fields.insert("usrmac".into(), mac.clone());
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
        self.logs.record(crate::logging::Event::Submit);
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
        self.logs.record(crate::logging::Event::WaitPortal);
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
            self.logs.record(crate::logging::Event::WaitDial);
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
        self.logs.record(crate::logging::Event::Accepted);
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
            self.logs.record(if token.is_some() {
                crate::logging::Event::PollPortal
            } else {
                crate::logging::Event::PollDial
            });
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

    #[test]
    fn rewrites_portal_network_context_without_touching_other_parameters() {
        let context = NetworkContext {
            interface_name: "en0".into(),
            ipv4: "10.35.1.125".into(),
            mac: "9e:5c:c7:20:84:a0".into(),
        };
        let value = CmccContext::with_network_context(
            "http://172.18.100.65/lfradius/libs/portal/unify/portal.php/login/main/nasid/4/?wlanuserip=old&clientip=old&wlanacname=route1&clientmac=old&paip=172.18.100.91&vlan=0.0&iarmdst=captive.apple.com/hotspot-detect.html",
            &context,
        )
        .unwrap();
        let url = Url::parse(&value.portal_url).unwrap();
        let query = url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            query.get("wlanuserip").map(String::as_str),
            Some("10.35.1.125")
        );
        assert_eq!(
            query.get("clientip").map(String::as_str),
            Some("10.35.1.125")
        );
        assert_eq!(
            query.get("clientmac").map(String::as_str),
            Some("9e:5c:c7:20:84:a0")
        );
        assert_eq!(query.get("paip").map(String::as_str), Some(PORTAL_PAIP));
        assert_eq!(query.get("wlanacname").map(String::as_str), Some("route1"));
        assert_eq!(
            query.get("iarmdst").map(String::as_str),
            Some("captive.apple.com/hotspot-detect.html")
        );
    }

    #[test]
    fn builds_dgcu_template_entry_without_probe_redirect() {
        let network = NetworkContext {
            interface_name: "en0".into(),
            ipv4: "10.90.211.78".into(),
            mac: "4e:46:d4:4d:cc:40".into(),
        };
        let context =
            CmccContext::from_server_context("http://172.18.100.65/lfradius/", &network).unwrap();
        let url = Url::parse(&context.portal_url).unwrap();
        assert!(url.path().ends_with("/login/main/nasid/4/"));
        let query = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            query.get("wlanuserip").map(|v| v.as_ref()),
            Some("10.90.211.78")
        );
        assert_eq!(
            query.get("clientmac").map(|v| v.as_ref()),
            Some("4e:46:d4:4d:cc:40")
        );
        assert_eq!(query.get("paip").map(|v| v.as_ref()), Some(PORTAL_PAIP));
        assert_eq!(query.get("wlanacname").map(|v| v.as_ref()), Some("route1"));
    }

    #[tokio::test]
    async fn selected_interface_values_are_sent_to_portal_form() {
        let mock = crate::tests::Mock::new("direct");
        let client = PortalClient::new(&mock.base).unwrap();
        let network = NetworkContext {
            interface_name: "en0".into(),
            ipv4: "192.0.2.77".into(),
            mac: "02:11:22:33:44:55".into(),
        };
        let context =
            CmccContext::with_network_context(&mock.context().portal_url, &network).unwrap();
        client
            .cmcc_login(&context, "test-user", "secret-placeholder")
            .await
            .unwrap();
        let initial = mock
            .requests
            .lock()
            .unwrap()
            .iter()
            .find(|request| request.contains("usrname="))
            .cloned()
            .unwrap();
        assert!(initial.contains("usrip=192.0.2.77"));
        assert!(initial.contains("usrmac=02%3A11%3A22%3A33%3A44%3A55"));
    }
}
