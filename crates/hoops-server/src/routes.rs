use std::collections::BTreeMap;
use std::fs;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use hoops_config::ConfigBundle;
use hoops_core::{
    decode_shot_clock_opt, CanonicalLeBytes, CommandRejectReason, CommandSide, GameRecordEventV1,
    MatchInitSpecV1, StateHashMode,
};
use hoops_output::{
    decode_raw_stream, decode_soa_zstd_archive_range, encode_soa_zstd_archive, Hps1Header,
    LiveBoxScoreState,
};
use serde::Deserialize;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use uuid::Uuid;

use crate::archive_store::{
    ArchiveStatus, ArchiveStore, MatchLifecycleState, SimulationDirectoryEntry, SimulationKind,
};
use crate::auth::{Claims, ExternalCoachCommand, ExternalCommandRequest, JwtAuth, Role};
use crate::batch_scheduler::{run_batch_simulation, BatchMode, BatchResultRecord};
use crate::config::{
    ServerConfig, LIVE_POOL_ADMIT_LIMIT_BYTES, MAX_ACTIVE_LIVE_GAMES, MAX_TRACKED_WAL_SIM_SLOTS,
    MAX_WS_CONNECTIONS,
};
use crate::error::ApiError;
use crate::live_actor::{
    decode_raw_bundle_for_browser, spawn_live_match, LiveMatchHandle, SessionTransportEvent,
};
use crate::quota::{QuotaGuard, ServerQuotas};
use crate::wal::{now_wall_ms, SharedDurableWalStore};
use crate::ws::ws_handler;

pub struct StoredBatchEntry {
    pub record: BatchResultRecord,
    pub _guard: QuotaGuard,
}

#[derive(Clone)]
pub struct AppState {
    pub config: ServerConfig,
    pub bundle: Arc<ConfigBundle>,
    pub auth: JwtAuth,
    pub quotas: Arc<ServerQuotas>,
    pub wal: SharedDurableWalStore,
    pub archive_store: Arc<ArchiveStore>,
    pub live_handles: Arc<RwLock<BTreeMap<String, LiveMatchHandle>>>,
    pub batch_results: Arc<RwLock<BTreeMap<String, StoredBatchEntry>>>,
}

pub async fn build_state(cfg: ServerConfig) -> Result<AppState, Box<dyn std::error::Error>> {
    fs::create_dir_all(&cfg.data_dir)?;
    let bundle = Arc::new(ConfigBundle::load_from_dir(&cfg.configs_dir)?);
    let auth = JwtAuth::new(&cfg.jwt_secret);
    let quotas = Arc::new(ServerQuotas::default());
    let wal = SharedDurableWalStore::open(cfg.data_dir.join("wal"))?;
    let archive_store = Arc::new(ArchiveStore::new(cfg.data_dir.join("archives"))?);

    Ok(AppState {
        config: cfg,
        bundle,
        auth,
        quotas,
        wal,
        archive_store,
        live_handles: Arc::new(RwLock::new(BTreeMap::new())),
        batch_results: Arc::new(RwLock::new(BTreeMap::new())),
    })
}

pub fn build_router(state: AppState) -> Router {
    let viewer_dir = state.config.viewer_dir.clone();
    let api = Router::new()
        .route("/api/v1/health", get(health_handler))
        .route("/api/v1/catalog", get(catalog_handler))
        .route("/api/v1/auth/demo-token", post(demo_token_handler))
        .route(
            "/api/v1/simulations",
            get(list_simulations_handler).post(create_simulation_handler),
        )
        .route(
            "/api/v1/simulations/{sim_id}",
            get(get_simulation_handler).delete(delete_simulation_handler),
        )
        .route("/api/v1/simulations/{sim_id}/frames", get(frames_handler))
        .route("/api/v1/simulations/{sim_id}/replay", get(replay_handler))
        .route("/api/v1/simulations/{sim_id}/events", get(events_handler))
        .route(
            "/api/v1/simulations/{sim_id}/boxscore",
            get(boxscore_handler),
        )
        .route(
            "/api/v1/simulations/{sim_id}/commands",
            post(submit_command_handler),
        )
        .route(
            "/api/v1/simulations/{sim_id}/playback",
            post(set_playback_handler),
        )
        .route(
            "/api/v1/simulations/{sim_id}/forfeit",
            post(forfeit_handler),
        )
        .route(
            "/api/v1/simulations/{sim_id}/whatif",
            post(crate::whatif::whatif_handler),
        )
        .route(
            "/api/v1/simulations/{sim_id}/analytics",
            get(crate::whatif::analytics_handler),
        )
        .route("/api/v1/ws/simulations/{sim_id}", get(ws_handler))
        .with_state(state);

    let static_svc = ServeDir::new(viewer_dir).append_index_html_on_directories(true);
    api.fallback_service(static_svc)
        .layer(CorsLayer::permissive())
}

