use hoops_core::HashTraceV1;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BisectReport {
    pub diverged: bool,
    pub first_diverged_tick: Option<u32>,
    pub compared_records: usize,
    pub reason: String,
}

/// Binary-search two frozen `HTR1` hash traces to locate exact first diverged `sim_tick` (§2.3).
pub fn bisect_hash_traces(a: &HashTraceV1, b: &HashTraceV1) -> BisectReport {
    if a.init_spec_sha256 != b.init_spec_sha256 {
        return BisectReport {
            diverged: true,
            first_diverged_tick: Some(a.start_tick),
            compared_records: 0,
            reason: "init_spec_sha256 differs".to_owned(),
        };
    }
    if a.applied_commands_sha256 != b.applied_commands_sha256 {
        return BisectReport {
            diverged: true,
            first_diverged_tick: Some(a.start_tick),
            compared_records: 0,
            reason: "applied_commands_sha256 differs".to_owned(),
        };
    }

    let n = a.records.len().min(b.records.len());
    if n == 0 {
        return BisectReport {
            diverged: a.records.len() != b.records.len(),
            first_diverged_tick: None,
            compared_records: 0,
            reason: "empty trace".to_owned(),
        };
    }

    let matches_at = |i: usize| -> bool {
        let ra = &a.records[i];
        let rb = &b.records[i];
        ra.tick == rb.tick
            && ra.state_hash == rb.state_hash
            && ra.event_hash == rb.event_hash
            && ra.rolling_hash == rb.rolling_hash
    };

    if matches_at(n - 1) && a.records.len() == b.records.len() {
        return BisectReport {
            diverged: false,
            first_diverged_tick: None,
            compared_records: n,
            reason: "bit-identical".to_owned(),
        };
    }

    if matches_at(n - 1) && a.records.len() != b.records.len() {
        return BisectReport {
            diverged: true,
            first_diverged_tick: Some(a.records[n - 1].tick + 1),
            compared_records: n,
            reason: format!(
                "trace lengths differ ({} vs {})",
                a.records.len(),
                b.records.len()
            ),
        };
    }

    // Binary search for smallest index `lo` where `!matches_at(lo)`
    let mut lo = 0usize;
    let mut hi = n - 1;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if matches_at(mid) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }

    let ra = &a.records[lo];
    let rb = &b.records[lo];
    let kind = if ra.state_hash != rb.state_hash {
        "state_hash"
    } else if ra.event_hash != rb.event_hash {
        "event_hash"
    } else {
        "rolling_hash_or_tick"
    };

    BisectReport {
        diverged: true,
        first_diverged_tick: Some(ra.tick),
        compared_records: n,
        reason: format!("diverged at tick {} on {kind}", ra.tick),
    }
}
