mod common;

use common::make_init_spec_with_ruleset;
use hoops_core::physics::*;
use hoops_core::rules::*;
use hoops_core::*;

#[test]
fn test_m1_shot_clock_matrix_and_sub_ds_arith() {
    let nba = RuleSet::nba_default();
    let fiba = RuleSet::fiba_default();

    // NBA airball OFF rebound keeps clock; FIBA Art. 29/50 airball/block keeps clock too
    assert_eq!(
        compute_shot_clock_reset(ShotClockScenario::OffReboundNoRimAirballOrBlock, Some(63), 1200, &nba),
        Some(63)
    );
    assert_eq!(
        compute_shot_clock_reset(ShotClockScenario::OffReboundAfterRimOrFtMiss, Some(50), 1200, &fiba),
        Some(140)
    );
    assert_eq!(
        compute_shot_clock_reset(ShotClockScenario::TurnoverOpponentToOwnFrontcourt, Some(50), 1200, &fiba),
        Some(140)
    );
    assert_eq!(
        compute_shot_clock_reset(ShotClockScenario::TurnoverOpponentToOwnFrontcourt, Some(50), 1200, &nba),
        Some(240)
    );

    // 3 * 0.04s sub-ds crossing exact 0.1s boundary without rounding drift
    let spec = make_init_spec_with_ruleset(7, RuleSet::nba_default(), "h", "a", 12);
    let mut sink = NullSimSink;
    let mut eng = SimulationEngine::new(&spec, &mut sink).unwrap();
    eng.state.quarter = 4;
    set_clocks_exact_ds(&mut eng.state, 15, Some(15)); // 1.5s = 150_000 units
    eng.state.game_clock_running = true;
    eng.state.shot_clock_running = true;
    advance_clocks_sub_ds(&mut eng.state, 0.4); // -4_000 -> 146_000 (15 ds ceil)
    assert_eq!(eng.state.game_clock_ds, 15);
    assert!(!eng.state.game_clock_ticked_this_step);
    advance_clocks_sub_ds(&mut eng.state, 0.4); // -4_000 -> 142_000 (15 ds ceil)
    assert_eq!(eng.state.game_clock_ds, 15);
    advance_clocks_sub_ds(&mut eng.state, 0.4); // -4_000 -> 138_000 (14 ds ceil) -> crossed!
    assert_eq!(eng.state.game_clock_ds, 14);
    assert!(eng.state.game_clock_ticked_this_step);
    assert_eq!(eng.state.game_clock_rem_10us, 138_000);
}

