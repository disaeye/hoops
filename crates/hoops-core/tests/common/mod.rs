#![allow(dead_code)]

use hoops_core::*;

pub fn make_player(id: &str, rank: usize, is_big: bool, three_pt: u8, ft: u8) -> CompiledPlayerSnapshot {
    let mut attrs = [72u8; 38];
    attrs[0] = 76; // layup
    attrs[1] = if is_big { 82 } else { 65 }; // dunk
    attrs[2] = 75; // close_touch
    attrs[3] = if is_big { 78 } else { 55 }; // post_control
    attrs[4] = 72; // draw_foul
    attrs[5] = 78; // hands_catch
    attrs[6] = 74; // mid_range
    attrs[7] = three_pt; // three_point
    attrs[8] = ft; // free_throw
    attrs[9] = three_pt; // catch_and_shoot
    attrs[10] = if is_big { 60 } else { 78 }; // off_dribble_shot
    attrs[11] = 72; // contested_shot
    attrs[12] = if three_pt >= 75 { 80 } else { 62 }; // shot_range
    attrs[13] = 76; // shot_selection_iq
    attrs[14] = if is_big { 60 } else { 85 }; // ball_handle
    attrs[15] = if is_big { 66 } else { 82 }; // pass_accuracy
    attrs[16] = if is_big { 64 } else { 80 }; // pass_vision
    attrs[17] = 75; // off_ball_move_iq
    attrs[18] = if is_big { 85 } else { 60 }; // screen_setting
    attrs[19] = if is_big { 62 } else { 80 }; // perim_d
    attrs[20] = if is_big { 86 } else { 60 }; // interior_d
    attrs[21] = if is_big { 84 } else { 58 }; // block
    attrs[22] = if is_big { 60 } else { 78 }; // steal
    attrs[23] = 75; // help_d_iq
    attrs[24] = 74; // def_closeout
    attrs[25] = 74; // screen_nav
    attrs[26] = if is_big { 85 } else { 60 }; // off_rebound
    attrs[27] = if is_big { 88 } else { 64 }; // def_rebound
    attrs[28] = if is_big { 86 } else { 65 }; // box_out
    attrs[29] = if is_big { 68 } else { 85 }; // speed
    attrs[30] = if is_big { 68 } else { 84 }; // accel
    attrs[31] = if is_big { 66 } else { 84 }; // agility
    attrs[32] = if is_big { 86 } else { 68 }; // strength
    attrs[33] = 76; // vertical
    attrs[34] = 84; // stamina
    attrs[35] = 85; // durability
    attrs[36] = 74; // hustle
    attrs[37] = 78; // composure

    let height_cm = if is_big { 211.0 } else { 193.0 };
    let wingspan_cm = if is_big { 223.0 } else { 203.0 };
    let weight_kg = if is_big { 114.0 } else { 90.0 };
    let pos = ((rank % 5) + 1) as u8;

    let mut scores = [0.0f64; 9];
    scores[0] = (attrs[14] as f64) * 0.4 + (attrs[15] as f64) * 0.35 + (attrs[16] as f64) * 0.25;
    scores[1] = (attrs[7] as f64) * 0.45 + (attrs[9] as f64) * 0.35 + (attrs[17] as f64) * 0.2;
    scores[2] = (attrs[18] as f64) * 0.45 + (attrs[1] as f64) * 0.3 + (attrs[26] as f64) * 0.25;
    scores[3] = (attrs[20] as f64) * 0.45 + (attrs[21] as f64) * 0.35 + (attrs[27] as f64) * 0.2;
    scores[4] = (attrs[19] as f64) * 0.5 + (attrs[22] as f64) * 0.25 + (attrs[29] as f64) * 0.25;
    scores[5] = scores[1];
    scores[6] = height_cm - 120.0;
    scores[7] = (attrs[23] as f64) * 0.5 + (attrs[24] as f64) * 0.5;
    scores[8] = (attrs[27] as f64) * 0.5 + (attrs[28] as f64) * 0.5;

    CompiledPlayerSnapshot {
        player_id: id.to_owned(),
        display_name: id.to_owned(),
        jersey_number_code: ((rank + 1) % 100) as u8,
        physical: PhysicalProfile {
            height_cm,
            wingspan_cm,
            standing_reach_cm: height_cm * 1.32,
            weight_kg,
            age: 26,
            dominant_hand: 0,
            display_position: pos,
        },
        attributes: PlayerAttributes38(attrs),
        tendencies: PlayerTendencies {
            zone_weights: [0.28, 0.14, 0.14, 0.14, 0.30],
            drive_vs_pullup: 0.5,
            pass_vs_shoot: 0.5,
            pnr_roll_vs_pop: if is_big { 0.8 } else { 0.3 },
            crash_orb: if is_big { 0.65 } else { 0.15 },
            gamble_steal: 0.25,
        },
        functional: PlayerFunctionalProfile {
            scores,
            guard_tiers: [
                height_cm <= 202.0 && scores[4] >= 60.0,
                height_cm <= 208.0,
                true,
                is_big || height_cm >= 200.0,
                is_big,
            ],
        },
        badge_indices: if rank < 2 { vec![0] } else { vec![] },
    }
}

