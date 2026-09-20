//! Optional shared CLI/GUI diagnostics. Only typed events enter this buffer:
//! no request bodies, URLs, credentials, session IDs or raw server errors.
use crate::settings::RefreshPolicy;
use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

const LIMIT: usize = 300;
#[derive(Clone, Copy)]
pub enum Event {
    Enabled,
    BackendLogin,
    BackendAccepted,
    Discover,
    ProbeFallback,
    ProbeHttpRedirect,
    ProbeHtmlRedirect,
    ProbeFound,
    ProbeNoRedirect,
    ProbeTimeout,
    ProbeUntrusted,
    ProbeLoop,
    ReadForm,
    Submit,
    WaitPortal,
    PollPortal,
    WaitDial,
    PollDial,
    Accepted,
    ReadSessions,
    SelectSession,
    Disconnect,
    Disconnected,
    LocalReleased,
    AutoRetry,
    RetryPaused,
    NetworkError,
    InterfaceError,
    Rejected,
    ResponseError,
    Timeout,
    SessionMissing,
    DisconnectPending,
    RefreshOneSecond,
    RefreshTwoSeconds,
    RefreshFiveSeconds,
    RefreshRandom,
    RefreshOneMinute,
    RefreshDisabled,
}
impl Event {
    pub fn refresh_policy(policy: RefreshPolicy) -> Self {
        match policy {
            RefreshPolicy::OneSecond => Self::RefreshOneSecond,
            RefreshPolicy::TwoSeconds => Self::RefreshTwoSeconds,
            RefreshPolicy::FiveSeconds => Self::RefreshFiveSeconds,
            RefreshPolicy::Random => Self::RefreshRandom,
            RefreshPolicy::OneMinute => Self::RefreshOneMinute,
            RefreshPolicy::Disabled => Self::RefreshDisabled,
        }
    }
    fn detail(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Enabled => (
                "info",
                "log.enabled",
                "日志已开启，仅记录本次进程的诊断事件",
            ),
            Self::BackendLogin => ("info", "backend.login", "正在登录自助后台"),
            Self::BackendAccepted => ("info", "backend.accepted", "自助后台登录成功"),
            Self::Discover => ("info", "portal.discover", "正在通过 HTTP 探测认证页面"),
            Self::ProbeFallback => (
                "info",
                "portal.probe.fallback",
                "首选探测未发现认证页，尝试备用 HTTP 探测",
            ),
            Self::ProbeHttpRedirect => (
                "info",
                "portal.probe.http_redirect",
                "发现 HTTP 跳转，继续寻找认证页",
            ),
            Self::ProbeHtmlRedirect => (
                "info",
                "portal.probe.html_redirect",
                "发现网页中的静态认证跳转",
            ),
            Self::ProbeFound => (
                "info",
                "portal.probe.found",
                "已定位配置服务器上的 Portal 认证页",
            ),
            Self::ProbeNoRedirect => (
                "warn",
                "portal.probe.no_redirect",
                "响应未包含认证跳转；可能已联网、域名被放行或页面需浏览器执行脚本",
            ),
            Self::ProbeTimeout => (
                "warn",
                "portal.probe.timeout",
                "探测请求超时；未进入 Portal 认证阶段",
            ),
            Self::ProbeUntrusted => (
                "error",
                "portal.probe.untrusted",
                "跳转服务器与配置不一致，已停止，不发送凭据",
            ),
            Self::ProbeLoop => (
                "error",
                "portal.probe.loop",
                "跳转循环或次数过多，已停止探测",
            ),
            Self::ReadForm => ("info", "portal.form", "正在读取认证表单"),
            Self::Submit => ("info", "portal.submit", "正在提交 Portal 认证"),
            Self::WaitPortal => ("info", "portal.wait", "正在等待 Portal 上线结果"),
            Self::PollPortal => ("info", "portal.poll", "查询 Portal 上线结果"),
            Self::WaitDial => ("info", "dial.wait", "进入代拨等待阶段"),
            Self::PollDial => ("info", "dial.poll", "查询代拨状态"),
            Self::Accepted => ("info", "portal.accepted", "认证系统已确认成功"),
            Self::ReadSessions => ("info", "sessions.read", "已读取后台会话列表"),
            Self::SelectSession => ("info", "session.select", "已选择要管理的会话"),
            Self::Disconnect => ("info", "session.disconnect", "正在请求指定会话下线"),
            Self::Disconnected => (
                "info",
                "session.disconnected",
                "已确认目标会话从后台列表消失",
            ),
            Self::LocalReleased => ("info", "local.clear", "已释放客户端会话和凭据"),
            Self::AutoRetry => ("info", "reconnect.start", "会话连续离线，正在自动重拨"),
            Self::RetryPaused => ("warn", "reconnect.paused", "自动重拨已暂停，请手动检查"),
            Self::NetworkError => ("error", "network.error", "网络请求未完成，请检查连接或代理"),
            Self::InterfaceError => ("error", "network.interface", "所选网卡没有可用 IPv4/MAC"),
            Self::Rejected => ("error", "auth.rejected", "认证被拒绝或后台登录已过期"),
            Self::ResponseError => ("error", "response.error", "响应或跳转不符合预期"),
            Self::Timeout => ("error", "auth.timeout", "认证或代拨等待超时"),
            Self::SessionMissing => ("warn", "session.missing", "无法确定目标会话，请手动选择"),
            Self::DisconnectPending => (
                "warn",
                "session.pending",
                "已提交下线请求，远端状态尚未确认",
            ),
            Self::RefreshOneSecond => ("info", "refresh.policy", "后台刷新策略：每 1 秒"),
            Self::RefreshTwoSeconds => ("info", "refresh.policy", "后台刷新策略：每 2 秒"),
            Self::RefreshFiveSeconds => ("info", "refresh.policy", "后台刷新策略：每 5 秒"),
            Self::RefreshRandom => ("info", "refresh.policy", "后台刷新策略：随机 1-10 秒"),
            Self::RefreshOneMinute => ("info", "refresh.policy", "后台刷新策略：每 1 分钟"),
            Self::RefreshDisabled => ("info", "refresh.policy", "后台刷新策略：禁止刷新"),
        }
    }
}
#[derive(Clone, Serialize)]
pub struct LogEntry {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub level: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}
#[derive(Default)]
struct Inner {
    enabled: bool,
    stderr: bool,
    sequence: u64,
    entries: VecDeque<LogEntry>,
}
#[derive(Clone, Default)]
pub struct LogBuffer(Arc<Mutex<Inner>>);
impl LogBuffer {
    pub fn set_enabled(&self, enabled: bool) {
        let changed = {
            let mut inner = self.0.lock().unwrap_or_else(|e| e.into_inner());
            let changed = inner.enabled != enabled;
            inner.enabled = enabled;
            if !enabled {
                inner.entries.clear();
            }
            changed
        };
        if changed && enabled {
            self.record(Event::Enabled);
        }
    }
    pub fn stderr(&self, enabled: bool) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).stderr = enabled;
    }
    pub fn record(&self, event: Event) {
        let mut inner = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !inner.enabled {
            return;
        }
        let (level, code, message) = event.detail();
        inner.sequence += 1;
        let entry = LogEntry {
            sequence: inner.sequence,
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            level,
            code,
            message,
        };
        if inner.stderr {
            eprintln!("[{}] [{}] {} {}", entry.timestamp_ms, level, code, message);
        }
        if inner.entries.len() == LIMIT {
            inner.entries.pop_front();
        }
        inner.entries.push_back(entry);
    }
    pub fn error(&self, error: &crate::AppError) {
        use crate::AppError;
        self.record(match error {
            AppError::Network => Event::NetworkError,
            AppError::NetworkInterface(_) => Event::InterfaceError,
            AppError::DiscoveryTimeout => Event::ProbeTimeout,
            AppError::DiscoveryNotFound => Event::ProbeNoRedirect,
            AppError::DiscoveryUntrusted => Event::ProbeUntrusted,
            AppError::DiscoveryLoop => Event::ProbeLoop,
            AppError::Rejected => Event::Rejected,
            AppError::Timeout(_) => Event::Timeout,
            AppError::SessionNotFound | AppError::AmbiguousSession => Event::SessionMissing,
            AppError::DisconnectPending => Event::DisconnectPending,
            _ => Event::ResponseError,
        });
    }
    pub fn entries(&self) -> Vec<LogEntry> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .iter()
            .cloned()
            .collect()
    }
    pub fn clear(&self) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_is_empty_and_disabling_removes_history() {
        let logs = LogBuffer::default();
        logs.record(Event::Submit);
        assert!(logs.entries().is_empty());
        logs.set_enabled(true);
        logs.record(Event::Submit);
        assert_eq!(logs.entries().len(), 2);
        logs.set_enabled(false);
        logs.record(Event::Accepted);
        assert!(logs.entries().is_empty());
    }
    #[test]
    fn bounded_and_shared_with_client_clones() {
        let logs = LogBuffer::default();
        logs.set_enabled(true);
        let other = logs.clone();
        for _ in 0..400 {
            other.record(Event::ReadSessions);
        }
        assert_eq!(logs.entries().len(), LIMIT);
        assert_eq!(logs.entries().last().unwrap().sequence, 401);
    }
    #[test]
    fn raw_errors_cannot_leak_private_text() {
        let logs = LogBuffer::default();
        logs.set_enabled(true);
        logs.error(&crate::AppError::InvalidResponse(
            "password=SYNTHETIC-SECRET",
        ));
        logs.error(&crate::AppError::Timeout("token=SYNTHETIC-SECRET"));
        assert!(!serde_json::to_string(&logs.entries())
            .unwrap()
            .contains("SYNTHETIC-SECRET"));
    }
}