#[test]
fn test_m1_mandatory_timeout_comments_iil_and_between_ft_rules() {
    let mut nba = RuleSet::nba_default();
    let mut sink = NullSimSink;
    // CanonicalAlphaFirst vs HomeFirst on mandatory timeout
    nba.timeouts.mandatory_first_charge_policy = MandatoryFirstChargePolicy::CanonicalAlphaFirst;
    let spec_alpha = make_init_spec_with_ruleset(11, nba.clone(), "zeta", "alpha", 12);
    let mut eng_a = SimulationEngine::new(&spec_alpha, &mut sink).unwrap();
    eng_a.state.quarter = 4;
    set_clocks_exact_ds(&mut eng_a.state, 4190, Some(240));
    let cid_alpha = eng_a.canonical_team_ids;
    let charged_a = check_auto_mandatory_timeout(&mut eng_a.state, &nba, cid_alpha, false);
    assert_eq!(charged_a, Some(CommandSide::Away)); // "alpha" < "zeta"

    nba.timeouts.mandatory_first_charge_policy = MandatoryFirstChargePolicy::HomeFirst;
    let spec_home = make_init_spec_with_ruleset(11, nba.clone(), "zeta", "alpha", 12);
    let mut eng_h = SimulationEngine::new(&spec_home, &mut sink).unwrap();
    eng_h.state.quarter = 4;
    set_clocks_exact_ds(&mut eng_h.state, 4190, Some(240));
    let cid_home = eng_h.canonical_team_ids;
    let charged_h = check_auto_mandatory_timeout(&mut eng_h.state, &nba, cid_home, false);
    assert_eq!(charged_h, Some(CommandSide::Home));

    // Comments II-L minimum 0.3s (30_000) deduction & checkpoint roundtrip
    let mut eng = SimulationEngine::new(&make_init_spec_with_ruleset(3, nba.clone(), "h", "a", 12), &mut sink).unwrap();
    eng.state.quarter = 4;
    eng.state.q1_game_clock_ever_started = true;
    eng.state.inbound_context = Some(InboundContext {
        side: CommandSide::Home,
        spot: InboundSpot::BackcourtEndline,
        sideline_side: CourtSideLine::TableSide,
        allow_endline_run: true,
        stop_clock_until_touch: false,
        inbound_ball_at_disposal: false,
        inbound_touched_court_player: false,
        inbound_5s_timer_ds: 50,
        original_spot: InboundSpot::BackcourtEndline,
        pending_shot_clock_ds: Some(240),
        is_period_start_arrow_inbound: false,
        allow_both_sides_advance: false,
        post_made_fg: false,
        post_made_last_ft: false,
    });
    set_clocks_exact_ds(&mut eng.state, 100, Some(100));
    eng.state.game_clock_running = true;
    // Instant deflection consuming 0.0s on clock triggers Comments II-L 0.3s minimum deduction
    eng.state.comments_iil_origin_clocks = Some(CommentsIILOrigin {
        kind: 0,
        start_gc_rem_10us: 1_000_000,
        start_sc_rem_10us: Some(1_000_000),
    });
    eng.trigger_comments_iil_if_needed();
    assert_eq!(eng.state.game_clock_rem_10us, 970_000);
    assert_eq!(eng.state.game_clock_ds, 97);
    assert!(eng.current_tick_events.as_slice().iter().any(|ev| matches!(
        ev.payload,
        GameRecordEventPayload::ClockAdjusted {
            delta_game_clock_ds_signed: -3,
            ..
        }
    )));

    // Checkpoint snapshot canonical roundtrip restores exact state & prev_rolling_hash
    let cp = eng.capture_checkpoint_snapshot().to_canonical_le_bytes();
    let cp_dec = EngineCheckpointSnapshotV1::from_canonical_le_bytes(&cp).unwrap();
    let eng_restored = SimulationEngine::from_checkpoint_snapshot(
        &make_init_spec_with_ruleset(3, nba, "h", "a", 12),
        cp_dec,
        &mut sink,
    )
    .unwrap();
    assert_eq!(eng_restored.state.game_clock_rem_10us, 970_000);

    // FIBA between-FT timeout (before last FT) & made-FG non-scorer inbound permissions
    let fiba = RuleSet::fiba_default();
    let mut eng_f = SimulationEngine::new(&make_init_spec_with_ruleset(5, fiba.clone(), "h", "a", 12), &mut sink).unwrap();
    eng_f.state.quarter = 4;
    eng_f.state.q1_game_clock_ever_started = true;
    eng_f.state.game_clock_ds = 600;
    let inb_fg = InboundContext {
        side: CommandSide::Away,
        spot: InboundSpot::BackcourtEndline,
        sideline_side: CourtSideLine::TableSide,
        allow_endline_run: true,
        stop_clock_until_touch: false,
        inbound_ball_at_disposal: false,
        inbound_touched_court_player: false,
        inbound_5s_timer_ds: 50,
        original_spot: InboundSpot::BackcourtEndline,
        pending_shot_clock_ds: Some(240),
        is_period_start_arrow_inbound: false,
        allow_both_sides_advance: false,
        post_made_fg: true,
        post_made_last_ft: false,
    };
    let perm = inbound_window_permissions(&eng_f.state, &inb_fg, &fiba);
    // FIBA non-scorer L2M can both timeout & sub after made FG; scorer cannot until unlocked
    assert!(perm.away_can_timeout && perm.away_can_sub);
    assert!(!perm.home_can_timeout && !perm.home_can_sub);
}

