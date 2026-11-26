use std::path::PathBuf;

use hoops_audit::*;
use hoops_config::ConfigBundle;
use hoops_core::*;

fn configs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../configs")
        .canonicalize()
        .unwrap()
}

#[test]
fn test_audit_linter_bisect_ab_and_realism() {
    let bundle = ConfigBundle::load_from_dir(&configs_dir()).unwrap();
    let spec_a = bundle
        .build_match_init_spec(
            "nba",
            "boston_five_out",
            "mile_high_hub",
            1234,
            StateHashMode::PerTickRolling,
        )
        .unwrap();
    let mut spec_b = spec_a.clone();
    spec_b.home_team.tactics.pace = 65;

    let report = lint_match(&spec_a, Some(1000)).unwrap();
    assert!(report.passed, "lint failed: {:?}", report.violations);
    assert_eq!(report.total_ticks, 1001);

    let ab = run_paired_ab(&spec_a, &spec_b, 2, 11, 600);
    assert_eq!(ab.exp_id, 11);
    assert_eq!(ab.base_seed, 1234);
    assert_eq!(ab.completed_pairs, 0);
    assert!(ab.pairs.iter().all(|pair| !pair.both_completed));

    let calibration = run_calibration_check(&spec_a, 11, 1, 600);
    assert!(!calibration.within_targets);
    assert!(calibration.failed_checks.iter().any(|reason| reason == "completed_games=0/1"));

    let realism = evaluate_realism_batch(&spec_a, 2, 77, 1500);
    assert_eq!(realism.games, 2);
    assert_eq!(realism.completed_games, 0);
    assert!(realism.avg_fga_per_team > 0.0);

    // Deterministic split bucketing
    assert_eq!(assign_split_bucket("0022300001", 11), assign_split_bucket("0022300001", 11));

    // Hash trace bisect exact binary search
    let mk_trace = |diverge_tick: Option<u32>| -> HashTraceV1 {
        let records = (0..100u32)
            .map(|t| {
                let mut sh = [0u8; 32];
                sh[0..4].copy_from_slice(&t.to_le_bytes());
                let mut rh = [0u8; 32];
                rh[0..4].copy_from_slice(&t.to_le_bytes());
                if diverge_tick.is_some_and(|dt| t >= dt) {
                    sh[10] ^= 0xFF;
                    rh[10] ^= 0xFF;
                }
                HashTraceRecordV1 {
                    tick: t,
                    state_hash: sh,
                    event_hash: [0u8; 32],
                    rolling_hash: rh,
                }
            })
            .collect();
        HashTraceV1 {
            format_version: HTR1_VERSION,
            start_tick: 0,
            tick_count: 100,
            init_spec_sha256: [1u8; 32],
            applied_commands_sha256: [2u8; 32],
            records,
        }
    };

    let t_base = mk_trace(None);
    let t_div = mk_trace(Some(73));
    let same = bisect_hash_traces(&t_base, &t_base);
    assert!(!same.diverged);
    let bis = bisect_hash_traces(&t_base, &t_div);
    assert!(bis.diverged);
    assert_eq!(bis.first_diverged_tick, Some(73));
}

#[test]
fn paired_complete_games_expose_effect_of_pace_below_75() {
    let bundle = ConfigBundle::load_from_dir(&configs_dir()).unwrap();
    let baseline = bundle.build_match_init_spec(
        "nba", "mile_high_hub", "twin_towers_grit", 42, StateHashMode::Disabled,
    ).unwrap();
    assert_eq!(baseline.home_team.tactics.pace, 56);
    let mut faster = baseline.clone();
    faster.home_team.tactics.pace = 70;
    let report = run_paired_ab(&baseline, &faster, 2, 11, MAX_SIM_TICKS as usize);
    assert_eq!(report.completed_pairs, 2);
    assert!(report.pairs.iter().any(|pair| pair.diff_a != pair.diff_b));
    assert!(report.ci95_low <= report.mean_delta_margin && report.mean_delta_margin <= report.ci95_high);
    let identity = run_paired_ab(&baseline, &baseline, 2, 11, MAX_SIM_TICKS as usize);
    for (change, control) in report.pairs.iter().zip(identity.pairs) {
        assert_eq!(control.seed, change.seed);
        assert_eq!(control.diff_a, change.diff_a);
        assert_eq!(control.diff_a, control.diff_b);
    }
}

#[test]
fn test_audit_linter_full_game_nba_and_fiba() {
    let bundle = ConfigBundle::load_from_dir(&configs_dir()).unwrap();
    for (ruleset, seed) in [("nba", 42_u64), ("fiba", 7_u64)] {
        let spec = bundle
            .build_match_init_spec(
                ruleset,
                "boston_five_out",
                "mile_high_hub",
                seed,
                StateHashMode::Disabled,
            )
            .unwrap();
        let report = lint_match(&spec, None).unwrap();
        assert!(
            report.passed,
            "{ruleset} seed={seed} full-game lint failed (ticks={}): {:?}",
            report.total_ticks,
            report.violations.iter().take(8).collect::<Vec<_>>()
        );
        assert!(
            report.score_home > 40 && report.score_away > 40,
            "{ruleset} scores too low: {}-{}",
            report.score_home,
            report.score_away
        );
    }
}
