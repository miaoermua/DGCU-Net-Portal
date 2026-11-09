//! 轮询与会话观察：把后台返回的会话列表翻译成控制器状态。
//!
//! 只依赖 `Controller` 上的共享原语，不感知重拨调度——掉线判定写完状态就
//! 结束，是否重拨由 `reconnect` 决定。
use super::{Controller, LossKind, NetworkResolver, Snapshot};
use crate::{network, AppError, OnlineSession, PortalClient};
use std::{
    collections::HashMap,
    net::IpAddr,
    time::{Duration, Instant},
};

/// 一次常规采集的票据：锁内领、锁外用、锁内提交。
///
/// 它只带走"发起时刻的只读拷贝"，不持有控制器内部状态的可变引用，所以可以在
/// 锁外安全地等网络。三样东西缺一不可：连接代次（提交时确认还是同一个连接）、
/// 票据序号（拒绝乱序返回的旧结果）、客户端克隆（`PortalClient` 内部是共享的
/// Cookie jar，克隆件照样带登录态）。
pub(crate) struct CollectTicket {
    generation: u64,
    attempt: u64,
    interface: String,
    bound: Option<network::NetworkContext>,
    api: PortalClient,
    resolver: NetworkResolver,
}

/// 手动刷新的提交结论：调用方据此区分"这次结果写进去了"和"被更新的操作取代"。
///
/// 两种情况都带着当前快照——被取代时那份快照来自别的请求，比这次拿到的更新，
/// 所以照样可以回给用户；区别只在提示语，不在数据可用性。
#[derive(Debug)]
pub(crate) enum ManualRefresh {
    /// 已提交：快照里包含本次采集的结果。
    Applied(Snapshot),
    /// 结果过期（连接换了、下线了，或有更新的采集先提交），一个字段都没写。
    Superseded(Snapshot),
}

#[cfg(test)]
impl ManualRefresh {
    pub(crate) fn into_snapshot(self) -> Snapshot {
        match self {
            Self::Applied(snapshot) | Self::Superseded(snapshot) => snapshot,
        }
    }
}

/// 锁外采集的结果，原样交回控制器提交。
pub(crate) struct CollectResult {
    /// 网卡解析结果。`Err` 只表示"这次没解析出来"，与认证失败无关。
    pub(super) network: Result<Option<network::NetworkContext>, String>,
    pub(super) rows: Result<Vec<OnlineSession>, AppError>,
    /// 实际发请求用的客户端（地址变过就已经重建）。
    client: PortalClient,
    /// 是否重建过，决定要不要把新绑定写回控制器。
    pub(super) rebound: bool,
}

/// 网卡解析的等待上限。
///
/// macOS 的 MAC 兜底要起 `ifconfig` + `networksetup` 两个子进程（`network.rs`
/// 注释记录本机实测约 0.35 秒），2 秒留了 5 倍余量。超时只表示"这次不等了"：
/// 请求照旧用旧绑定发出，解析失败不会被当成认证失败。
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);

/// 在阻塞线程池里解析网卡，避免子进程把异步线程堵住。
///
/// `std` 无法在超时后杀掉已经起的子进程，那个阻塞线程会自己跑完并把结果丢掉；
/// 至少调用方（以及控制器锁）不必再等它。
async fn resolve_off_lock(
    resolver: NetworkResolver,
    interface: String,
) -> Result<Option<network::NetworkContext>, String> {
    let task = tokio::task::spawn_blocking(move || resolver(&interface));
    match tokio::time::timeout(RESOLVE_TIMEOUT, task).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(format!("网卡解析任务异常：{error}")),
        Err(_) => Err("网卡解析超时".into()),
    }
}

impl Controller {
    /// 领票：拿发起采集所需的只读拷贝，并登记票据序号。
    ///
    /// 这里不做任何 IO。`api` 为空（未登录后台）时返回 `None`，调用方直接跳过。
    pub(crate) fn begin_collect(&mut self) -> Option<CollectTicket> {
        let api = self.api.as_ref()?.clone();
        self.collect_attempt = self.collect_attempt.wrapping_add(1);
        Some(CollectTicket {
            generation: self.connection_generation,
            attempt: self.collect_attempt,
            interface: self.settings.interface_name.clone(),
            bound: self.bound_network.clone(),
            api,
            resolver: self.resolve_network.clone(),
        })
    }

    /// 锁外执行：解析网卡、必要时重建客户端、拉一次 `/sessions`。
    ///
    /// 全程只用一个克隆出来的 `PortalClient`，不碰控制器上的任何字段。
    pub(crate) async fn collect(ticket: &CollectTicket) -> CollectResult {
        let network = resolve_off_lock(ticket.resolver.clone(), ticket.interface.clone()).await;
        let mut client = ticket.api.clone();
        let mut rebound = false;
        if let Ok(current) = &network {
            if current != &ticket.bound {
                let local = current
                    .as_ref()
                    .and_then(|context| context.ipv4.parse::<IpAddr>().ok());
                rebound = client.rebind(local).is_ok();
            }
        }
        let rows = client.sessions().await;
        CollectResult {
            network,
            rows,
            client,
            rebound,
        }
    }