#[test]
fn mandatory_timeout_is_charged_once_and_resumes_the_ruling() {
    let spec = make_init_spec_with_ruleset(3, RuleSet::nba_default(), "h", "a", 12);
    let mut sink = VecSimSink::default();
    let mut engine = SimulationEngine::new(&spec, &mut sink).unwrap();
    engine.state.q1_game_clock_ever_started = true;
    engine.state.game_phase = GamePhase::LiveBall;
    engine.state.possession_side = Some(CommandSide::Home);
    engine.state.ball.last_team_control_side = Some(CommandSide::Home);
    engine.state.ball.ball_state = BallState::Held;
    engine.state.ball.owner_slot = Some(0);
    engine.state.players[0].x_m = engine.geom.half_length_m + 0.25;
    engine.state.ball.position_m = [engine.state.players[0].x_m, 0.0, 1.3];
    set_clocks_exact_ds(&mut engine.state, 4190, Some(200));
    engine.state.game_clock_running = true;
    engine.state.shot_clock_running = true;
    let initial_timeouts = engine.state.team_states[0].timeouts_remaining;
    let mut commands = Vec::new();
    assert!(matches!(step_with_batch_ai(&mut engine, &mut sink, &mut commands), StepStatus::TickCompleted { .. }));
    assert_eq!(engine.state.game_phase, GamePhase::Timeout);
    assert_eq!(engine.state.team_states[0].timeouts_remaining, initial_timeouts - 1);
    assert!(sink.events.iter().any(|event| matches!(event.payload,
        GameRecordEventPayload::TimeoutGranted { reason_tag: 1, charged_side: 0, .. }
    )));
    let stopped_clock = engine.state.game_clock_ds;
    assert!(matches!(engine.step(&mut sink), StepStatus::TickCompleted { .. }));
    assert_eq!(engine.state.game_phase, GamePhase::InboundSetup);
    assert_eq!(engine.state.inbound_context.unwrap().side, CommandSide::Away);
    assert_eq!(engine.state.team_states[0].timeouts_remaining, initial_timeouts - 1);
    assert_eq!(engine.state.game_clock_ds, stopped_clock);
    assert_eq!(engine.last_public_frame.ball_flags & 0x20, 0);
}

#[test]
fn backcourt_clock_runs_during_pass_flight() {
    let spec = make_init_spec_with_ruleset(3, RuleSet::fiba_default(), "h", "a", 12);
    let mut sink = VecSimSink::default();
    let mut engine = SimulationEngine::new(&spec, &mut sink).unwrap();
    engine.state.game_phase = GamePhase::LiveBall;
    engine.state.possession_side = Some(CommandSide::Home);
    engine.state.ball.last_team_control_side = Some(CommandSide::Home);
    engine.state.ball.ball_state = BallState::PassFlight;
    engine.state.ball.owner_slot = None;
    engine.state.backcourt_8s_timer_ds = 79;
    engine.state.game_clock_running = true;
    engine.state.shot_clock_running = true;
    set_clocks_exact_ds(&mut engine.state, 4000, Some(160));
    engine.state.ball.position_m = [-5.0, -3.0, 1.35];
    engine.state.ball.pass_ctx = Some(PassFlightContext {
        passer_slot: 0, target_slot: 1, pass_kind: 0, is_extra_pass: false,
        ticks_elapsed: 0, total_flight_ticks: 4, start_pos_m: [-5.0, -3.0, 1.35],
        target_pos_m: [-3.0, -2.8, 1.35], intercepted_by: None, will_bobble: false,
    });
    let mut commands = Vec::new();
    assert!(matches!(step_with_batch_ai(&mut engine, &mut sink, &mut commands), StepStatus::TickCompleted { .. }));
    assert_eq!(engine.state.inbound_context.unwrap().side, CommandSide::Away);
    assert!(sink.events.iter().any(|event| matches!(event.payload,
        GameRecordEventPayload::ViolationTriggered { rule_code, .. } if rule_code == RuleCode::VIO_8S as u16
    )));
}

