use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use hoops_core::{
    sha256_bytes, AnchoredCommand, ChallengeWindowState, CommandPayload, CommandPhase,
    CommandSide, DebugOverlayFrameV1, EngineCheckpointSnapshotV1, EngineCursor,
    GameRecordEventV1, IngressCommand, MatchInitSpecV1, PublicFrameV1, ReplayManifest,
    ReplayVerificationV1, SimulationEngine, StepStatus, SystemCommand, VecSimSink,
};
use hoops_output::{
    compute_whatif_causal_delta, decode_raw_stream, decode_soa_zstd_archive,
    decode_soa_zstd_archive_range, encode_soa_zstd_archive, project_match_analytics,
    render_event_zh, AnalyticsVisibility, HReplayContainer, Hps1Header, LiveBoxScoreState,
    MatchAnalyticsSummary, StreamVisibility, WhatIfCausalDelta,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::archive_store::{
    ArchiveStatus, ArchiveStore, MatchLifecycleState, SimulationDirectoryEntry, SimulationKind,
};
use crate::auth::{Claims, ExternalCoachCommand, ExternalCommandRequest, Role};
use crate::batch_scheduler::{BatchMode, BatchResultRecord};
use crate::config::BATCH_POOL_HWM_BYTES;
use crate::error::ApiError;
use crate::live_actor::decode_raw_bundle_for_browser;
use crate::routes::{AppState, StoredBatchEntry};
use crate::wal::now_wall_ms;

const MAX_OVERRIDE_COMMANDS: usize = 64;
const MAX_WHATIF_SEED_VARIANTS: u8 = 8;
const MAX_WHATIF_HORIZON_TICKS: u32 = 50_000;
const WHATIF_VARIANT_RESERVE_BYTES: usize = 512 * 1024;
const WHATIF_PERSIST_RESERVE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub struct WhatIfCommandSpec {
    #[serde(default)]
    pub apply_at_tick: Option<u32>,
    #[serde(default)]
    pub phase: Option<u8>,
    #[serde(flatten)]
    pub command: ExternalCoachCommand,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WhatIfRequest {
    pub branch_tick: u32,
    #[serde(default)]
    pub side: Option<CommandSide>,
    #[serde(default)]
    pub override_commands: Vec<WhatIfCommandSpec>,
    #[serde(default = "default_seed_variants")]
    pub seed_variants: u8,
    #[serde(default)]
    pub horizon_ticks: Option<u32>,
    #[serde(default)]
    pub retain_side_history_after_branch: bool,
    #[serde(default)]
    pub persist_counterfactual_replay: bool,
}

fn default_seed_variants() -> u8 {
    1
}

#[derive(Debug, Clone, Serialize)]
pub struct WhatIfCommandOutcome {
    pub index: usize,
    pub apply_at_tick: u32,
    pub phase: u8,
    pub accepted: bool,
    pub rejected_reason: Option<String>,
    pub anchored_tick: Option<u32>,
    pub ingress_seq: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WhatIfVariantSummary {
    pub variant_index: u8,
    pub seed: u64,
    pub final_tick: u32,
    pub score_home: u16,
    pub score_away: u16,
    pub margin_from_side: i16,
    pub baseline_paired_score: [u16; 2],
    pub baseline_paired_margin_from_side: i16,
    pub won: bool,
    pub causal_delta: WhatIfCausalDelta,
}

#[derive(Debug, Clone, Serialize)]
pub struct WhatIfResponse {
    pub sim_id: String,
    pub branch_tick: u32,
    pub side: CommandSide,
    pub seed_variants: u8,
    pub baseline_score: [u16; 2],
    pub baseline_margin_from_side: i16,
    pub whatif_mean_margin_from_side: f64,
    pub margin_ci95: [f64; 2],
    pub whatif_win_rate: f64,
    pub causal_delta: WhatIfCausalDelta,
    pub primary_causal_delta: WhatIfCausalDelta,
    pub command_outcomes: Vec<WhatIfCommandOutcome>,
    pub variants: Vec<WhatIfVariantSummary>,
    pub counterfactual_sim_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AnalyticsQuery {
    #[serde(default)]
    pub side: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnalyticsResponse {
    pub sim_id: String,
    pub visibility: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<CommandSide>,
    #[serde(flatten)]
    pub summary: MatchAnalyticsSummary,
}

struct PreparedOverrideCommand {
    index: usize,
    side: CommandSide,
    scheduled_tick: u32,
    phase: CommandPhase,
    raw_phase: u8,
    payload: hoops_core::CommandPayload,
}

struct VariantRunOutput {
    variant_index: u8,
    seed: u64,
    final_tick: u32,
    final_state_hash: [u8; 32],
    final_rolling_hash: [u8; 32],
    cursor_finished: bool,
    score_home: u16,
    score_away: u16,
    margin_from_side: i16,
    baseline_paired_score: [u16; 2],
    baseline_paired_margin_from_side: i16,
    causal_delta: WhatIfCausalDelta,
    command_outcomes: Vec<WhatIfCommandOutcome>,
    persisted_frames: Option<Vec<PublicFrameV1>>,
    persisted_debug_frames: Option<Vec<DebugOverlayFrameV1>>,
    persisted_events: Option<Vec<GameRecordEventV1>>,
    persisted_hashes: Option<Vec<(u32, [u8; 32], [u8; 32], [u8; 32])>>,
    persisted_commands: Option<Vec<AnchoredCommand>>,
}

struct SimulationRunResult {
    sink: VecSimSink,
    applied_commands: Vec<AnchoredCommand>,
    score_home: u16,
    score_away: u16,
    final_tick: u32,
    final_state_hash: [u8; 32],
    final_rolling_hash: [u8; 32],
    cursor_finished: bool,
    command_outcomes: Vec<WhatIfCommandOutcome>,
}

struct BaselineReplayOutput {
    init_spec: MatchInitSpecV1,
    applied_commands: Vec<AnchoredCommand>,
    base_sink: VecSimSink,
    branch_snapshot: EngineCheckpointSnapshotV1,
    branch_prefix_frames: usize,
    branch_prefix_debug: usize,
    branch_prefix_events: usize,
    branch_prefix_hashes: usize,
    branch_cmd_split_idx: usize,
    baseline_score: [u16; 2],
    baseline_analytics: MatchAnalyticsSummary,
}

pub async fn whatif_handler(
    State(state): State<AppState>,
    Path(sim_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<WhatIfRequest>,
) -> Result<Json<WhatIfResponse>, ApiError> {
    let claims = state.auth.extract_from_headers_or_query(&headers, None)?;
    check_sim_token_binding(&claims, &sim_id)?;
    if claims.role == Role::Spectator {
        return Err(ApiError::forbidden(
            "SpectatorCannotRunWhatIf",
            "spectators cannot run counterfactual what-if simulations",
        ));
    }

    let entry = state
        .archive_store
        .get_entry(&sim_id)
        .ok_or_else(|| ApiError::not_found("SimulationNotFound", format!("unknown {sim_id}")))?;

    if entry.kind == SimulationKind::BatchFast {
        return Err(ApiError::conflict(
            "ArtifactsDisabledInFastBatch",
            "FastBatch simulations have no replay artifacts for what-if branching",
        ));
    }
    if entry.lifecycle == MatchLifecycleState::Aborted {
        return Err(ApiError::gone(
            "MatchAborted",
            entry.abort_reason.unwrap_or_else(|| "aborted".to_owned()),
        ));
    }
    if entry.lifecycle != MatchLifecycleState::Finished {
        return Err(ApiError::forbidden(
            "WhatIfForbiddenBeforeMatchEnd",
            "what-if branching is strictly locked until match completion",
        ));
    }

    let side = resolve_whatif_side(&claims, &entry, req.side)?;
    validate_whatif_request(&req)?;

    let effective_branch_tick = if req.branch_tick == 0 {
        1
    } else {
        req.branch_tick
    };
    let prepared_overrides = prepare_override_commands(
        &req.override_commands,
        side,
        req.branch_tick,
        effective_branch_tick,
    )?;

    let replay_uri = resolve_replay_uri(&entry)?;
    let replay_bytes = state
        .archive_store
        .read_artifact(&replay_uri)
        .map_err(|e| ApiError::internal(format!("read replay: {e}")))?;
    let hreplay = HReplayContainer::from_bytes(&replay_bytes)
        .map_err(|e| ApiError::internal(format!("decode replay: {e}")))?;

    let orig_final_tick = hreplay
        .manifest
        .verification
        .as_ref()
        .map_or(entry.last_tick, |v| v.final_tick);
    if req.branch_tick > orig_final_tick {
        return Err(ApiError::bad_request(
            "BranchTickOutOfRange",
            format!(
                "branch_tick {} exceeds match final_tick {}",
                req.branch_tick, orig_final_tick
            ),
        ));
    }

    if state.quotas.batch_pool.used() >= BATCH_POOL_HWM_BYTES {
        return Err(ApiError::too_many(
            "BatchPoolBackpressure",
            "batch quota pool reached high-water mark (176 MiB)",
            2,
        ));
    }

    let _permit = state
        .quotas
        .batch_semaphore
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| {
            ApiError::service_unavailable("BatchSemaphoreClosed", "shutting down", Some(1))
        })?;

    let runtime_reserve = usize::from(req.seed_variants) * WHATIF_VARIANT_RESERVE_BYTES;
    let runtime_guard = state
        .quotas
        .batch_pool
        .try_reserve(runtime_reserve)
        .ok_or_else(|| {
            ApiError::insufficient_storage(
                "ArtifactQuotaExhausted",
                "batch artifact quota exhausted for what-if execution",
            )
        })?;

    let mut persist_guard = if req.persist_counterfactual_replay {
        Some(
            state
                .quotas
                .batch_pool
                .try_reserve(WHATIF_PERSIST_RESERVE_BYTES)
                .ok_or_else(|| {
                    ApiError::insufficient_storage(
                        "ArtifactQuotaExhausted",
                        "batch artifact quota exhausted for counterfactual replay persistence",
                    )
                })?,
        )
    } else {
        None
    };

    let branch_tick = req.branch_tick;
    let seed_variants = req.seed_variants;
    let horizon_cutoff = req
        .horizon_ticks
        .map(|h| h.min(MAX_WHATIF_HORIZON_TICKS));
    let retain_side_history = req.retain_side_history_after_branch;
    let persist_replay = req.persist_counterfactual_replay;
    let archive_store = Arc::clone(&state.archive_store);
    let orig_sim_id = sim_id.clone();
    let owner_sub = claims.sub.clone();
    let base_entry = entry.clone();

    let blocking_out = tokio::task::spawn_blocking(move || -> Result<(WhatIfResponse, Option<(String, BatchResultRecord)>), ApiError> {
        let baseline = replay_baseline_to_branch_and_finish(
            hreplay.manifest,
            branch_tick,
            horizon_cutoff,
        )?;

        let baseline_margin_from_side =
            margin_for_side(side, baseline.baseline_score[0], baseline.baseline_score[1]);
        let post_branch_cmds = &baseline.applied_commands[baseline.branch_cmd_split_idx..];

        let mut variant_outputs = Vec::with_capacity(usize::from(seed_variants));
        for v_idx in 0..seed_variants {
            let v_seed = if v_idx == 0 {
                baseline.init_spec.seed
            } else {
                derive_crn_seed(baseline.init_spec.seed, branch_tick, v_idx)
            };

            let out = execute_single_variant(
                &baseline,
                post_branch_cmds,
                side,
                v_idx,
                v_seed,
                horizon_cutoff,
                retain_side_history,
                &prepared_overrides,
                persist_replay && v_idx == 0,
            )?;
            variant_outputs.push(out);
        }

        let mut counterfactual_sim_id = None;
        let mut persisted_record = None;
        if persist_replay {
            if let Some(v0) = variant_outputs.first_mut() {
                let cf_id = format!("whatif_{}", Uuid::new_v4().simple());
                let rec = persist_variant_zero_artifacts(
                    &cf_id,
                    &owner_sub,
                    &base_entry,
                    &baseline.init_spec,
                    v0,
                    &archive_store,
                )?;
                counterfactual_sim_id = Some(cf_id.clone());
                persisted_record = Some((cf_id, rec));
            }
        }

        let resp = build_whatif_response(
            orig_sim_id,
            branch_tick,
            side,
            seed_variants,
            baseline.baseline_score,
            baseline_margin_from_side,
            variant_outputs,
            counterfactual_sim_id,
        );
        Ok((resp, persisted_record))
    })
    .await
    .map_err(|e| ApiError::internal(format!("whatif task panic: {e}")))??;

    let (resp, persisted_opt) = blocking_out;
    if let (Some((cf_id, rec)), Some(pg)) = (persisted_opt, persist_guard.take()) {
        state.batch_results.write().await.insert(
            cf_id,
            StoredBatchEntry {
                record: rec,
                _guard: pg,
            },
        );
    }
    drop(runtime_guard);
    drop(_permit);

    Ok(Json(resp))
}

pub async fn analytics_handler(
    State(state): State<AppState>,
    Path(sim_id): Path<String>,
    Query(q): Query<AnalyticsQuery>,
    headers: HeaderMap,
) -> Result<Json<AnalyticsResponse>, ApiError> {
    let claims = state
        .auth
        .extract_from_headers_or_query(&headers, q.token.as_deref())?;
    check_sim_token_binding(&claims, &sim_id)?;

    let entry = state
        .archive_store
        .get_entry(&sim_id)
        .ok_or_else(|| ApiError::not_found("SimulationNotFound", format!("unknown {sim_id}")))?;

    if entry.kind == SimulationKind::BatchFast {
        return Err(ApiError::conflict(
            "ArtifactsDisabledInFastBatch",
            "FastBatch simulations do not retain frame or analytics artifacts",
        ));
    }
    if entry.lifecycle == MatchLifecycleState::Aborted {
        return Err(ApiError::gone(
            "MatchAborted",
            entry.abort_reason.unwrap_or_else(|| "aborted".to_owned()),
        ));
    }
    if claims.role == Role::Spectator {
        return Err(ApiError::forbidden(
            "AnalyticsForbiddenForSpectator",
            "spectators cannot access match tactical analytics",
        ));
    }

    if entry.lifecycle != MatchLifecycleState::Finished {
        let own_side = match claims.role {
            Role::Home => {
                if entry.kind == SimulationKind::Live
                    && entry.home_participant_id != "*"
                    && entry.home_participant_id != claims.sub
                {
                    return Err(ApiError::forbidden(
                        "NotHomeParticipant",
                        "caller is not bound as Home coach",
                    ));
                }
                CommandSide::Home
            }
            Role::Away => {
                if entry.kind == SimulationKind::Live
                    && entry.away_participant_id != "*"
                    && entry.away_participant_id != claims.sub
                {
                    return Err(ApiError::forbidden(
                        "NotAwayParticipant",
                        "caller is not bound as Away coach",
                    ));
                }
                CommandSide::Away
            }
            Role::ServiceAdmin => match q.side.as_deref() {
                Some("away") => CommandSide::Away,
                _ => CommandSide::Home,
            },
            Role::Spectator => unreachable!(),
        };

        let handle = state
            .live_handles
            .read()
            .await
            .get(&sim_id)
            .cloned()
            .ok_or_else(|| {
                ApiError::service_unavailable(
                    "ArchiveFinalizingInProgress",
                    "live match actor unavailable",
                    Some(1),
                )
            })?;
        let snap = handle.query_state().await?;
        let init_spec = Arc::clone(&handle.init_spec);
        let archive_store = Arc::clone(&state.archive_store);

        let summary = tokio::task::spawn_blocking(move || {
            let mut all_frames = Vec::new();
            for (_, _, c_path) in &snap.sealed_chunk_paths {
                if let Ok(bytes) = archive_store.read_artifact(c_path) {
                    if let Ok(dec) = decode_soa_zstd_archive_range(&bytes, 0, u32::MAX) {
                        all_frames.extend(dec.frames);
                    }
                }
            }
            all_frames.extend(snap.tail_frames);
            all_frames.sort_by_key(|f| f.tick);
            all_frames.dedup_by_key(|f| f.tick);
            let events: Vec<GameRecordEventV1> = snap.events.into_iter().map(|(e, _)| e).collect();
            project_match_analytics(
                &init_spec,
                &all_frames,
                None,
                &events,
                AnalyticsVisibility::PublicLiveOwnSide(own_side),
            )
        })
        .await
        .map_err(|e| ApiError::internal(format!("live analytics task panic: {e}")))?;

        return Ok(Json(AnalyticsResponse {
            sim_id,
            visibility: "public_live_own_side",
            side: Some(own_side),
            summary,
        }));
    }

    let replay_uri = resolve_replay_uri(&entry)?;
    let archive_store = Arc::clone(&state.archive_store);
    let summary = tokio::task::spawn_blocking(move || -> Result<MatchAnalyticsSummary, ApiError> {
        let replay_bytes = archive_store
            .read_artifact(&replay_uri)
            .map_err(|e| ApiError::internal(format!("read replay: {e}")))?;
        let hreplay = HReplayContainer::from_bytes(&replay_bytes)
            .map_err(|e| ApiError::internal(format!("decode replay: {e}")))?;
        let init_spec = hreplay.manifest.init_spec;

        if let Some(ref dbg_uri) = entry.published_debug_uri {
            if let Ok(raw_bytes) = archive_store.read_artifact(dbg_uri) {
                if raw_bytes.len() >= 4 {
                    let decoded = if &raw_bytes[..4] == b"HRW1" {
                        let hdr =
                            Hps1Header::from_init_spec(&init_spec, StreamVisibility::AuthoritativeFull);
                        decode_raw_stream(
                            &raw_bytes,
                            hdr.roster_count,
                            hdr.play_dict.len() as u16,
                            hdr.def_dict.len() as u16,
                        )
                    } else {
                        let hlen = u32::from_le_bytes(raw_bytes[0..4].try_into().unwrap()) as usize;
                        if raw_bytes.len() >= 4 + hlen {
                            let hdr = Hps1Header::from_bytes(&raw_bytes[4..4 + hlen])
                                .map_err(|e| ApiError::internal(format!("decode hps1: {e}")))?;
                            decode_raw_stream(
                                &raw_bytes[4 + hlen..],
                                hdr.roster_count,
                                hdr.play_dict.len() as u16,
                                hdr.def_dict.len() as u16,
                            )
                        } else {
                            return Err(ApiError::internal("corrupt debug bundle header"));
                        }
                    };
                    if let Ok((frames, dbg_opt, events)) = decoded {
                        return Ok(project_match_analytics(
                            &init_spec,
                            &frames,
                            dbg_opt.as_deref(),
                            &events,
                            AnalyticsVisibility::PostGameFull,
                        ));
                    }
                }
            }
        }

        let art_uri = match &entry.archive_status {
            ArchiveStatus::ArchiveComplete {
                published_artifact_uri,
                ..
            } => Some(published_artifact_uri.clone()),
            _ => entry.published_artifact_uri.clone(),
        };
        if let Some(uri) = art_uri {
            let zstd_bytes = archive_store
                .read_artifact(&uri)
                .map_err(|e| ApiError::internal(format!("read frames archive: {e}")))?;
            let dec = decode_soa_zstd_archive(&zstd_bytes)
                .map_err(|e| ApiError::internal(format!("decode frames archive: {e}")))?;
            return Ok(project_match_analytics(
                &init_spec,
                &dec.frames,
                None,
                &dec.events,
                AnalyticsVisibility::PostGameFull,
            ));
        }

        Err(ApiError::service_unavailable(
            "ArchiveFinalizingInProgress",
            "post-game analytics artifacts finalizing",
            Some(1),
        ))
    })
    .await
    .map_err(|e| ApiError::internal(format!("postgame analytics task panic: {e}")))??;

    Ok(Json(AnalyticsResponse {
        sim_id,
        visibility: "post_game_full",
        side: None,
        summary,
    }))
}

fn check_sim_token_binding(claims: &Claims, sim_id: &str) -> Result<(), ApiError> {
    if let Some(ref bound_sim) = claims.sim_id {
        if bound_sim != sim_id {
            return Err(ApiError::forbidden(
                "SimTokenMismatch",
                "JWT sim_id does not match target match",
            ));
        }
    }
    Ok(())
}

fn resolve_whatif_side(
    claims: &Claims,
    entry: &SimulationDirectoryEntry,
    requested: Option<CommandSide>,
) -> Result<CommandSide, ApiError> {
    match claims.role {
        Role::Home => {
            if let Some(s) = requested {
                if s != CommandSide::Home {
                    return Err(ApiError::forbidden(
                        "SidePermissionDenied",
                        "Home role may only run what-if for Home side",
                    ));
                }
            }
            if entry.kind == SimulationKind::Live
                && entry.home_participant_id != "*"
                && entry.home_participant_id != claims.sub
            {
                return Err(ApiError::forbidden(
                    "NotHomeParticipant",
                    "caller is not bound as Home coach",
                ));
            }
            Ok(CommandSide::Home)
        }
        Role::Away => {
            if let Some(s) = requested {
                if s != CommandSide::Away {
                    return Err(ApiError::forbidden(
                        "SidePermissionDenied",
                        "Away role may only run what-if for Away side",
                    ));
                }
            }
            if entry.kind == SimulationKind::Live
                && entry.away_participant_id != "*"
                && entry.away_participant_id != claims.sub
            {
                return Err(ApiError::forbidden(
                    "NotAwayParticipant",
                    "caller is not bound as Away coach",
                ));
            }
            Ok(CommandSide::Away)
        }
        Role::ServiceAdmin => match requested {
            None | Some(CommandSide::Home) => Ok(CommandSide::Home),
            Some(CommandSide::Away) => Ok(CommandSide::Away),
            Some(CommandSide::System) => Err(ApiError::bad_request(
                "InvalidWhatIfSide",
                "what-if side must be home or away",
            )),
        },
        Role::Spectator => Err(ApiError::forbidden(
            "SpectatorCannotRunWhatIf",
            "spectators cannot run counterfactual what-if simulations",
        )),
    }
}

fn validate_whatif_request(req: &WhatIfRequest) -> Result<(), ApiError> {
    if !(1..=MAX_WHATIF_SEED_VARIANTS).contains(&req.seed_variants) {
        return Err(ApiError::bad_request(
            "InvalidSeedVariants",
            format!("seed_variants must be in 1..={MAX_WHATIF_SEED_VARIANTS}"),
        ));
    }
    if req.override_commands.len() > MAX_OVERRIDE_COMMANDS {
        return Err(ApiError::bad_request(
            "TooManyOverrideCommands",
            format!("override_commands cannot exceed {MAX_OVERRIDE_COMMANDS}"),
        ));
    }
    if let Some(h) = req.horizon_ticks {
        if h < req.branch_tick {
            return Err(ApiError::bad_request(
                "InvalidHorizonTicks",
                "horizon_ticks must be >= branch_tick",
            ));
        }
    }
    Ok(())
}

fn prepare_override_commands(
    specs: &[WhatIfCommandSpec],
    side: CommandSide,
    branch_tick: u32,
    effective_branch_tick: u32,
) -> Result<Vec<PreparedOverrideCommand>, ApiError> {
    let mut out = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        if let Some(t) = spec.apply_at_tick {
            if t < branch_tick {
                return Err(ApiError::bad_request(
                    "CommandTickBeforeBranch",
                    format!(
                        "override command {index} apply_at_tick {t} is before branch_tick {branch_tick}"
                    ),
                ));
            }
        }
        let ext = ExternalCommandRequest {
            client_request_id: 0,
            apply_at_tick: spec.apply_at_tick,
            phase: spec.phase,
            command: spec.command.clone(),
        };
        let ingress = ext.into_ingress_command(side)?;
        let raw_tick = spec.apply_at_tick.unwrap_or(branch_tick);
        let scheduled_tick = if branch_tick == 0 && raw_tick == 0 {
            effective_branch_tick
        } else {
            raw_tick
        };
        out.push(PreparedOverrideCommand {
            index,
            side,
            scheduled_tick,
            phase: ingress.phase,
            raw_phase: spec.phase.unwrap_or(0),
            payload: ingress.payload,
        });
    }
    Ok(out)
}

fn resolve_replay_uri(entry: &SimulationDirectoryEntry) -> Result<String, ApiError> {
    let uri_opt = match &entry.archive_status {
        ArchiveStatus::ArchiveComplete {
            published_replay_uri,
            ..
        } => Some(published_replay_uri.clone()),
        ArchiveStatus::ArchiveFinishedPendingBackfill { replay_uri, .. }
        | ArchiveStatus::ArchiveFinalizingRetry { replay_uri, .. } => Some(replay_uri.clone()),
        ArchiveStatus::ArchiveFailed { reason } => {
            return Err(ApiError::service_unavailable(
                "ArchiveFailed",
                format!("replay failed: {reason}"),
                None,
            ));
        }
        _ => entry.published_replay_uri.clone(),
    };
    uri_opt.ok_or_else(|| {
        ApiError::service_unavailable(
            "ReplayFinalizingInProgress",
            "replay artifact finalizing",
            Some(1),
        )
    })
}

fn replay_baseline_to_branch_and_finish(
    manifest: ReplayManifest,
    branch_tick: u32,
    horizon_cutoff: Option<u32>,
) -> Result<BaselineReplayOutput, ApiError> {
    let init_spec = manifest.init_spec;
    let applied_commands = manifest.applied_commands;
    let mut base_sink = VecSimSink::default();
    let mut eng = SimulationEngine::new(&init_spec, &mut base_sink)
        .map_err(|e| ApiError::internal(format!("baseline engine init: {e:?}")))?;

    let mut ci = 0usize;
    let mut branch_snapshot: Option<EngineCheckpointSnapshotV1> = None;
    let mut branch_prefix_frames = 0usize;
    let mut branch_prefix_debug = 0usize;
    let mut branch_prefix_events = 0usize;
    let mut branch_prefix_hashes = 0usize;
    let mut branch_cmd_split_idx = 0usize;

    if branch_tick == 0 {
        branch_snapshot = Some(eng.capture_checkpoint_snapshot());
        branch_prefix_frames = base_sink.public_frames.len();
        branch_prefix_debug = base_sink.debug_frames.len();
        branch_prefix_events = base_sink.events.len();
        branch_prefix_hashes = base_sink.hashes.len();
        branch_cmd_split_idx = 0;
    }

    let mut guard = 0usize;
    while !matches!(eng.state.cursor, EngineCursor::GameFinished) && guard < 220_000 {
        if branch_snapshot.is_none() && eng.state.sim_tick >= branch_tick {
            branch_snapshot = Some(eng.capture_checkpoint_snapshot());
            branch_prefix_frames = base_sink.public_frames.len();
            branch_prefix_debug = base_sink.debug_frames.len();
            branch_prefix_events = base_sink.events.len();
            branch_prefix_hashes = base_sink.hashes.len();
            branch_cmd_split_idx = ci;
        }
        if let Some(h) = horizon_cutoff {
            if eng.state.sim_tick > h && branch_snapshot.is_some() {
                break;
            }
        }
        guard += 1;

        while ci < applied_commands.len()
            && applied_commands[ci].resolved_tick == eng.state.sim_tick
            && applied_commands[ci].resolved_phase == CommandPhase::PreTick
        {
            eng.replay_anchored_command(applied_commands[ci], &mut base_sink)
                .map_err(|e| ApiError::internal(format!("replay baseline pretick cmd: {e:?}")))?;
            ci += 1;
        }
        let mut st = eng.step(&mut base_sink);
        while let StepStatus::AwaitingPostWhistle { .. } = st {
            while ci < applied_commands.len()
                && applied_commands[ci].resolved_tick == eng.state.sim_tick
                && applied_commands[ci].resolved_phase == CommandPhase::PostWhistle
            {
                eng.replay_anchored_command(applied_commands[ci], &mut base_sink)
                    .map_err(|e| {
                        ApiError::internal(format!("replay baseline postwhistle cmd: {e:?}"))
                    })?;
                ci += 1;
            }
            st = eng.step(&mut base_sink);
        }
        if matches!(st, StepStatus::Finished { .. } | StepStatus::Aborted { .. }) {
            break;
        }
    }

    let branch_snapshot = branch_snapshot.unwrap_or_else(|| {
        branch_prefix_frames = base_sink.public_frames.len();
        branch_prefix_debug = base_sink.debug_frames.len();
        branch_prefix_events = base_sink.events.len();
        branch_prefix_hashes = base_sink.hashes.len();
        branch_cmd_split_idx = ci;
        eng.capture_checkpoint_snapshot()
    });

    let baseline_score = extract_engine_scores(&eng);
    let baseline_analytics = project_match_analytics(
        &init_spec,
        &base_sink.public_frames,
        Some(&base_sink.debug_frames),
        &base_sink.events,
        AnalyticsVisibility::PostGameFull,
    );

    Ok(BaselineReplayOutput {
        init_spec,
        applied_commands,
        base_sink,
        branch_snapshot,
        branch_prefix_frames,
        branch_prefix_debug,
        branch_prefix_events,
        branch_prefix_hashes,
        branch_cmd_split_idx,
        baseline_score,
        baseline_analytics,
    })
}

#[allow(clippy::too_many_arguments)]
fn execute_single_variant(
    baseline: &BaselineReplayOutput,
    post_branch_cmds: &[AnchoredCommand],
    side: CommandSide,
    variant_index: u8,
    variant_seed: u64,
    horizon_cutoff: Option<u32>,
    retain_side_history: bool,
    prepared_overrides: &[PreparedOverrideCommand],
    keep_artifacts: bool,
) -> Result<VariantRunOutput, ApiError> {
    let mut variant_spec = baseline.init_spec.clone();
    variant_spec.seed = variant_seed;
    if !keep_artifacts {
        variant_spec.state_hash_mode = hoops_core::StateHashMode::Disabled;
    }

    let (paired_baseline_score, paired_baseline_analytics) = if variant_index == 0 {
        (baseline.baseline_score, None)
    } else {
        let paired_hist: Vec<AnchoredCommand> = post_branch_cmds
            .iter()
            .filter(|c| matches!(c.payload, CommandPayload::Player(_)))
            .copied()
            .collect();
        let paired_res = run_from_branch_checkpoint(
            &variant_spec,
            baseline.branch_snapshot.clone(),
            &paired_hist,
            &[],
            horizon_cutoff,
        )?;
        let (pf, df, ev) = stitch_prefix_and_suffix(baseline, &paired_res.sink);
        let analytics = project_match_analytics(
            &variant_spec,
            &pf,
            Some(&df),
            &ev,
            AnalyticsVisibility::PostGameFull,
        );
        (
            [paired_res.score_home, paired_res.score_away],
            Some(analytics),
        )
    };

    let cf_hist: Vec<AnchoredCommand> = post_branch_cmds
        .iter()
        .filter(|c| {
            matches!(c.payload, CommandPayload::Player(_))
                && (retain_side_history || c.side == side.opponent())
        })
        .copied()
        .collect();

    let cf_res = run_from_branch_checkpoint(
        &variant_spec,
        baseline.branch_snapshot.clone(),
        &cf_hist,
        prepared_overrides,
        horizon_cutoff,
    )?;

    let (full_frames, full_debug, full_events) = stitch_prefix_and_suffix(baseline, &cf_res.sink);
    let cf_analytics = project_match_analytics(
        &variant_spec,
        &full_frames,
        Some(&full_debug),
        &full_events,
        AnalyticsVisibility::PostGameFull,
    );

    let baseline_ref = paired_baseline_analytics
        .as_ref()
        .unwrap_or(&baseline.baseline_analytics);
    let causal_delta = compute_whatif_causal_delta(side, baseline_ref, &cf_analytics);

    let (persisted_frames, persisted_debug_frames, persisted_events, persisted_hashes, persisted_commands) =
        if keep_artifacts {
            let mut hashes = baseline.base_sink.hashes[..baseline.branch_prefix_hashes].to_vec();
            hashes.extend_from_slice(&cf_res.sink.hashes);
            let mut cmds = baseline.applied_commands[..baseline.branch_cmd_split_idx].to_vec();
            cmds.extend_from_slice(&cf_res.applied_commands);
            (
                Some(full_frames),
                Some(full_debug),
                Some(full_events),
                Some(hashes),
                Some(cmds),
            )
        } else {
            (None, None, None, None, None)
        };

    Ok(VariantRunOutput {
        variant_index,
        seed: variant_seed,
        final_tick: cf_res.final_tick,
        final_state_hash: cf_res.final_state_hash,
        final_rolling_hash: cf_res.final_rolling_hash,
        cursor_finished: cf_res.cursor_finished,
        score_home: cf_res.score_home,
        score_away: cf_res.score_away,
        margin_from_side: margin_for_side(side, cf_res.score_home, cf_res.score_away),
        baseline_paired_score: paired_baseline_score,
        baseline_paired_margin_from_side: margin_for_side(
            side,
            paired_baseline_score[0],
            paired_baseline_score[1],
        ),
        causal_delta,
        command_outcomes: cf_res.command_outcomes,
        persisted_frames,
        persisted_debug_frames,
        persisted_events,
        persisted_hashes,
        persisted_commands,
    })
}

fn stitch_prefix_and_suffix(
    baseline: &BaselineReplayOutput,
    suffix: &VecSimSink,
) -> (
    Vec<PublicFrameV1>,
    Vec<DebugOverlayFrameV1>,
    Vec<GameRecordEventV1>,
) {
    let mut full_frames =
        Vec::with_capacity(baseline.branch_prefix_frames + suffix.public_frames.len());
    full_frames.extend_from_slice(&baseline.base_sink.public_frames[..baseline.branch_prefix_frames]);
    full_frames.extend_from_slice(&suffix.public_frames);

    let mut full_debug =
        Vec::with_capacity(baseline.branch_prefix_debug + suffix.debug_frames.len());
    full_debug.extend_from_slice(&baseline.base_sink.debug_frames[..baseline.branch_prefix_debug]);
    full_debug.extend_from_slice(&suffix.debug_frames);

    let mut full_events =
        Vec::with_capacity(baseline.branch_prefix_events + suffix.events.len());
    full_events.extend_from_slice(&baseline.base_sink.events[..baseline.branch_prefix_events]);
    full_events.extend_from_slice(&suffix.events);

    (full_frames, full_debug, full_events)
}

fn run_from_branch_checkpoint(
    spec: &MatchInitSpecV1,
    snapshot: EngineCheckpointSnapshotV1,
    historical_cmds: &[AnchoredCommand],
    overrides: &[PreparedOverrideCommand],
    horizon_cutoff: Option<u32>,
) -> Result<SimulationRunResult, ApiError> {
    let mut sink = VecSimSink::default();
    let mut eng = SimulationEngine::from_checkpoint_snapshot(spec, snapshot, &mut sink)
        .map_err(|e| ApiError::internal(format!("branch checkpoint init: {e:?}")))?;

    let mut next_client_req_id = [
        eng.state.highest_client_request_id[0].saturating_add(1),
        eng.state.highest_client_request_id[1].saturating_add(1),
    ];
    let mut applied_commands = Vec::new();
    let mut hist_idx = 0usize;
    let mut override_outcomes: Vec<Option<WhatIfCommandOutcome>> = vec![None; overrides.len()];

    let max_steps = MAX_WHATIF_HORIZON_TICKS as usize;
    let mut steps = 0usize;
    let cutoff = horizon_cutoff.unwrap_or(MAX_WHATIF_HORIZON_TICKS);

    while !matches!(eng.state.cursor, EngineCursor::GameFinished) && steps < max_steps {
        if horizon_cutoff.is_some() && eng.state.sim_tick > cutoff {
            break;
        }
        steps += 1;
        let cur_tick = eng.state.sim_tick;

        while hist_idx < historical_cmds.len() && historical_cmds[hist_idx].resolved_tick < cur_tick
        {
            hist_idx += 1;
        }
        while hist_idx < historical_cmds.len()
            && historical_cmds[hist_idx].resolved_tick == cur_tick
            && historical_cmds[hist_idx].resolved_phase == CommandPhase::PreTick
        {
            try_enqueue_historical_cmd(
                &mut eng,
                historical_cmds[hist_idx],
                &mut next_client_req_id,
                &mut sink,
                &mut applied_commands,
            );
            hist_idx += 1;
        }

        for ov in overrides {
            if ov.scheduled_tick == cur_tick
                && ov.phase == CommandPhase::PreTick
                && override_outcomes[ov.index].is_none()
            {
                let outcome = enqueue_override_cmd(
                    &mut eng,
                    ov,
                    &mut next_client_req_id,
                    &mut sink,
                    &mut applied_commands,
                );
                override_outcomes[ov.index] = Some(outcome);
            }
        }

        let mut st = eng.step(&mut sink);
        while let StepStatus::AwaitingPostWhistle {
            ruling_id,
            window_state,
            ..
        } = st
        {
            while hist_idx < historical_cmds.len()
                && historical_cmds[hist_idx].resolved_tick == eng.state.sim_tick
                && historical_cmds[hist_idx].resolved_phase == CommandPhase::PostWhistle
            {
                try_enqueue_historical_cmd(
                    &mut eng,
                    historical_cmds[hist_idx],
                    &mut next_client_req_id,
                    &mut sink,
                    &mut applied_commands,
                );
                hist_idx += 1;
            }

            for ov in overrides {
                if ov.scheduled_tick == eng.state.sim_tick
                    && ov.phase == CommandPhase::PostWhistle
                    && override_outcomes[ov.index].is_none()
                {
                    let outcome = enqueue_override_cmd(
                        &mut eng,
                        ov,
                        &mut next_client_req_id,
                        &mut sink,
                        &mut applied_commands,
                    );
                    override_outcomes[ov.index] = Some(outcome);
                }
            }

            if let Some(ruling) = eng.state.active_pending_ruling.as_ref() {
                let sys_payload = match window_state {
                    ChallengeWindowState::PendingRefereeGrant { .. } => {
                        SystemCommand::ConfirmChallengeGrant {
                            ruling_id,
                            request_seq: ruling.request_seq,
                        }
                    }
                    _ => SystemCommand::CloseChallengeWindow {
                        ruling_id,
                        window_epoch: ruling.window_epoch,
                    },
                };
                let cmd = IngressCommand {
                    client_request_id: eng.state.next_ingress_seq,
                    apply_at_tick: None,
                    phase: CommandPhase::PostWhistle,
                    side: CommandSide::System,
                    payload: CommandPayload::System(sys_payload),
                };
                if let Ok(anchored) = eng.enqueue_command(cmd, &mut sink) {
                    applied_commands.push(anchored);
                }
            }
            st = eng.step(&mut sink);
        }

        if matches!(st, StepStatus::Finished { .. } | StepStatus::Aborted { .. }) {
            break;
        }
    }

    let command_outcomes: Vec<WhatIfCommandOutcome> = overrides
        .iter()
        .map(|ov| {
            override_outcomes[ov.index].clone().unwrap_or_else(|| {
                let reason = if ov.phase == CommandPhase::PostWhistle
                    && eng.state.sim_tick > ov.scheduled_tick
                {
                    "InvalidPhase".to_owned()
                } else {
                    "MatchEndedOrHorizonReached".to_owned()
                };
                WhatIfCommandOutcome {
                    index: ov.index,
                    apply_at_tick: ov.scheduled_tick,
                    phase: ov.raw_phase,
                    accepted: false,
                    rejected_reason: Some(reason),
                    anchored_tick: None,
                    ingress_seq: None,
                }
            })
        })
        .collect();

    let [score_home, score_away] = extract_engine_scores(&eng);
    let final_tick = sink
        .public_frames
        .last()
        .map_or(eng.state.sim_tick, |f| f.tick);
    let (final_state_hash, _, final_rolling_hash) = eng.last_hashes;
    let cursor_finished = matches!(eng.state.cursor, EngineCursor::GameFinished);

    Ok(SimulationRunResult {
        sink,
        applied_commands,
        score_home,
        score_away,
        final_tick,
        final_state_hash,
        final_rolling_hash,
        cursor_finished,
        command_outcomes,
    })
}

fn try_enqueue_historical_cmd(
    eng: &mut SimulationEngine,
    orig: AnchoredCommand,
    next_client_req_id: &mut [u64; 2],
    sink: &mut VecSimSink,
    applied_commands: &mut Vec<AnchoredCommand>,
) {
    let Some(team_idx) = orig.side.team_index() else {
        return;
    };
    let cid = next_client_req_id[team_idx];
    next_client_req_id[team_idx] = next_client_req_id[team_idx].saturating_add(1);
    let ingress = IngressCommand {
        client_request_id: cid,
        apply_at_tick: None,
        phase: orig.resolved_phase,
        side: orig.side,
        payload: orig.payload,
    };
    if let Ok(anchored) = eng.enqueue_command(ingress, sink) {
        applied_commands.push(anchored);
    }
}

fn enqueue_override_cmd(
    eng: &mut SimulationEngine,
    ov: &PreparedOverrideCommand,
    next_client_req_id: &mut [u64; 2],
    sink: &mut VecSimSink,
    applied_commands: &mut Vec<AnchoredCommand>,
) -> WhatIfCommandOutcome {
    let team = ov.side.team_index().unwrap();
    let client_request_id = next_client_req_id[team];
    next_client_req_id[team] = client_request_id.saturating_add(1);
    let mut outcome = WhatIfCommandOutcome {
        index: ov.index,
        apply_at_tick: ov.scheduled_tick,
        phase: ov.raw_phase,
        accepted: false,
        rejected_reason: None,
        anchored_tick: None,
        ingress_seq: None,
    };
    match eng.enqueue_command(IngressCommand {
        client_request_id,
        apply_at_tick: None,
        phase: ov.phase,
        side: ov.side,
        payload: ov.payload,
    }, sink) {
        Ok(anchored) => {
            applied_commands.push(anchored);
            outcome.accepted = true;
            outcome.anchored_tick = Some(anchored.resolved_tick);
            outcome.ingress_seq = Some(anchored.ingress_seq);
        }
        Err(error) => outcome.rejected_reason = Some(format!("{error:?}")),
    }
    outcome
}

fn extract_engine_scores(engine: &SimulationEngine) -> [u16; 2] {
    engine.state.team_states.each_ref().map(|team| team.score_on_court)
}

fn margin_for_side(side: CommandSide, home: u16, away: u16) -> i16 {
    let margin = if side == CommandSide::Away {
        i32::from(away) - i32::from(home)
    } else {
        i32::from(home) - i32::from(away)
    };
    margin.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

fn derive_crn_seed(seed: u64, branch_tick: u32, variant_index: u8) -> u64 {
    let mut bytes = [0; 13];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    bytes[8..12].copy_from_slice(&branch_tick.to_le_bytes());
    bytes[12] = variant_index;
    u64::from_le_bytes(sha256_bytes(&bytes)[..8].try_into().unwrap())
}

#[allow(clippy::too_many_arguments)]
fn build_whatif_response(
    sim_id: String,
    branch_tick: u32,
    side: CommandSide,
    seed_variants: u8,
    baseline_score: [u16; 2],
    baseline_margin_from_side: i16,
    outputs: Vec<VariantRunOutput>,
    counterfactual_sim_id: Option<String>,
) -> WhatIfResponse {
    let primary_causal_delta = outputs[0].causal_delta.clone();
    let command_outcomes = outputs[0].command_outcomes.clone();
    let n = f64::from(seed_variants);
    let mut margins: Vec<_> = outputs.iter().map(|o| f64::from(o.margin_from_side)).collect();
    let whatif_mean_margin_from_side = margins.iter().sum::<f64>() / n;
    let mut bootstrap_means = [0.0; 512];
    let bootstrap_seed = outputs[0].seed;
    for (sample, mean) in bootstrap_means.iter_mut().enumerate() {
        for draw in 0..seed_variants {
            let seed = derive_crn_seed(bootstrap_seed, sample as u32, draw);
            *mean += margins[(seed % u64::from(seed_variants)) as usize] / n;
        }
    }
    bootstrap_means.sort_unstable_by(f64::total_cmp);
    let margin_ci95 = [bootstrap_means[12], bootstrap_means[499]];
    let whatif_win_rate = margins.drain(..).filter(|&margin| margin > 0.0).count() as f64 / n;
    let mut causal_delta = WhatIfCausalDelta::default();
    let variants = outputs.into_iter().map(|o| {
        let d = &o.causal_delta;
        causal_delta.delta_margin_from_side += d.delta_margin_from_side / n;
        causal_delta.delta_ortg += d.delta_ortg / n;
        causal_delta.delta_drtg += d.delta_drtg / n;
        causal_delta.delta_efg_pct += d.delta_efg_pct / n;
        causal_delta.delta_tpa_share += d.delta_tpa_share / n;
        causal_delta.delta_tov_pct += d.delta_tov_pct / n;
        causal_delta.delta_orb_pct += d.delta_orb_pct / n;
        causal_delta.delta_crowding_ratio += d.delta_crowding_ratio / n;
        WhatIfVariantSummary {
            variant_index: o.variant_index,
            seed: o.seed,
            final_tick: o.final_tick,
            score_home: o.score_home,
            score_away: o.score_away,
            margin_from_side: o.margin_from_side,
            baseline_paired_score: o.baseline_paired_score,
            baseline_paired_margin_from_side: o.baseline_paired_margin_from_side,
            won: o.margin_from_side > 0,
            causal_delta: o.causal_delta,
        }
    }).collect();
    WhatIfResponse {
        sim_id, branch_tick, side, seed_variants, baseline_score, baseline_margin_from_side,
        whatif_mean_margin_from_side, margin_ci95, whatif_win_rate, causal_delta,
        primary_causal_delta, command_outcomes, variants, counterfactual_sim_id,
    }
}

fn persist_variant_zero_artifacts(
    sim_id: &str,
    owner_sub: &str,
    original: &SimulationDirectoryEntry,
    init_spec: &MatchInitSpecV1,
    variant: &mut VariantRunOutput,
    store: &ArchiveStore,
) -> Result<BatchResultRecord, ApiError> {
    if !variant.cursor_finished {
        return Err(ApiError::bad_request(
            "CounterfactualNotFinished", "replay persistence requires a completed simulation",
        ));
    }
    let frames = variant.persisted_frames.take().unwrap();
    let debug = variant.persisted_debug_frames.take().unwrap();
    let events = variant.persisted_events.take().unwrap();
    let mut boxscore = LiveBoxScoreState::from_init_spec(init_spec);
    let mut event_idx = 0;
    for frame in &frames {
        boxscore.on_frame(frame);
        while event_idx < events.len() && events[event_idx].tick <= frame.tick {
            boxscore.apply_event(&events[event_idx]);
            event_idx += 1;
        }
    }
    let final_frame = frames.last().ok_or_else(|| ApiError::internal("empty counterfactual projection"))?;
    let header_pub = Hps1Header::from_init_spec(init_spec, StreamVisibility::PublicOnly);
    let header_auth = Hps1Header::from_init_spec(init_spec, StreamVisibility::AuthoritativeFull);
    let archive = encode_soa_zstd_archive(&header_pub, &frames, &events, 3)
        .map_err(|e| ApiError::internal(format!("encode counterfactual archive: {e}")))?;
    let debug_bundle = decode_raw_bundle_for_browser(&header_auth, &frames, Some(&debug), &events);
    let replay = HReplayContainer {
        manifest: ReplayManifest {
            init_spec: init_spec.clone(),
            applied_commands: variant.persisted_commands.take().unwrap(),
            verification: Some(ReplayVerificationV1 {
                state_hash_checkpoints: variant.persisted_hashes.take().unwrap().into_iter()
                    .filter(|row| row.0 % 100 == 0).map(|row| (row.0, row.1)).collect(),
                final_tick: variant.final_tick,
                final_state_hash: variant.final_state_hash,
            }),
        },
        hash_trace: None,
    }.to_bytes();
    if replay.len() > hoops_output::MAX_HREPLAY_MANIFEST_BYTES {
        return Err(ApiError::insufficient_storage("ReplayTooLarge", "counterfactual replay exceeds 2 MiB"));
    }
    let archive_uri = format!("{sim_id}/epoch_1_gen_0.hframes.zst");
    let replay_uri = format!("{sim_id}/epoch_1.hreplay");
    let debug_uri = format!("{sim_id}/epoch_1_gen_0.hdbg.bundle");
    for (uri, bytes) in [(&archive_uri, &archive), (&replay_uri, &replay), (&debug_uri, &debug_bundle)] {
        store.write_atomic_artifact(uri, bytes)
            .map_err(|e| ApiError::insufficient_storage("ArtifactWriteFailed", e.to_string()))?;
    }
    let events = events.into_iter().map(|event| {
        let narration = render_event_zh(&header_pub, &event);
        (event, narration)
    }).collect();
    store.insert_entry(SimulationDirectoryEntry {
        sim_id: sim_id.to_owned(), kind: SimulationKind::BatchFullTrace,
        owner_user_id: owner_sub.to_owned(), home_participant_id: owner_sub.to_owned(),
        away_participant_id: owner_sub.to_owned(), rule_id: original.rule_id.clone(),
        home_team_id: original.home_team_id.clone(), away_team_id: original.away_team_id.clone(),
        pace_multiplier: 1, playback_paused: false, lease_epoch: 1,
        owner_node_id: original.owner_node_id.clone(), lifecycle: MatchLifecycleState::Finished,
        abort_reason: None,
        archive_status: ArchiveStatus::ArchiveComplete {
            published_artifact_uri: archive_uri.clone(), published_replay_uri: replay_uri.clone(),
        },
        published_artifact_uri: Some(archive_uri), published_replay_uri: Some(replay_uri),
        published_debug_uri: Some(debug_uri), score_home: final_frame.score_home,
        score_away: final_frame.score_away, quarter: final_frame.quarter,
        game_clock_ds: final_frame.game_clock_ds, game_phase: final_frame.game_phase,
        last_tick: final_frame.tick, created_wall_ms: now_wall_ms(),
    });
    Ok(BatchResultRecord {
        sim_id: sim_id.to_owned(), mode: BatchMode::FullTrace,
        header_pub: Some(header_pub), header_auth: Some(header_auth), boxscore, events,
        final_rolling_hash: hex::encode(variant.final_rolling_hash),
        total_ticks: frames.len() as u32, _quota_guard_bytes: WHATIF_PERSIST_RESERVE_BYTES,
    })
}
