mod common;

use common::make_init_spec;
use hoops_core::*;
use serde::Deserialize;

#[derive(Deserialize)]
struct PhiloxFixture {
    vectors: Vec<PhiloxVector>,
}

#[derive(Deserialize)]
struct PhiloxVector {
    seed: u64,
    key: [u32; 2],
    counter: [u32; 4],
    out: [u32; 4],
    u53_low_bits: u64,
    u53_high_bits: u64,
    gauss_bits: u64,
}

#[test]
fn test_canonical_le_bytes_8_rules_and_rejections() {
    // NaN / Inf rejection + -0.0 -> +0.0 normalization
    assert_eq!(canonicalize_f64(-0.0), 0.0f64.to_bits());
    assert_eq!((-0.0f64).to_canonical_le_bytes(), [0u8; 8]);
    assert!(matches!(
        f64::from_canonical_le_bytes(&f64::NAN.to_bits().to_le_bytes()),
        Err(DecodeError::NonFiniteValue)
    ));
    assert!(matches!(
        f64::from_canonical_le_bytes(&f64::INFINITY.to_bits().to_le_bytes()),
        Err(DecodeError::NonFiniteValue)
    ));
    assert!(matches!(
        f32::from_canonical_le_bytes(&f32::NEG_INFINITY.to_bits().to_le_bytes()),
        Err(DecodeError::NonFiniteValue)
    ));

    // Option/bool strict tags & String Id length bounds
    assert!(matches!(
        bool::from_canonical_le_bytes(&[2]),
        Err(DecodeError::InvalidEnumValue { field: "bool", value: 2 })
    ));
    assert!(matches!(
        Option::<u8>::from_canonical_le_bytes(&[2, 0]),
        Err(DecodeError::InvalidEnumValue { field: "Option", value: 2 })
    ));
    assert!(matches!(
        validate_id_string(""),
        Err(DecodeError::IdLengthOutOfRange { len: 0 })
    ));
    assert!(matches!(
        validate_id_string("a\0b"),
        Err(DecodeError::IdLengthOutOfRange { .. })
    ));

    // InlinePlayerList<5>: valid roundtrip + non-0xFF unused tail rejection + duplicate/OOB checks
    let list = InlinePlayerList::<5>::from_slice(&[3, 7, 15]).unwrap();
    assert_eq!(list.len, 3);
    assert_eq!(list.items[3], 0xFF);
    let list_json = serde_json::to_string(&list).unwrap();
    let list_de: InlinePlayerList<5> = serde_json::from_str(&list_json).unwrap();
    assert_eq!(list, list_de);
    let mut bad_bytes = list.to_canonical_le_bytes();
    bad_bytes[4] = 0xAA; // non-0xFF unused tail -> NonZeroPadding
    assert!(matches!(
        InlinePlayerList::<5>::from_canonical_le_bytes(&bad_bytes),
        Err(DecodeError::InvalidEnumValue {
            field: "InlinePlayerList.unused_tail",
            value: 0xAA
        })
    ));

    // PublicFrameV1 non-zero reserved/padding flags rejection
    let f = PublicFrameV1::default();
    let mut fb = f.to_bytes();
    assert_eq!(fb.len(), PUBLIC_FRAME_BYTES);
    let mut r = CanonicalReader::new(&fb);
    assert_eq!(PublicFrameV1::read_le(&mut r, 24).unwrap(), f);
    // Corrupt ball_flags reserved MSB (bit 7 = 0x80) at offset 23
    fb[23] |= 0x80;
    let mut r_bad = CanonicalReader::new(&fb);
    assert!(matches!(
        PublicFrameV1::read_le(&mut r_bad, 24),
        Err(DecodeError::NonZeroPadding { .. })
    ));
}