#[test]
fn test_m1_goaltending_and_ft_interference_nba_vs_fiba() {
    let rim_xy = (12.725, 0.0);
    let rim_z = 3.048;

    // FIBA Art. 12.2.4: vz == 0.0 (not strictly downward) is legal block if above ring and has chance to score
    let fiba_vz0 = evaluate_flight_touch(
        1, // FIBA
        false,
        0,
        None,
        false,
        false,
        false,
        BackboardContact::None,
        [11.4, 0.0, 3.6],
        [4.0, 0.0, 0.0],
        rim_xy,
        rim_z,
        false,
        false,
        false,
    );
    assert_eq!(fiba_vz0, FlightTouchOutcome::Blocked);

    let fiba_vz_neg = evaluate_flight_touch(
        1,
        false,
        0,
        None,
        false,
        false,
        false,
        BackboardContact::None,
        [12.4, 0.0, 3.5],
        [1.0, 0.0, -0.5],
        rim_xy,
        rim_z,
        false,
        false,
        false,
    );
    assert_eq!(fiba_vz_neg, FlightTouchOutcome::Goaltending);

    // Paired FT flight defense touch: NBA Rule 9 §I(h) (+1 FT iff last & in-play) vs FIBA Art. 31.3.3 (+tech iff final/only)
    let nba_ft1_of_2 = evaluate_flight_touch(
        0,
        true,
        2,
        Some(PostFtOutcome::LiveReboundOnLastMiss),
        false,
        false,
        false,
        BackboardContact::None,
        [12.7, 0.0, 3.4],
        [0.5, 0.0, -1.0],
        rim_xy,
        rim_z,
        false,
        false,
        false,
    );
    assert_eq!(
        nba_ft1_of_2,
        FlightTouchOutcome::BasketInterferenceDefenseScore {
            add_extra_ft: false,
            add_tech_foul: false,
        }
    );

    let nba_ft_last_live = evaluate_flight_touch(
        0,
        true,
        1,
        Some(PostFtOutcome::LiveReboundOnLastMiss),
        false,
        false,
        false,
        BackboardContact::None,
        [12.7, 0.0, 3.4],
        [0.5, 0.0, -1.0],
        rim_xy,
        rim_z,
        false,
        false,
        false,
    );
    assert_eq!(
        nba_ft_last_live,
        FlightTouchOutcome::BasketInterferenceDefenseScore {
            add_extra_ft: true,
            add_tech_foul: false,
        }
    );

    let fiba_ft_last = evaluate_flight_touch(
        1,
        true,
        1,
        Some(PostFtOutcome::LiveReboundOnLastMiss),
        false,
        false,
        false,
        BackboardContact::None,
        [12.7, 0.0, 3.4],
        [0.5, 0.0, -1.0],
        rim_xy,
        rim_z,
        false,
        false,
        false,
    );
    assert_eq!(
        fiba_ft_last,
        FlightTouchOutcome::BasketInterferenceDefenseScore {
            add_extra_ft: false,
            add_tech_foul: true,
        }
    );
}

