use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
};

pub(crate) fn row(id: &str, time: u64, up: u64, down: u64) -> OnlineSession {
    OnlineSession {
        radacctid: id.into(),
        username: "test-user".into(),
        acctstarttime: "2026-01-01 00:00:00".into(),
        acctsessiontime: time,
        framedipaddress: Some("192.0.2.2".parse().unwrap()),
        acctinputoctets: up,
        acctoutputoctets: down,
    }
}
pub(crate) struct Mock {
    pub(crate) base: String,
    stop: Arc<AtomicBool>,
    pub(crate) requests: Arc<Mutex<Vec<String>>>,
    handle: Option<thread::JoinHandle<()>>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
impl Mock {
    pub(crate) fn new(mode: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{addr}/lfradius/");
        let base2 = base.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let req2 = requests.clone();
        let handle = thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut data = Vec::new();
                let mut buf = [0; 4096];
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(&buf[..n]);
                    if let Some(end) = data.windows(4).position(|x| x == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&data[..end]);
                        let len = header
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|v| v.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if data.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                if data.is_empty() {
                    continue;
                }
                let request = String::from_utf8(data).unwrap();
                req2.lock().unwrap().push(request.clone());
                let path = request
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
                let mut status = 200;
                let mut extra = String::new();
                let reply = if mode.starts_with("discovery") && path == "/probe" {
                    let portal=format!("{base2}libs/portal/unify/portal.php/login/main/nasid/4/?clientmac=synthetic-secret&clientip=192.0.2.7");
                    match mode {
                        "discovery-http" => {
                            status = 302;
                            extra = format!("Location: {portal}\r\n");
                            String::new()
                        }
                        "discovery-js" => {
                            format!("<script>window.location.href='{portal}';</script>")
                        }
                        "discovery-meta" => {
                            format!("<meta http-equiv='refresh' content='0; URL={portal}'>")
                        }
                        "discovery-loop" => {
                            status = 302;
                            extra = "Location: /probe\r\n".into();
                            String::new()
                        }
                        "discovery-foreign" => {
                            status = 302;
                            extra="Location: http://untrusted.test/lfradius/libs/portal/unify/portal.php/login/main/nasid/4/\r\n".into();
                            String::new()
                        }
                        _ => "<html>Success</html>".into(),
                    }
                } else if path == "/backup" {
                    status = 302;
                    extra = format!(
                        "Location: {base2}libs/portal/unify/portal.php/login/main/nasid/4/\r\n"
                    );
                    String::new()
                } else if path.contains("main/nasid/") {
                    format!("<form method='post' action='{base2}libs/portal/unify/portal.php/login/cmcc_login/'><input name='usrname'><input name='passwd'><input name='nasid' value='4'><input name='usrmac' value='02:00:00:00:00:01'><input name='usrip' value='192.0.2.2'><input name='basip' value='192.0.2.10'><input name='portal_version' value='1'><input name='portal_papchap' value='pap'><input name='success' value='{base2}libs/portal/unify/portal.php/login/success/'><input name='fail' value='{base2}libs/portal/unify/portal.php/login/fail/'></form>")
                } else if path.ends_with("cmcc_login/") && body.starts_with("cmcc_login_value=") {
                    "<script>xhr.send(\"l=b3BhcXVl+/==\");</script>".into()
                } else if path.ends_with("cmcc_login/") {
                    extra = "Set-Cookie: portal=synthetic; Path=/\r\n".into();
                    "<form method='post' action='/lfradius/libs/portal/unify/portal.php/login/cmcc_login/'><input value='opaque+/==' name='cmcc_login_value'></form>".into()
                } else if path.ends_with("cmcc_login_result/") {
                    "success".into()
                } else if path.ends_with("success/") && mode != "direct" && mode != "fake" {
                    status = 302;
                    extra = if mode == "evil" {
                        "Location: https://example.org/__coa_search_page/\r\n".into()
                    } else {
                        "Location: /lfradius/libs/portal/unify/portal.php/login/__coa_search_page/\r\n".into()
                    };
                    String::new()
                } else if path.ends_with("__coa_search_page/") {
                    extra = "Set-Cookie: dial=started; Path=/\r\n".into();
                    "<div>[代拨]</div><script>xhr.open('POST','__coa_search/')</script>".into()
                } else if path.ends_with("__coa_search/") {
                    if mode == "timeout" {
                        String::new()
                    } else {
                        "success".into()
                    }
                } else if path.contains("user_login") {
                    extra = "Set-Cookie: backend=synthetic; Path=/\r\n".into();
                    r#"{"v":{"success":1},"d":[]}"#.into()
                } else if path.contains("onlinelog") {
                    if request.contains("backend=synthetic") {
                        r#"{"v":{"success":1},"d":{"data":[],"total":"0"}}"#.into()
                    } else {
                        r#"{"v":{"success":0},"d":[]}"#.into()
                    }
                } else if mode == "fake" {
                    "<div>请登录</div>".into()
                } else {
                    "<div class='mbody'>登录成功</div>".into()
                };
                let response=format!("HTTP/1.1 {status} OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{reply}",reply.len());
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            base,
            stop,
            requests,
            handle: Some(handle),
        }
    }
    pub(crate) fn context(&self) -> CmccContext {
        CmccContext::from_portal_url(&format!(
            "{}libs/portal/unify/portal.php/login/main/nasid/4/?paip=192.0.2.99",
            self.base
        ))
        .unwrap()
    }
}
#[tokio::test]
async fn direct_and_hidden_basip_and_encoded_token() {
    let mock = Mock::new("direct");
    let client = PortalClient::new(&mock.base).unwrap();
    assert_eq!(
        client
            .cmcc_login(&mock.context(), "test-user", "secret-placeholder")
            .await
            .unwrap(),
        PortalLoginOutcome::Direct
    );
    let requests = mock.requests.lock().unwrap();
    let initial = requests.iter().find(|r| r.contains("usrname=")).unwrap();
    assert!(initial.contains("basip=192.0.2.10"));
    assert!(!initial.contains("basip=192.0.2.99"));
    let hidden = requests
        .iter()
        .find(|r| r.contains("cmcc_login_value="))
        .unwrap();
    assert!(hidden.contains("opaque%2B%2F%3D%3D"));
    assert!(hidden.contains("portal=synthetic"));
}
#[tokio::test]
async fn dial_follows_wait_page_and_retains_cookies() {
    let mock = Mock::new("dial");
    let client = PortalClient::new(&mock.base).unwrap();
    assert_eq!(
        client
            .cmcc_login(&mock.context(), "test-user", "secret-placeholder")
            .await
            .unwrap(),
        PortalLoginOutcome::Dialed
    );
    let requests = mock.requests.lock().unwrap();
    let poll = requests
        .iter()
        .find(|r| r.starts_with("POST /lfradius/libs/portal/unify/portal.php/login/__coa_search/"))
        .unwrap();
    assert!(poll.contains("dial=started"));
    assert!(poll.ends_with("\r\n\r\n"));
    assert!(requests.last().unwrap().contains("success/success/1"));
}
#[tokio::test]
async fn untrusted_redirect_is_rejected() {
    let mock = Mock::new("evil");
    let client = PortalClient::new(&mock.base).unwrap();
    assert!(matches!(
        client
            .cmcc_login(&mock.context(), "test-user", "test-password")
            .await,
        Err(AppError::Redirect)
    ));
}
#[tokio::test]
async fn http_200_does_not_imply_auth_success() {
    let mock = Mock::new("fake");
    let client = PortalClient::new(&mock.base).unwrap();
    assert!(matches!(
        client
            .cmcc_login(&mock.context(), "test-user", "test-password")
            .await,
        Err(AppError::InvalidResponse(_))
    ));
}
#[tokio::test]
async fn dial_wait_has_deadline() {
    let mock = Mock::new("timeout");
    let client = PortalClient::new(&mock.base).unwrap();
    let result = client
        .cmcc_login(&mock.context(), "test-user", "test-password")
        .await;
    assert!(
        matches!(result, Err(AppError::Timeout("代拨"))),
        "unexpected result: {result:?}"
    );
}
#[tokio::test]
async fn backend_cookie_survives_across_commands() {
    let mock = Mock::new("direct");
    let logs = crate::logging::LogBuffer::default();
    logs.set_enabled(true);
    let client = PortalClient::new(&mock.base)
        .unwrap()
        .with_logs(logs.clone());
    assert!(client.sessions().await.is_err());
    client.login("test-user", "test-password").await.unwrap();
    assert!(client.clone().sessions().await.unwrap().is_empty());
    assert!(logs
        .entries()
        .iter()
        .any(|entry| entry.code == "backend.accepted"));
    let text = serde_json::to_string(&logs.entries()).unwrap();
    assert!(!text.contains("test-password"));
    assert!(!text.contains("test-user"));
    assert!(!text.contains("synthetic"));
}
#[test]
fn counters_use_accounting_seconds_not_poll_seconds() {
    let mut rates = traffic::AccountingRates::default();
    assert!(rates.update(&[row("1", 60, 100, 1000)])["1"]
        .download_bps
        .is_none());
    assert!(rates.update(&[row("1", 60, 100, 1000)])["1"]
        .download_bps
        .is_none());
    assert_eq!(
        rates.update(&[row("1", 120, 700, 7000)])["1"].download_bps,
        Some(100.0)
    );
    assert!(rates.update(&[row("1", 120, 700, 7000)])["1"]
        .download_bps
        .is_none());
    assert!(rates.update(&[row("2", 30, 80000, 80000)])["2"]
        .download_bps
        .is_none());
}
#[test]
fn parse_har_numbers_and_missing_ip() {
    let row:OnlineSession=serde_json::from_value(serde_json::json!({"radacctid":"s","username":"u","acctstarttime":"t","acctsessiontime":"60","framedipaddress":"","acctinputoctets":"7","acctoutputoctets":12})).unwrap();
    assert_eq!(row.acctsessiontime, 60);
    assert_eq!(row.framedipaddress, None);
}
#[test]
fn refuses_ambiguous_or_old_session() {
    assert!(new_session_id(&[row("old", 1, 0, 0)], &[row("old", 1, 0, 0)]).is_none());
    assert!(new_session_id(&[], &[row("a", 1, 0, 0), row("b", 1, 0, 0)]).is_none());
}
#[test]
fn transient_settings_cannot_persist_account_or_background_mode() {
    let mut s = settings::Settings {
        username: "test-user".into(),
        reconnect_mode: settings::ReconnectMode::NewSession,
        service_enabled: true,
        credential_store: settings::CredentialStore::Memory,
        ..Default::default()
    };
    s.normalize().unwrap();
    assert!(s.username.is_empty());
    assert_eq!(s.credential_store, settings::CredentialStore::Memory);
    assert_eq!(s.reconnect_mode, settings::ReconnectMode::Disabled);
    assert!(!s.service_enabled);
}
#[test]
fn unicode_redaction_does_not_panic() {
    assert_eq!(redact("测试用户名六"), "测试***名六");
}