#[derive(Debug, Deserialize, Default)]
pub struct DemoTokenRequest {
    #[serde(default)]
    pub sub: Option<String>,
    #[serde(default)]
    pub role: Option<Role>,
    #[serde(default)]
    pub sim_id: Option<String>,
}

async fn demo_token_handler(
    State(state): State<AppState>,
    payload: Option<Json<DemoTokenRequest>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let req = payload.map(|j| j.0).unwrap_or_default();
    let role = req.role.unwrap_or(Role::Home);
    let sub = req.sub.unwrap_or_else(|| match role {
        Role::Home => "coach-home".to_owned(),
        Role::Away => "coach-away".to_owned(),
        Role::Spectator => "spectator-1".to_owned(),
        Role::ServiceAdmin => "admin-1".to_owned(),
    });
    let token = state
        .auth
        .issue_token(sub.clone(), role, req.sim_id.clone(), 86_400)?;
    Ok(Json(serde_json::json!({
        "token": token,
        "sub": sub,
        "role": role,
        "sim_id": req.sim_id,
    })))
}

async fn health_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let live_count = state.live_handles.read().await.len();
    let ws_count = state.quotas.ws_connections.load(Ordering::Relaxed);
    let (wal_seq, wal_batches, wal_lat_us) = state.wal.stats();
    Json(serde_json::json!({
        "ok": true,
        "bind": state.config.bind,
        "active_live_games": live_count,
        "max_live_games": MAX_ACTIVE_LIVE_GAMES,
        "active_ws_connections": ws_count,
        "max_ws_connections": MAX_WS_CONNECTIONS,
        "wal_seq": wal_seq,
        "wal_batches": wal_batches,
        "wal_last_latency_us": wal_lat_us,
        "pools": {
            "batch_used_bytes": state.quotas.batch_pool.used(),
            "batch_cap_bytes": state.quotas.batch_pool.cap(),
            "live_used_bytes": state.quotas.live_pool.used(),
            "live_cap_bytes": state.quotas.live_pool.cap(),
            "config_cache_used_bytes": state.quotas.config_cache_pool.used(),
            "phase_b_retain_used_bytes": state.quotas.phase_b_retain_pool.used(),
        }
    }))
}