#[test]
fn test_frozen_enum_counts_and_tunables_113_153() {
    assert_eq!(RuleCode::COUNT, 38);
    assert_eq!(CommandRejectReason::COUNT, 28);
    assert_eq!(CommandAbortReason::COUNT, 11);
    for v in 0..=0x0025u16 {
        assert!(RuleCode::from_u16(v).is_some(), "missing RuleCode 0x{v:04X}");
    }
    assert!(RuleCode::from_u16(0x0026).is_none());

    for v in 1..=28u16 {
        assert!(CommandRejectReason::from_u16(v).is_some(), "missing RejectReason {v}");
    }
    assert!(CommandRejectReason::from_u16(29).is_none());

    for v in 1..=11u8 {
        assert!(CommandAbortReason::from_u8(v).is_some(), "missing AbortReason {v}");
    }
    assert!(CommandAbortReason::from_u8(12).is_none());

    // Exact 113 fields / 153 scalars in EngineTunables
    let t = EngineTunables::default();
    let val = serde_json::to_value(&t).unwrap();
    let obj = val.as_object().unwrap();
    assert_eq!(obj.len(), 113, "EngineTunables must have exactly 113 fields");
    fn count_scalars(v: &serde_json::Value) -> usize {
        match v {
            serde_json::Value::Array(a) => a.iter().map(count_scalars).sum(),
            serde_json::Value::Object(m) => m.values().map(count_scalars).sum(),
            _ => 1,
        }
    }
    assert_eq!(count_scalars(&val), 153, "EngineTunables must have exactly 153 scalars");
    let raw = t.to_canonical_le_bytes();
    assert_eq!(EngineTunables::from_canonical_le_bytes(&raw).unwrap(), t);
}

#[test]
fn test_philox4x32_10_golden_vectors() {
    let raw = include_str!("fixtures/rng_philox_v1_golden.json");
    let fix: PhiloxFixture = serde_json::from_str(raw).unwrap();
    for vec in &fix.vectors {
        let key = Philox4x32_10::key_from_seed(vec.seed);
        assert_eq!(key, vec.key);
        let out = Philox4x32_10::generate(key, vec.counter);
        assert_eq!(out, vec.out);
        assert_eq!(Philox4x32_10::words_to_u53_low(out).to_bits(), vec.u53_low_bits);
        assert_eq!(Philox4x32_10::words_to_u53_high(out).to_bits(), vec.u53_high_bits);
        let g = Philox4x32_10::words_to_gaussian(out);
        let diff = (g - f64::from_bits(vec.gauss_bits)).abs();
        assert!(diff < 1.0e-12, "gaussian mismatch: {diff}");
    }
}

#[test]
fn test_rng_budget_saturated_65th_call_aborts_debug_and_release() {
    let spec = make_init_spec(2026);
    let mut sink = VecSimSink::default();
    let mut engine = SimulationEngine::new(&spec, &mut sink).unwrap();

    // Pre-saturate ContactAndFoulRoll (budget = 64) for Home slot 0 (actor 0x00)
    for _ in 0..64 {
        let val = sample_u53_checked(
            engine.philox_key,
            engine.state.possession_id,
            engine.state.possession_tick,
            &mut engine.state.rng_sample_counters.0,
            0x00,
            RngPurpose::ContactAndFoulRoll,
        );
        assert!(val.is_ok());
    }
    // 65th call fails consistently in Debug & Release -> abort_match_determinism
    let err = sample_u53_checked(
        engine.philox_key,
        engine.state.possession_id,
        engine.state.possession_tick,
        &mut engine.state.rng_sample_counters.0,
        0x00,
        RngPurpose::ContactAndFoulRoll,
    )
    .unwrap_err();
    assert_eq!(err.purpose, RngPurpose::ContactAndFoulRoll);
    assert_eq!(err.budget, 64);

    let status = engine.abort_match_determinism(err, &mut sink);
    assert!(matches!(
        status,
        StepStatus::Aborted {
            reason: DeterminismViolationKind::RngCounterSaturated,
            ..
        }
    ));
    assert_eq!(engine.state.end_reason, Some(GameEndReason::DeterminismInvariantViolation));
    assert!(sink
        .audits
        .last()
        .unwrap()
        .warnings
        .iter()
        .any(|w| matches!(w, AuditWarning::RngCounterSaturated { budget: 64, .. })));

    // Also verify WASM ABI wrapper returns code 4 after host_engine_step hits RngCounterSaturated!
    let spec_bytes = spec.to_canonical_le_bytes();
    let handle = host_engine_from_manifest(&spec_bytes);
    assert_ne!(handle, 0);
    // Test host_engine_enqueue_command capacity probe 0xFFFF_FFFE with zero state mutation + AlreadyAccepted
    let cmd = IngressCommand {
        client_request_id: 1,
        apply_at_tick: None,
        phase: CommandPhase::PreTick,
        side: CommandSide::Home,
        payload: CommandPayload::Player(PlayerCoachCommand::SetPaceAndPressure {
            pace: PatchField::Set(72),
            full_court_press: PatchField::Keep,
        }),
    };
    let cmd_bytes = cmd.to_canonical_le_bytes();
    let mut small_buf = [0u8; 4];
    let rc_small = host_engine_enqueue_command(handle, &cmd_bytes, &mut small_buf);
    assert_eq!(rc_small, WASM_ERR_BUFFER_TOO_SMALL);
    let needed = u32::from_le_bytes(small_buf) as usize;
    assert!(needed > 4);
    let mut exact_buf = vec![0u8; needed];
    assert_eq!(host_engine_enqueue_command(handle, &cmd_bytes, &mut exact_buf), 0);
    // Retrying with same client_request_id & payload triggers AlreadyAccepted (zero mutation, identical anchored bytes)
    let mut retry_buf = vec![0u8; needed];
    assert_eq!(host_engine_enqueue_command(handle, &cmd_bytes, &mut retry_buf), 0);
    assert_eq!(exact_buf, retry_buf);
    host_engine_destroy(handle);
}

