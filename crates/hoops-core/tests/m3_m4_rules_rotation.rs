mod common;

use common::make_init_spec_with_ruleset;
use hoops_core::rotation::*;
use hoops_core::rules::*;
use hoops_core::*;

#[test]
fn test_m3_nba_rule14_4_row_timeout_table_and_vi_made_basket() {
    let rules = RuleSet::nba_default();
    let mut base_state = SimulationEngine::new(
        &make_init_spec_with_ruleset(1, rules.clone(), "h", "a", 12),
        &mut NullSimSink,
    )
    .unwrap()
    .state;
    base_state.team_states[0].timeouts_remaining = 5;
    base_state.team_states[1].timeouts_remaining = 5;
    base_state.team_states[0].challenges_remaining = 1;
    base_state.team_states[1].challenges_remaining = 1;

    // Row 1: Team-called by Challenger, Win -> refunded timeout, kept bonus challenge
    let mut s1 = base_state.clone();
    s1.team_states[0].timeouts_remaining = 4; // deducted upon calling TO
    let charged = apply_nba_challenge_timeout_and_bonus(
        &mut s1,
        CommandSide::Home,
        ChallengeWindowKind::TimeoutPending {
            reason: TimeoutReason::TeamCalled { side: CommandSide::Home },
        },
        true,
        false,
        &rules,
    );
    assert_eq!(charged, None);
    assert_eq!(s1.team_states[0].timeouts_remaining, 5);
    assert_eq!(s1.team_states[0].challenges_remaining, 1);
    assert!(s1.team_states[0].first_challenge_used_and_won);

    // Row 4 (Rule 14 §V-e): Opponent-called TO, Challenger Wins -> NO double charge!
    let mut s4_win = base_state.clone();
    s4_win.team_states[1].timeouts_remaining = 4; // Away called TO
    let charged4w = apply_nba_challenge_timeout_and_bonus(
        &mut s4_win,
        CommandSide::Home,
        ChallengeWindowKind::TimeoutPending {
            reason: TimeoutReason::TeamCalled { side: CommandSide::Away },
        },
        true,
        false,
        &rules,
    );
    assert_eq!(charged4w, Some(CommandSide::Away));
    assert_eq!(s4_win.team_states[0].timeouts_remaining, 5);
    assert_eq!(s4_win.team_states[1].timeouts_remaining, 4);

    // Row 4 Loses -> Away refunded, Home charged
    let mut s4_lose = base_state.clone();
    s4_lose.team_states[1].timeouts_remaining = 4;
    let charged4l = apply_nba_challenge_timeout_and_bonus(
        &mut s4_lose,
        CommandSide::Home,
        ChallengeWindowKind::TimeoutPending {
            reason: TimeoutReason::TeamCalled { side: CommandSide::Away },
        },
        false,
        false,
        &rules,
    );
    assert_eq!(charged4l, Some(CommandSide::Home));
    assert_eq!(s4_lose.team_states[0].timeouts_remaining, 4);
    assert_eq!(s4_lose.team_states[1].timeouts_remaining, 5);

    // Rule 14 §VI: Made basket retained strictly on 2 cases (Def foul after motion start OR OOB simultaneous after motion start)
    let none_call = RuleCallSnapshot {
        rule_code: RuleCode::NONE,
        offending_side: CommandSide::System,
        offender_roster_idx: ROSTER_IDX_NONE,
        victim_roster_idx: None,
        points_if_made: 2,
        basket_made_on_play: true,
        restart_shot_clock_ds: Some(240),
        ..RuleCallSnapshot::default()
    };
    let mut foul_call = none_call;
    foul_call.rule_code = RuleCode::FOUL_REACH_COMMON;
    foul_call.offending_side = CommandSide::Away;
    foul_call.foul_occurred_after_shooting_motion_started = true;
    assert!(nba_rule_14_vi_retains_made_basket(&foul_call, &none_call));

    let mut charge_call = foul_call;
    charge_call.rule_code = RuleCode::FOUL_CHARGE;
    assert!(!nba_rule_14_vi_retains_made_basket(&charge_call, &none_call));
}

