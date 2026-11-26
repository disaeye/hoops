use std::fs;
use std::path::PathBuf;

use hoops_config::ConfigBundle;
use hoops_core::*;
use hoops_output::*;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

#[test]
fn test_hps1_public_only_rejects_private_dicts_and_raw_roundtrip() {
    let bundle = ConfigBundle::load_from_dir(&repo_root().join("configs")).unwrap();
    let spec = bundle
        .build_match_init_spec(
            "nba",
            "boston_five_out",
            "bay_motion_split",
            101,
            StateHashMode::Disabled,
        )
        .unwrap();

    let mut hdr_pub = Hps1Header::from_init_spec(&spec, StreamVisibility::PublicOnly);
    let pub_bytes = hdr_pub.to_bytes();
    assert_eq!(Hps1Header::from_bytes(&pub_bytes).unwrap(), hdr_pub);

    // Injecting private play_dict in PublicOnly stream must be rejected with PrivateDictInPublicStream
    hdr_pub.play_dict.push((0, "secret_play".to_owned()));
    let bad_bytes = hdr_pub.to_bytes();
    assert!(matches!(
        Hps1Header::from_bytes(&bad_bytes),
        Err(DecodeError::PrivateDictInPublicStream)
    ));

    let hdr_auth = Hps1Header::from_init_spec(&spec, StreamVisibility::AuthoritativeFull);
    assert_eq!(hdr_auth.play_dict.len(), 8);
    let expected_rules: Vec<_> = spec.defenses.iter().flat_map(|plan| &plan.pnr_rules)
        .enumerate().map(|(i, rule)| (i as u16, rule.rule_id.as_str().to_owned())).collect();
    assert_eq!(hdr_auth.def_dict, expected_rules);
}

#[test]
fn test_soa_zstd_chunking_half_split_seek_slice_and_golden_vectors() {
    let root = repo_root();
    let bundle = ConfigBundle::load_from_dir(&root.join("configs")).unwrap();
    let spec = bundle
        .build_match_init_spec(
            "nba",
            "boston_five_out",
            "mile_high_hub",
            777,
            StateHashMode::PerTickRolling,
        )
        .unwrap();

    let mut sink = VecSimSink::default();
    let mut eng = SimulationEngine::new(&spec, &mut sink).unwrap();
    let mut cmds = Vec::new();
    for _ in 0..700 {
        let _ = step_with_batch_ai(&mut eng, &mut sink, &mut cmds);
    }

    let hdr = Hps1Header::from_init_spec(&spec, StreamVisibility::PublicOnly);
    let zstd_bytes = encode_soa_zstd_archive(&hdr, &sink.public_frames, &sink.events, 3).unwrap();
    let decoded = decode_soa_zstd_archive(&zstd_bytes).unwrap();
    assert_eq!(decoded.header, hdr);
    assert_eq!(decoded.frames, sink.public_frames);
    assert_eq!(decoded.events, sink.events);
    // > 600 ticks -> >= 2 chunks in SeekIndexFooterV1
    assert!(decoded.seek_index.len() >= 2);

    // 按 600 帧封口的分块归档增量拼装后与整体编码逐字节一致；tick 重叠的分块被拒收
    let mut assembled = Vec::new();
    {
        let mut assembler = SoaArchiveAssembler::new(&hdr, &mut assembled).unwrap();
        for chunk_frames in sink.public_frames.chunks(CHUNK_MAX_TICKS) {
            let t_start = chunk_frames[0].tick;
            let t_end = chunk_frames[chunk_frames.len() - 1].tick;
            let chunk_events: Vec<GameRecordEventV1> = sink
                .events
                .iter()
                .filter(|event| event.tick >= t_start && event.tick <= t_end)
                .cloned()
                .collect();
            let chunk_archive =
                encode_soa_zstd_archive(&hdr, chunk_frames, &chunk_events, 3).unwrap();
            assembler.append_chunk_archive(&chunk_archive).unwrap();
            if t_start == 0 {
                assert!(matches!(
                    assembler.append_chunk_archive(&chunk_archive),
                    Err(OutputError::Decode(DecodeError::FrameSequenceGap { expected: 600, got: 0 }))
                ));
            }
        }
        assembler.finish().unwrap();
    }
    assert_eq!(assembled, zstd_bytes);

    let range = 590..=620;
    let window = decode_soa_zstd_archive_range(&zstd_bytes, *range.start(), *range.end()).unwrap();
    let expected_frames: Vec<_> = sink
        .public_frames
        .iter()
        .filter(|frame| range.contains(&frame.tick))
        .copied()
        .collect();
    let expected_events: Vec<_> = sink
        .events
        .iter()
        .filter(|event| range.contains(&event.tick))
        .cloned()
        .collect();
    assert_eq!(window.header, hdr);
    assert_eq!(window.frames, expected_frames);
    assert_eq!(window.events, expected_events);

    // Forced small max_raw_chunk_bytes exercises half-chunk split path cleanly
    let split_bytes =
        encode_soa_zstd_archive_with_limit(&hdr, &sink.public_frames, &sink.events, 3, 25_000)
            .unwrap();
    let decoded_split = decode_soa_zstd_archive(&split_bytes).unwrap();
    assert_eq!(decoded_split.frames, sink.public_frames);
    assert!(decoded_split.seek_index.len() > decoded.seek_index.len());

    // Extract slice (?from_tick=610&after_event_seq=last_seq_at_600)
    let cut_seq = sink
        .events
        .iter()
        .filter(|e| e.tick < 610)
        .map(|e| e.event_seq)
        .max()
        .unwrap_or(0);
    let (_, slice_f, slice_e) = extract_archive_slice(&zstd_bytes, 610, cut_seq).unwrap();
    assert!(slice_f.iter().all(|f| f.tick >= 610));
    assert!(slice_e.iter().all(|e| e.event_seq > cut_seq));

    // Write golden vectors into crates/hoops-output/tests/fixtures for JS codec verification
    let fixtures_dir = root.join("crates/hoops-output/tests/fixtures");
    fs::create_dir_all(&fixtures_dir).unwrap();
    let golden_frame = &sink.public_frames[10];
    fs::write(
        fixtures_dir.join("frame_v1_golden.bin"),
        golden_frame.to_bytes(),
    )
    .unwrap();
    fs::write(
        fixtures_dir.join("frame_v1_golden.json"),
        serde_json::to_string_pretty(golden_frame).unwrap(),
    )
    .unwrap();

    // Small 10-tick archive golden fixture + JSON reference + uncompressed raw chunks for JS Zstd or Node decode
    let small_frames = &sink.public_frames[..10];
    let last_t = small_frames.last().unwrap().tick;
    let small_events: Vec<GameRecordEventV1> = sink
        .events
        .iter()
        .filter(|e| e.tick <= last_t)
        .cloned()
        .collect();
    let small_archive = encode_soa_zstd_archive(&hdr, small_frames, &small_events, 3).unwrap();
    fs::write(
        fixtures_dir.join("archive_soa_zstd_v1_golden.hframes.zst"),
        &small_archive,
    )
    .unwrap();
    let summary_json = serde_json::json!({
        "header": hdr,
        "frames": small_frames,
        "events": small_events,
    });
    fs::write(
        fixtures_dir.join("archive_soa_zstd_v1_golden.json"),
        serde_json::to_string_pretty(&summary_json).unwrap(),
    )
    .unwrap();
}