#[test]
fn test_m1_swept_ccd_9_scenarios_and_fallback() {
    let geom = CourtGeometry::from_rules(&RuleSet::nba_default().court);
    let r = 0.28;
    let bounds = geom.playable_bounds(r);

    // 1. High-speed opposite pairs (16.8 m/s relative): penetration <= 0.5 mm, zero fallback warnings
    let mut actors = Vec::new();
    for i in 0..5 {
        let y = (i as f64 - 2.0) * 1.5;
        actors.push(CcdActor {
            slot: i * 2,
            canonical_actor_id: (i * 2) as u8,
            x: -0.5,
            y,
            vx: 8.4,
            vy: 0.0,
            radius: r,
            inv_mass: 1.0 / 95.0,
            bounds,
        });
        actors.push(CcdActor {
            slot: i * 2 + 1,
            canonical_actor_id: (i * 2 + 1) as u8,
            x: 0.5,
            y,
            vx: -8.4,
            vy: 0.0,
            radius: r,
            inv_mass: 1.0 / 95.0,
            bounds,
        });
    }
    let mut audit = AuditGroundTruthRecord::default();
    let pairs = run_swept_ccd_step(&mut actors, 0.1, 1, CcdDebugHooks::default(), &mut audit);
    assert_eq!(pairs.len(), 5);
    assert!(audit.warnings.is_empty());
    for i in 0..actors.len() {
        assert_eq!(bounds.violation(actors[i].x, actors[i].y), 0.0);
        for j in (i + 1)..actors.len() {
            let d = libm::hypot(actors[j].x - actors[i].x, actors[j].y - actors[i].y);
            assert!(d + 0.0005 >= actors[i].radius + actors[j].radius, "penetration > 0.5mm: {d}");
        }
    }

    // 2. 10-player wall chain + 4-player corner lock < 0.001 mm with zero fallback
    let mut corner_actors = vec![
        CcdActor { slot: 0, canonical_actor_id: 0x00, x: bounds.max_x + 0.2, y: bounds.max_y + 0.2, vx: 2.0, vy: 2.0, radius: r, inv_mass: 0.01, bounds },
        CcdActor { slot: 1, canonical_actor_id: 0x01, x: bounds.max_x, y: bounds.max_y, vx: 1.0, vy: 1.0, radius: r, inv_mass: 0.01, bounds },
        CcdActor { slot: 2, canonical_actor_id: 0x10, x: bounds.max_x - 0.1, y: bounds.max_y - 0.1, vx: 1.0, vy: 1.0, radius: r, inv_mass: 0.01, bounds },
        CcdActor { slot: 3, canonical_actor_id: 0x11, x: bounds.max_x - 0.2, y: bounds.max_y - 0.2, vx: 1.0, vy: 1.0, radius: r, inv_mass: 0.01, bounds },
    ];
    let mut audit2 = AuditGroundTruthRecord::default();
    let _ = run_swept_ccd_step(&mut corner_actors, 0.1, 2, CcdDebugHooks::default(), &mut audit2);
    assert!(audit2.warnings.is_empty());
    for i in 0..corner_actors.len() {
        assert!(bounds.violation(corner_actors[i].x, corner_actors[i].y) < 1.0e-5);
        for j in (i + 1)..corner_actors.len() {
            let d = libm::hypot(
                corner_actors[j].x - corner_actors[i].x,
                corner_actors[j].y - corner_actors[i].y,
            );
            assert!(d + 1.0e-5 >= corner_actors[i].radius + corner_actors[j].radius);
        }
    }

    // 3. Forced Dykstra early exit triggers deterministic 4x3 grid fallback and emits AuditWarning::DepenetrationFallbackUsed
    let mut stuck = vec![
        CcdActor { slot: 0, canonical_actor_id: 0x00, x: bounds.max_x, y: bounds.max_y, vx: 0.0, vy: 0.0, radius: r, inv_mass: 0.01, bounds },
        CcdActor { slot: 1, canonical_actor_id: 0x01, x: bounds.max_x, y: bounds.max_y, vx: 0.0, vy: 0.0, radius: r, inv_mass: 0.01, bounds },
    ];
    let mut audit3 = AuditGroundTruthRecord::default();
    let _ = run_swept_ccd_step(
        &mut stuck,
        0.1,
        3,
        CcdDebugHooks { force_dykstra_early_exit: true },
        &mut audit3,
    );
    assert!(audit3.warnings.iter().any(|w| matches!(w, AuditWarning::DepenetrationFallbackUsed { tick: 3, .. })));
    let d_stuck = libm::hypot(stuck[1].x - stuck[0].x, stuck[1].y - stuck[0].y);
    assert!(d_stuck + 1.0e-4 >= stuck[0].radius + stuck[1].radius);
}

