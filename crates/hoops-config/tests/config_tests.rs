use std::path::PathBuf;

use hoops_config::*;
use hoops_core::*;

fn configs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../configs")
        .canonicalize()
        .unwrap()
}

#[test]
fn test_load_all_workspace_configs_and_deterministic_hashes() {
    let bundle = ConfigBundle::load_from_dir(&configs_dir()).unwrap();
    assert_eq!(bundle.nba_ruleset.rule_id, "nba_2023_24");
    assert_eq!(bundle.fiba_ruleset.rule_id, "fiba_2024");
    assert_eq!(bundle.playbooks.len(), 8);
    assert_eq!(bundle.defenses.len(), 4);
    assert_eq!(bundle.teams.len(), 4);
    assert_eq!(bundle.badges.len(), 8);

    // Verify SlotMetricKey ascending order in all compiled playbooks
    for pb in &bundle.playbooks {
        for sw in &pb.slot_weights {
            assert!(sw.windows(2).all(|w| w[0].0 < w[1].0));
        }
    }

    let spec1 = bundle
        .build_match_init_spec(
            "nba",
            "boston_five_out",
            "mile_high_hub",
            42,
            StateHashMode::Disabled,
        )
        .unwrap();
    let spec2 = bundle
        .build_match_init_spec(
            "nba",
            "boston_five_out",
            "mile_high_hub",
            42,
            StateHashMode::Disabled,
        )
        .unwrap();
    assert_eq!(spec1.config_hashes, spec2.config_hashes);
    assert!(spec1.to_canonical_le_bytes().len() <= MAX_INIT_SPEC_BYTES);

    // Run a short 50-tick simulation from compiled MatchInitSpecV1
    let mut sink = VecSimSink::default();
    let mut eng = SimulationEngine::new(&spec1, &mut sink).unwrap();
    let mut cmds = Vec::new();
    for _ in 0..50 {
        let _ = step_with_batch_ai(&mut eng, &mut sink, &mut cmds);
    }
    assert!(sink.public_frames.len() >= 50);
}

#[test]
fn attribute_keys_follow_wire_order_and_bind_real_playbook_roles() {
    use hoops_core::tactics::{assign_playbook_roles, eval_role_weight_metric, eval_slot_metric};

    let bundle = ConfigBundle::load_from_dir(&configs_dir()).unwrap();
    let spec = bundle.build_match_init_spec(
        "nba", "mile_high_hub", "twin_towers_grit", 42, StateHashMode::Disabled,
    ).unwrap();
    let hub = &spec.home_team.players[4];
    let mut probe = hub.clone();
    for (i, attr) in AttrIdx::ALL.into_iter().enumerate() {
        probe.attributes[attr] = i as u8;
        assert_eq!(attr as usize, i);
        assert_eq!(metric_key_from_name(attr.name()), Some(7 + i as u16));
        assert_eq!(AttrIdx::from_metric_key(attr.metric_key()), Some(attr));
        assert_eq!(eval_slot_metric(&probe, attr.metric_key()), i as f64);
    }
    assert_eq!(probe.attributes.to_canonical_le_bytes(), (0_u8..38).collect::<Vec<_>>());
    for key in 0..=6 {
        assert!((0.0..=100.0).contains(&eval_role_weight_metric(hub, key)));
    }
    assert_eq!(eval_slot_metric(hub, 1), hub.physical.wingspan_cm);

    let play = bundle.playbooks.iter().find(|p| p.play_id.as_str() == "high_post_hub").unwrap();
    let players = spec.home_team.players[..5].iter().enumerate()
        .map(|(slot, player)| (slot as u8, player)).collect::<Vec<_>>();
    assert_eq!(assign_playbook_roles(play, &players)[4], 4);
    assert!(hub.display_name.contains("Jokic"));
}

#[derive(Default)]
struct FullGameChecks {
    frame_minutes_ds: [u32; 2],
    clock_adjustments_ds: [i32; 2],
    final_minutes_ds: Option<[u32; 2]>,
    disqualified: [bool; 30],
    forced_substitutions: u32,
    defensive_three_seconds: [u32; 2],
}

impl SimSink for FullGameChecks {
    fn on_debug_frame(&mut self, _: &DebugOverlayFrameV1) {}
    fn on_audit(&mut self, _: &AuditGroundTruthRecord) {}
    fn on_anchored_command(&mut self, _: &AnchoredCommand) {}
    fn on_tick_hashes(&mut self, _: u32, _: [u8; 32], _: [u8; 32], _: [u8; 32]) {}

    fn on_public_frame(&mut self, frame: &PublicFrameV1) {
        if frame.ball_flags & 0x20 != 0 {
            for player in &frame.players {
                let r = usize::from(player.roster_idx);
                if r < 30 {
                    self.frame_minutes_ds[r / 15] += 1;
                }
            }
        }
    }

