use hoops_core::{sha256_bytes, GameEndReason, MatchInitSpecV1, NullSimSink, SimulationEngine, StepStatus};
use hoops_core::step_with_batch_ai;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

pub const BOOTSTRAP_BASE_SEED: u64 = 0x484F_4F50_5331_3600;

#[inline]
pub fn bootstrap_base_seed_for_exp(exp_id: u64) -> u64 {
    BOOTSTRAP_BASE_SEED ^ exp_id
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbGameResult {
    pub seed: u64,
    pub diff_a: f64,
    pub diff_b: f64,
    pub both_completed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbComparisonReport {
    pub exp_id: u64,
    pub base_seed: u64,
    pub games: usize,
    pub completed_pairs: usize,
    pub pairs: Vec<AbGameResult>,
    pub mean_delta_margin: f64,
    pub ci95_low: f64,
    pub ci95_high: f64,
}

fn eval_single_margin(spec: &MatchInitSpecV1, seed: u64, max_ticks: usize) -> (f64, bool) {
    let mut s = spec.clone();
    s.seed = seed;
    s.state_hash_mode = hoops_core::StateHashMode::Disabled;
    let mut sink = NullSimSink;
    let mut eng = SimulationEngine::new(&s, &mut sink).expect("initialize paired game");
    let mut cmds = Vec::new();
    for _ in 0..max_ticks {
        cmds.clear();
        let st = step_with_batch_ai(&mut eng, &mut sink, &mut cmds);
        if matches!(st, StepStatus::Finished { .. } | StepStatus::Aborted { .. }) {
            break;
        }
    }
    let h = eng.state.team_states[0].score_on_court as f64;
    let a = eng.state.team_states[1].score_on_court as f64;
    (h - a, eng.state.end_reason == Some(GameEndReason::Normal))
}

pub fn run_paired_ab(
    spec_a: &MatchInitSpecV1,
    spec_b: &MatchInitSpecV1,
    games: usize,
    exp_id: u64,
    max_ticks: usize,
) -> AbComparisonReport {
    let base_seed = spec_a.seed;
    let pairs: Vec<_> = (0..games)
        .into_par_iter()
        .map(|i| {
            let mut buf = [0; 24];
            buf[..8].copy_from_slice(&base_seed.to_le_bytes());
            buf[8..16].copy_from_slice(&exp_id.to_le_bytes());
            buf[16..].copy_from_slice(&(i as u64).to_le_bytes());
            let h = sha256_bytes(&buf);
            let seed = u64::from_le_bytes(h[..8].try_into().unwrap());
            let (diff_a, completed_a) = eval_single_margin(spec_a, seed, max_ticks);
            let (diff_b, completed_b) = eval_single_margin(spec_b, seed, max_ticks);
            AbGameResult { seed, diff_a, diff_b, both_completed: completed_a && completed_b }
        })
        .collect();
    let completed_pairs = pairs.iter().filter(|pair| pair.both_completed).count();
    let deltas: Vec<_> = pairs.iter().filter(|pair| pair.both_completed)
        .map(|pair| pair.diff_b - pair.diff_a).collect();
    if deltas.is_empty() {
        return AbComparisonReport {
            exp_id, base_seed, games, completed_pairs, pairs,
            mean_delta_margin: 0.0, ci95_low: 0.0, ci95_high: 0.0,
        };
    }
    let n = deltas.len() as f64;
    let mean = deltas.iter().sum::<f64>() / n;

    // Deterministic bootstrap CI using exp_id-derived base seed (0x484F_4F50_5331_3600 ^ exp_id)
    let boot_base_seed = bootstrap_base_seed_for_exp(exp_id);
    let n_boot = 250usize;
    let mut boot_means = Vec::with_capacity(n_boot);
    for b in 0..n_boot {
        let mut sum = 0.0;
        for j in 0..deltas.len() {
            let mut buf = [0; 24];
            buf[..8].copy_from_slice(&boot_base_seed.to_le_bytes());
            buf[8..16].copy_from_slice(&exp_id.to_le_bytes());
            buf[16..].copy_from_slice(&((b * deltas.len() + j) as u64).to_le_bytes());
            let h = sha256_bytes(&buf);
            let idx = u64::from_le_bytes(h[..8].try_into().unwrap()) % deltas.len() as u64;
            sum += deltas[idx as usize];
        }
        boot_means.push(sum / n);
    }
    boot_means.sort_by(|a, b| a.total_cmp(b));
    let ci_low = boot_means[((0.025 * (n_boot as f64)) as usize).min(n_boot - 1)];
    let ci_high = boot_means[((0.975 * (n_boot as f64)) as usize).min(n_boot - 1)];

    AbComparisonReport {
        exp_id,
        base_seed,
        games,
        completed_pairs,
        pairs,
        mean_delta_margin: mean,
        ci95_low: ci_low,
        ci95_high: ci_high,
    }
}
