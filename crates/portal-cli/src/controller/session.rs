//! 轮询与会话观察：把后台返回的会话列表翻译成控制器状态。
//!
//! 只依赖 `Controller` 上的共享原语，不感知重拨调度——掉线判定写完状态就
//! 结束，是否重拨由 `reconnect` 决定。
use super::{Controller, LossKind, Snapshot};
use crate::{network, AppError, OnlineSession};
use std::{collections::HashMap, net::IpAddr, time::Instant};

impl Controller {
    /// 唯一发起 `onlinelog` 采集的地方。
    ///
    /// 先按当前网卡重建 client（地址变了就换新源地址），再拉一次服务端会话列表。
    /// 成功后写入 `rows` 并推进 `rows_epoch`：这一代数据同时供会话观察、绑定和
    /// 之后的流量结算使用，不会再额外请求一次。
    ///
    /// 失败时不动 `rows`：拿不到列表不等于会话消失，最后一次有效数据继续供展示与
    /// 判定使用。只有传输层失败/超时才记成"本机连接异常"的证据——后台 5xx、
    /// 登录态失效或解析失败不能反推本机网络有问题。
    pub(super) async fn sync_sessions(&mut self) -> Result<(), AppError> {
        self.rebind_changed_network();
        let api = self.api.as_ref().ok_or(AppError::Rejected)?;
        let rows = match api.sessions().await {
            Ok(rows) => rows,
            Err(error) => {
                if Self::poll_failure_is_network(&error) {
                    self.last_poll_failure = Some(Instant::now());
                }
                return Err(error);
            }
        };
        self.rows = rows;
        self.rows_epoch = self.rows_epoch.wrapping_add(1);
        // 只有真正拿到列表才算"这一次采集成功"。失败路径不刷新，界面才能从
        // `collect_age_ms` 看出数据正在变旧。
        self.last_collect_at = Some(Instant::now());
        Ok(())
    }
    pub async fn refresh(&mut self) -> Result<Snapshot, AppError> {
        self.sync_sessions().await?;
        self.bind_new_session();
        // 手动或低频刷新也更新状态，但不推进缺失计数：计数只由 5 秒 worker
        // 负责，否则两条路径会让 `missing >= 3` 提前到达。
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
        // 本次结算消费的是当前这一代采集结果；下一轮结算要等到又拉到新数据，
        // 才能把同一份快照再次当成"新样本"。
        self.rates_epoch = self.rows_epoch;
    }
    /// 清掉计费基线：关闭流量统计、关闭后台刷新、清空会话状态时都要走这里。
    ///
    /// 基线一旦跨过"关闭期"就不再是本会话的采样起点：保留它会让恢复后的第一笔
    /// 速率变成"关闭期累计字节 ÷ 整段关闭时长"，看起来像当前速率其实是最长数小时
    /// 的平均值。同时把 `rates_epoch` 追平，关闭期间攒下的采集结果不补结算。
    pub(super) fn clear_rate_baseline(&mut self) {
        self.accounting.clear();
        self.latest_rates.clear();
        self.rates_epoch = self.rows_epoch;
    }

    /// 丢掉单条会话的速率快照与计费基线。
    ///
    /// 会话确认下线后，它的速率不该继续出现在快照里；基线也必须一起丢——同一条
    /// `radacctid` 再出现时应当是全新样本，不能跨过下线期做差分。
    pub(super) fn forget_session_rate(&mut self, id: &str) {
        self.latest_rates.remove(id);
        self.accounting.forget(id);
    }

    /// 用当前会话列表重建基线，但**不产出速率**。
    ///
    /// `AccountingRates::update` 在没有历史样本时只记录基线并返回空速率，所以这一步
    /// 既算不出假数据，也不会被界面当成一次有效结算。重新打开流量统计后不必再等
    /// 满两轮结算，下一轮定时结算就能给出第一个真实区间。
    pub(super) fn seed_rate_baseline(&mut self) {
        self.accounting.clear();
        self.latest_rates.clear();
        if self.settings.traffic_enabled
            && self.settings.refresh_policy != crate::settings::RefreshPolicy::Disabled
        {
            self.latest_rates = self.accounting.update(&self.rows);
        }
        self.rates_epoch = self.rows_epoch;
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

    /// 流量统计的定时结算：只看时钟，不发任何 HTTP 请求。
    ///
    /// 由后台 worker 按原有的约 1 分钟周期（含抖动）调用，消费统一采集器最近一次
    /// 成功采集写入的 `rows`。因此"采集频率"与"结算频率"解耦：5 秒一次的采集不会
    /// 变成 5 秒一次的流量结算，也不会靠"每 N 次采集算一次"来凑周期。
    ///
    /// 没有新的有效采集结果时直接返回，`latest_rates` 保持上一次的值——与改造前
    /// "这一轮拿不到数据就不结算"的语义一致，不把旧数据伪装成新样本。
    pub fn rate_tick(&mut self) {
        if self.api.is_none() {
            return;
        }
        // 两种"不结算"的情况都要顺手清基线：只停结算而不清样本，恢复后的第一笔
        // 速率就会把关闭期的累计字节算进来。清空操作是幂等的，空转时几乎不花钱。
        if self.settings.refresh_policy == crate::settings::RefreshPolicy::Disabled
            || !self.settings.traffic_enabled
        {
            self.clear_rate_baseline();
            return;
        }
        if self.rows_epoch == self.rates_epoch {
            return;
        }
        self.update_rates();
    }
}
