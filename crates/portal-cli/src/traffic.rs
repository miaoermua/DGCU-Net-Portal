//! Only LFRadius web counters; no OS/NIC access.
use crate::OnlineSession;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Clone, Default, Serialize, serde::Deserialize, Debug)]
pub struct Rate {
    pub upload_bps: Option<f64>,
    pub download_bps: Option<f64>,
    pub sample_seconds: Option<u64>,
}
#[derive(Clone, Default)]
pub struct AccountingRates {
    previous: HashMap<String, (u64, u64, u64)>,
}
impl AccountingRates {
    pub fn clear(&mut self) {
        self.previous.clear();
    }
    pub fn update(&mut self, rows: &[OnlineSession]) -> HashMap<String, Rate> {
        self.previous
            .retain(|id, _| rows.iter().any(|r| r.radacctid == *id));
        let mut rates = HashMap::new();
        for row in rows {
            let now = (
                row.acctsessiontime,
                row.acctinputoctets,
                row.acctoutputoctets,
            );
            let mut rate = Rate::default();
            if let Some(prev) = self.previous.get(&row.radacctid) {
                // NAS accounting time, not 5-second UI polling time: avoid a false 12x spike.
                if now.0 > prev.0 && now.1 >= prev.1 && now.2 >= prev.2 {
                    let elapsed = now.0 - prev.0;
                    rate = Rate {
                        upload_bps: Some((now.1 - prev.1) as f64 / elapsed as f64),
                        download_bps: Some((now.2 - prev.2) as f64 / elapsed as f64),
                        sample_seconds: Some(elapsed),
                    };
                }
            }
            // Identical snapshots return None, meaning waiting for accounting update, not 0.
            if self
                .previous
                .get(&row.radacctid)
                .is_none_or(|v| now.0 != v.0)
            {
                self.previous.insert(row.radacctid.clone(), now);
            }
            rates.insert(row.radacctid.clone(), rate);
        }
        rates
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session(id: &str, seconds: u64, up: u64, down: u64) -> OnlineSession {
        OnlineSession {
            radacctid: id.into(),
            username: "test-user".into(),
            acctstarttime: "2026-01-01 00:00:00".into(),
            acctsessiontime: seconds,
            framedipaddress: Some("192.0.2.2".parse().unwrap()),
            acctinputoctets: up,
            acctoutputoctets: down,
        }
    }

    /// NAS 换机器或重新计时会让累计字节归零。此时绝不能拿「现在的值减上一次」
    /// 去算速率：`0 - 80000` 会绕成天文数字，界面直接炸出一根离谱的柱子。
    #[test]
    fn counters_restarting_to_zero_do_not_produce_a_wrapped_rate() {
        let mut rates = AccountingRates::default();
        rates.update(&[session("1", 60, 100, 80000)]);
        // 累计值回退：用例会算出负差，靠 u64 减法回绕成巨大的速率。
        let after = rates.update(&[session("1", 120, 50, 0)]);
        assert_eq!(after["1"].download_bps, None);
        assert_eq!(after["1"].upload_bps, None);
    }

    /// accounting 时间本身回退（后台重启）同样不能出速率。
    #[test]
    fn accounting_time_going_backwards_is_not_divided_by() {
        let mut rates = AccountingRates::default();
        rates.update(&[session("1", 120, 700, 7000)]);
        let after = rates.update(&[session("1", 60, 800, 8000)]);
        assert_eq!(after["1"].download_bps, None);
        assert_eq!(after["1"].sample_seconds, None);
    }

    /// 会话从后台列表消失后要清掉它的基线：同一 id 若再出现（重新登录），
    /// 应当从「首次采样」重新开始，而不是拿上一条生命周期的计数做差。
    #[test]
    fn vanished_session_drops_its_baseline() {
        let mut rates = AccountingRates::default();
        rates.update(&[session("1", 60, 100, 1000)]);
        // 会话消失：列表里不再有 id 1。
        rates.update(&[]);
        // 同一 id 再次出现，必须重新采样而不是接着上一段算。
        assert!(rates.update(&[session("1", 90, 200, 2000)])["1"]
            .download_bps
            .is_none());
    }

    /// `clear()` 是「关掉流量统计」时的复位：不留基线，下次打开重新采样。
    #[test]
    fn clear_forgets_baselines_so_measurement_restarts() {
        let mut rates = AccountingRates::default();
        rates.update(&[session("1", 60, 100, 1000)]);
        rates.clear();
        assert!(rates.update(&[session("1", 120, 700, 7000)])["1"]
            .download_bps
            .is_none());
    }

    /// 时间没有推进就不给速率：界面显示的秒数也会一并为空，
    /// 免得出现「0 秒内传了 6 MB」这种自相矛盾的读数。
    #[test]
    fn unchanged_snapshot_reports_no_rate() {
        let mut rates = AccountingRates::default();
        rates.update(&[session("1", 60, 100, 1000)]);
        let after = rates.update(&[session("1", 60, 900, 9000)]);
        assert_eq!(after["1"].download_bps, None);
        assert_eq!(after["1"].sample_seconds, None);
    }
}
