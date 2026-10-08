//! 一次性的连通性检测：分别探测认证服务器与外部探测站点。
//!
//! 只在用户手动触发时执行，永不读取或发送任何凭据。认证探测的地址、
//! 代理绕过与网卡绑定都沿用认证会话的同一套设置，否则“检测通过但认证
//! 失败”会变成常见的误判。
//!
//! 外网探测则同时跑两路，因为只绑定网卡会误报：本机开着 TUN 模式代理
//! （Clash、Surge 之类）时，DNS 会被接管成假地址（198.18.0.0/15），该地址
//! 只在那张虚拟网卡内可路由。此时把源地址钉在物理网卡上，数据包必然
//! 打不通，可用户的上网恰恰是正常的——这是代理接管，不是校园网故障。
//! 因此绑定路径无响应时，再按系统实际路由测一次才下结论。
use crate::{network, settings::Settings, validate_url, AppError};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{
    net::IpAddr,
    time::{Duration, Instant},
};
use url::Url;

/// 单项探测的结论。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reachability {
    /// 收到了预期响应。
    Reachable,
    /// 有响应，但被认证页接管：这通常意味着尚未完成认证。
    Captive,
    /// 超时、连接被拒或响应无法读取。
    Unreachable,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Diagnostic {
    pub auth: Reachability,
    pub auth_latency_ms: Option<u64>,
    /// 绑定所选网卡（与认证同一条路径）的探测结果，代表“校园网直连”。
    pub internet_direct: Reachability,
    /// 外网结论：直连通了以直连为准；直连无响应但系统路径通了仍算可达。
    pub internet: Reachability,
    pub internet_latency_ms: Option<u64>,
}

/// 内网认证服务器要么在内网可达，要么不可达，因此给较短的超时。
const AUTH_TIMEOUT: Duration = Duration::from_secs(3);
/// 外网探测允许更慢一点，弱网下不至于误报故障。
const INTERNET_TIMEOUT: Duration = Duration::from_secs(5);