#[test]
fn test_self_contained_hreplay_independent_of_disk_configs() {
    let root = repo_root();
    let bundle = ConfigBundle::load_from_dir(&root.join("configs")).unwrap();
    let spec = bundle
        .build_match_init_spec(
            "fiba",
            "bay_motion_split",
            "twin_towers_grit",
            31337,
            StateHashMode::PerTickRolling,
        )
        .unwrap();

    let mut sink = VecSimSink::default();
    let mut eng = SimulationEngine::new(&spec, &mut sink).unwrap();
    let mut cmds = Vec::new();
    while !matches!(eng.state.cursor, EngineCursor::GameFinished) {
        let st = step_with_batch_ai(&mut eng, &mut sink, &mut cmds);
        if matches!(st, StepStatus::Finished { .. } | StepStatus::Aborted { .. }) {
            break;
        }
    }
    let (final_sh, _, final_rh) = eng.last_hashes;
    let manifest = ReplayManifest {
        init_spec: spec,
        applied_commands: cmds,
        verification: Some(ReplayVerificationV1 {
            state_hash_checkpoints: vec![(0, sink.hashes[0].1)],
            final_tick: eng.state.sim_tick,
            final_state_hash: final_sh,
        }),
    };
    let container = HReplayContainer {
        manifest,
        hash_trace: None,
    };
    let mut bytes = container.to_bytes();
    assert!(bytes.len() <= 2_097_152);
    let mut bad_pad = bytes.clone();
    bad_pad[7] = 1;
    assert!(matches!(
        HReplayContainer::from_bytes(&bad_pad),
        Err(OutputError::Decode(DecodeError::NonZeroPadding {
            offset: 7,
            value: 1
        }))
    ));
    let decoded = HReplayContainer::from_bytes(&bytes).unwrap();
    // Replay completely offline without touching any config directory on disk!
    let outcome = decoded.replay_standalone().unwrap();
    assert_eq!(outcome.final_rolling_hash, final_rh);
    assert_eq!(outcome.final_state_hash, final_sh);
    let _ = &mut bytes;
}

