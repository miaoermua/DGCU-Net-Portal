//! Bounded HTTP/HTML Portal discovery; never evaluates a page script or reads NICs.
use crate::{logging::Event, validate_url, AppError, CmccContext, PortalClient};
use dom_query::Document;
use std::{collections::HashSet, time::Duration};
use tokio::time::timeout;
use url::Url;

const ENTRY: &str = "/portal.php/login/main/nasid/";
const BACKUPS: [&str; 2] = [
    "http://www.msftconnecttest.com/redirect",
    "http://connectivitycheck.gstatic.com/generate_204",
];

fn refresh_target(value: &str) -> Option<&str> {
    let lower = value.to_ascii_lowercase();
    let offset = lower.find("url=")? + 4;
    Some(value[offset..].trim().trim_matches(['\'', '"']))
}

fn html_redirect(base: &Url, html: &str) -> Option<Url> {
    let doc = Document::from(html);
    for meta in doc.select("meta[http-equiv][content]").nodes() {
        if meta
            .attr("http-equiv")
            .is_some_and(|v| v.eq_ignore_ascii_case("refresh"))
        {
            if let Some(content) = meta.attr("content") {
                if let Some(target) = refresh_target(&content) {
                    if let Ok(url) = base.join(target) {
                        return Some(url);
                    }
                }
            }
        }
    }
    // Only literal assignment or literal replace/assign calls are supported.
    // Any concatenation, template expression or computation requires a browser.
    let assignment=regex::Regex::new(r#"(?m)(?:window\.|top\.|self\.|document\.)?location(?:\.href)?\s*=\s*(?:"([^"\r\n]+)"|'([^'\r\n]+)')\s*(?:;|$|\})"#).unwrap();
    let call=regex::Regex::new(r#"(?:window\.|top\.|self\.|document\.)?location\.(?:replace|assign)\(\s*(?:"([^"\r\n]+)"|'([^'\r\n]+)')\s*\)"#).unwrap();
    for script in doc.select("script:not([src])").nodes() {
        let text = script.text();
        for re in [&assignment, &call] {
            for capture in re.captures_iter(&text) {
                let literal = capture.get(1).or_else(|| capture.get(2))?.as_str();
                let target = literal
                    .replace("\\/", "/")
                    .replace("\\u0026", "&")
                    .replace("\\x26", "&");
                if target.contains('\\') {
                    continue;
                }
                if let Ok(url) = base.join(&target) {
                    return Some(url);
                }
            }
        }
    }
    // A visible Portal link is also safe to follow; do not follow arbitrary page links.
    for node in doc.select("a[href]").nodes() {
        if let Some(href) = node.attr("href") {
            if let Ok(url) = base.join(&href) {
                if url.path().contains(ENTRY) {
                    return Some(url);
                }
            }
        }
    }
    None
}

impl PortalClient {
    pub async fn discover(&self, probe_url: &str) -> Result<CmccContext, AppError> {
        let mut probes = vec![validate_url(probe_url)?];
        for value in BACKUPS {
            let url = validate_url(value)?;
            if !probes.contains(&url) {
                probes.push(url);
            }
        }
        self.discover_probes(&probes).await
    }
    async fn discover_probes(&self, probes: &[Url]) -> Result<CmccContext, AppError> {
        self.logs.record(Event::Discover);
        let mut failure = AppError::DiscoveryNotFound;
        for (index, probe) in probes.iter().enumerate() {
            if index > 0 {
                self.logs.record(Event::ProbeFallback);
            }
            let result = timeout(Duration::from_secs(8), self.discover_chain(probe)).await;
            match result {
                Ok(Ok(context)) => return Ok(context),
                Ok(Err(AppError::DiscoveryUntrusted)) => return Err(AppError::DiscoveryUntrusted),
                Ok(Err(error)) => {
                    self.logs.error(&error);
                    failure = error;
                }
                Err(_) => {
                    self.logs.record(Event::ProbeTimeout);
                    failure = AppError::DiscoveryTimeout;
                }
            }
        }
        Err(failure)
    }
    async fn discover_chain(&self, probe: &Url) -> Result<CmccContext, AppError> {
        let mut current = probe.clone();
        let mut seen = HashSet::new();
        for _ in 0..6 {
            if current.path().contains(ENTRY) {
                self.trusted_url(current.as_str())
                    .map_err(|_| AppError::DiscoveryUntrusted)?;
                self.logs.record(Event::ProbeFound);
                return CmccContext::from_portal_url(current.as_str());
            }
            if !seen.insert(current.to_string()) {
                return Err(AppError::DiscoveryLoop);
            }
            let response = self
                .client
                .get(current.clone())
                .header("User-Agent", "Mozilla/5.0 DGCU-Portal/0.1.1")
                .header("Accept", "text/html,application/xhtml+xml,*/*;q=0.8")
                .header("Cache-Control", "no-cache")
                .send()
                .await?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or(AppError::DiscoveryNotFound)?;
                self.logs.record(Event::ProbeHttpRedirect);
                current = validate_url(current.join(location)?.as_str())?;
                continue;
            }
            if let Some(refresh) = response
                .headers()
                .get("refresh")
                .and_then(|v| v.to_str().ok())
                .and_then(refresh_target)
            {
                self.logs.record(Event::ProbeHtmlRedirect);
                current = validate_url(current.join(refresh)?.as_str())?;
                continue;
            }
            let body = Self::text(response).await?;
            if let Some(target) = html_redirect(&current, &body) {
                self.logs.record(Event::ProbeHtmlRedirect);
                current = validate_url(target.as_str())?;
            } else {
                return Err(AppError::DiscoveryNotFound);
            }
        }
        Err(AppError::DiscoveryLoop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn base() -> Url {
        Url::parse("http://gateway.test/probe").unwrap()
    }
    #[test]
    fn parses_literal_js_but_does_not_evaluate_expressions() {
        assert_eq!(
            html_redirect(&base(), "<script>window.location.href='/login';</script>")
                .unwrap()
                .path(),
            "/login"
        );
        assert!(html_redirect(
            &base(),
            "<script>window.location.href='/login'+secret;</script>"
        )
        .is_none());
        assert_eq!(
            html_redirect(
                &base(),
                "<script>location.replace('http:\\/\\/gateway.test/next');</script>"
            )
            .unwrap()
            .path(),
            "/next"
        );
    }
    #[test]
    fn handles_refresh_and_html_entities() {
        let url = html_redirect(
            &base(),
            r#"<meta http-equiv="Refresh" content="0; url=/next?a=1&amp;b=2">"#,
        )
        .unwrap();
        assert_eq!(url.query(), Some("a=1&b=2"));
    }
    #[test]
    fn no_redirect_is_not_a_portal() {
        assert!(html_redirect(&base(), "<html><body>Success</body></html>").is_none());
    }
    #[tokio::test]
    async fn discovers_http_and_static_html_redirects_without_logging_parameters() {
        for mode in ["discovery-http", "discovery-js", "discovery-meta"] {
            let mock = crate::tests::Mock::new(mode);
            let logs = crate::logging::LogBuffer::default();
            logs.set_enabled(true);
            let client = PortalClient::new(&mock.base)
                .unwrap()
                .with_logs(logs.clone());
            let probe = Url::parse(&mock.base).unwrap().join("/probe").unwrap();
            let context = client.discover_probes(&[probe]).await.unwrap();
            assert!(context.portal_url.contains(ENTRY));
            let json = serde_json::to_string(&logs.entries()).unwrap();
            assert!(!json.contains("synthetic-secret"));
            assert!(!json.contains("192.0.2.7"));
        }
    }
    #[tokio::test]
    async fn falls_back_on_a_normal_page_without_claiming_online() {
        let mock = crate::tests::Mock::new("discovery-normal");
        let client = PortalClient::new(&mock.base).unwrap();
        let base = Url::parse(&mock.base).unwrap();
        assert!(matches!(
            client
                .discover_probes(&[base.join("/probe").unwrap()])
                .await,
            Err(AppError::DiscoveryNotFound)
        ));
        assert!(client
            .discover_probes(&[base.join("/probe").unwrap(), base.join("/backup").unwrap()])
            .await
            .is_ok());
    }
    #[tokio::test]
    async fn rejects_foreign_portal_and_cycles() {
        for mode in ["discovery-foreign", "discovery-loop"] {
            let mock = crate::tests::Mock::new(mode);
            let client = PortalClient::new(&mock.base).unwrap();
            let result = client
                .discover_probes(&[Url::parse(&mock.base).unwrap().join("/probe").unwrap()])
                .await;
            assert!(matches!(
                result,
                Err(AppError::DiscoveryUntrusted | AppError::DiscoveryLoop)
            ));
        }
    }
}