#[test]
fn test_m3_fiba_art17_art19_art42_and_ring_256_eviction() {
    // FIBA Art. 17.2.4: L2M stoppage sideline clamped vs made-FG free choice
    let fiba = RuleSet::fiba_default();
    let mut inb_stoppage = InboundContext {
        side: CommandSide::Home,
        spot: InboundSpot::BackcourtEndline,
        sideline_side: CourtSideLine::OppositeTableSide,
        allow_endline_run: true,
        stop_clock_until_touch: false,
        inbound_ball_at_disposal: false,
        inbound_touched_court_player: false,
        inbound_5s_timer_ds: 50,
        original_spot: InboundSpot::BackcourtEndline,
        pending_shot_clock_ds: Some(200),
        is_period_start_arrow_inbound: false,
        allow_both_sides_advance: false,
        post_made_fg: false,
        post_made_last_ft: false,
    };
    apply_l2m_inbound_advance(
        &mut inb_stoppage,
        AdvanceOption::AdvanceTableSide,
        CourtSideLine::OppositeTableSide,
        4,
        1000,
        1,
        false,
        false,
        &fiba,
    );
    assert_eq!(inb_stoppage.spot, InboundSpot::FibaFrontcourtThrowInLine);
    assert_eq!(inb_stoppage.sideline_side, CourtSideLine::OppositeTableSide);
    assert_eq!(inb_stoppage.pending_shot_clock_ds, Some(140));

    // FIBA Art. 19.2.4: SubbedInCannotLeave & SubbedOutCannotReenter unlocked only by Δu > 0
    let mut state = SimulationEngine::new(
        &make_init_spec_with_ruleset(2, fiba.clone(), "h", "a", 12),
        &mut NullSimSink,
    )
    .unwrap()
    .state;
    state.quarter = 4;
    state.q1_game_clock_ever_started = true;
    state.game_phase = GamePhase::InboundSetup;
    state.jump_ball_context = None;
    state.inbound_context = Some(inb_stoppage);
    set_clocks_exact_ds(&mut state, 1000, Some(240));
    state.team_states[0].active_lineup_slots = [0, 1, 2, 3, 4];
    for r in 0..5 {
        state.players[r].on_court_slot = Some(r as u8);
    }
    state.players[5].fiba_sub_reentry_lock = FibaSubReentryLock::SubbedOutCannotReenter;
    assert!(matches!(
        validate_substitution(&state, CommandSide::Home, 0, 5, &fiba, false),
        Err(CommandRejectReason::SubIneligibleUntilClockRun)
    ));
    // Run clock 0.4 tenths (Δu = 4,000 > 0) -> unlocks
    state.game_clock_running = true;
    advance_clocks_sub_ds(&mut state, 0.4);
    assert!(validate_substitution(&state, CommandSide::Home, 0, 5, &fiba, false).is_ok());

    // BoundedReviewableEventRing<256> Tier-1 / Tier-2 eviction
    let mut ring = BoundedReviewableEventRing::<256>::new();
    for seq in 1..=256u64 {
        let mut entry = ReviewableEventEntry::default();
        entry.event_seq = seq;
        entry.stoppage_occurred_since = seq % 2 == 0;
        assert_eq!(ring.push_with_eviction(entry, &[2]), None);
    }
    // Pushing 257th with seq=2 pinned must evict oldest stoppage_occurred_since && !pinned == seq 4
    let mut e257 = ReviewableEventEntry::default();
    e257.event_seq = 257;
    let evicted = ring.push_with_eviction(e257, &[2]);
    assert_eq!(evicted, Some(4));

    // FIBA Art. 42.2.4 common tech shot FIRST even if chronologically later; opposing equals cancel
    let penalties = vec![
        QueuedPenalty {
            seq_order: 1,
            offended_side: CommandSide::Home,
            kind: PenaltyKind::PersonalFoul { ft_count: 2 },
            grants_possession: false,
        },
        QueuedPenalty {
            seq_order: 2,
            offended_side: CommandSide::Away,
            kind: PenaltyKind::PersonalFoul { ft_count: 2 },
            grants_possession: false,
        },
        QueuedPenalty {
            seq_order: 3,
            offended_side: CommandSide::Home,
            kind: PenaltyKind::UnsportsmanlikeOrDisqualifying { ft_count: 2 },
            grants_possession: true,
        },
        QueuedPenalty {
            seq_order: 4,
            offended_side: CommandSide::Away,
            kind: PenaltyKind::CommonTechnicalFoul,
            grants_possession: false,
        },
    ];
    let resolved = resolve_fiba_art42_penalties(&penalties);
    assert_eq!(resolved.len(), 2);
    assert!(matches!(resolved[0].kind, PenaltyKind::CommonTechnicalFoul));
    assert!(matches!(resolved[1].kind, PenaltyKind::UnsportsmanlikeOrDisqualifying { .. }));
}

