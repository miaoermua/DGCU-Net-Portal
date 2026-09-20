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
#[derive(Default)]
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