/// 判定外网探测站的响应。
///
/// 认证页会拦截并重定向任何外网请求，所以“不是预期的成功页面”应当视为
/// 受限而不是外网故障；只有彻底没有响应才算不可达。
fn classify_probe(redirected: bool, body: &str) -> Reachability {
    if body.contains("Success") {
        Reachability::Reachable
    } else if redirected || body.trim_start().starts_with('<') {
        Reachability::Captive
    } else {
        Reachability::Unreachable
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

fn build_client(
    settings: &Settings,
    local: Option<IpAddr>,
    timeout: Duration,
) -> Result<Client, AppError> {
    let mut builder = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout);
    if settings.bypass_proxy {
        builder = builder.no_proxy();
    }
    if let Some(address) = local {
        builder = builder.local_address(address);
    }
    Ok(builder.build()?)
}

/// 认证服务器只要回了任何 HTTP 状态码就算可达；只有连接层失败才是不可达。
async fn probe_auth(client: &Client, url: &Url) -> (Reachability, Option<u64>) {
    let started = Instant::now();
    match client.get(url.clone()).send().await {
        Ok(_) => (Reachability::Reachable, Some(elapsed_ms(started))),
        Err(_) => (Reachability::Unreachable, None),
    }
}

async fn probe_internet(client: &Client, url: &Url) -> (Reachability, Option<u64>) {
    let started = Instant::now();
    let Ok(response) = client.get(url.clone()).send().await else {
        return (Reachability::Unreachable, None);
    };
    let latency = elapsed_ms(started);
    if response.status().is_redirection() {
        return (Reachability::Captive, Some(latency));
    }
    match response.text().await {
        Ok(body) => (classify_probe(false, &body), Some(latency)),
        Err(_) => (Reachability::Unreachable, None),
    }
}

/// 汇总直连与系统两路外网探测。
///
/// 直连有响应时以它为准（这条路径和认证同源，能证明校园网侧正常）；直连
/// 完全没有响应而系统路径通了，说明本机绕过了物理网卡上网（代理/VPN），
/// 此时仍算外网可达，只是把直连那一档如实上报给界面提示。
fn combine_internet(
    direct: (Reachability, Option<u64>),
    system: (Reachability, Option<u64>),
) -> (Reachability, Option<u64>) {
    match (direct.0, system.0) {
        (Reachability::Reachable, _) => (Reachability::Reachable, direct.1),
        (_, Reachability::Reachable) => (Reachability::Reachable, system.1),
        (Reachability::Captive, _) | (_, Reachability::Captive) => {
            (Reachability::Captive, direct.1.or(system.1))
        }
        _ => (Reachability::Unreachable, None),
    }
}

/// 三项探测并行执行，总耗时取决于较慢的一项而不是三者相加。
pub async fn run(settings: &Settings) -> Result<Diagnostic, AppError> {
    let local = network::resolve(&settings.interface_name)
        .ok()
        .flatten()
        .and_then(|context| context.ipv4.parse::<IpAddr>().ok());
    let auth_url = validate_url(&settings.server)?;
    let probe_url = validate_url(&settings.probe_url)?;
    let auth_client = build_client(settings, local, AUTH_TIMEOUT)?;
    let direct_client = build_client(settings, local, INTERNET_TIMEOUT)?;
    // 没有选定网卡时两路完全等价，省掉重复请求；未绑定的那一份代表系统路由。
    let system_client = match local {
        Some(_) => Some(build_client(settings, None, INTERNET_TIMEOUT)?),
        None => None,
    };
    let (auth, direct, system) = match &system_client {
        Some(client) => {
            let (auth, direct, system) = tokio::join!(
                probe_auth(&auth_client, &auth_url),
                probe_internet(&direct_client, &probe_url),
                probe_internet(client, &probe_url),
            );
            (auth, direct, Some(system))
        }
        None => {
            let (auth, direct) = tokio::join!(
                probe_auth(&auth_client, &auth_url),
                probe_internet(&direct_client, &probe_url),
            );
            (auth, direct, None)
        }
    };
    let (internet, internet_latency_ms) = combine_internet(direct, system.unwrap_or(direct));
    Ok(Diagnostic {
        auth: auth.0,
        auth_latency_ms: auth.1,
        internet_direct: direct.0,
        internet,
        internet_latency_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::{classify_probe, combine_internet, Reachability};

    #[test]
    fn successful_probe_page_is_reachable() {
        let body = "<HTML><HEAD><TITLE>Success</TITLE></HEAD><BODY>Success</BODY></HTML>";
        assert_eq!(classify_probe(false, body), Reachability::Reachable);
    }

    #[test]
    fn portal_redirect_and_portal_html_are_captive() {
        assert_eq!(classify_probe(true, ""), Reachability::Captive);
        assert_eq!(
            classify_probe(false, "  \n<html>请先认证</html>"),
            Reachability::Captive
        );
    }

    #[test]
    fn empty_or_unexpected_body_is_unreachable() {
        assert_eq!(classify_probe(false, ""), Reachability::Unreachable);
        assert_eq!(classify_probe(false, "blocked"), Reachability::Unreachable);
    }

    #[test]
    fn direct_probe_wins_when_it_answers() {
        let result = combine_internet(
            (Reachability::Reachable, Some(30)),
            (Reachability::Reachable, Some(70)),
        );
        assert_eq!(result, (Reachability::Reachable, Some(30)));
    }

    #[test]
    fn a_system_route_keeps_the_internet_reachable_when_direct_is_dead() {
        // 本机 TUN 代理把物理网卡的直连探测全挡住，但系统路由能上网：
        // 这是代理接管，不是校园网故障，不能报不可达。
        assert_eq!(
            combine_internet(
                (Reachability::Unreachable, None),
                (Reachability::Reachable, Some(46))
            ),
            (Reachability::Reachable, Some(46))
        );
        assert_eq!(
            combine_internet(
                (Reachability::Unreachable, None),
                (Reachability::Unreachable, None)
            ),
            (Reachability::Unreachable, None)
        );
    }

    #[test]
    fn an_intercepted_system_route_is_still_captive() {
        assert_eq!(
            combine_internet(
                (Reachability::Unreachable, None),
                (Reachability::Captive, Some(9))
            )
            .0,
            Reachability::Captive
        );
    }
}