#[test]
fn nba_review_uses_scoring_side_to_award_endline_throw_in() {
    let spec = make_init_spec_with_ruleset(42, RuleSet::nba_default(), "h", "a", 12);
    for retain in [true, false] {
        let mut sink = VecSimSink::default();
        let mut engine = SimulationEngine::new(&spec, &mut sink).unwrap();
        let tick = engine.state.sim_tick;
        engine.state.q1_game_clock_ever_started = true;
        engine.state.game_phase = GamePhase::WhistlePendingCommit;
        engine.state.possession_side = Some(CommandSide::Home);
        engine.state.jump_ball_context = None;
        engine.state.cursor = EngineCursor::PausedInPostWhistle { window_open_tick: tick, ruling_id: 1 };
        engine.state.team_states[0].score_on_court = 2;
        let shot_event_seq = engine.state.next_event_seq;
        engine.state.next_event_seq += 1;
        engine.state.recent_reviewable_events.push_with_eviction(ReviewableEventEntry {
            event_seq: shot_event_seq,
            tick,
            quarter: 1,
            points_awarded: 2,
            scorer_side: Some(CommandSide::Home),
            shooter_roster_idx: Some(0),
            original_fga_counted: true,
            ..ReviewableEventEntry::default()
        }, &[]);
        engine.state.active_pending_ruling = Some(PendingRuling {
            ruling_id: 1,
            whistle_tick: tick,
            window_open_tick: tick,
            request_seq: 0,
            window_epoch: 0,
            challenge_attempts_in_window: 0,
            window_state: ChallengeWindowState::Open,
            window_kind: ChallengeWindowKind::LiveDeadball,
            perceived_call: RuleCallSnapshot {
                rule_code: RuleCode::FOUL_SHOOTING,
                offending_side: CommandSide::Away,
                offender_roster_idx: 15,
                victim_roster_idx: Some(0),
                basket_made_on_play: true,
                foul_occurred_after_shooting_motion_started: retain,
                ..RuleCallSnapshot::default()
            },
            ground_truth: RuleCallSnapshot::default(),
            eligible_sides: EligibleSidesMask(0b10),
            period_expired_after_ruling: false,
        });
        engine.enqueue_command(IngressCommand {
            client_request_id: 1,
            apply_at_tick: None,
            phase: CommandPhase::PostWhistle,
            side: CommandSide::Away,
            payload: CommandPayload::Player(PlayerCoachCommand::ChallengeCall {
                target: ChallengeTarget::Ruling(1),
                expected_window_epoch: Some(0),
            }),
        }, &mut sink).unwrap();
        assert_eq!(engine.step(&mut sink), StepStatus::TickCompleted { tick });
        let inbound = engine.state.inbound_context.unwrap();
        if retain {
            assert_eq!(engine.last_public_frame.score_home, 2);
            assert_eq!(inbound.side, CommandSide::Away);
            assert_eq!(inbound.spot, InboundSpot::BackcourtEndline);
            assert_eq!(inbound.pending_shot_clock_ds, Some(240));
        } else {
            assert_eq!(engine.last_public_frame.score_home, 0);
            assert_eq!(inbound.side, CommandSide::Home);
            assert!(sink.events.iter().any(|event| event.tick == tick && matches!(event.payload,
                GameRecordEventPayload::ScoringEventAmended {
                    original_shot_event_seq, old_points: 2, new_points: 0,
                    pts_delta: -2, fga_delta: -1, fgm_delta: -1, ..
                } if original_shot_event_seq == shot_event_seq
            )));
        }
    }
}