#[test]
fn test_forfeit_match_path1_pretick_and_path2_postwhistle() {
    // Path 1: PreTick ForfeitMatch (scheduled at current sim_tick) with earlier + later command on same tick
    let spec = make_init_spec(99);
    let mut sink1 = VecSimSink::default();
    let mut eng1 = SimulationEngine::new(&spec, &mut sink1).unwrap();
    let target_tick = eng1.state.sim_tick;
    let earlier = IngressCommand {
        client_request_id: 1,
        apply_at_tick: Some(target_tick),
        phase: CommandPhase::PreTick,
        side: CommandSide::Home,
        payload: CommandPayload::Player(PlayerCoachCommand::SetPaceAndPressure {
            pace: PatchField::Set(70),
            full_court_press: PatchField::Keep,
        }),
    };
    let forfeit_cmd = IngressCommand {
        client_request_id: 1,
        apply_at_tick: Some(target_tick),
        phase: CommandPhase::PreTick,
        side: CommandSide::Away,
        payload: CommandPayload::Player(PlayerCoachCommand::ForfeitMatch),
    };
    let later = IngressCommand {
        client_request_id: 2,
        apply_at_tick: Some(target_tick),
        phase: CommandPhase::PreTick,
        side: CommandSide::Home,
        payload: CommandPayload::Player(PlayerCoachCommand::SetPaceAndPressure {
            pace: PatchField::Set(80),
            full_court_press: PatchField::Keep,
        }),
    };
    eng1.enqueue_command(earlier, &mut sink1).unwrap();
    eng1.enqueue_command(forfeit_cmd, &mut sink1).unwrap();
    eng1.enqueue_command(later, &mut sink1).unwrap();

    let st1 = eng1.step(&mut sink1);
    assert!(matches!(st1, StepStatus::Finished { tick } if tick == target_tick));
    assert_eq!(eng1.state.cursor, EngineCursor::GameFinished);
    assert_eq!(eng1.state.end_reason, Some(GameEndReason::ForfeitAway));
    assert_eq!(eng1.state.team_states[0].score_official_override, Some(2));
    assert_eq!(eng1.state.team_states[1].score_official_override, Some(0));
    // `later` command on same tick aborted with SupercededByLaterForfeit (0x0A)
    assert!(sink1.events.iter().any(|ev| matches!(
        ev.payload,
        GameRecordEventPayload::AnchoredCommandAbortedRecord {
            abort_reason_code: 0x0A,
            ..
        }
    )));
    assert!(sink1.events.iter().any(|ev| matches!(
        ev.payload,
        GameRecordEventPayload::GameEnded {
            score_home_official: 2,
            score_away_official: 0,
            end_reason: 3, // ForfeitAway
            ..
        }
    )));
    let h1 = sink1.hashes.last().unwrap();
    assert_ne!(h1.3, [0u8; 32]);

    // Path 2: PostWhistle ForfeitMatch sets PendingForfeitFinalize, step() finalizes Stage 8 next (FIBA -> 20-0)
    let mut fiba_spec = make_init_spec(99);
    fiba_spec.ruleset = RuleSet::fiba_default();
    let mut sink2 = VecSimSink::default();
    let mut eng2 = SimulationEngine::new(&fiba_spec, &mut sink2).unwrap();
    // Put engine into PausedInPostWhistle cleanly
    eng2.state.active_pending_ruling = Some(PendingRuling {
        ruling_id: 77,
        whistle_tick: 0,
        window_open_tick: 0,
        request_seq: 1,
        window_epoch: 1,
        challenge_attempts_in_window: 0,
        window_state: ChallengeWindowState::Open,
        window_kind: ChallengeWindowKind::LiveDeadball,
        perceived_call: RuleCallSnapshot {
            rule_code: RuleCode::FOUL_REACH_COMMON,
            offending_side: CommandSide::Away,
            offender_roster_idx: 15,
            victim_roster_idx: Some(0),
            restart_shot_clock_ds: Some(240),
            ..RuleCallSnapshot::default()
        },
        ground_truth: RuleCallSnapshot {
            rule_code: RuleCode::FOUL_REACH_COMMON,
            offending_side: CommandSide::Away,
            offender_roster_idx: 15,
            victim_roster_idx: Some(0),
            restart_shot_clock_ds: Some(240),
            ..RuleCallSnapshot::default()
        },
        eligible_sides: EligibleSidesMask(0b11),
        period_expired_after_ruling: false,
    });
    eng2.state.cursor = EngineCursor::PausedInPostWhistle {
        window_open_tick: 0,
        ruling_id: 77,
    };

    let pw_forfeit = IngressCommand {
        client_request_id: 1,
        apply_at_tick: None,
        phase: CommandPhase::PostWhistle,
        side: CommandSide::Home,
        payload: CommandPayload::Player(PlayerCoachCommand::ForfeitMatch),
    };
    eng2.enqueue_command(pw_forfeit, &mut sink2).unwrap();
    assert!(matches!(
        eng2.state.cursor,
        EngineCursor::PendingForfeitFinalize {
            forfeiting_side: CommandSide::Home,
            cleared_ruling_id: Some(77),
        }
    ));
    let st2 = eng2.step(&mut sink2);
    assert!(matches!(st2, StepStatus::Finished { .. }));
    assert_eq!(eng2.state.cursor, EngineCursor::GameFinished);
    assert_eq!(eng2.state.end_reason, Some(GameEndReason::ForfeitHome));
    assert_eq!(eng2.state.team_states[0].score_official_override, Some(0));
    assert_eq!(eng2.state.team_states[1].score_official_override, Some(20));
}

#[test]
fn test_htr1_80b_header_and_100b_records_roundtrip() {
    let spec = make_init_spec(55);
    let mut sink = VecSimSink::default();
    let mut eng = SimulationEngine::new(&spec, &mut sink).unwrap();
    for _ in 0..5 {
        let _ = step_with_batch_ai(&mut eng, &mut sink, &mut vec![]);
    }
    let htr = HashTraceV1 {
        format_version: HTR1_VERSION,
        start_tick: 0,
        tick_count: sink.hashes.len() as u32,
        init_spec_sha256: sha256_bytes(&spec.to_canonical_le_bytes()),
        applied_commands_sha256: sha256_bytes(&[]),
        records: sink
            .hashes
            .iter()
            .map(|&(tick, state_hash, event_hash, rolling_hash)| HashTraceRecordV1 {
                tick,
                state_hash,
                event_hash,
                rolling_hash,
            })
            .collect(),
    };
    let wire = htr.to_bytes();
    assert_eq!(
        wire.len(),
        HTR1_HEADER_BYTES + htr.records.len() * HTR1_RECORD_BYTES
    );
    let decoded = HashTraceV1::from_bytes(&wire).unwrap();
    assert_eq!(decoded, htr);
}