async fn catalog_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let rules = vec![
        serde_json::json!({
            "rule_id": "nba",
            "name": "NBA 2023-24",
            "court_length_m": state.bundle.nba_ruleset.court.length_m,
            "court_width_m": state.bundle.nba_ruleset.court.width_m,
            "quarter_sec": state.bundle.nba_ruleset.clocks.quarter_sec,
            "foul_out_limit": state.bundle.nba_ruleset.fouls.foul_out_limit,
        }),
        serde_json::json!({
            "rule_id": "fiba",
            "name": "FIBA 2024",
            "court_length_m": state.bundle.fiba_ruleset.court.length_m,
            "court_width_m": state.bundle.fiba_ruleset.court.width_m,
            "quarter_sec": state.bundle.fiba_ruleset.clocks.quarter_sec,
            "foul_out_limit": state.bundle.fiba_ruleset.fouls.foul_out_limit,
        }),
    ];

    let teams: Vec<_> = state
        .bundle
        .teams
        .iter()
        .map(|t| {
            let players: Vec<_> = t
                .players
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    serde_json::json!({
                        "rank": i,
                        "player_id": p.player_id,
                        "display_name": p.display_name,
                        "jersey_number_code": p.jersey_number_code,
                        "height_cm": p.physical.height_cm,
                        "overall_ovr": (p.attributes.0.iter().map(|&v| v as u32).sum::<u32>() / 38),
                    })
                })
                .collect();
            serde_json::json!({
                "team_id": t.team_id,
                "pace": t.tactics.pace,
                "defensive_gameplan_id": t.tactics.defensive_gameplan_id.as_str(),
                "players": players,
            })
        })
        .collect();

    let playbooks: Vec<_> = state
        .bundle
        .playbooks
        .iter()
        .map(|p| {
            serde_json::json!({
                "play_id": p.play_id.as_str(),
                "phases": p.phases.len(),
                "min_shot_clock_ds": p.min_shot_clock_ds,
            })
        })
        .collect();

    let defenses: Vec<_> = state
        .bundle
        .defenses
        .iter()
        .map(|d| {
            serde_json::json!({
                "gameplan_id": d.gameplan_id.as_str(),
                "base_scheme": d.base_scheme,
                "pnr_rules": d.pnr_rules.len(),
            })
        })
        .collect();

    Json(serde_json::json!({
        "rules": rules,
        "teams": teams,
        "playbooks": playbooks,
        "defenses": defenses,
        "badges": state.bundle.badges.iter().map(|b| &b.badge_id).collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct CreateSimulationRequest {
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_rule")]
    pub rule_id: String,
    #[serde(default = "default_home")]
    pub home_team_id: String,
    #[serde(default = "default_away")]
    pub away_team_id: String,
    #[serde(default = "default_seed")]
    pub seed: u64,
    #[serde(default = "default_pace")]
    pub pace_multiplier: u8,
    #[serde(default)]
    pub home_participant_id: Option<String>,
    #[serde(default)]
    pub away_participant_id: Option<String>,
    #[serde(default)]
    pub max_ticks: Option<u32>,
    #[serde(default)]
    pub custom_init_spec_json: Option<MatchInitSpecV1>,
}

fn default_mode() -> String {
    "live".to_owned()
}
fn default_rule() -> String {
    "nba".to_owned()
}
fn default_home() -> String {
    "boston_five_out".to_owned()
}
fn default_away() -> String {
    "mile_high_hub".to_owned()
}
fn default_seed() -> u64 {
    42
}
fn default_pace() -> u8 {
    1
}

async fn create_simulation_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateSimulationRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let claims = state.auth.extract_from_headers_or_query(&headers, None)?;
    if claims.role == Role::Spectator {
        return Err(ApiError::forbidden(
            "SpectatorCannotCreateMatch",
            "spectators cannot create simulations",
        ));
    }

    let sim_id = format!("sim_{}", Uuid::new_v4().simple());
    let mut init_spec = if let Some(spec) = req.custom_init_spec_json {
        spec
    } else {
        let mode_hash = if req.mode == "fast_batch" {
            StateHashMode::Disabled
        } else {
            StateHashMode::PerTickRolling
        };
        state
            .bundle
            .build_match_init_spec(
                &req.rule_id,
                &req.home_team_id,
                &req.away_team_id,
                req.seed,
                mode_hash,
            )
            .map_err(|e| ApiError::bad_request("InvalidMatchConfig", e.to_string()))?
    };

    match req.mode.as_str() {
        "fast_batch" | "full_trace" => {
            let batch_mode = if req.mode == "fast_batch" {
                BatchMode::FastBatch
            } else {
                init_spec.state_hash_mode = StateHashMode::PerTickRolling;
                BatchMode::FullTrace
            };
            let kind = if batch_mode == BatchMode::FastBatch {
                SimulationKind::BatchFast
            } else {
                SimulationKind::BatchFullTrace
            };

            let pf_home = req.home_team_id.clone();
            let pf_away = req.away_team_id.clone();
            let entry = SimulationDirectoryEntry {
                sim_id: sim_id.clone(),
                kind,
                owner_user_id: claims.sub.clone(),
                home_participant_id: claims.sub.clone(),
                away_participant_id: "ai".to_owned(),
                rule_id: init_spec.ruleset.rule_id.clone(),
                home_team_id: init_spec.home_team.team_id.clone(),
                away_team_id: init_spec.away_team.team_id.clone(),
                pace_multiplier: 1,
                playback_paused: false,
                lease_epoch: 1,
                owner_node_id: "node-1".to_owned(),
                lifecycle: MatchLifecycleState::Running,
                abort_reason: None,
                archive_status: ArchiveStatus::ArchiveLiveHealthy,
                published_artifact_uri: None,
                published_replay_uri: None,
                published_debug_uri: None,
                score_home: 0,
                score_away: 0,
                quarter: 1,
                game_clock_ds: init_spec.ruleset.clocks.quarter_sec * 10,
                game_phase: 0,
                last_tick: 0,
                created_wall_ms: now_wall_ms(),
            };
            state.archive_store.insert_entry(entry);

            let out = run_batch_simulation(
                sim_id.clone(),
                batch_mode,
                init_spec,
                req.max_ticks,
                state.quotas.clone(),
                state.archive_store.clone(),
            )
            .await?;

            let summary_json = serde_json::json!({
                "sim_id": sim_id,
                "mode": req.mode,
                "home_team_id": pf_home,
                "away_team_id": pf_away,
                "total_ticks": out.record.total_ticks,
                "score_home": out.record.boxscore.home.score_official,
                "score_away": out.record.boxscore.away.score_official,
                "final_rolling_hash": out.record.final_rolling_hash,
                "directory": state.archive_store.get_entry(&sim_id),
            });

            state.batch_results.write().await.insert(
                sim_id,
                StoredBatchEntry {
                    record: out.record,
                    _guard: out.guard,
                },
            );
            Ok((StatusCode::CREATED, Json(summary_json)))
        }
        "live" => {
            if !matches!(req.pace_multiplier, 1 | 2 | 4 | 5) {
                return Err(ApiError::bad_request(
                    "InvalidPaceMultiplier",
                    "pace_multiplier must be one of {1, 2, 4, 5}",
                ));
            }
            if state.live_handles.read().await.len() >= MAX_ACTIVE_LIVE_GAMES
                || state.wal.active_slots_count() >= MAX_TRACKED_WAL_SIM_SLOTS
                || state.quotas.live_pool.used() > LIVE_POOL_ADMIT_LIMIT_BYTES
            {
                return Err(ApiError::too_many(
                    "LiveCapacityReached",
                    "max concurrent live games or live pool admission ceiling reached",
                    2,
                ));
            }
            init_spec.state_hash_mode = StateHashMode::PerTickRolling;

            let home_pid = req.home_participant_id.unwrap_or_else(|| "*".to_owned());
            let away_pid = req.away_participant_id.unwrap_or_else(|| "*".to_owned());

            let entry = SimulationDirectoryEntry {
                sim_id: sim_id.clone(),
                kind: SimulationKind::Live,
                owner_user_id: claims.sub.clone(),
                home_participant_id: home_pid.clone(),
                away_participant_id: away_pid.clone(),
                rule_id: init_spec.ruleset.rule_id.clone(),
                home_team_id: init_spec.home_team.team_id.clone(),
                away_team_id: init_spec.away_team.team_id.clone(),
                pace_multiplier: req.pace_multiplier,
                playback_paused: false,
                lease_epoch: 1,
                owner_node_id: "node-1".to_owned(),
                lifecycle: MatchLifecycleState::Running,
                abort_reason: None,
                archive_status: ArchiveStatus::ArchiveLiveHealthy,
                published_artifact_uri: None,
                published_replay_uri: None,
                published_debug_uri: None,
                score_home: 0,
                score_away: 0,
                quarter: 1,
                game_clock_ds: init_spec.ruleset.clocks.quarter_sec * 10,
                game_phase: 0,
                last_tick: 0,
                created_wall_ms: now_wall_ms(),
            };
            state.archive_store.insert_entry(entry);

            let (handle, _) = spawn_live_match(
                sim_id.clone(),
                init_spec,
                home_pid,
                away_pid,
                req.pace_multiplier,
                state.quotas.clone(),
                state.wal.clone(),
                state.archive_store.clone(),
            )
            .await?;

            let resp_entry = state.archive_store.get_entry(&sim_id);
            state
                .live_handles
                .write()
                .await
                .insert(sim_id.clone(), handle);
            Ok((
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "sim_id": sim_id,
                    "mode": "live",
                    "pace_multiplier": req.pace_multiplier,
                    "ws_url": format!("/api/v1/ws/simulations/{sim_id}"),
                    "directory": resp_entry,
                })),
            ))
        }
        other => Err(ApiError::bad_request(
            "InvalidSimulationMode",
            format!("unknown mode '{other}', expected live|fast_batch|full_trace"),
        )),
    }
}

