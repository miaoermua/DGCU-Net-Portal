//! 轮询与会话观察：把后台返回的会话列表翻译成控制器状态。
//!
//! 只依赖 `Controller` 上的共享原语，不感知重拨调度——掉线判定写完状态就
//! 结束，是否重拨由 `reconnect` 决定。
use super::{Controller, LossKind, Snapshot};
use crate::{network, AppError, OnlineSession};
use std::{collections::HashMap, net::IpAddr, time::Instant};

impl Controller {
    pub async fn refresh(&mut self) -> Result<Snapshot, AppError> {
        self.rebind_changed_network();
        let api = self.api.as_ref().ok_or(AppError::Rejected)?;
        let rows = match api.sessions().await {
            Ok(rows) => rows,
            Err(error) => {
                // 拿不到列表就不动会话状态：请求失败不等于会话消失。
                // 只有传输层失败/超时才算"本机连接异常"的证据，后台 5xx、
                // 登录态失效或解析失败不能反推本机网络有问题。
                if Self::poll_failure_is_network(&error) {
                    self.last_poll_failure = Some(Instant::now());
                }
                return Err(error);
            }
        };
        self.rows = rows;
        self.bind_new_session();
        // 手动或低频刷新也更新状态，但不推进缺失计数：计数只由 5 秒 worker
        // 负责，否则两个 worker 会让 `missing >= 3` 提前到达。
        self.observe_session(false);
        self.last_poll_failure = None;
        self.update_rates();
        Ok(self.snapshot())
    }
    fn update_rates(&mut self) {
        self.latest_rates = if self.settings.traffic_enabled {
            self.accounting.update(&self.rows)
        } else {
            self.accounting.clear();
            HashMap::new()
        };
    }
    /// 判定并记录选中会话的在线状态。`count` 为 true 时顺带推进掉线计数。
    ///
    /// 判定必须在 `drops_state_on_loss()` 清理之前完成：清理会丢掉 `selected`，
    /// 之后就无从知道是哪条会话消失、原因是什么。
    fn observe_session(&mut self, count: bool) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        if self.rows.iter().any(|row| row.radacctid == id) {
            self.missing = 0;
            self.status = "session_online".into();
            // 原会话回来了（或用户已重新绑定）：旧目标丢失的告警不能永久残留。
            // 但「疑似被挤下线」的重拨保护是策略，不是"这一轮列表里有没有它"
            // 的事实——普通轮询成功、切换会话都不能解除它，只有显式恢复才行。
            self.loss = None;
            self.message = if self.redial_hold {
                "所选会话在后台在线列表中；疑似被挤下线期间暂停的自动重拨仍未恢复，需要时请手动重新连接".into()
            } else {
                "所选会话在后台在线列表中".into()
            };
            return;
        }
        if count {
            self.missing += 1;
        }
        let kind = if self.local_ip_in(&self.rows) == Some(false)
            && self.rows.len() > 2
            && self.recent_poll_failure()
        {
            LossKind::SuspectedEviction
        } else {
            LossKind::Unclear
        };
        // 先把结论与提示落定，再执行可能清空 `selected` 的释放。
        self.record_loss(&id, kind);
        self.status = "offline".into();
        self.message = self
            .loss
            .as_ref()
            .map(|loss| self.loss_message(loss))
            .unwrap_or_default();
        if self.drops_state_on_loss() {
            self.release();
        }
    }
    /// 旧的按次调用入口：保留 `missing` 的原有语义，供 5 秒 worker 与测试使用。
    pub(super) fn update_session_status(&mut self) {
        self.observe_session(true);
    }
    pub fn select(&mut self, id: &str) -> Result<Snapshot, AppError> {
        if !self.rows.iter().any(|r| r.radacctid == id) {
            return Err(AppError::SessionNotFound);
        }
        self.selected = Some(id.into());
        self.awaiting_session = None;
        self.logs.record(crate::logging::Event::SelectSession);
        self.missing = 0;
        // 用户显式选了一条，旧目标丢失的告警到此为止。
        self.loss = None;
        // 但「疑似被挤下线」期间的重拨保护不跟着解除：选择只换了管理目标，
        // 不代表本机已经安全上线。随手点中的那条很可能是挤掉本机的设备，
        // 此时恢复自动重拨会用本机账号再认证一次，反而可能把对方挤下去。
        // 想恢复自动重拨必须走一次显式 connect（它会先 clear）。
        self.message = if self.redial_hold {
            "已切换管理会话；疑似被挤下线期间暂停的自动重拨仍未恢复，需要时请手动重新连接".into()
        } else {
            "已切换管理会话".into()
        };
        Ok(self.snapshot())
    }
    pub(super) fn bind_new_session(&mut self) {
        // 已经判定原会话丢失时，列表里新出现的会话更可能属于挤掉本机的设备：
        // 不自动认领，交给用户显式选择。
        if self.loss.is_some() {
            return;
        }
        let Some(before) = &self.awaiting_session else {
            return;
        };
        let mut added = self
            .rows
            .iter()
            .filter(|row| !before.contains(&row.radacctid));
        match (added.next(), added.next()) {
            (Some(row), None) => {
                self.selected = Some(row.radacctid.clone());
                self.awaiting_session = None;
            }
            (Some(_), Some(_)) => {
                self.awaiting_session = None;
            } // An explicit selection is required.
            _ => {}
        }
    }
    pub(super) fn unique_local_session(
        rows: &[OnlineSession],
        network: Option<&network::NetworkContext>,
    ) -> Option<String> {
        let ip = network?.ipv4.parse::<IpAddr>().ok()?;
        let mut matches = rows.iter().filter(|row| row.framedipaddress == Some(ip));
        let first = matches.next()?;
        if matches.next().is_some() {
            None
        } else {
            Some(first.radacctid.clone())
        }
    }

    /// Invoked by the app's worker, not by the WebView timer (works with window hidden).
    pub async fn tick(&mut self) {
        if self.api.is_none()
            || self.settings.refresh_policy == crate::settings::RefreshPolicy::Disabled
        {
            return;
        }
        if let Err(e) = self.refresh().await {
            self.logs.error(&e);
            // 已判定的会话丢失提示不能被一次网络抖动冲掉。
            if self.loss.is_none() {
                self.message = e.to_string();
            }
        }
    }
}