#[test]
fn manual_matchup_lock_holds_until_clear_or_either_player_leaves() {
    let spec = make_init_spec_with_ruleset(7, RuleSet::nba_default(), "h", "a", 12);
    let send_override = |engine: &mut SimulationEngine, sink: &mut VecSimSink, patch: PatchField<(u8, u8)>| {
        let request_id = engine.state.next_ingress_seq as u64 + 1;
        engine.enqueue_command(IngressCommand {
            client_request_id: request_id,
            apply_at_tick: None,
            phase: CommandPhase::PreTick,
            side: CommandSide::Home,
            payload: CommandPayload::Player(PlayerCoachCommand::SetDefensiveGameplan {
                gameplan_id: PatchField::Keep,
                protect_targets: PatchField::Keep,
                pnr_rule_override: PatchField::Keep,
                matchup_override: patch,
            }),
        }, sink).unwrap();
        assert_eq!(engine.step(sink), StepStatus::TickCompleted { tick: engine.state.sim_tick - 1 });
    };
    let locked_engine = |sink: &mut VecSimSink| {
        let mut engine = SimulationEngine::new(&spec, sink).unwrap();
        send_override(&mut engine, sink, PatchField::Set((4, 15)));
        assert_eq!(engine.state.defense_plans[0].inspect().manual_matchup_lock, [4, 15]);
        assert_eq!(engine.state.defense_plans[0].matchups()[4], 0);
        engine
    };

    // 防守人下场与进攻人下场都解除锁
    let mut sink = VecSimSink::default();
    let mut engine = locked_engine(&mut sink);
    engine.apply_substitution(CommandSide::Home, 4, 9, 1);
    assert_eq!(engine.state.defense_plans[0].inspect().manual_matchup_lock, [0xFF; 2]);
    let mut sink = VecSimSink::default();
    let mut engine = locked_engine(&mut sink);
    engine.apply_substitution(CommandSide::Away, 15, 20, 1);
    assert_eq!(engine.state.defense_plans[0].inspect().manual_matchup_lock, [0xFF; 2]);

    // 进攻人不在场上时不建立锁
    let mut sink = VecSimSink::default();
    let mut engine = SimulationEngine::new(&spec, &mut sink).unwrap();
    send_override(&mut engine, &mut sink, PatchField::Set((4, 22)));
    assert_eq!(engine.state.defense_plans[0].inspect().manual_matchup_lock, [0xFF; 2]);

    // 锁在全部对位重算与换防逻辑下保持，只有被锁定球员下场才解除
    let mut sink = VecSimSink::default();
    let mut engine = locked_engine(&mut sink);
    let mut observed_locked_ticks = 0usize;
    let mut cleared_by_substitution = false;
    let mut recorded = Vec::new();
    for _ in 0..3000 {
        let status = step_with_batch_ai(&mut engine, &mut sink, &mut recorded);
        assert!(matches!(status, StepStatus::TickCompleted { .. }), "{status:?}");
        let home = engine.state.team_states[0].active_lineup_slots;
        let away = engine.state.team_states[1].active_lineup_slots;
        match engine.state.defense_plans[0].inspect().manual_matchup_lock {
            [4, 15] => {
                assert!(!cleared_by_substitution);
                let defender_pos = home.iter().position(|&r| r == 4).unwrap();
                let attacker_pos = away.iter().position(|&r| r == 15).unwrap();
                assert_eq!(engine.state.defense_plans[0].matchups()[defender_pos], attacker_pos as u8);
                observed_locked_ticks += 1;
            }
            [0xFF, 0xFF] => {
                if !cleared_by_substitution {
                    let tick = engine.state.sim_tick - 1;
                    assert!(sink.events.iter().any(|event| event.tick == tick && matches!(event.payload,
                        GameRecordEventPayload::SubstitutionApplied { team_side: 0, out_roster_idx: 4, .. }
                        | GameRecordEventPayload::SubstitutionApplied { team_side: 1, out_roster_idx: 15, .. }
                    )), "lock cleared at tick {tick} while both locked players are on court");
                    cleared_by_substitution = true;
                }
            }
            other => panic!("unexpected manual matchup lock {other:?}"),
        }
    }
    assert!(observed_locked_ticks >= 1000, "lock held for {observed_locked_ticks} ticks");

    // Clear 之后不再恢复
    let mut sink = VecSimSink::default();
    let mut engine = locked_engine(&mut sink);
    send_override(&mut engine, &mut sink, PatchField::Clear);
    assert_eq!(engine.state.defense_plans[0].inspect().manual_matchup_lock, [0xFF; 2]);
    for _ in 0..1500 {
        let status = step_with_batch_ai(&mut engine, &mut sink, &mut recorded);
        assert!(matches!(status, StepStatus::TickCompleted { .. }), "{status:?}");
        assert_eq!(engine.state.defense_plans[0].inspect().manual_matchup_lock, [0xFF; 2]);
    }
}