async fn list_simulations_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<SimulationDirectoryEntry>>, ApiError> {
    let _claims = state.auth.extract_from_headers_or_query(&headers, None)?;
    Ok(Json(state.archive_store.list_entries()))
}

async fn get_simulation_handler(
    State(state): State<AppState>,
    Path(sim_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<SimulationDirectoryEntry>, ApiError> {
    let claims = state.auth.extract_from_headers_or_query(&headers, None)?;
    let mut entry = state
        .archive_store
        .get_entry(&sim_id)
        .ok_or_else(|| ApiError::not_found("SimulationNotFound", format!("unknown {sim_id}")))?;
    if entry.lifecycle == MatchLifecycleState::Aborted {
        return Err(ApiError::gone(
            "MatchAborted",
            entry.abort_reason.unwrap_or_else(|| "aborted".to_owned()),
        ));
    }
    // Spoiler protection for live PvP before GameOver
    if entry.kind == SimulationKind::Live
        && entry.lifecycle != MatchLifecycleState::Finished
        && claims.role != Role::ServiceAdmin
    {
        entry.published_replay_uri = None;
        entry.published_debug_uri = None;
    }
    Ok(Json(entry))
}

#[derive(Debug, Deserialize)]
struct FramesQuery {
    #[serde(default)]
    from_tick: Option<u32>,
    #[serde(default)]
    to_tick: Option<u32>,
    #[serde(default)]
    stream: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    token: Option<String>,
}

async fn frames_handler(
    State(state): State<AppState>,
    Path(sim_id): Path<String>,
    Query(q): Query<FramesQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let claims = state
        .auth
        .extract_from_headers_or_query(&headers, q.token.as_deref())?;
    let entry = state
        .archive_store
        .get_entry(&sim_id)
        .ok_or_else(|| ApiError::not_found("SimulationNotFound", format!("unknown {sim_id}")))?;

    if entry.kind == SimulationKind::BatchFast {
        return Err(ApiError::conflict(
            "ArtifactsDisabledInFastBatch",
            "FastBatch simulations do not write frame or replay artifacts",
        ));
    }
    if entry.lifecycle == MatchLifecycleState::Aborted {
        return Err(ApiError::gone(
            "MatchAborted",
            entry.abort_reason.unwrap_or_else(|| "aborted".to_owned()),
        ));
    }

    let want_debug = q.stream.as_deref() == Some("debug");
    if want_debug {
        if entry.lifecycle != MatchLifecycleState::Finished {
            return Err(ApiError::forbidden(
                "DebugStreamForbiddenBeforeMatchEnd",
                "debug frame stream is locked until GameOver",
            ));
        }
        if claims.role == Role::Spectator {
            return Err(ApiError::forbidden(
                "DebugStreamForbiddenForSpectator",
                "spectators cannot access AuthoritativeFull debug stream",
            ));
        }
    }

    let want_raw = q.format.as_deref() == Some("raw");

    if want_debug {
        let dbg_uri = entry.published_debug_uri.ok_or_else(|| {
            ApiError::service_unavailable(
                "ArchiveFinalizingInProgress",
                "debug stream not ready",
                Some(1),
            )
        })?;
        let raw_bytes = state
            .archive_store
            .read_artifact(&dbg_uri)
            .map_err(|e| ApiError::internal(format!("read debug artifact: {e}")))?;

        let (hdr_opt, frames, dbg_frames, events) = if &raw_bytes[..4] == b"HRW1" {
            let batch_rec = state.batch_results.read().await;
            let hdr = batch_rec
                .get(&sim_id)
                .and_then(|b| b.record.header_auth.clone());
            let mut hr = hdr.unwrap_or_else(|| {
                // fallback parse from live_handles if still registered
                Hps1Header {
                    format_version: 1,
                    stream_visibility: hoops_output::StreamVisibility::AuthoritativeFull,
                    roster_count: 30,
                    frame_size_bytes: 124,
                    debug_frame_size_bytes: 118,
                    court_length_mm: 28650,
                    court_width_mm: 15240,
                    home_team_id: entry.home_team_id.clone(),
                    away_team_id: entry.away_team_id.clone(),
                    roster: Vec::new(),
                    play_dict: Vec::new(),
                    def_dict: Vec::new(),
                }
            });
            if hr.roster.is_empty() {
                if let Some(h) = state.live_handles.read().await.get(&sim_id) {
                    hr = h.header_auth.clone();
                }
            }
            let (f, d, e) = decode_raw_stream(
                &raw_bytes,
                hr.roster_count,
                hr.play_dict.len() as u16,
                hr.def_dict.len() as u16,
            )
            .map_err(|e| ApiError::internal(format!("decode raw debug stream: {e}")))?;
            (Some(hr), f, d, e)
        } else {
            let hlen = u32::from_le_bytes(raw_bytes[0..4].try_into().unwrap()) as usize;
            let hdr = Hps1Header::from_bytes(&raw_bytes[4..4 + hlen])
                .map_err(|e| ApiError::internal(format!("decode hps1: {e}")))?;
            let (f, d, e) = decode_raw_stream(
                &raw_bytes[4 + hlen..],
                hdr.roster_count,
                hdr.play_dict.len() as u16,
                hdr.def_dict.len() as u16,
            )
            .map_err(|e| ApiError::internal(format!("decode raw stream: {e}")))?;
            (Some(hdr), f, d, e)
        };

        let hdr = hdr_opt.unwrap();
        let lo = q.from_tick.unwrap_or(0);
        let hi = q.to_tick.unwrap_or(u32::MAX);
        let mut f_sub = Vec::new();
        let mut d_sub = Vec::new();
        for (i, f) in frames.iter().enumerate() {
            if f.tick >= lo && f.tick <= hi {
                f_sub.push(*f);
                if let Some(ref dv) = dbg_frames {
                    d_sub.push(dv[i]);
                }
            }
        }
        if f_sub.is_empty() && !frames.is_empty() {
            return Err(ApiError::range_not_satisfiable(
                "from_tick beyond archive",
                1,
            ));
        }
        let e_sub: Vec<_> = events
            .into_iter()
            .filter(|e| e.tick >= lo && e.tick <= hi)
            .collect();
        let bytes = decode_raw_bundle_for_browser(&hdr, &f_sub, Some(&d_sub), &e_sub);
        return Ok(binary_response(
            if q.from_tick.is_some() || q.to_tick.is_some() {
                StatusCode::PARTIAL_CONTENT
            } else {
                StatusCode::OK
            },
            bytes,
        ));
    }

    match &entry.archive_status {
        ArchiveStatus::ArchiveFailed { reason } => Err(ApiError::service_unavailable(
            "ArchiveFailed",
            format!("archive failed: {reason}"),
            None,
        )),
        ArchiveStatus::ArchiveComplete {
            published_artifact_uri,
            ..
        } => {
            let zstd_bytes = state
                .archive_store
                .read_artifact(published_artifact_uri)
                .map_err(|e| ApiError::internal(format!("read archive: {e}")))?;
            if !want_raw && q.from_tick.is_none() && q.to_tick.is_none() {
                return Ok(binary_response(StatusCode::OK, zstd_bytes));
            }
            let dec = decode_soa_zstd_archive_range(
                &zstd_bytes,
                q.from_tick.unwrap_or(0),
                q.to_tick.unwrap_or(u32::MAX),
            )
            .map_err(|e| ApiError::internal(format!("decode zstd archive: {e}")))?;
            if dec.frames.is_empty() {
                return Err(ApiError::range_not_satisfiable(
                    "requested tick range not available",
                    1,
                ));
            }
            let out_bytes = if want_raw {
                decode_raw_bundle_for_browser(&dec.header, &dec.frames, None, &dec.events)
            } else {
                encode_soa_zstd_archive(&dec.header, &dec.frames, &dec.events, 3)
                    .map_err(|e| ApiError::internal(format!("encode slice: {e}")))?
            };
            let st = if q.from_tick.is_some() || q.to_tick.is_some() {
                StatusCode::PARTIAL_CONTENT
            } else {
                StatusCode::OK
            };
            Ok(binary_response(st, out_bytes))
        }
        _ => {
            // Live match running or finalizing: assemble from actor snapshot + sealed chunks
            if let Some(handle) = state.live_handles.read().await.get(&sim_id).cloned() {
                let snap = handle.query_state().await?;
                let lo = q.from_tick.unwrap_or(0);
                let hi = q.to_tick.unwrap_or(u32::MAX);
                let mut all_f = Vec::new();
                for (start, count, c_path) in &snap.sealed_chunk_paths {
                    if *start > hi || start.saturating_add(u32::from(*count).saturating_sub(1)) < lo
                    {
                        continue;
                    }
                    if let Ok(b) = state.archive_store.read_artifact(c_path) {
                        if let Ok(dec) = decode_soa_zstd_archive_range(&b, lo, hi) {
                            all_f.extend(dec.frames);
                        }
                    }
                }
                all_f.extend(snap.tail_frames);
                all_f.sort_by_key(|f| f.tick);
                all_f.dedup_by_key(|f| f.tick);

                let sliced_f: Vec<_> = all_f
                    .into_iter()
                    .filter(|f| f.tick >= lo && f.tick <= hi)
                    .collect();
                if sliced_f.is_empty() {
                    return Err(ApiError::range_not_satisfiable(
                        "requested tick range not yet simulated",
                        1,
                    ));
                }
                let sliced_e: Vec<_> = snap
                    .events
                    .into_iter()
                    .map(|(e, _)| e)
                    .filter(|e| e.tick >= lo && e.tick <= hi)
                    .collect();
                let bytes = if want_raw {
                    decode_raw_bundle_for_browser(&snap.header_pub, &sliced_f, None, &sliced_e)
                } else {
                    encode_soa_zstd_archive(&snap.header_pub, &sliced_f, &sliced_e, 3)
                        .map_err(|e| ApiError::internal(format!("encode zstd: {e}")))?
                };
                return Ok(binary_response(StatusCode::PARTIAL_CONTENT, bytes));
            }
            Err(ApiError::service_unavailable(
                "ArchiveFinalizingInProgress",
                "frames finalizing",
                Some(1),
            ))
        }
    }
}

async fn replay_handler(
    State(state): State<AppState>,
    Path(sim_id): Path<String>,
    Query(q): Query<FramesQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let claims = state
        .auth
        .extract_from_headers_or_query(&headers, q.token.as_deref())?;
    let entry = state
        .archive_store
        .get_entry(&sim_id)
        .ok_or_else(|| ApiError::not_found("SimulationNotFound", format!("unknown {sim_id}")))?;

    if entry.kind == SimulationKind::BatchFast {
        return Err(ApiError::conflict(
            "ArtifactsDisabledInFastBatch",
            "FastBatch simulations have no .hreplay artifact",
        ));
    }
    if entry.lifecycle == MatchLifecycleState::Aborted {
        return Err(ApiError::gone(
            "MatchAborted",
            entry.abort_reason.unwrap_or_else(|| "aborted".to_owned()),
        ));
    }
    if entry.kind == SimulationKind::Live
        && entry.lifecycle != MatchLifecycleState::Finished
        && claims.role != Role::ServiceAdmin
    {
        return Err(ApiError::forbidden(
            "MatchNotFinished",
            "PvP replay is locked until GameOver",
        ));
    }

    let replay_uri = match &entry.archive_status {
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
        _ => None,
    };

    let Some(uri) = replay_uri else {
        return Err(ApiError::service_unavailable(
            "ReplayFinalizingInProgress",
            "replay artifact finalizing",
            Some(1),
        ));
    };
    let bytes = state
        .archive_store
        .read_artifact(&uri)
        .map_err(|e| ApiError::internal(format!("read replay: {e}")))?;
    Ok(binary_response(StatusCode::OK, bytes))
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    #[serde(default)]
    after_event_seq: u64,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    token: Option<String>,
}

async fn events_handler(
    State(state): State<AppState>,
    Path(sim_id): Path<String>,
    Query(q): Query<EventsQuery>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let _claims = state
        .auth
        .extract_from_headers_or_query(&headers, q.token.as_deref())?;
    let entry = state
        .archive_store
        .get_entry(&sim_id)
        .ok_or_else(|| ApiError::not_found("SimulationNotFound", format!("unknown {sim_id}")))?;
    if entry.kind == SimulationKind::BatchFast {
        return Err(ApiError::conflict(
            "ArtifactsDisabledInFastBatch",
            "FastBatch simulations do not retain event logs",
        ));
    }

    let items: Vec<(GameRecordEventV1, String)> =
        if let Some(handle) = state.live_handles.read().await.get(&sim_id).cloned() {
            handle.query_state().await?.events
        } else if let Some(b) = state.batch_results.read().await.get(&sim_id) {
            b.record.events.clone()
        } else {
            Vec::new()
        };

    let start = items.partition_point(|(ev, _)| ev.event_seq <= q.after_event_seq);
    let start = q.limit.map_or(start, |limit| {
        start.max(items.len().saturating_sub(limit.min(1_000)))
    });
    let filtered: Vec<_> = items
        .into_iter()
        .skip(start)
        .map(|(ev, pbp_zh)| {
            serde_json::json!({
                "event_seq": ev.event_seq,
                "tick": ev.tick,
                "quarter": ev.quarter,
                "game_clock_ds": ev.game_clock_ds,
                "shot_clock_ds": ev.shot_clock_ds,
                "event": ev,
                "pbp_zh": pbp_zh,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "sim_id": sim_id,
        "events": filtered,
    })))
}

async fn boxscore_handler(
    State(state): State<AppState>,
    Path(sim_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<LiveBoxScoreState>, ApiError> {
    let _claims = state.auth.extract_from_headers_or_query(&headers, None)?;
    if let Some(handle) = state.live_handles.read().await.get(&sim_id).cloned() {
        return Ok(Json(handle.query_state().await?.boxscore));
    }
    if let Some(b) = state.batch_results.read().await.get(&sim_id) {
        return Ok(Json(b.record.boxscore.clone()));
    }
    Err(ApiError::not_found(
        "SimulationNotFound",
        format!("unknown simulation {sim_id}"),
    ))
}

async fn submit_command_handler(
    State(state): State<AppState>,
    Path(sim_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<ExternalCommandRequest>,
) -> Result<(StatusCode, Json<SessionTransportEvent>), ApiError> {
    let claims = state.auth.extract_from_headers_or_query(&headers, None)?;
    let entry = state
        .archive_store
        .get_entry(&sim_id)
        .ok_or_else(|| ApiError::not_found("SimulationNotFound", format!("unknown {sim_id}")))?;
    if entry.kind != SimulationKind::Live {
        return Err(ApiError::forbidden(
            "CommandsOnlyInLiveMode",
            "Batch simulations do not accept runtime coach commands",
        ));
    }
    let side = check_participant_side(&claims, &entry)?;
    let handle = state
        .live_handles
        .read()
        .await
        .get(&sim_id)
        .cloned()
        .ok_or_else(|| ApiError::gone("MatchActorStopped", "live match actor not running"))?;

    let ingress = req.into_ingress_command(side)?;
    let reply = handle.submit_command(ingress).await?;
    let status = match &reply {
        SessionTransportEvent::CommandAccepted { .. } => StatusCode::OK,
        SessionTransportEvent::IngressCommandRejected { reason_code, .. } => {
            if *reason_code == CommandRejectReason::MatchAlreadyEnded as u16
                || *reason_code == CommandRejectReason::IdempotencyPayloadMismatch as u16
            {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            }
        }
        _ => StatusCode::OK,
    };
    Ok((status, Json(reply)))
}

#[derive(Debug, Deserialize)]
struct PlaybackRequest {
    paused: bool,
}

async fn set_playback_handler(
    State(state): State<AppState>,
    Path(sim_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<PlaybackRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let claims = state.auth.extract_from_headers_or_query(&headers, None)?;
    let entry = state
        .archive_store
        .get_entry(&sim_id)
        .ok_or_else(|| ApiError::not_found("SimulationNotFound", format!("unknown {sim_id}")))?;
    check_participant_side(&claims, &entry)?;
    if claims.sub != entry.owner_user_id {
        return Err(ApiError::forbidden(
            "NotSimulationOwner",
            "only the match owner may pause or resume live simulation",
        ));
    }
    if entry.kind != SimulationKind::Live || entry.lifecycle != MatchLifecycleState::Running {
        return Err(ApiError::conflict(
            "LivePlaybackUnavailable",
            "playback control requires a running live simulation",
        ));
    }
    let handle = state
        .live_handles
        .read()
        .await
        .get(&sim_id)
        .cloned()
        .ok_or_else(|| ApiError::gone("MatchActorStopped", "simulation actor has terminated"))?;
    let (paused, tick) = handle.set_playback_paused(req.paused).await?;
    Ok(Json(serde_json::json!({"paused": paused, "tick": tick})))
}

#[derive(Debug, Deserialize)]
struct ForfeitRequest {
    client_request_id: u64,
}

async fn forfeit_handler(
    State(state): State<AppState>,
    Path(sim_id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<ForfeitRequest>,
) -> Result<Json<SessionTransportEvent>, ApiError> {
    let claims = state.auth.extract_from_headers_or_query(&headers, None)?;
    let entry = state
        .archive_store
        .get_entry(&sim_id)
        .ok_or_else(|| ApiError::not_found("SimulationNotFound", format!("unknown {sim_id}")))?;
    if entry.kind != SimulationKind::Live {
        return Err(ApiError::forbidden(
            "ForfeitOnlyForLivePvp",
            "forfeit is only valid for Live PvP matches",
        ));
    }
    let side = check_participant_side(&claims, &entry)?;
    if entry.lifecycle == MatchLifecycleState::Finished {
        // Still pass through actor so AlreadyAccepted idempotency returns cleanly, else 409
    }
    let handle = state
        .live_handles
        .read()
        .await
        .get(&sim_id)
        .cloned()
        .ok_or_else(|| ApiError::conflict("MatchAlreadyEnded", "match already ended"))?;

    let ext = ExternalCommandRequest {
        client_request_id: req.client_request_id,
        apply_at_tick: None,
        phase: Some(0),
        command: ExternalCoachCommand::ForfeitMatch,
    };
    let ingress = ext.into_ingress_command(side)?;
    let reply = handle.forfeit(ingress).await?;
    match &reply {
        SessionTransportEvent::CommandAccepted { .. } => Ok(Json(reply)),
        SessionTransportEvent::IngressCommandRejected {
            reason_code,
            reason,
            ..
        } => {
            if *reason_code == CommandRejectReason::MatchAlreadyEnded as u16
                || *reason_code == CommandRejectReason::IdempotencyPayloadMismatch as u16
            {
                Err(ApiError::conflict(
                    "MatchAlreadyEndedOrMismatch",
                    reason.clone(),
                ))
            } else {
                Err(ApiError::bad_request("ForfeitRejected", reason.clone()))
            }
        }
        _ => Ok(Json(reply)),
    }
}

async fn delete_simulation_handler(
    State(state): State<AppState>,
    Path(sim_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let claims = state.auth.extract_from_headers_or_query(&headers, None)?;
    let entry = state
        .archive_store
        .get_entry(&sim_id)
        .ok_or_else(|| ApiError::not_found("SimulationNotFound", format!("unknown {sim_id}")))?;

    if entry.kind == SimulationKind::Live && claims.role != Role::ServiceAdmin {
        return Err(ApiError::forbidden(
            "UseForfeitForLivePvp",
            "Live PvP participants cannot delete the match; use POST /forfeit instead",
        ));
    }

    if entry.lifecycle != MatchLifecycleState::Running && claims.role != Role::ServiceAdmin {
        return Err(ApiError::conflict(
            "SimulationAlreadyTerminated",
            "cannot cancel an already finished simulation",
        ));
    }

    if claims.role != Role::ServiceAdmin && claims.sub != entry.owner_user_id {
        return Err(ApiError::forbidden(
            "NotSimulationOwner",
            "only the Batch initiator or ServiceAdmin may delete",
        ));
    }

    if let Some(handle) = state.live_handles.write().await.remove(&sim_id) {
        let _ = handle
            .ctrl_tx
            .send(crate::live_actor::ActorControlMsg::FenceLoseLease)
            .await;
    }
    state.batch_results.write().await.remove(&sim_id);

    Ok(Json(serde_json::json!({
        "deleted": true,
        "sim_id": sim_id,
    })))
}

fn check_participant_side(
    claims: &Claims,
    entry: &SimulationDirectoryEntry,
) -> Result<CommandSide, ApiError> {
    if let Some(ref b) = claims.sim_id {
        if b != &entry.sim_id {
            return Err(ApiError::forbidden(
                "SimTokenMismatch",
                "JWT sim_id does not match target match",
            ));
        }
    }
    match claims.role {
        Role::Home => {
            if entry.home_participant_id == "*" || entry.home_participant_id == claims.sub {
                Ok(CommandSide::Home)
            } else {
                Err(ApiError::forbidden(
                    "NotHomeParticipant",
                    "caller is not bound as Home coach",
                ))
            }
        }
        Role::Away => {
            if entry.away_participant_id == "*" || entry.away_participant_id == claims.sub {
                Ok(CommandSide::Away)
            } else {
                Err(ApiError::forbidden(
                    "NotAwayParticipant",
                    "caller is not bound as Away coach",
                ))
            }
        }
        _ => Err(ApiError::forbidden(
            "RoleNotParticipant",
            "only Home/Away match participants may execute this action",
        )),
    }
}

fn binary_response(status: StatusCode, bytes: Vec<u8>) -> Response {
    let mut resp = Response::new(Body::from(bytes));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    resp
}

pub fn _unused_helpers_routes(sc: u16, spec: &MatchInitSpecV1) -> bool {
    decode_shot_clock_opt(sc).is_ok() && !spec.to_canonical_le_bytes().is_empty()
}