#[test]
fn test_boxscore_scoring_amendment_and_28_pbp_zh() {
    let root = repo_root();
    let bundle = ConfigBundle::load_from_dir(&root.join("configs")).unwrap();
    let spec = bundle
        .build_match_init_spec(
            "nba",
            "boston_five_out",
            "twin_towers_grit",
            12,
            StateHashMode::Disabled,
        )
        .unwrap();
    let hdr = Hps1Header::from_init_spec(&spec, StreamVisibility::PublicOnly);
    let home_names: Vec<_> = spec
        .home_team
        .players
        .iter()
        .enumerate()
        .map(|(i, p)| {
            (
                i as u8,
                p.player_id.clone(),
                p.display_name.clone(),
                p.jersey_number_code,
            )
        })
        .collect();
    let away_names: Vec<_> = spec
        .away_team
        .players
        .iter()
        .enumerate()
        .map(|(i, p)| {
            (
                (15 + i) as u8,
                p.player_id.clone(),
                p.display_name.clone(),
                p.jersey_number_code,
            )
        })
        .collect();
    let mut boxs = LiveBoxScoreState::from_roster_names(
        spec.home_team.team_id.clone(),
        spec.away_team.team_id.clone(),
        &home_names,
        &away_names,
    );

    let ev_shot = GameRecordEventV1 {
        tick: 10,
        event_seq: 1,
        quarter: 1,
        game_clock_ds: 7100,
        shot_clock_ds: 180,
        payload: GameRecordEventPayload::ShotResolved {
            shot_id: 1,
            shooter_roster_idx: 2,
            scorer_team: 0,
            points_awarded: 3,
            outcome_class: IntendedOutcomeClass::Swish as u8,
            rim_touched: 0,
            backboard_contact: 0,
            is_blocked: 0,
            blocker_roster_idx: 0xFF,
            is_goaltend: 0,
            assist_roster_idx: 0,
            foul_on_shot: 0,
            telemetry: ShotTelemetryPublic {
                release_tick: 5,
                release_game_clock_ds: 7105,
                whistle_game_clock_ds: 0xFFFF,
                whistle_tick: 0xFFFF_FFFF,
                flight_resolved_tick: 10,
                basket_scored_tick: 10,
            },
        },
    };
    boxs.apply_event(&ev_shot);
    assert_eq!(boxs.home.score_on_court, 3);
    assert_eq!(boxs.players[2].tpm, 1);
    assert_eq!(boxs.players[2].tpa, 1);
    assert_eq!(boxs.players[0].ast, 1);

    // Missed 3PA tracked via ShotReleased -> ShotResolved pair
    let ev_rel3 = GameRecordEventV1 {
        tick: 11,
        event_seq: 2,
        quarter: 1,
        game_clock_ds: 7090,
        shot_clock_ds: 170,
        payload: GameRecordEventPayload::ShotReleased {
            shot_id: 2,
            shooter_roster_idx: 2,
            shot_value: 3,
            release_zone: 4,
            defender_slot: 5,
            nearest_def_dist_mm: 950,
            c_geom_centi: 42,
            release_x_mm: 6000,
            release_y_mm: 2500,
        },
    };
    let ev_miss3 = GameRecordEventV1 {
        tick: 11,
        event_seq: 3,
        quarter: 1,
        game_clock_ds: 7090,
        shot_clock_ds: 170,
        payload: GameRecordEventPayload::ShotResolved {
            shot_id: 2,
            shooter_roster_idx: 2,
            scorer_team: 0,
            points_awarded: 0,
            outcome_class: IntendedOutcomeClass::RimOut as u8,
            rim_touched: 1,
            backboard_contact: 0,
            is_blocked: 0,
            blocker_roster_idx: 0xFF,
            is_goaltend: 0,
            assist_roster_idx: 0xFF,
            foul_on_shot: 0,
            telemetry: ShotTelemetryPublic {
                release_tick: 10,
                release_game_clock_ds: 7100,
                whistle_game_clock_ds: 0xFFFF,
                whistle_tick: 0xFFFF_FFFF,
                flight_resolved_tick: 11,
                basket_scored_tick: 0xFFFF_FFFF,
            },
        },
    };
    boxs.apply_event(&ev_rel3);
    boxs.apply_event(&ev_miss3);
    assert_eq!(boxs.players[2].tpa, 2);
    assert_eq!(boxs.players[2].tpm, 1);

    // 0x19 downgrade 3 -> 2 pointer cleanly amends player + team boxscore
    let ev_amend = GameRecordEventV1 {
        tick: 12,
        event_seq: 4,
        quarter: 1,
        game_clock_ds: 7100,
        shot_clock_ds: 180,
        payload: GameRecordEventPayload::ScoringEventAmended {
            original_shot_event_seq: 1,
            shooter_roster_idx: 2,
            assist_roster_idx: 0,
            team_side: 0,
            amend_kind: 0,
            old_points: 3,
            new_points: 2,
            pts_delta: -1,
            fga_delta: 0,
            fgm_delta: 0,
            tpa_delta: -1,
            tpm_delta: -1,
            ast_delta: 0,
            blocker_roster_idx: 0xFF,
            blk_delta: 0,
        },
    };
    boxs.apply_event(&ev_amend);
    assert_eq!(boxs.home.score_on_court, 2);
    assert_eq!(boxs.players[2].tpm, 0);
    assert_eq!(boxs.players[2].pts, 2);

    let line1 = render_event_zh(&hdr, &ev_shot);
    assert!(line1.contains("Q1"));
    assert!(line1.contains("命中 3 分球"));
    let line2 = render_event_zh(&hdr, &ev_amend);
    assert!(line2.contains("由 3 分改为 2 分"));
}
