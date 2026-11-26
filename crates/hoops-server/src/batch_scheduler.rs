use std::sync::Arc;

use hoops_core::{
    step_with_batch_ai, EngineCursor, GameRecordEventV1, MatchInitSpecV1, ReplayManifest,
    ReplayVerificationV1, SimulationEngine, StepStatus, VecSimSink,
};
use hoops_output::{
    encode_raw_stream, encode_soa_zstd_archive, render_event_zh, HReplayContainer, Hps1Header,
    LiveBoxScoreState, StreamVisibility,
};
use serde::{Deserialize, Serialize};

use crate::archive_store::{ArchiveStatus, ArchiveStore, MatchLifecycleState};
use crate::config::BATCH_POOL_HWM_BYTES;
use crate::error::ApiError;
use crate::live_actor::decode_raw_bundle_for_browser;
use crate::quota::{QuotaGuard, ServerQuotas};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BatchMode {
    FastBatch,
    FullTrace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResultRecord {
    pub sim_id: String,
    pub mode: BatchMode,
    pub header_pub: Option<Hps1Header>,
    pub header_auth: Option<Hps1Header>,
    pub boxscore: LiveBoxScoreState,
    pub events: Vec<(GameRecordEventV1, String)>,
    pub final_rolling_hash: String,
    pub total_ticks: u32,
    pub _quota_guard_bytes: usize,
}

pub struct BatchOutputWithGuard {
    pub record: BatchResultRecord,
    pub guard: QuotaGuard,
}

pub async fn run_batch_simulation(
    sim_id: String,
    mode: BatchMode,
    init_spec: MatchInitSpecV1,
    max_ticks: Option<u32>,
    quotas: Arc<ServerQuotas>,
    archive_store: Arc<ArchiveStore>,
) -> Result<BatchOutputWithGuard, ApiError> {
    if mode == BatchMode::FullTrace && quotas.batch_pool.used() >= BATCH_POOL_HWM_BYTES {
        return Err(ApiError::too_many(
            "BatchPoolBackpressure",
            "batch quota pool reached high-water mark (176 MiB)",
            2,
        ));
    }

    let permit = quotas
        .batch_semaphore
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| {
            ApiError::service_unavailable("BatchSemaphoreClosed", "shutting down", Some(1))
        })?;

    let est_bytes = if mode == BatchMode::FullTrace {
        2 * 1024 * 1024
    } else {
        128 * 1024
    };
    let guard = quotas.batch_pool.try_reserve(est_bytes).ok_or_else(|| {
        ApiError::insufficient_storage("ArtifactQuotaExhausted", "batch artifact quota exhausted")
    })?;

    let limit = max_ticks.unwrap_or(50_000).min(200_000) as usize;
    let sim_id_clone = sim_id.clone();

    let res = tokio::task::spawn_blocking(move || -> Result<BatchResultRecord, ApiError> {
        let mut sink = VecSimSink::default();
        let mut eng = SimulationEngine::new(&init_spec, &mut sink)
            .map_err(|e| ApiError::bad_request("EngineInitError", format!("{e:?}")))?;
        let mut cmds = Vec::new();
        for _ in 0..limit {
            if matches!(eng.state.cursor, EngineCursor::GameFinished) {
                break;
            }
            let st = step_with_batch_ai(&mut eng, &mut sink, &mut cmds);
            if matches!(st, StepStatus::Finished { .. } | StepStatus::Aborted { .. }) {
                break;
            }
        }

        let header_pub = Hps1Header::from_init_spec(&init_spec, StreamVisibility::PublicOnly);
        let mut boxscore = LiveBoxScoreState::from_init_spec(&init_spec);
        let mut ev_idx = 0usize;
        for f in &sink.public_frames {
            boxscore.on_frame(f);
            while ev_idx < sink.events.len() && sink.events[ev_idx].tick <= f.tick {
                boxscore.apply_event(&sink.events[ev_idx]);
                ev_idx += 1;
            }
        }
        while ev_idx < sink.events.len() {
            boxscore.apply_event(&sink.events[ev_idx]);
            ev_idx += 1;
        }

        let pf = &eng.last_public_frame;
        let final_rh = hex::encode(eng.last_hashes.2);
        let total_ticks = sink.public_frames.len() as u32;

        if mode == BatchMode::FastBatch {
            // FastBatch: ZERO frames/events/replay artifacts persisted
            archive_store.update_progress(
                &sim_id_clone,
                1,
                pf.score_home,
                pf.score_away,
                pf.quarter,
                pf.game_clock_ds,
                pf.game_phase,
                pf.tick,
                MatchLifecycleState::Finished,
            );
            return Ok(BatchResultRecord {
                sim_id: sim_id_clone,
                mode,
                header_pub: None,
                header_auth: None,
                boxscore,
                events: Vec::new(),
                final_rolling_hash: final_rh,
                total_ticks,
                _quota_guard_bytes: est_bytes,
            });
        }

        let header_auth =
            Hps1Header::from_init_spec(&init_spec, StreamVisibility::AuthoritativeFull);
        let events_with_pbp: Vec<_> = sink
            .events
            .iter()
            .map(|e| (e.clone(), render_event_zh(&header_pub, e)))
            .collect();

        let zstd_arc = encode_soa_zstd_archive(&header_pub, &sink.public_frames, &sink.events, 3)
            .map_err(|e| ApiError::internal(format!("encode archive: {e}")))?;

        let raw_dbg_bundle = decode_raw_bundle_for_browser(
            &header_auth,
            &sink.public_frames,
            Some(&sink.debug_frames),
            &sink.events,
        );

        let checkpoints: Vec<(u32, [u8; 32])> = sink
            .hashes
            .iter()
            .filter(|&&(t, _, _, _)| t % 100 == 0)
            .map(|&(t, sh, _, _)| (t, sh))
            .collect();

        let replay = HReplayContainer {
            manifest: ReplayManifest {
                init_spec,
                applied_commands: cmds,
                verification: if matches!(eng.state.cursor, EngineCursor::GameFinished) {
                    Some(ReplayVerificationV1 {
                        state_hash_checkpoints: checkpoints,
                        final_tick: eng.state.sim_tick,
                        final_state_hash: eng.last_hashes.0,
                    })
                } else {
                    None
                },
            },
            hash_trace: None,
        };
        let replay_bytes = replay.to_bytes();

        let art_uri = format!("{sim_id_clone}/epoch_1_gen_0.hframes.zst");
        let rep_uri = format!("{sim_id_clone}/epoch_1.hreplay");
        let dbg_uri = format!("{sim_id_clone}/epoch_1_gen_0.hdbg.bundle");
        archive_store
            .write_atomic_artifact(&art_uri, &zstd_arc)
            .map_err(|e| ApiError::insufficient_storage("ArtifactWriteFailed", e.to_string()))?;
        archive_store
            .write_atomic_artifact(&rep_uri, &replay_bytes)
            .map_err(|e| ApiError::insufficient_storage("ReplayWriteFailed", e.to_string()))?;
        archive_store
            .write_atomic_artifact(&dbg_uri, &raw_dbg_bundle)
            .map_err(|e| ApiError::insufficient_storage("DebugWriteFailed", e.to_string()))?;

        archive_store.update_progress(
            &sim_id_clone,
            1,
            pf.score_home,
            pf.score_away,
            pf.quarter,
            pf.game_clock_ds,
            pf.game_phase,
            pf.tick,
            MatchLifecycleState::Finished,
        );
        let _ =
            archive_store.cas_publish_complete(&sim_id_clone, 1, art_uri, rep_uri, Some(dbg_uri));

        // Free raw helper import warning cleanly
        let _ = encode_raw_stream;

        Ok(BatchResultRecord {
            sim_id: sim_id_clone,
            mode,
            header_pub: Some(header_pub),
            header_auth: Some(header_auth),
            boxscore,
            events: events_with_pbp,
            final_rolling_hash: final_rh,
            total_ticks,
            _quota_guard_bytes: est_bytes,
        })
    })
    .await
    .map_err(|e| ApiError::internal(format!("batch task panic: {e}")))??;

    drop(permit);
    Ok(BatchOutputWithGuard { record: res, guard })
}

pub fn _check_archive_complete_status(s: &ArchiveStatus) -> bool {
    matches!(s, ArchiveStatus::ArchiveComplete { .. })
}