    /// 提交校验：连接代次没变、并且比已应用的票据更新，才允许写入。
    pub(super) fn accept_collection(&mut self, ticket: &CollectTicket) -> bool {
        if ticket.generation != self.connection_generation {
            return false;
        }
        if ticket.attempt <= self.applied_collect {
            return false;
        }
        self.applied_collect = ticket.attempt;
        true
    }

    /// 把网卡解析结果与重建后的客户端写回控制器。
    pub(super) fn apply_network(&mut self, ticket: &CollectTicket, result: &CollectResult) {
        // 解析失败时地址未知：本地 IP 不能作为"被挤下线"的证据。
        self.current_network = result.network.as_ref().ok().cloned().flatten();
        if result.rebound {
            self.bound_network = self.current_network.clone();
            self.api = Some(result.client.clone());
            self.logs.record(crate::logging::Event::NetworkRebind);
        } else if matches!(&result.network, Ok(current) if current != &ticket.bound) {
            // 解析出了新地址但重建失败：保持旧绑定，等下一轮再试。
            self.logs.record(crate::logging::Event::NetworkRebindFailed);
        }
    }

    /// 写入一代有效会话数据。只有真正拿到列表才走这里，因此心跳也只在这里刷新。
    pub(super) fn commit_rows(&mut self, rows: Vec<OnlineSession>) {
        self.rows = rows;
        self.rows_epoch = self.rows_epoch.wrapping_add(1);
        self.collect_clock.mark();
    }

    /// 把"这次请求是传输层失败/超时"记成连接异常的证据，其他错误不算。
    pub(super) fn note_poll_failure(&mut self, error: &AppError) {
        if Self::poll_failure_is_network(error) {
            self.last_poll_failure = Some(Instant::now());
        }
    }

    /// 提交一次手动刷新的采集结果。
    ///
    /// 结果过期（期间连接换了、下线了，或有更新的采集先提交）时一个字都不写，
    /// 直接返回当前快照：界面看到的是最新状态，而不是一个迟到的旧响应。
    pub(crate) fn finish_manual_refresh(
        &mut self,
        ticket: CollectTicket,
        result: CollectResult,
    ) -> Result<ManualRefresh, AppError> {
        if !self.accept_collection(&ticket) {
            return Ok(ManualRefresh::Superseded(self.snapshot()));
        }
        self.apply_network(&ticket, &result);
        let rows = match result.rows {
            Ok(rows) => rows,
            Err(error) => {
                self.note_poll_failure(&error);
                return Err(error);
            }
        };
        self.commit_rows(rows);
        self.bind_new_session();
        // 手动或低频刷新也更新状态，但不推进缺失计数：计数只由 5 秒 worker
        // 负责，否则两条路径会让 `missing >= 3` 提前到达。
        self.observe_session(false);
        self.last_poll_failure = None;
        if self.rate_settlement_allowed() {
            self.update_rates();
        } else {
            // 只跳过 `update_rates()` 不够：手动刷新刚拉到的这一代数据会留在
            // `rows_epoch` 里，而关闭前/手动建立过的基线还在 accounting 里，
            // 恢复后第一笔速率就会跨过这段关闭期。这里连同基线一起清掉。
            self.clear_rate_baseline();
        }
        Ok(ManualRefresh::Applied(self.snapshot()))
    }
    /// 手动刷新：锁内领票 → 锁外请求 → 锁内提交。
    ///
    /// 生产路径由 daemon 的 `State::refresh()` 拆成三段调用；这个方法把三步放在
    /// 一次持锁里跑完，只给需要"一步到位"的单元测试用。
    #[cfg(test)]
    pub async fn refresh(&mut self) -> Result<Snapshot, AppError> {
        let Some(ticket) = self.begin_collect() else {
            return Err(AppError::Rejected);
        };
        let result = Self::collect(&ticket).await;
        Ok(self.finish_manual_refresh(ticket, result)?.into_snapshot())
    }
    /// 当前设置是否允许结算并发布流量速率。
    ///
    /// 后台结算 worker、手动刷新、重建基线三处必须共用同一套判断，否则会出现
    /// "一处允许结算、另一处把基线清掉"的自相矛盾状态。
    fn rate_settlement_allowed(&self) -> bool {
        self.settings.traffic_enabled
            && self.settings.refresh_policy != crate::settings::RefreshPolicy::Disabled
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
        if self.rate_settlement_allowed() {
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
        if !self.rate_settlement_allowed() {
            self.clear_rate_baseline();
            return;
        }
        if self.rows_epoch == self.rates_epoch {
            return;
        }
        self.update_rates();
    }
}