    fn on_game_event(&mut self, event: &GameRecordEventV1) {
        match &event.payload {
            GameRecordEventPayload::ClockAdjusted {
                delta_game_clock_ds_signed, affected_slot_mask, affected_roster_indices, ..
            } => {
                for (slot, &r) in affected_roster_indices.iter().enumerate() {
                    if affected_slot_mask & (1 << slot) != 0 {
                        self.clock_adjustments_ds[usize::from(r) / 15] -= i32::from(*delta_game_clock_ds_signed);
                    }
                }
            }
            GameRecordEventPayload::FoulAssessedRecord {
                offender_roster_idx, ejected_or_fouled_out_flags, ..
            } if ejected_or_fouled_out_flags & 1 != 0 => {
                let r = usize::from(*offender_roster_idx);
                assert!(!self.disqualified[r], "disqualified player {r} fouled again at tick {}", event.tick);
                self.disqualified[r] = true;
            }
            GameRecordEventPayload::SubstitutionApplied { out_roster_idx, sub_priority_tier: 0, .. }
                if self.disqualified[usize::from(*out_roster_idx)] =>
            {
                self.forced_substitutions += 1;
            }
            GameRecordEventPayload::ViolationTriggered { rule_code, team_side, .. }
                if *rule_code == RuleCode::VIO_DEF_3S as u16 =>
            {
                self.defensive_three_seconds[usize::from(*team_side)] += 1;
            }
            GameRecordEventPayload::GameEnded { final_court_time_ds, .. } => {
                self.final_minutes_ds = Some([
                    final_court_time_ds.0[..15].iter().sum(),
                    final_court_time_ds.0[15..].iter().sum(),
                ]);
            }
            _ => {}
        }
    }
}

#[test]
fn full_games_count_minutes_once_and_replace_disqualified_players() {
    let bundle = ConfigBundle::load_from_dir(&configs_dir()).unwrap();
    let mut forced_substitutions = 0;
    for rule in ["nba", "fiba"] {
        let spec = bundle.build_match_init_spec(
            rule, "boston_five_out", "twin_towers_grit", 42, StateHashMode::Disabled,
        ).unwrap();
        let mut sink = FullGameChecks::default();
        let mut engine = SimulationEngine::new(&spec, &mut sink).unwrap();
        let mut commands = Vec::new();
        let end = loop {
            commands.clear();
            let step = step_with_batch_ai(&mut engine, &mut sink, &mut commands);
            if !matches!(step, StepStatus::TickCompleted { .. }) {
                break step;
            }
        };
        assert!(matches!(end, StepStatus::Finished { .. }), "{rule}: {end:?}");
        assert_eq!(engine.state.end_reason, Some(GameEndReason::Normal), "{rule}");
        let period_ds = u32::from(spec.ruleset.clocks.quarter_sec) * 40;
        let ot_ds = u32::from(engine.state.quarter.saturating_sub(4)) * u32::from(spec.ruleset.clocks.ot_sec) * 10;
        let expected = (period_ds + ot_ds) * 5;
        assert_eq!(sink.final_minutes_ds, Some([expected; 2]), "{rule}");
        for team in 0..2 {
            assert_eq!(sink.frame_minutes_ds[team].saturating_add_signed(sink.clock_adjustments_ds[team]), expected, "{rule} team {team}");
        }
        forced_substitutions += sink.forced_substitutions;
    }
    assert!(forced_substitutions > 0);
}

#[test]
fn nba_zone_defense_clears_the_lane() {
    let bundle = ConfigBundle::load_from_dir(&configs_dir()).unwrap();
    let mut spec = bundle.build_match_init_spec(
        "nba", "boston_five_out", "twin_towers_grit", 42, StateHashMode::Disabled,
    ).unwrap();
    spec.away_team.tactics.defensive_gameplan_id = GameplanId::new("zone_2_3").unwrap();
    let mut sink = FullGameChecks::default();
    let mut engine = SimulationEngine::new(&spec, &mut sink).unwrap();
    let mut commands = Vec::new();
    loop {
        commands.clear();
        match step_with_batch_ai(&mut engine, &mut sink, &mut commands) {
            StepStatus::TickCompleted { .. } => {}
            StepStatus::Finished { .. } => break,
            status => panic!("unexpected game status: {status:?}"),
        }
    }
    assert_eq!(engine.state.end_reason, Some(GameEndReason::Normal));
    println!("defensive_three_seconds={:?}", sink.defensive_three_seconds);
    assert!(sink.defensive_three_seconds[1] <= 10, "{} away defensive three-second violations", sink.defensive_three_seconds[1]);
}

#[test]
fn test_config_rejects_invalid_runoff_and_deep_predicate() {
    let mut bad_rules = RuleSet::nba_default();
    bad_rules.court.oob_runoff_m = 1.5; // < 2.0 min!
    assert!(validate_ruleset(&bad_rules).is_err());
    bad_rules.court.oob_runoff_m = 6.5; // > 6.0 max!
    assert!(validate_ruleset(&bad_rules).is_err());

    let deep_def_toml = r#"
gameplan_id = "too_deep"
base_scheme = 0
switch_max_height_diff_cm = 12.0
help_shrink_level = 50
hide_on_weakest_shooter = false
pre_switch_enabled = false
pre_switch_trigger_radius_m = 1.5
scram_switch_enabled = false
peel_switch_enabled = false

[[pnr_rules]]
rule_id = "depth_5_rule"
coverage = "Drop"
on_ball_action = 0
help_rule = 0

[[pnr_rules.predicates]]
kind = "Not"
[pnr_rules.predicates.inner]
kind = "Not"
[pnr_rules.predicates.inner.inner]
kind = "Not"
[pnr_rules.predicates.inner.inner.inner]
kind = "Not"
[pnr_rules.predicates.inner.inner.inner.inner]
kind = "Cmp"
subject = 1
metric_key = 7
op = 0
val = 70.0
"#;
    let raw: hoops_config::loader::RawDefenseFile = toml::from_str(deep_def_toml).unwrap();
    assert!(compile_defense(raw).is_err());
}