pub fn make_team(team_id: &str, roster_size: usize) -> CompiledTeamSnapshot {
    let mut players = Vec::with_capacity(roster_size);
    for r in 0..roster_size {
        let is_big = r % 5 >= 3;
        let three_pt = if is_big { 70 } else { 84 };
        let ft = if is_big { 74 } else { 86 };
        players.push(make_player(&format!("{team_id}_p{r:02}"), r, is_big, three_pt, ft));
    }
    CompiledTeamSnapshot {
        team_id: team_id.to_owned(),
        tactics: TeamTacticsConfig {
            pace: 62,
            transition_push: 60,
            default_spacing: 70,
            primary_handlers: InlinePlayerList::from_slice(&[0, 1]).unwrap(),
            play_weights: vec![("spread_pnr".to_owned(), 1.0)],
            orb_crash_count: 1,
            defensive_gameplan_id: GameplanId::new("drop_base").unwrap(),
            sag_off_3pt_threshold: 65,
            foul_up_three_policy: FoulUpThreePolicy::Under6sIfBonus,
            matchup_overrides: vec![],
            full_court_press_clutch: false,
            rotation_script: CompiledRotationScript::default(),
        },
        players,
    }
}

pub fn make_playbook(play_id: &str) -> CompiledPlaybook {
    CompiledPlaybook {
        play_id: PlayId::new(play_id).unwrap(),
        min_shot_clock_ds: 80,
        allow_mirror: true,
        role_priority: [0, 1, 2, 3, 4],
        slot_weights: [
            vec![(14, 0.5), (15, 0.5)],
            vec![(7, 0.6), (9, 0.4)],
            vec![(7, 0.5), (17, 0.5)],
            vec![(61, 0.6), (7, 0.4)],
            vec![(18, 0.6), (26, 0.4)],
        ],
        initial_positions: [
            (1, 0, -2.0, 0.0),
            (2, 1, 0.0, 0.0),
            (3, 2, 0.0, 0.0),
            (4, 1, 0.0, 0.0),
            (5, 0, 0.0, 0.0),
        ],
        phases: vec![CompiledPlayPhase {
            base_epv: 1.06,
            max_duration_ds: 60,
            exit_trigger_kind: 0,
            exit_target_role: 0,
            exit_fallback_ds: 50,
            actions: [
                (0, 1, 0, 0, 0.0, 0.0, 1.0),
                (1, 3, 1, 1, 0.0, 0.0, 0.8),
                (1, 3, 2, 2, 0.0, 0.0, 0.8),
                (2, 2, 1, 3, 0.0, 0.0, 0.8),
                (3, 5, 0, 4, 0.0, 0.0, 0.9),
            ],
        }],
    }
}