#[test]
fn fiba_scoring_review_corrects_booked_points_before_throw_in() {
    let spec = make_init_spec_with_ruleset(42, RuleSet::fiba_default(), "h", "a", 12);
    let mut sink = VecSimSink::default();
    let mut engine = SimulationEngine::new(&spec, &mut sink).unwrap();
    engine.state.quarter = 2;
    engine.state.q1_game_clock_ever_started = true;
    engine.state.game_phase = GamePhase::InboundSetup;
    engine.state.jump_ball_context = None;
    engine.state.inbound_context = Some(InboundContext {
        side: CommandSide::Away,
        spot: InboundSpot::BackcourtEndline,
        sideline_side: CourtSideLine::TableSide,
        allow_endline_run: true,
        stop_clock_until_touch: true,
        inbound_ball_at_disposal: false,
        inbound_touched_court_player: false,
        inbound_5s_timer_ds: 0,
        original_spot: InboundSpot::BackcourtEndline,
        pending_shot_clock_ds: Some(240),
        is_period_start_arrow_inbound: false,
        allow_both_sides_advance: false,
        post_made_fg: true,
        post_made_last_ft: false,
    });
    let shot_event_seq = engine.state.next_event_seq;
    engine.state.next_event_seq += 1;
    engine.state.team_states[0].score_on_court = 3;
    engine.state.recent_reviewable_events.push_with_eviction(ReviewableEventEntry {
        event_seq: shot_event_seq,
        tick: engine.state.sim_tick - 1,
        quarter: 2,
        points_awarded: 3,
        scorer_side: Some(CommandSide::Home),
        shooter_roster_idx: Some(0),
        assist_roster_idx: Some(1),
        original_is_three_pointer: true,
        original_fga_counted: true,
        ground_truth_is_three_pointer: false,
        ..ReviewableEventEntry::default()
    }, &[]);
    let review_tick = engine.state.sim_tick;
    engine.enqueue_command(IngressCommand {
        client_request_id: 1,
        apply_at_tick: None,
        phase: CommandPhase::PreTick,
        side: CommandSide::Away,
        payload: CommandPayload::Player(PlayerCoachCommand::ChallengeCall {
            target: ChallengeTarget::RecentScoringOrClockEvent { event_seq: shot_event_seq },
            expected_window_epoch: None,
        }),
    }, &mut sink).unwrap();
    assert_eq!(engine.step(&mut sink), StepStatus::TickCompleted { tick: review_tick });
    assert_eq!(engine.state.team_states[0].score_on_court, 2);
    assert_eq!(engine.state.team_states[1].challenges_remaining, 0);
    assert_eq!(engine.last_public_frame.score_home, 2);
    assert_eq!(engine.last_public_frame.ball_flags & 0x20, 0);
    assert!(sink.events.iter().any(|event| event.tick == review_tick && matches!(event.payload,
        GameRecordEventPayload::ScoringEventAmended {
            original_shot_event_seq, old_points: 3, new_points: 2,
            pts_delta: -1, fga_delta: 0, fgm_delta: 0, tpa_delta: -1, tpm_delta: -1, ast_delta: 0, ..
        } if original_shot_event_seq == shot_event_seq
    )));
}

