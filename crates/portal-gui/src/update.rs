//! 检查 GitHub Releases 上有没有新版本。
//!
//! 这里只回答“有没有新版本、到哪去看”：不下载、不替换自身，也不调用系统包管理器。
//! Arch 包的二进制由 pacman 记账，AppImage / zip 用户要自己挑对应平台的产物，
//! 选哪条渠道只有用户知道，所以只把 Release 页面交给系统浏览器打开。

use std::time::Duration;

const RELEASE_API: &str = "https://api.github.com/repos/miaoermua/DGCU-Net-Portal/releases/latest";
const RELEASES_PAGE: &str = "https://github.com/miaoermua/DGCU-Net-Portal/releases";
const RELEASE_PREFIX: &str = "https://github.com/miaoermua/DGCU-Net-Portal/releases/";
/// GitHub 要求每个请求都带 User-Agent，缺了直接 403。
const USER_AGENT: &str = concat!("DGCU-Net-Portal/", env!("CARGO_PKG_VERSION"));
/// 校园网常把 api.github.com 挂住：超时后立即给结论，不留悬挂的请求。
const TIMEOUT: Duration = Duration::from_secs(10);
const TIMEOUT_MESSAGE: &str = "检测更新超时，请检查网络后重试";

#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    html_url: String,
}

/// 交给前端的检查结果；`newer` 为假时表示当前已经是最新版本。
#[derive(serde::Serialize)]
pub struct UpdateStatus {
    /// 当前运行版本，编译期注入。
    pub current: String,
    /// 最新 Release 的 tag，可能带 v 前缀。
    pub latest: String,
    pub newer: bool,
    /// 本仓库的 Release 页面：下载与安装由用户在那个页面里自己完成。
    pub url: String,
}

/// 查询最新 Release 并与当前版本比较；10 秒内没有结果就返回超时说明。
pub async fn check() -> Result<UpdateStatus, String> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|_| "无法初始化网络请求".to_string())?;
    let release = tokio::time::timeout(TIMEOUT, fetch(&client))
        .await
        .map_err(|_| TIMEOUT_MESSAGE.to_string())??;
    let current = env!("CARGO_PKG_VERSION").to_string();
    Ok(UpdateStatus {
        newer: is_newer(&current, &release.tag_name),
        latest: release.tag_name,
        current,
        url: release_page(&release.html_url),
    })
}

async fn fetch(client: &reqwest::Client) -> Result<Release, String> {
    let response = client
        .get(RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                TIMEOUT_MESSAGE.to_string()
            } else {
                "无法访问 GitHub，请检查网络后重试".to_string()
            }
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            403 | 429 => "GitHub 暂时拒绝访问，可能触发限流，请稍后再试".to_string(),
            404 => "GitHub 上还没有已发布的版本".to_string(),
            code => format!("GitHub 返回异常状态码 {code}"),
        });
    }
    response
        .json::<Release>()
        .await
        .map_err(|_| "GitHub 返回的数据无法解析".to_string())
}

/// 只把本仓库的 Release 页面交给系统浏览器，避免把接口返回的任意地址打开。
fn release_page(html_url: &str) -> String {
    let url = html_url.trim();
    if url.starts_with(RELEASE_PREFIX) {
        url.to_string()
    } else {
        RELEASES_PAGE.to_string()
    }
}

/// 把 `v0.5.0`、`0.5.0-1` 这类版本串归一成数字段。
///
/// 先去 `v` 前缀，再丢掉第一个 `-` 或 `+` 之后的内容：Arch 的 pkgrel
/// （`0.5.0-1`）正落在这里，严格按 semver 解析会把它当成比 `0.5.0` 更旧的
/// 预发布版本，结论就反了。
fn version_key(value: &str) -> Vec<u64> {
    let trimmed = value.trim();
    let body = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    let release = body.split(&['-', '+'][..]).next().unwrap_or_default();
    release
        .split('.')
        .map(|part| part.trim().parse().unwrap_or(0))
        .collect()
}

fn is_newer(current: &str, candidate: &str) -> bool {
    let current = version_key(current);
    let candidate = version_key(candidate);
    for index in 0..current.len().max(candidate.len()) {
        let old = current.get(index).copied().unwrap_or(0);
        let new = candidate.get(index).copied().unwrap_or(0);
        if old != new {
            return new > old;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_across_tag_and_pkgrel_spellings() {
        assert!(!is_newer("0.5.0", "v0.5.0"));
        // Arch 资产名是 0.5.0-1，tag 是 v0.5.0：pkgrel 不能把版本判成更旧。
        assert!(!is_newer("0.5.0", "0.5.0-1"));
        assert!(is_newer("0.5.0", "v0.5.1"));
        assert!(is_newer("0.4.15", "v0.5.0"));
        assert!(is_newer("0.9.9", "1.0.0"));
        assert!(!is_newer("0.5.0", "v0.4.15"));
        assert!(!is_newer("1.0.0", "v1.0"));
        assert!(!is_newer("0.5.0", "v0.5.0-rc.1"));
    }

    #[test]
    fn only_opens_our_own_release_page() {
        let page = "https://github.com/miaoermua/DGCU-Net-Portal/releases/tag/v0.6.0";
        assert_eq!(release_page(page), page);
        assert_eq!(release_page("https://example.test/evil"), RELEASES_PAGE);
        assert_eq!(release_page("  "), RELEASES_PAGE);
        assert_eq!(release_page(""), RELEASES_PAGE);
    }
}