pub fn make_defense(id: &str) -> CompiledDefenseGameplan {
    CompiledDefenseGameplan {
        gameplan_id: GameplanId::new(id).unwrap(),
        base_scheme: 0,
        switch_max_height_diff_cm: 12.0,
        help_shrink_level: 50,
        role_defs: vec![],
        protect_roles: vec![],
        hide_on_weakest_shooter: false,
        pre_switch_enabled: false,
        pre_switch_trigger_radius_m: 1.8,
        scram_switch_enabled: false,
        peel_switch_enabled: false,
        pnr_rules: vec![CompiledPnrRule {
            rule_id: PnrRuleId::new("drop_big").unwrap(),
            predicates: vec![PredicateExpr::Cmp {
                subject: 1,
                metric_key: 7,
                op: 3,
                val: 75.0,
            }],
            coverage: PnrCoverageKind::Drop,
            on_ball_action: 0,
            help_rule: 0,
        }],
        adaptation_triggers: vec![CompiledAdaptationTrigger {
            watch_coverage: PnrCoverageKind::Drop,
            min_possessions: 6,
            prior_weight: 10.0,
            prior_ppp: 1.0,
            escalate_ppp_threshold: 1.15,
            deescalate_ppp_threshold: 0.95,
            revert_if_escalated_ppp_exceeds: 1.25,
            revert_min_possessions: 4,
            deescalate_min_possessions: 5,
            threat_absent_three_point_lt: 65,
            cooldown_possessions: 2,
            escalate_to_coverage: PnrCoverageKind::Blitz,
        }],
    }
}

pub fn make_badges() -> Vec<BadgeDef> {
    vec![BadgeDef {
        badge_id: "b01_catch_shoot".to_owned(),
        hook: 0,
        op: 0,
        value: 0.12,
    }]
}

pub fn make_init_spec_with_ruleset(
    seed: u64,
    ruleset: RuleSet,
    home_id: &str,
    away_id: &str,
    roster_size: usize,
) -> MatchInitSpecV1 {
    let tunables = EngineTunables::default();
    let playbooks = vec![make_playbook("spread_pnr")];
    let defenses = vec![make_defense("drop_base")];
    let badges = make_badges();
    let home_team = make_team(home_id, roster_size);
    let away_team = make_team(away_id, roster_size);
    let mut roster_bytes = home_team.to_canonical_le_bytes();
    roster_bytes.extend_from_slice(&away_team.to_canonical_le_bytes());

    let config_hashes = ConfigHashes {
        ruleset_sha256: sha256_bytes(&ruleset.to_canonical_le_bytes()),
        tunables_sha256: sha256_bytes(&tunables.to_canonical_le_bytes()),
        playbooks_sha256: sha256_bytes(&playbooks.to_canonical_le_bytes()),
        defenses_sha256: sha256_bytes(&defenses.to_canonical_le_bytes()),
        badges_sha256: sha256_bytes(&badges.to_canonical_le_bytes()),
        rosters_sha256: sha256_bytes(&roster_bytes),
    };

    MatchInitSpecV1 {
        schema_version: 1,
        engine_semver: "0.1.0".to_owned(),
        engine_git_sha: "dev".to_owned(),
        rustc_version_sha256: [0x11; 32],
        engine_wasm_sha256: [0x22; 32],
        cargo_lock_sha256: [0x33; 32],
        seed,
        canonical_team_id_override: None,
        intentional_foul_policy: IntentionalFoulPolicy::Auto,
        config_hashes,
        ruleset,
        tunables,
        playbooks,
        defenses,
        badges,
        home_team,
        away_team,
        state_hash_mode: StateHashMode::PerTickRolling,
    }
}

pub fn make_init_spec(seed: u64) -> MatchInitSpecV1 {
    make_init_spec_with_ruleset(seed, RuleSet::nba_default(), "alpha_home", "beta_away", 12)
}