#[test]
fn test_m3_m4_10_replay_equivalence_native_vs_wasm_host_and_rotation_locks() {
    // P0 Stint lock protects recently subbed-in player from voluntary P2-P5 benching
    let spec_nba = make_init_spec_with_ruleset(101, RuleSet::nba_default(), "h", "a", 12);
    let mut sink = NullSimSink;
    let mut eng_rot = SimulationEngine::new(&spec_nba, &mut sink).unwrap();
    eng_rot.state.quarter = 2;
    eng_rot.state.game_clock_ds = 3000;
    eng_rot.state.players[0].cumulative_stamina = 40.0; // low stamina -> wants P2 out
    eng_rot.state.players[0].stint_lock_remaining_live_ticks = 30; // protected by P0!
    let subs = plan_ai_substitutions(
        CommandSide::Home,
        0,
        &eng_rot.teams[0],
        &eng_rot.state.team_states[0],
        eng_rot.state.team_states[1].score_on_court,
        &eng_rot.state.players,
        eng_rot.state.quarter,
        eng_rot.state.game_clock_ds,
        &eng_rot.ruleset,
        &eng_rot.tunables,
        eng_rot.canonical_team_ids[0],
    );
    assert!(
        !subs.iter().any(|&(out_r, _, tier)| out_r == 0 && tier > 0),
        "P0 lock must prevent voluntary sub-out of slot 0"
    );

    // 10 multi-seed full-game ReplayManifest equivalence tests + host WASM wrapper parity
    for seed in 1u64..=10 {
        let ruleset = if seed % 2 == 0 {
            RuleSet::fiba_default()
        } else {
            RuleSet::nba_default()
        };
        let spec = make_init_spec_with_ruleset(seed, ruleset, "team_a", "team_b", 12);
        let mut rec_sink = VecSimSink::default();
        let mut eng = SimulationEngine::new(&spec, &mut rec_sink).unwrap();
        let mut cmds = Vec::new();
        let mut ticks = 0usize;
        while !matches!(eng.state.cursor, EngineCursor::GameFinished) && ticks < 35_000 {
            let st = step_with_batch_ai(&mut eng, &mut rec_sink, &mut cmds);
            ticks += 1;
            if matches!(st, StepStatus::Finished { .. } | StepStatus::Aborted { .. }) {
                break;
            }
        }
        assert_eq!(eng.state.cursor, EngineCursor::GameFinished, "seed {seed} did not finish cleanly");
        assert!(
            eng.state.team_states[0].score_on_court > 0 || eng.state.team_states[1].score_on_court > 0,
            "seed {seed} scored 0 total points"
        );
        let final_hash = rec_sink.hashes.last().unwrap().3;

        // Deterministic replay from AnchoredCommand log & host_engine_* wrappers must match bit-identically
        let mut rep_sink = VecSimSink::default();
        let mut eng_rep = SimulationEngine::new(&spec, &mut rep_sink).unwrap();
        let mut cmd_idx = 0usize;
        let mut rep_steps = 0usize;
        while !matches!(eng_rep.state.cursor, EngineCursor::GameFinished) && rep_steps < 40_000 {
            rep_steps += 1;
            while cmd_idx < cmds.len()
                && cmds[cmd_idx].resolved_tick == eng_rep.state.sim_tick
                && cmds[cmd_idx].resolved_phase == CommandPhase::PreTick
            {
                eng_rep
                    .replay_anchored_command(cmds[cmd_idx], &mut rep_sink)
                    .unwrap();
                cmd_idx += 1;
            }
            let mut st = eng_rep.step(&mut rep_sink);
            let mut pw_guard = 0usize;
            while let StepStatus::AwaitingPostWhistle { .. } = st {
                pw_guard += 1;
                assert!(pw_guard < 16, "seed {seed} stuck in PostWhistle at tick={}", eng_rep.state.sim_tick);
                while cmd_idx < cmds.len()
                    && cmds[cmd_idx].resolved_tick == eng_rep.state.sim_tick
                    && cmds[cmd_idx].resolved_phase == CommandPhase::PostWhistle
                {
                    eng_rep
                        .replay_anchored_command(cmds[cmd_idx], &mut rep_sink)
                        .unwrap();
                    cmd_idx += 1;
                }
                st = eng_rep.step(&mut rep_sink);
            }
            if matches!(st, StepStatus::Finished { .. } | StepStatus::Aborted { .. }) {
                break;
            }
        }
        assert_eq!(rep_sink.hashes.last().unwrap().3, final_hash, "replay hash mismatch on seed {seed}");

        // Check host_engine_* WASM ABI wrappers on first seed (full run bit-exactness)
        if seed == 1 {
            let h = host_engine_from_manifest(&spec.to_canonical_le_bytes());
            assert_ne!(h, 0);
            let mut ci = 0usize;
            let mut w_steps = 0usize;
            loop {
                w_steps += 1;
                assert!(w_steps < 40_000, "wasm host stuck");
                let st_code = host_engine_step(h);
                if st_code == 2 {
                    while ci < cmds.len() && cmds[ci].resolved_phase == CommandPhase::PostWhistle {
                        let cb = cmds[ci].to_canonical_le_bytes();
                        assert_eq!(host_engine_replay_anchored_command(h, &cb), 0);
                        ci += 1;
                        if host_engine_step(h) != 2 {
                            break;
                        }
                    }
                    continue;
                }
                if st_code == 3 || st_code == 4 {
                    break;
                }
            }
            let wh = host_engine_last_hashes(h).unwrap();
            assert_eq!(&wh[64..96], &final_hash);
            host_engine_destroy(h);
        }
    }
}