#[test]
fn test_oob_and_backcourt_and_5s_inbound_rules() {
    let nba = RuleSet::nba_default();
    let geom = CourtGeometry::from_rules(&nba.court);

    // 1. Out-of-bounds live handler -> VIO_OOB and InboundSetup outside court lines
    let spec_oob = make_init_spec_with_ruleset(5, nba.clone(), "h", "a", 12);
    let mut sink_oob = VecSimSink::default();
    let mut eng_oob = SimulationEngine::new(&spec_oob, &mut sink_oob).unwrap();
    eng_oob.state.game_phase = GamePhase::LiveBall;
    eng_oob.state.possession_side = Some(CommandSide::Home);
    eng_oob.state.game_clock_running = true;
    eng_oob.state.shot_clock_running = true;
    eng_oob.state.shot_clock_ds = Some(200);
    eng_oob.state.ball.ball_state = BallState::Held;
    eng_oob.state.ball.owner_slot = Some(0);
    let hr = eng_oob.state.team_states[0].active_lineup_slots[0];
    eng_oob.state.players[hr as usize].x_m = geom.half_length_m + 0.25;
    eng_oob.state.players[hr as usize].y_m = 2.0;
    eng_oob.state.ball.position_m = [geom.half_length_m + 0.25, 2.0, 1.1];
    let _ = eng_oob.step(&mut sink_oob);
    assert!(sink_oob.events.iter().any(|e| matches!(
        e.payload,
        GameRecordEventPayload::ViolationTriggered { rule_code, .. } if rule_code == RuleCode::VIO_OOB as u16
    )));
    // Step through challenge window commit via batch AI to verify InboundSetup spots inbounder outside court lines
    let mut cmds_oob = Vec::new();
    for _ in 0..8 {
        let _ = step_with_batch_ai(&mut eng_oob, &mut sink_oob, &mut cmds_oob);
        if eng_oob.state.game_phase == GamePhase::InboundSetup {
            break;
        }
    }
    assert_eq!(eng_oob.state.game_phase, GamePhase::InboundSetup);
    let _ = step_with_batch_ai(&mut eng_oob, &mut sink_oob, &mut cmds_oob);
    let inb_slot = eng_oob.state.ball.owner_slot.expect("inbounder should hold ball") as usize;
    let away_inb_r = eng_oob.state.team_states[inb_slot / 5].active_lineup_slots[inb_slot % 5];
    let p_inb = &eng_oob.state.players[away_inb_r as usize];
    assert!(
        p_inb.x_m.abs() > geom.half_length_m || p_inb.y_m.abs() > geom.half_width_m,
        "inbounder positioned inside court: ({}, {})",
        p_inb.x_m,
        p_inb.y_m
    );

    // 2. Backcourt violation after frontcourt established
    let spec_bc = make_init_spec_with_ruleset(6, nba.clone(), "h", "a", 12);
    let mut sink_bc = VecSimSink::default();
    let mut eng_bc = SimulationEngine::new(&spec_bc, &mut sink_bc).unwrap();
    eng_bc.state.game_phase = GamePhase::LiveBall;
    eng_bc.state.possession_side = Some(CommandSide::Home);
    eng_bc.state.ball_in_frontcourt = true;
    eng_bc.state.game_clock_running = true;
    eng_bc.state.shot_clock_running = true;
    eng_bc.state.shot_clock_ds = Some(200);
    eng_bc.state.ball.ball_state = BallState::Held;
    eng_bc.state.ball.owner_slot = Some(0);
    let hr_bc = eng_bc.state.team_states[0].active_lineup_slots[0];
    eng_bc.state.players[hr_bc as usize].x_m = -2.0;
    eng_bc.state.players[hr_bc as usize].y_m = 0.0;
    eng_bc.state.ball.position_m = [-2.0, 0.0, 1.1];
    let _ = eng_bc.step(&mut sink_bc);
    assert!(sink_bc.events.iter().any(|e| matches!(
        e.payload,
        GameRecordEventPayload::ViolationTriggered { rule_code, .. } if rule_code == RuleCode::VIO_BACKCOURT as u16
    )));

    // 3. 5-second inbound violation
    let spec_5s = make_init_spec_with_ruleset(9, nba, "h", "a", 12);
    let mut sink_5s = VecSimSink::default();
    let mut eng_5s = SimulationEngine::new(&spec_5s, &mut sink_5s).unwrap();
    eng_5s.state.game_phase = GamePhase::InboundSetup;
    eng_5s.state.game_clock_ds = 6000;
    eng_5s.state.game_clock_rem_10us = 600_000_00;
    eng_5s.state.inbound_context = Some(InboundContext {
        side: CommandSide::Home,
        spot: InboundSpot::FrontcourtSideline28ft,
        sideline_side: CourtSideLine::TableSide,
        allow_endline_run: false,
        stop_clock_until_touch: true,
        inbound_ball_at_disposal: true,
        inbound_touched_court_player: false,
        inbound_5s_timer_ds: 50,
        original_spot: InboundSpot::FrontcourtSideline28ft,
        pending_shot_clock_ds: Some(140),
        is_period_start_arrow_inbound: false,
        allow_both_sides_advance: false,
        post_made_fg: false,
        post_made_last_ft: false,
    });
    let _ = eng_5s.step(&mut sink_5s);
    assert!(sink_5s.events.iter().any(|e| matches!(
        e.payload,
        GameRecordEventPayload::ViolationTriggered { rule_code, .. } if rule_code == RuleCode::VIO_5S_INBOUND as u16
    )));
}
