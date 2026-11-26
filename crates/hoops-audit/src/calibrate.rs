use hoops_core::{sha256_bytes, MatchInitSpecV1};
use serde::{Deserialize, Serialize};

use crate::realism::{evaluate_realism_batch, RealismMetricsReport};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationReport {
    pub exp_id: u64,
    pub league: String,
    pub eval_games: usize,
    pub realism: RealismMetricsReport,
    pub within_targets: bool,
    pub failed_checks: Vec<String>,
}

/// Deterministic game_id split helper (`bucket = sha256(game_id:exp_id)[0..8] % 10`).
pub fn assign_split_bucket(game_id: &str, exp_id: u64) -> u64 {
    let mut input = Vec::with_capacity(game_id.len() + 32);
    input.extend_from_slice(game_id.as_bytes());
    input.push(b':');
    input.extend_from_slice(exp_id.to_string().as_bytes());
    let h = sha256_bytes(&input);
    u64::from_le_bytes(h[0..8].try_into().unwrap()) % 10
}

fn check_range(failed: &mut Vec<String>, name: &str, val: f64, lo: f64, hi: f64) {
    if !val.is_finite() || val < lo || val > hi {
        failed.push(format!("{name}={val:.4} outside [{lo:.4}, {hi:.4}]"));
    }
}

pub fn run_calibration_check(
    spec_template: &MatchInitSpecV1,
    exp_id: u64,
    games: usize,
    max_ticks: usize,
) -> CalibrationReport {
    let report = evaluate_realism_batch(spec_template, games, spec_template.seed ^ exp_id, max_ticks);
    let is_fiba = spec_template.ruleset.clocks.shot_clock_policy == hoops_core::ShotClockResetPolicy::FibaArt29;
    let mut failed = Vec::new();
    if games == 0 || report.completed_games != games {
        failed.push(format!("completed_games={}/{}", report.completed_games, games));
    }

    if is_fiba {
        check_range(&mut failed, "avg_total_ppg", report.avg_total_ppg, 66.0, 112.0);
        check_range(&mut failed, "pace_per_48", report.pace_per_48, 76.0, 114.0);
        check_range(&mut failed, "ortg", report.ortg, 90.0, 128.0);
        check_range(&mut failed, "efg_pct", report.efg_pct, 0.43, 0.62);
        check_range(&mut failed, "avg_tpa_share", report.avg_tpa_share, 0.20, 0.55);
        check_range(&mut failed, "avg_fta_per_fga", report.avg_fta_per_fga, 0.12, 0.40);
        check_range(&mut failed, "ft_pct", report.ft_pct, 0.62, 0.88);
        check_range(&mut failed, "avg_orb_rate", report.avg_orb_rate, 0.12, 0.38);
        check_range(&mut failed, "avg_tov_rate", report.avg_tov_rate, 0.07, 0.23);
        check_range(&mut failed, "ast_pct", report.ast_pct, 0.38, 0.82);
    } else {
        check_range(&mut failed, "avg_total_ppg", report.avg_total_ppg, 110.0, 118.0);
        check_range(&mut failed, "pace_per_48", report.pace_per_48, 96.0, 102.0);
        check_range(&mut failed, "ortg", report.ortg, 96.0, 130.0);
        check_range(&mut failed, "efg_pct", report.efg_pct, 0.52, 0.57);
        check_range(&mut failed, "avg_tpa_share", report.avg_tpa_share, 0.36, 0.44);
        check_range(&mut failed, "avg_fta_per_fga", report.avg_fta_per_fga, 0.23, 0.30);
        check_range(&mut failed, "ft_pct", report.ft_pct, 0.66, 0.88);
        check_range(&mut failed, "avg_orb_rate", report.avg_orb_rate, 0.22, 0.28);
        check_range(&mut failed, "avg_tov_rate", report.avg_tov_rate, 0.12, 0.16);
        check_range(&mut failed, "avg_pf_per_team", report.avg_pf_per_team, 17.0, 21.0);
        check_range(&mut failed, "ast_pct", report.ast_pct, 0.60, 0.66);
    }
    let crowding_limit = if is_fiba { 0.35 } else { 0.03 };
    if !report.crowding_ratio.is_finite() || !(0.0..crowding_limit).contains(&report.crowding_ratio) {
        failed.push(format!("crowding_ratio={:.4} outside [0, {crowding_limit:.4})", report.crowding_ratio));
    }
    let min_spacing = if is_fiba { 2.0 } else { 2.15 };
    if !report.median_min_off_pair_m.is_finite() || report.median_min_off_pair_m < min_spacing {
        failed.push(format!("median_min_off_pair_m={:.4} below {min_spacing:.2}", report.median_min_off_pair_m));
    }

    CalibrationReport {
        exp_id,
        league: spec_template.ruleset.rule_id.clone(),
        eval_games: games,
        realism: report,
        within_targets: failed.is_empty(),
        failed_checks: failed,
    }
}
