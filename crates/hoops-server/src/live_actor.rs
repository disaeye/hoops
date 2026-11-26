use std::collections::BTreeMap;
use std::io::{self, BufWriter, Read, Write};
use std::sync::Arc;
use std::time::Duration;

use hoops_core::{
    decode_shot_clock_opt, AnchoredCommand, CanonicalReader, CommandPhase, CommandRejectReason,
    CommandSide, DebugOverlayFrameV1, EngineCursor, GameRecordEventV1, IngressCommand,
    MatchInitSpecV1, PreparedOutcome, PublicFrameV1, ReplayManifest, ReplayVerificationV1,
    SimulationEngine, StepStatus, SystemCommand, VecSimSink, DEBUG_FRAME_BYTES,
};
use hoops_output::{
    decode_soa_zstd_archive_range, encode_raw_stream, encode_soa_zstd_archive, render_event_zh,
    write_raw_stream_header, HReplayContainer, Hps1Header, LiveBoxScoreState, SoaArchiveAssembler,
    StreamVisibility,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::archive_store::{ArchiveStatus, ArchiveStore, FinalizeOriginPath, MatchLifecycleState};
use crate::config::{
    FINALIZE_FLUSH_TIMEOUT_MS, LIVE_EVENT_LOG_BUF_BYTES, LIVE_PLAYER_CMD_BUF_BYTES,
    LIVE_SYSTEM_CMD_RESERVE_BYTES,
};
use crate::error::ApiError;
use crate::quota::{QuotaGuard, ServerQuotas};
use crate::wal::{
    now_wall_ms, LiveServiceCheckpointV1, LiveSessionConfigV1, SealedChunkMetaV1,
    SealedCmdSegMetaV1, ServiceTimerKind, ServiceTimerState, SharedDurableWalStore,
    StorageFaultDomain, WalError, WalRecord, WalWriteClass,
};

pub const CHUNK_FRAMES: usize = 600;
pub const MAX_LIVE_SIM_TICKS: u32 = 200_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeWindowWallClock {
    pub ruling_id: u64,
    pub window_epoch: u32,
    pub window_state: u8,
    pub request_seq: u32,
    pub grant_requester_side: u8,
    pub eligible_sides_mask: u8,
    pub challenge_deadline_wall_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoxScoreBaselineSnapshotV1 {
    pub as_of_frame_tick: u32,
    pub as_of_event_seq: u64,
    pub boxscore: LiveBoxScoreState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionTransportEvent {
    SessionSync {
        sim_id: String,
        lease_epoch: u64,
        pace_multiplier: u8,
        game_phase: u8,
        quarter: u8,
        game_clock_ds: u16,
        shot_clock_ds: u16,
        score_home: u16,
        score_away: u16,
        last_published_frame_tick: u32,
        last_published_event_seq: u64,
        boxscore_snapshot: Option<BoxScoreBaselineSnapshotV1>,
        active_challenge_window: Option<ChallengeWindowWallClock>,
        playback_paused: bool,
    },
    PlaybackChanged {
        paused: bool,
        tick: u32,
    },
    ChallengeWindowWallClock(ChallengeWindowWallClock),
    ChallengeWindowClosed,
    CommandAccepted {
        client_request_id: u64,
        anchored: AnchoredCommand,
    },
    IngressCommandRejected {
        client_request_id: u64,
        reason_code: u16,
        reason: String,
    },
    CatchUpComplete {
        from_tick: u32,
        resumed_at_tick: u32,
        after_event_seq: u64,
    },
    HistoricalFramesUnavailable {
        missing_frame_ranges: Vec<(u32, u32)>,
    },
    Heartbeat {
        server_wall_ms: u64,
    },
}

#[derive(Debug, Clone)]
pub enum LiveStreamItem {
    PublicFrame(PublicFrameV1),
    Event {
        event: GameRecordEventV1,
        pbp_zh: String,
    },
    Transport(SessionTransportEvent),
    Close {
        code: u16,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SealingFlushProgress {
    pub max_sealed_disk_tick: u32,
    pub max_sealing_inflight_tick: u32,
    pub has_inflight_sealing: bool,
    pub storage_fault: u8,
}

#[derive(Debug)]
pub struct CatchUpPhaseBSnapshot {
    pub sim_id: String,
    pub lease_epoch: u64,
    pub pace_multiplier: u8,
    pub header_pub: Hps1Header,
    pub tail_frames_arc: Arc<Vec<PublicFrameV1>>,
    pub tail_events_arc: Arc<Vec<(GameRecordEventV1, String)>>,
    pub sealed_chunk_paths: Vec<(u32, u16, String)>,
    pub boxscore_snapshot: Option<BoxScoreBaselineSnapshotV1>,
    pub active_window: Option<ChallengeWindowWallClock>,
    pub playback_paused: bool,
    pub missing_frame_ranges: Vec<(u32, u32)>,
    pub last_published_frame_tick: u32,
    pub last_published_event_seq: u64,
    pub game_phase: u8,
    pub quarter: u8,
    pub game_clock_ds: u16,
    pub shot_clock_ds: u16,
    pub score_home: u16,
    pub score_away: u16,
    pub _retain_guard: Option<QuotaGuard>,
    pub rx: broadcast::Receiver<LiveStreamItem>,
}

pub enum CatchUpHandoffOutcome {
    Ready(CatchUpPhaseBSnapshot),
    AwaitSealingFlush(watch::Receiver<SealingFlushProgress>),
    RetryPhaseA,
    BudgetExhausted(String),
}

pub enum ActorControlMsg {
    SetPlaybackPaused {
        paused: bool,
        reply: oneshot::Sender<Result<(bool, u32), ApiError>>,
    },
    SubmitCommand {
        ingress: IngressCommand,
        reply: oneshot::Sender<SessionTransportEvent>,
    },
    Forfeit {
        ingress: IngressCommand,
        reply: oneshot::Sender<SessionTransportEvent>,
    },
    TransitionPhaseAToB {
        from_tick: u32,
        after_event_seq: u64,
        sealed_covered_through_tick: Option<u32>,
        reply: oneshot::Sender<CatchUpHandoffOutcome>,
    },
    GetSnapshotState {
        reply: oneshot::Sender<LiveSnapshotQuery>,
    },
    FenceLoseLease,
}

#[derive(Debug, Clone)]
pub struct LiveSnapshotQuery {
    pub header_pub: Hps1Header,
    pub header_auth: Hps1Header,
    pub boxscore: LiveBoxScoreState,
    pub events: Vec<(GameRecordEventV1, String)>,
    pub tail_frames: Vec<PublicFrameV1>,
    pub tail_debug_frames: Vec<DebugOverlayFrameV1>,
    pub sealed_chunk_paths: Vec<(u32, u16, String)>,
    pub finished: bool,
}

#[derive(Clone)]
pub struct LiveMatchHandle {
    pub sim_id: String,
    pub pace_multiplier: u8,
    pub lease_epoch: u64,
    pub init_spec: Arc<MatchInitSpecV1>,
    pub header_pub: Hps1Header,
    pub header_auth: Hps1Header,
    pub ctrl_tx: mpsc::Sender<ActorControlMsg>,
    pub sealing_rx: watch::Receiver<SealingFlushProgress>,
}

impl LiveMatchHandle {
    pub async fn submit_command(
        &self,
        ingress: IngressCommand,
    ) -> Result<SessionTransportEvent, ApiError> {
        let (tx, rx) = oneshot::channel();
        self.send_command(ActorControlMsg::SubmitCommand { ingress, reply: tx }, rx)
            .await
    }

    pub async fn forfeit(
        &self,
        ingress: IngressCommand,
    ) -> Result<SessionTransportEvent, ApiError> {
        let (tx, rx) = oneshot::channel();
        self.send_command(ActorControlMsg::Forfeit { ingress, reply: tx }, rx)
            .await
    }

    async fn send_command(
        &self,
        message: ActorControlMsg,
        reply: oneshot::Receiver<SessionTransportEvent>,
    ) -> Result<SessionTransportEvent, ApiError> {
        self.ctrl_tx
            .send(message)
            .await
            .map_err(|_| ApiError::gone("MatchActorStopped", "simulation actor has terminated"))?;
        reply
            .await
            .map_err(|_| ApiError::gone("MatchActorStopped", "simulation actor dropped reply"))
    }

    pub async fn set_playback_paused(&self, paused: bool) -> Result<(bool, u32), ApiError> {
        let (tx, rx) = oneshot::channel();
        self.ctrl_tx
            .send(ActorControlMsg::SetPlaybackPaused { paused, reply: tx })
            .await
            .map_err(|_| ApiError::gone("MatchActorStopped", "simulation actor has terminated"))?;
        rx.await
            .map_err(|_| ApiError::gone("MatchActorStopped", "simulation actor dropped reply"))?
    }

    pub async fn query_state(&self) -> Result<LiveSnapshotQuery, ApiError> {
        let (tx, rx) = oneshot::channel();
        self.ctrl_tx
            .send(ActorControlMsg::GetSnapshotState { reply: tx })
            .await
            .map_err(|_| ApiError::gone("MatchActorStopped", "simulation actor has terminated"))?;
        rx.await
            .map_err(|_| ApiError::gone("MatchActorStopped", "simulation actor dropped reply"))
    }
}

pub async fn spawn_live_match(
    sim_id: String,
    init_spec: MatchInitSpecV1,
    home_participant_id: String,
    away_participant_id: String,
    pace_multiplier: u8,
    quotas: Arc<ServerQuotas>,
    wal: SharedDurableWalStore,
    archive_store: Arc<ArchiveStore>,
) -> Result<(LiveMatchHandle, Vec<QuotaGuard>), ApiError> {
    if !matches!(pace_multiplier, 1 | 2 | 4 | 5) {
        return Err(ApiError::bad_request(
            "InvalidPaceMultiplier",
            "pace_multiplier must be one of {1, 2, 4, 5}",
        ));
    }

    // Reserve Live pool guards atomically BEFORE spawning
    let g_player = quotas
        .live_pool
        .try_reserve(LIVE_PLAYER_CMD_BUF_BYTES)
        .ok_or_else(|| {
            ApiError::too_many("LivePoolQuotaExhausted", "live quota pool exhausted", 1)
        })?;
    let g_sys = quotas
        .live_pool
        .try_reserve(LIVE_SYSTEM_CMD_RESERVE_BYTES)
        .ok_or_else(|| {
            ApiError::too_many("LivePoolQuotaExhausted", "live quota pool exhausted", 1)
        })?;
    let g_ev = quotas
        .live_pool
        .try_reserve(LIVE_EVENT_LOG_BUF_BYTES)
        .ok_or_else(|| {
            ApiError::too_many("LivePoolQuotaExhausted", "live quota pool exhausted", 1)
        })?;
    let g_tail = quotas
        .live_pool
        .try_reserve(CHUNK_FRAMES * 242)
        .ok_or_else(|| {
            ApiError::too_many("LivePoolQuotaExhausted", "live quota pool exhausted", 1)
        })?;
    let guards = vec![g_player, g_sys, g_ev, g_tail];

    let mut init_sink = VecSimSink::default();
    let engine = SimulationEngine::new(&init_spec, &mut init_sink)
        .map_err(|e| ApiError::bad_request("EngineInitError", format!("{e:?}")))?;

    let header_pub = Hps1Header::from_init_spec(&init_spec, StreamVisibility::PublicOnly);
    let header_auth = Hps1Header::from_init_spec(&init_spec, StreamVisibility::AuthoritativeFull);

    let mut boxscore = LiveBoxScoreState::from_init_spec(&init_spec);
    if let Some(f0) = init_sink.public_frames.first() {
        boxscore.on_frame(f0);
    }
    let mut events_with_pbp = Vec::new();
    for ev in &init_sink.events {
        boxscore.apply_event(ev);
        events_with_pbp.push((ev.clone(), render_event_zh(&header_pub, ev)));
    }

    let lease_epoch = 1u64;
    let session_cfg = LiveSessionConfigV1 {
        schema_version: 1,
        sim_id: sim_id.clone(),
        tick_rate_hz: 10,
        pace_multiplier,
        home_participant_id,
        away_participant_id,
        spectator_policy: 1,
        film_room_policy: 1,
    };

    let init_frame = *init_sink
        .public_frames
        .first()
        .unwrap_or(&engine.last_public_frame);
    let init_dbg = *init_sink
        .debug_frames
        .first()
        .unwrap_or(&engine.last_debug_frame);
    let cp0 = LiveServiceCheckpointV1 {
        schema_version: 1,
        sim_id: sim_id.clone(),
        cp_gen: 0,
        writer_lease_epoch: lease_epoch,
        wal_applied_seq: 0,
        sim_wal_applied_seq: 0,
        service_timer: ServiceTimerState::default(),
        engine_checkpoint: engine.capture_checkpoint_snapshot(),
        boxscore_state: boxscore.clone(),
        unsealed_tail_frames: vec![init_frame],
        unsealed_debug_frames: vec![init_dbg],
        sealed_chunks: Vec::new(),
        sealed_cmd_segs: Vec::new(),
        max_sealed_cmd_ingress_seq: 0,
        durable_published_frame_tick: 0,
        durable_published_rolling_hash: engine.last_hashes.2,
        durable_published_event_seq: init_sink.events.last().map_or(0, |e| e.event_seq),
        applied_commands: Vec::new(),
        events_snapshot: init_sink.events.clone(),
        state_hash_checkpoints: vec![(0, engine.last_hashes.2)],
        missing_frame_ranges: Vec::new(),
    };

    wal.register_live_simulation(&sim_id, lease_epoch, &init_spec, &session_cfg, &cp0)
        .map_err(|e| ApiError::insufficient_storage("WalRegistrationFailed", e.to_string()))?;

    let (ctrl_tx, ctrl_rx) = mpsc::channel(256);
    let (stream_tx, _) = broadcast::channel(512);
    let (sealing_tx, sealing_rx) = watch::channel(SealingFlushProgress::default());

    let init_spec_arc = Arc::new(init_spec);
    let handle = LiveMatchHandle {
        sim_id: sim_id.clone(),
        pace_multiplier,
        lease_epoch,
        init_spec: Arc::clone(&init_spec_arc),
        header_pub: header_pub.clone(),
        header_auth: header_auth.clone(),
        ctrl_tx,
        sealing_rx,
    };

    let actor = LiveActor {
        sim_id,
        lease_epoch,
        pace_multiplier,
        init_spec: init_spec_arc,
        engine,
        header_pub,
        header_auth,
        boxscore,
        quotas,
        wal,
        archive_store,
        ctrl_rx,
        stream_tx,
        sealing_tx,
        service_timer: ServiceTimerState::default(),
        paused_at_wall_ms: None,
        cp_gen: 0,
        applied_commands: Vec::new(),
        player_cmd_bytes_used: 0,
        sys_cmd_bytes_used: 0,
        events_all: init_sink.events,
        events_with_pbp,
        open_tail_frames: vec![init_frame],
        open_tail_debug: vec![init_dbg],
        open_tail_events: cp0.events_snapshot,
        sealed_chunks: Vec::new(),
        sealed_chunk_paths: Vec::new(),
        sealed_debug_chunk_paths: Vec::new(),
        state_hash_checkpoints: vec![(0, cp0.durable_published_rolling_hash)],
        missing_frame_ranges: Vec::new(),
        max_sealed_disk_tick: 0,
        has_inflight_sealing: false,
        inflight_chunks: BTreeMap::new(),
        last_published_frame_tick: 0,
        last_published_event_seq: cp0.durable_published_event_seq,
        finished: false,
    };

    tokio::spawn(async move {
        let _guards = guards;
        actor.run().await;
    });

    Ok((handle, Vec::new()))
}

struct LiveActor {
    sim_id: String,
    lease_epoch: u64,
    pace_multiplier: u8,
    init_spec: Arc<MatchInitSpecV1>,
    engine: SimulationEngine,
    header_pub: Hps1Header,
    header_auth: Hps1Header,
    boxscore: LiveBoxScoreState,
    quotas: Arc<ServerQuotas>,
    wal: SharedDurableWalStore,
    archive_store: Arc<ArchiveStore>,
    ctrl_rx: mpsc::Receiver<ActorControlMsg>,
    stream_tx: broadcast::Sender<LiveStreamItem>,
    sealing_tx: watch::Sender<SealingFlushProgress>,
    service_timer: ServiceTimerState,
    paused_at_wall_ms: Option<u64>,
    cp_gen: u64,
    applied_commands: Vec<AnchoredCommand>,
    player_cmd_bytes_used: usize,
    sys_cmd_bytes_used: usize,
    events_all: Vec<GameRecordEventV1>,
    events_with_pbp: Vec<(GameRecordEventV1, String)>,
    open_tail_frames: Vec<PublicFrameV1>,
    open_tail_debug: Vec<DebugOverlayFrameV1>,
    open_tail_events: Vec<GameRecordEventV1>,
    sealed_chunks: Vec<SealedChunkMetaV1>,
    sealed_chunk_paths: Vec<(u32, u16, String)>,
    sealed_debug_chunk_paths: Vec<(u32, u16, String)>,
    state_hash_checkpoints: Vec<(u32, [u8; 32])>,
    missing_frame_ranges: Vec<(u32, u32)>,
    max_sealed_disk_tick: u32,
    has_inflight_sealing: bool,
    inflight_chunks: BTreeMap<u32, ()>,
    last_published_frame_tick: u32,
    last_published_event_seq: u64,
    finished: bool,
}

impl LiveActor {
    async fn run(mut self) {
        let mut tick_interval = tokio::time::interval(Duration::from_millis(100));
        tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            let playback_running = self.paused_at_wall_ms.is_none();
            let timer_sleep = async {
                if self.service_timer.kind != ServiceTimerKind::None as u8
                    && self.service_timer.deadline_wall_ms > 0
                {
                    let now = now_wall_ms();
                    if now < self.service_timer.deadline_wall_ms {
                        tokio::time::sleep(Duration::from_millis(
                            self.service_timer.deadline_wall_ms - now,
                        ))
                        .await;
                    }
                } else {
                    std::future::pending::<()>().await;
                }
            };

            tokio::select! {
                biased;

                // (1) & (2) Control messages / external commands
                maybe_msg = self.ctrl_rx.recv() => {
                    match maybe_msg {
                        Some(ActorControlMsg::FenceLoseLease) | None => {
                            let _ = self.stream_tx.send(LiveStreamItem::Close {
                                code: 4009,
                                reason: "LeaseLostReconnect".to_owned(),
                            });
                            break;
                        }
                        Some(ActorControlMsg::GetSnapshotState { reply }) => {
                            let _ = reply.send(LiveSnapshotQuery {
                                header_pub: self.header_pub.clone(),
                                header_auth: self.header_auth.clone(),
                                boxscore: self.boxscore.clone(),
                                events: self.events_with_pbp.clone(),
                                tail_frames: self.open_tail_frames.clone(),
                                tail_debug_frames: self.open_tail_debug.clone(),
                                sealed_chunk_paths: self.sealed_chunk_paths.clone(),
                                finished: self.finished,
                            });
                        }
                        Some(ActorControlMsg::TransitionPhaseAToB {
                            from_tick,
                            after_event_seq,
                            sealed_covered_through_tick,
                            reply,
                        }) => {
                            let outcome = self.handle_phase_a_to_b(
                                from_tick,
                                after_event_seq,
                                sealed_covered_through_tick,
                            );
                            let _ = reply.send(outcome);
                        }
                        Some(ActorControlMsg::SetPlaybackPaused { paused, reply }) => {
                            let result = self.set_playback_paused(paused);
                            if result.is_ok() && !paused && !playback_running {
                                tick_interval.reset();
                            }
                            let _ = reply.send(result);
                        }
                        Some(ActorControlMsg::SubmitCommand { ingress, reply }) => {
                            if self.handle_external_command(ingress, reply).await {
                                break;
                            }
                        }
                        Some(ActorControlMsg::Forfeit { mut ingress, reply }) => {
                            ingress.phase = self.forfeit_phase(&ingress);
                            if self.handle_external_command(ingress, reply).await {
                                break;
                            }
                        }
                    }
                }

                // (2b) ServiceTimer expiration (PostWhistle Confirm/Close or DeadTimeDwell wake)
                _ = timer_sleep, if playback_running => {
                    let lease_lost = self.handle_service_timer_expired().await;
                    if lease_lost {
                        break;
                    }
                }

                // (3) 10 Hz simulation step timer (paused during WindowClose / GrantConfirm / DeadTimeDwell)
                _ = tick_interval.tick(), if playback_running && !self.finished && self.service_timer.kind == ServiceTimerKind::None as u8 => {
                    let lease_lost = self.handle_step_batch().await;
                    if lease_lost {
                        break;
                    }
                }
            }
        }
    }

    fn forfeit_phase(&self, ingress: &IngressCommand) -> CommandPhase {
        if let Some(previous) = ingress.side.team_index().and_then(|side| {
            self.engine.state.client_dedup_ring[side].find(ingress.client_request_id)
        }) {
            return previous.anchored.enqueued_phase;
        }
        if matches!(
            self.engine.state.cursor,
            EngineCursor::PausedInPostWhistle { .. }
        ) {
            CommandPhase::PostWhistle
        } else {
            CommandPhase::PreTick
        }
    }

    fn set_playback_paused(&mut self, paused: bool) -> Result<(bool, u32), ApiError> {
        let tick = self.last_published_frame_tick;
        if self.finished
            || !self
                .archive_store
                .set_playback_paused(&self.sim_id, self.lease_epoch, paused)
        {
            return Err(ApiError::conflict(
                "LivePlaybackUnavailable",
                "live simulation is no longer running",
            ));
        }
        if paused != self.paused_at_wall_ms.is_some() {
            let now = now_wall_ms();
            if paused {
                self.paused_at_wall_ms = Some(now);
            } else if let Some(start) = self.paused_at_wall_ms.take() {
                if self.service_timer.deadline_wall_ms != 0 {
                    self.service_timer.deadline_wall_ms = self
                        .service_timer
                        .deadline_wall_ms
                        .saturating_add(now.saturating_sub(start));
                }
            }
            let _ = self.stream_tx.send(LiveStreamItem::Transport(
                SessionTransportEvent::PlaybackChanged { paused, tick },
            ));
            self.broadcast_window();
        }
        Ok((paused, tick))
    }

    fn broadcast_window(&self) {
        let event = self.current_active_window().map_or(
            SessionTransportEvent::ChallengeWindowClosed,
            SessionTransportEvent::ChallengeWindowWallClock,
        );
        let _ = self.stream_tx.send(LiveStreamItem::Transport(event));
    }

    fn current_active_window(&self) -> Option<ChallengeWindowWallClock> {
        let snap = self.engine.active_challenge_window_snapshot()?;
        if snap.window_state == 3 {
            return None;
        }
        let eligible_sides_mask = self.engine.state.team_states.iter().enumerate().fold(
            snap.eligible_sides_mask,
            |mask, (index, team)| {
                if team.challenges_remaining == 0
                    || (self
                        .init_spec
                        .ruleset
                        .challenges
                        .requires_and_charges_timeout
                        && team.timeouts_remaining == 0)
                {
                    mask & !(1 << index)
                } else {
                    mask
                }
            },
        );
        Some(ChallengeWindowWallClock {
            ruling_id: snap.ruling_id,
            window_epoch: snap.window_epoch,
            window_state: snap.window_state,
            request_seq: snap.request_seq,
            grant_requester_side: snap.grant_requester_side,
            eligible_sides_mask,
            challenge_deadline_wall_ms: self.service_timer.deadline_wall_ms,
        })
    }

    fn handle_phase_a_to_b(
        &self,
        from_tick: u32,
        after_event_seq: u64,
        sealed_covered_through_tick: Option<u32>,
    ) -> CatchUpHandoffOutcome {
        let tail_start = self
            .open_tail_frames
            .first()
            .map_or(self.last_published_frame_tick, |f| f.tick);
        let fd = self.wal.fault_domain();

        // Check if Phase A still has unsealed inflight gap between sealed_covered_through_tick and tail_start
        if let Some(covered) = sealed_covered_through_tick {
            if self.has_inflight_sealing
                && covered + 1 < tail_start
                && from_tick <= self.max_sealed_disk_tick.max(tail_start)
            {
                return CatchUpHandoffOutcome::AwaitSealingFlush(self.sealing_tx.subscribe());
            }
            if fd == StorageFaultDomain::Healthy
                && self.max_sealed_disk_tick >= covered + 1
                && covered + 1 < tail_start
            {
                return CatchUpHandoffOutcome::RetryPhaseA;
            }
        } else if self.has_inflight_sealing && from_tick < tail_start {
            return CatchUpHandoffOutcome::AwaitSealingFlush(self.sealing_tx.subscribe());
        } else if fd == StorageFaultDomain::Healthy
            && from_tick < tail_start
            && !self.sealed_chunk_paths.is_empty()
        {
            return CatchUpHandoffOutcome::RetryPhaseA;
        }

        let tail_frames: Vec<PublicFrameV1> = self
            .open_tail_frames
            .iter()
            .filter(|f| f.tick >= from_tick)
            .copied()
            .collect();
        let tail_events: Vec<(GameRecordEventV1, String)> = self
            .events_with_pbp
            .iter()
            .filter(|(e, _)| e.event_seq > after_event_seq)
            .cloned()
            .collect();

        let retain_bytes = tail_frames.len() * 124 + tail_events.len() * 96 + 1024;
        let retain_guard = match self.quotas.phase_b_retain_pool.try_reserve(retain_bytes) {
            Some(g) => Some(g),
            None => {
                return CatchUpHandoffOutcome::BudgetExhausted(
                    "PhaseBSnapshotRetainPool exhausted".to_owned(),
                )
            }
        };

        let needs_baseline = !self.missing_frame_ranges.is_empty() || from_tick > tail_start;
        let boxscore_snapshot = if needs_baseline {
            Some(BoxScoreBaselineSnapshotV1 {
                as_of_frame_tick: self.last_published_frame_tick,
                as_of_event_seq: self.last_published_event_seq,
                boxscore: self.boxscore.clone(),
            })
        } else {
            None
        };

        let rx = self.stream_tx.subscribe();
        let pf = &self.engine.last_public_frame;
        CatchUpHandoffOutcome::Ready(CatchUpPhaseBSnapshot {
            sim_id: self.sim_id.clone(),
            lease_epoch: self.lease_epoch,
            pace_multiplier: self.pace_multiplier,
            header_pub: self.header_pub.clone(),
            tail_frames_arc: Arc::new(tail_frames),
            tail_events_arc: Arc::new(tail_events),
            sealed_chunk_paths: self.sealed_chunk_paths.clone(),
            boxscore_snapshot,
            active_window: self.current_active_window(),
            playback_paused: self.paused_at_wall_ms.is_some(),
            missing_frame_ranges: self.missing_frame_ranges.clone(),
            last_published_frame_tick: self.last_published_frame_tick,
            last_published_event_seq: self.last_published_event_seq,
            game_phase: pf.game_phase,
            quarter: pf.quarter,
            game_clock_ds: pf.game_clock_ds,
            shot_clock_ds: pf.shot_clock_ds,
            score_home: pf.score_home,
            score_away: pf.score_away,
            _retain_guard: retain_guard,
            rx,
        })
    }

    async fn handle_external_command(
        &mut self,
        ingress: IngressCommand,
        reply: oneshot::Sender<SessionTransportEvent>,
    ) -> bool {
        let client_request_id = ingress.client_request_id;
        if self.wal.fault_domain() == StorageFaultDomain::WalWriteStalled {
            let _ = reply.send(SessionTransportEvent::IngressCommandRejected {
                client_request_id,
                reason_code: CommandRejectReason::CommandLogStorageStalled as u16,
                reason: "CommandLogStorageStalled (WAL)".to_owned(),
            });
            return false;
        }

        let outcome = match self.engine.prepare_command(ingress) {
            Ok(o) => o,
            Err(reason) => {
                let _ = reply.send(SessionTransportEvent::IngressCommandRejected {
                    client_request_id,
                    reason_code: reason as u16,
                    reason: format!("{reason:?}"),
                });
                return false;
            }
        };

        match outcome {
            PreparedOutcome::AlreadyAccepted {
                existing_anchored, ..
            } => {
                let _ = reply.send(SessionTransportEvent::CommandAccepted {
                    client_request_id,
                    anchored: existing_anchored,
                });
                false
            }
            PreparedOutcome::NewCommand(prep) => {
                let payload = prep.candidate_anchored.payload;
                if self.paused_at_wall_ms.is_some() && !payload.is_forfeit() {
                    let _ = reply.send(SessionTransportEvent::IngressCommandRejected {
                        client_request_id,
                        reason_code: 0xFFFF,
                        reason: "PlaybackPaused".to_owned(),
                    });
                    return false;
                }
                let is_sys = payload.is_system();
                let cmd_len = prep.encoded_len as usize;
                if is_sys {
                    if self.sys_cmd_bytes_used + cmd_len > LIVE_SYSTEM_CMD_RESERVE_BYTES {
                        let _ = reply.send(SessionTransportEvent::IngressCommandRejected {
                            client_request_id,
                            reason_code: CommandRejectReason::CommandLogStorageStalled as u16,
                            reason: "System command log reserve exhausted".to_owned(),
                        });
                        self.abort_storage("ReplayIncompleteStorageFailure").await;
                        return true;
                    }
                } else if self.wal.fault_domain() == StorageFaultDomain::ArchiveStoreOnly
                    && self.player_cmd_bytes_used + cmd_len > LIVE_PLAYER_CMD_BUF_BYTES
                {
                    let _ = reply.send(SessionTransportEvent::IngressCommandRejected {
                        client_request_id,
                        reason_code: CommandRejectReason::CommandLogStorageStalled as u16,
                        reason:
                            "CommandLogStorageStalled (player buffer full during ArchiveStoreOnly)"
                                .to_owned(),
                    });
                    return false;
                }

                let new_timer = if prep.candidate_anchored.payload.is_forfeit() {
                    ServiceTimerState::default()
                } else {
                    self.service_timer
                };

                let rec = WalRecord::AnchoredCommand {
                    wal_seq: 0,
                    anchored: prep.candidate_anchored,
                    request_fingerprint: prep.request_fingerprint,
                    post_service_timer: new_timer,
                };
                match self
                    .wal
                    .append_record(
                        &self.sim_id,
                        self.lease_epoch,
                        WalWriteClass::NormalWrite,
                        rec,
                    )
                    .await
                {
                    Ok((_wseq, _sseq)) => {}
                    Err(WalError::StaleLeaseEpoch { .. }) => {
                        let _ = self.stream_tx.send(LiveStreamItem::Close {
                            code: 4009,
                            reason: "LeaseLostReconnect".to_owned(),
                        });
                        return true;
                    }
                    Err(e) => {
                        self.wal
                            .set_fault_domain(StorageFaultDomain::WalWriteStalled);
                        let _ = reply.send(SessionTransportEvent::IngressCommandRejected {
                            client_request_id,
                            reason_code: CommandRejectReason::CommandLogStorageStalled as u16,
                            reason: format!("WalWriteStalled: {e}"),
                        });
                        self.abort_storage("StorageWalUnavailable").await;
                        return true;
                    }
                }

                if is_sys {
                    self.sys_cmd_bytes_used += cmd_len;
                } else {
                    self.player_cmd_bytes_used += cmd_len;
                }

                let mut cmd_sink = VecSimSink::default();
                let anchored = self.engine.commit_prepared_command(prep, &mut cmd_sink);
                self.applied_commands.push(anchored);
                self.service_timer = new_timer;

                let _ = reply.send(SessionTransportEvent::CommandAccepted {
                    client_request_id,
                    anchored,
                });

                // Emit immediate PostWhistle events if not PendingForfeitFinalize
                if !matches!(
                    self.engine.state.cursor,
                    EngineCursor::PendingForfeitFinalize { .. }
                ) {
                    self.update_timer_for_window();
                    self.publish_immediate_events(&cmd_sink.events);
                    false
                } else {
                    self.publish_immediate_events(&cmd_sink.events);
                    self.broadcast_window();
                    self.finalize_forfeit_stage8().await
                }
            }
        }
    }

    fn update_timer_for_window(&mut self) {
        if let Some(snap) = self.engine.active_challenge_window_snapshot() {
            let now = now_wall_ms();
            if snap.window_state == 1 {
                // PendingRefereeGrant -> 2s GrantConfirm timer; preserve paused remaining time
                let rem = if self.service_timer.kind == ServiceTimerKind::WindowClose as u8 {
                    self.service_timer
                        .deadline_wall_ms
                        .saturating_sub(now)
                        .min(u32::MAX as u64) as u32
                } else {
                    self.service_timer.paused_open_remaining_ms
                };
                self.service_timer = ServiceTimerState {
                    kind: ServiceTimerKind::GrantConfirm as u8,
                    ruling_id: snap.ruling_id,
                    window_epoch: snap.window_epoch,
                    request_seq: snap.request_seq,
                    deadline_wall_ms: now + 2_000,
                    paused_open_remaining_ms: rem,
                };
            } else if snap.window_state == 0 {
                let dur = if self.service_timer.paused_open_remaining_ms > 0 {
                    (self.service_timer.paused_open_remaining_ms as u64).max(500)
                } else {
                    5_000
                };
                self.service_timer = ServiceTimerState {
                    kind: ServiceTimerKind::WindowClose as u8,
                    ruling_id: snap.ruling_id,
                    window_epoch: snap.window_epoch,
                    request_seq: snap.request_seq,
                    deadline_wall_ms: now + dur,
                    paused_open_remaining_ms: 0,
                };
            }
            self.broadcast_window();
        }
    }

    fn publish_immediate_events(&mut self, events: &[GameRecordEventV1]) {
        for ev in events {
            self.boxscore.apply_event(ev);
            let pbp = render_event_zh(&self.header_pub, ev);
            self.events_all.push(ev.clone());
            self.open_tail_events.push(ev.clone());
            self.events_with_pbp.push((ev.clone(), pbp.clone()));
            self.last_published_event_seq = self.last_published_event_seq.max(ev.event_seq);
            let _ = self.stream_tx.send(LiveStreamItem::Event {
                event: ev.clone(),
                pbp_zh: pbp,
            });
        }
    }

    async fn finalize_forfeit_stage8(&mut self) -> bool {
        let mut sink = VecSimSink::default();
        let pre_sim_tick = self.engine.state.sim_tick;
        let pre_cursor = self.engine.state.cursor;
        let _ = self.engine.step(&mut sink);
        self.service_timer = ServiceTimerState::default();

        let pf = self.engine.last_public_frame;
        let (sh, _eh, rh) = self.engine.last_hashes;
        let last_ev_seq = self
            .events_all
            .last()
            .map_or(self.last_published_event_seq, |e| e.event_seq)
            .max(sink.events.last().map_or(0, |e| e.event_seq));

        let rec = WalRecord::StepCommit {
            wal_seq: 0,
            pre_sim_tick,
            step_count: 1,
            pre_cursor,
            post_sim_tick: self.engine.state.sim_tick,
            post_cursor: self.engine.state.cursor,
            post_last_event_seq: last_ev_seq,
            post_state_hash: sh,
            post_rolling_hash: rh,
            post_service_timer: ServiceTimerState::default(),
            durable_completed_frame_tick: pf.tick,
            durable_completed_rolling_hash: rh,
            durable_published_event_seq: last_ev_seq,
            expected_resume_sim_tick: self.engine.state.sim_tick,
            expected_resume_cursor: self.engine.state.cursor,
        };
        if let Err(e) = self
            .wal
            .append_record(
                &self.sim_id,
                self.lease_epoch,
                WalWriteClass::TakeoverCloseoutWrite,
                rec,
            )
            .await
        {
            let _ = e;
            return true;
        }

        self.consume_and_broadcast_sink(sink).await;
        self.finish_and_publish_archives().await;
        false
    }

    async fn handle_service_timer_expired(&mut self) -> bool {
        let kind = self.service_timer.kind;
        if kind == ServiceTimerKind::DeadTimeDwell as u8 {
            self.service_timer = ServiceTimerState::default();
            return false;
        }
        if kind == ServiceTimerKind::None as u8 {
            return false;
        }

        let sys_payload = if kind == ServiceTimerKind::GrantConfirm as u8 {
            SystemCommand::ConfirmChallengeGrant {
                ruling_id: self.service_timer.ruling_id,
                request_seq: self.service_timer.request_seq,
            }
        } else {
            SystemCommand::CloseChallengeWindow {
                ruling_id: self.service_timer.ruling_id,
                window_epoch: self.service_timer.window_epoch,
            }
        };

        let ingress = IngressCommand {
            client_request_id: self.engine.state.next_ingress_seq,
            apply_at_tick: None,
            phase: CommandPhase::PostWhistle,
            side: CommandSide::System,
            payload: hoops_core::CommandPayload::System(sys_payload),
        };

        if let Ok(PreparedOutcome::NewCommand(prep)) = self.engine.prepare_command(ingress) {
            let rec = WalRecord::AnchoredCommand {
                wal_seq: 0,
                anchored: prep.candidate_anchored,
                request_fingerprint: prep.request_fingerprint,
                post_service_timer: ServiceTimerState::default(),
            };
            if self
                .wal
                .append_record(
                    &self.sim_id,
                    self.lease_epoch,
                    WalWriteClass::NormalWrite,
                    rec,
                )
                .await
                .is_err()
            {
                return true;
            }
            let mut cmd_sink = VecSimSink::default();
            let anchored = self.engine.commit_prepared_command(prep, &mut cmd_sink);
            self.applied_commands.push(anchored);
            self.publish_immediate_events(&cmd_sink.events);
        }
        self.service_timer = ServiceTimerState::default();
        self.broadcast_window();

        // Advance Stage 8 completion step after closing window
        self.handle_step_batch().await
    }

    async fn handle_step_batch(&mut self) -> bool {
        if self.finished {
            return false;
        }
        let pre_sim_tick = self.engine.state.sim_tick;
        let pre_cursor = self.engine.state.cursor;
        let mut sink = VecSimSink::default();
        let mut steps_taken = 0u8;
        let mut new_timer = ServiceTimerState::default();

        let target_steps = self.pace_multiplier.clamp(1, 5);
        for _ in 0..target_steps {
            let st = self.engine.step(&mut sink);
            steps_taken += 1;

            match st {
                StepStatus::AwaitingPostWhistle {
                    ruling_id,
                    window_state,
                    ..
                } => {
                    // Flush-on-Window: break batch immediately
                    if let Some(ruling) = self.engine.state.active_pending_ruling.as_ref() {
                        let now = now_wall_ms();
                        new_timer = match window_state {
                            hoops_core::ChallengeWindowState::PendingRefereeGrant { .. } => {
                                ServiceTimerState {
                                    kind: ServiceTimerKind::GrantConfirm as u8,
                                    ruling_id,
                                    window_epoch: ruling.window_epoch,
                                    request_seq: ruling.request_seq,
                                    deadline_wall_ms: now + 2_000,
                                    paused_open_remaining_ms: 0,
                                }
                            }
                            _ => ServiceTimerState {
                                kind: ServiceTimerKind::WindowClose as u8,
                                ruling_id,
                                window_epoch: ruling.window_epoch,
                                request_seq: ruling.request_seq,
                                deadline_wall_ms: now + 5_000,
                                paused_open_remaining_ms: 0,
                            },
                        };
                    }
                    break;
                }
                StepStatus::Finished { .. } => break,
                StepStatus::Aborted { .. } => {
                    self.abort_determinism().await;
                    return true;
                }
                StepStatus::TickCompleted { .. } => {
                    // Flush-on-DeadTime if 0x17 DeadTime emitted this tick
                    if let Some(dt_ds) = sink.events.iter().rev().find_map(|e| {
                        if e.tick == self.engine.last_public_frame.tick {
                            if let hoops_core::GameRecordEventPayload::DeadTime {
                                duration_ds,
                                ..
                            } = e.payload
                            {
                                Some(duration_ds)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }) {
                        let dwell_ms = (dt_ds as u64 * 100 + self.pace_multiplier as u64 - 1)
                            / (self.pace_multiplier as u64);
                        new_timer = ServiceTimerState {
                            kind: ServiceTimerKind::DeadTimeDwell as u8,
                            ruling_id: 0,
                            window_epoch: 0,
                            request_seq: 0,
                            deadline_wall_ms: now_wall_ms() + dwell_ms.min(8_000),
                            paused_open_remaining_ms: 0,
                        };
                        break;
                    }
                }
            }
        }

        let last_tick = sink
            .public_frames
            .last()
            .map_or(self.last_published_frame_tick, |f| f.tick);
        let last_ev_seq = sink
            .events
            .last()
            .map_or(self.last_published_event_seq, |e| e.event_seq);
        let (sh, _eh, rh) = self.engine.last_hashes;

        let rec = WalRecord::StepCommit {
            wal_seq: 0,
            pre_sim_tick,
            step_count: steps_taken,
            pre_cursor,
            post_sim_tick: self.engine.state.sim_tick,
            post_cursor: self.engine.state.cursor,
            post_last_event_seq: last_ev_seq,
            post_state_hash: sh,
            post_rolling_hash: rh,
            post_service_timer: new_timer,
            durable_completed_frame_tick: last_tick,
            durable_completed_rolling_hash: rh,
            durable_published_event_seq: last_ev_seq,
            expected_resume_sim_tick: self.engine.state.sim_tick,
            expected_resume_cursor: self.engine.state.cursor,
        };

        match self
            .wal
            .append_record(
                &self.sim_id,
                self.lease_epoch,
                WalWriteClass::NormalWrite,
                rec,
            )
            .await
        {
            Ok((wal_seq, sim_wal_seq)) => {
                self.service_timer = new_timer;
                self.consume_and_broadcast_sink(sink).await;
                if new_timer.kind != ServiceTimerKind::None as u8 {
                    let _ = self.maybe_checkpoint(wal_seq, sim_wal_seq).await;
                    if new_timer.kind != ServiceTimerKind::DeadTimeDwell as u8 {
                        self.broadcast_window();
                    }
                }
                if matches!(self.engine.state.cursor, EngineCursor::GameFinished) {
                    self.finish_and_publish_archives().await;
                }
                false
            }
            Err(WalError::StaleLeaseEpoch { .. }) => {
                let _ = self.stream_tx.send(LiveStreamItem::Close {
                    code: 4009,
                    reason: "LeaseLostReconnect".to_owned(),
                });
                true
            }
            Err(_) => {
                self.wal
                    .set_fault_domain(StorageFaultDomain::WalWriteStalled);
                self.abort_storage("StorageWalUnavailable").await;
                true
            }
        }
    }

    async fn consume_and_broadcast_sink(&mut self, sink: VecSimSink) {
        let mut ev_idx = 0usize;
        for f in &sink.public_frames {
            self.boxscore.on_frame(f);
            let _ = self.stream_tx.send(LiveStreamItem::PublicFrame(*f));
            self.last_published_frame_tick = self.last_published_frame_tick.max(f.tick);
            self.open_tail_frames.push(*f);

            while ev_idx < sink.events.len() && sink.events[ev_idx].tick <= f.tick {
                let ev = &sink.events[ev_idx];
                self.boxscore.apply_event(ev);
                let pbp = render_event_zh(&self.header_pub, ev);
                self.events_all.push(ev.clone());
                self.open_tail_events.push(ev.clone());
                self.events_with_pbp.push((ev.clone(), pbp.clone()));
                self.last_published_event_seq = self.last_published_event_seq.max(ev.event_seq);
                let _ = self.stream_tx.send(LiveStreamItem::Event {
                    event: ev.clone(),
                    pbp_zh: pbp,
                });
                ev_idx += 1;
            }
        }
        for df in &sink.debug_frames {
            self.open_tail_debug.push(*df);
        }
        // Broadcast any remaining events (e.g. PostWhistle events whose frame has not finished yet)
        while ev_idx < sink.events.len() {
            let ev = &sink.events[ev_idx];
            if ev.event_seq > self.last_published_event_seq {
                let pbp = render_event_zh(&self.header_pub, ev);
                self.events_all.push(ev.clone());
                self.open_tail_events.push(ev.clone());
                self.events_with_pbp.push((ev.clone(), pbp.clone()));
                self.last_published_event_seq = ev.event_seq;
                let _ = self.stream_tx.send(LiveStreamItem::Event {
                    event: ev.clone(),
                    pbp_zh: pbp,
                });
            }
            ev_idx += 1;
        }
        for &(t, sh, _eh, _rh) in &sink.hashes {
            if t % 100 == 0 {
                self.state_hash_checkpoints.push((t, sh));
            }
        }

        // Update directory live scoreboard
        let pf = &self.engine.last_public_frame;
        self.archive_store.update_progress(
            &self.sim_id,
            self.lease_epoch,
            pf.score_home,
            pf.score_away,
            pf.quarter,
            pf.game_clock_ds,
            pf.game_phase,
            self.last_published_frame_tick,
            if matches!(self.engine.state.cursor, EngineCursor::GameFinished) {
                MatchLifecycleState::Finished
            } else {
                MatchLifecycleState::Running
            },
        );

        // Seal 600-tick chunk to ArchiveStore cleanly
        while self.open_tail_frames.len() >= CHUNK_FRAMES {
            self.seal_open_chunk().await;
        }
    }

    async fn seal_open_chunk(&mut self) {
        let take_n = CHUNK_FRAMES.min(self.open_tail_frames.len());
        let chunk_frames: Vec<PublicFrameV1> = self.open_tail_frames.drain(..take_n).collect();
        let dbg_take_n = take_n.min(self.open_tail_debug.len());
        let chunk_debug: Vec<DebugOverlayFrameV1> =
            self.open_tail_debug.drain(..dbg_take_n).collect();
        let first_tick = chunk_frames[0].tick;
        let last_tick = chunk_frames[chunk_frames.len() - 1].tick;

        if !chunk_debug.is_empty() {
            let mut raw_dbg = Vec::with_capacity(chunk_debug.len() * DEBUG_FRAME_BYTES);
            for df in &chunk_debug {
                df.write_le(&mut raw_dbg);
            }
            drop(chunk_debug);
            if let Ok(zst_dbg) = zstd::encode_all(&raw_dbg[..], 3) {
                drop(raw_dbg);
                let dbg_chunk_rel = format!(
                    "{}/chunks/epoch_{}_dbg_{}_{}.bin.zst",
                    self.sim_id, self.lease_epoch, first_tick, last_tick
                );
                if self
                    .archive_store
                    .write_atomic_artifact(&dbg_chunk_rel, &zst_dbg)
                    .is_ok()
                {
                    self.sealed_debug_chunk_paths.push((
                        first_tick,
                        dbg_take_n as u16,
                        dbg_chunk_rel,
                    ));
                }
            }
        }

        let mut chunk_events = Vec::new();
        let mut keep_events = Vec::new();
        for ev in self.open_tail_events.drain(..) {
            if ev.tick >= first_tick && ev.tick <= last_tick {
                chunk_events.push(ev);
            } else {
                keep_events.push(ev);
            }
        }
        self.open_tail_events = keep_events;

        self.has_inflight_sealing = true;
        self.inflight_chunks.insert(first_tick, ());
        let _ = self.sealing_tx.send(SealingFlushProgress {
            max_sealed_disk_tick: self.max_sealed_disk_tick,
            max_sealing_inflight_tick: last_tick,
            has_inflight_sealing: true,
            storage_fault: self.wal.fault_domain() as u8,
        });

        let rel_path = format!(
            "{}/chunks/epoch_{}_chunk_{}_{}.hframes.zst",
            self.sim_id, self.lease_epoch, first_tick, last_tick
        );
        match encode_soa_zstd_archive(&self.header_pub, &chunk_frames, &chunk_events, 3) {
            Ok(bytes) => {
                if self
                    .archive_store
                    .write_atomic_artifact(&rel_path, &bytes)
                    .is_ok()
                {
                    let first_es = chunk_events.first().map_or(0, |e| e.event_seq);
                    let last_es = chunk_events.last().map_or(0, |e| e.event_seq);
                    self.sealed_chunks.push(SealedChunkMetaV1 {
                        first_tick,
                        tick_count: chunk_frames.len() as u16,
                        first_event_seq: first_es,
                        last_event_seq: last_es,
                        writer_lease_epoch: self.lease_epoch,
                        chunk_bytes: bytes.len() as u32,
                    });
                    self.sealed_chunk_paths
                        .push((first_tick, chunk_frames.len() as u16, rel_path));
                    self.max_sealed_disk_tick = self.max_sealed_disk_tick.max(last_tick);
                } else {
                    self.missing_frame_ranges.push((first_tick, last_tick));
                }
            }
            Err(_) => {
                self.missing_frame_ranges.push((first_tick, last_tick));
            }
        }
        self.inflight_chunks.remove(&first_tick);
        self.has_inflight_sealing = !self.inflight_chunks.is_empty();
        let _ = self.sealing_tx.send(SealingFlushProgress {
            max_sealed_disk_tick: self.max_sealed_disk_tick,
            max_sealing_inflight_tick: self.max_sealed_disk_tick,
            has_inflight_sealing: self.has_inflight_sealing,
            storage_fault: self.wal.fault_domain() as u8,
        });
    }

    async fn maybe_checkpoint(&mut self, wal_seq: u64, sim_wal_seq: u64) -> Result<(), WalError> {
        if self.has_inflight_sealing {
            return Ok(());
        }
        self.cp_gen += 1;
        let cp = LiveServiceCheckpointV1 {
            schema_version: 1,
            sim_id: self.sim_id.clone(),
            cp_gen: self.cp_gen,
            writer_lease_epoch: self.lease_epoch,
            wal_applied_seq: wal_seq,
            sim_wal_applied_seq: sim_wal_seq,
            service_timer: self.service_timer,
            engine_checkpoint: self.engine.capture_checkpoint_snapshot(),
            boxscore_state: self.boxscore.clone(),
            unsealed_tail_frames: self.open_tail_frames.clone(),
            unsealed_debug_frames: self.open_tail_debug.clone(),
            sealed_chunks: self.sealed_chunks.clone(),
            sealed_cmd_segs: Vec::<SealedCmdSegMetaV1>::new(),
            max_sealed_cmd_ingress_seq: 0,
            durable_published_frame_tick: self.last_published_frame_tick,
            durable_published_rolling_hash: self.engine.last_hashes.2,
            durable_published_event_seq: self.last_published_event_seq,
            applied_commands: self.applied_commands.clone(),
            events_snapshot: self.open_tail_events.clone(),
            state_hash_checkpoints: self.state_hash_checkpoints.clone(),
            missing_frame_ranges: self.missing_frame_ranges.clone(),
        };
        self.wal
            .save_and_register_checkpoint(&self.sim_id, self.lease_epoch, &cp)
            .await
    }

    async fn finish_and_publish_archives(&mut self) {
        self.finished = true;
        // 已封口公共分块与调试分块按首拍排序，终场归档逐块转写，内存只驻留当前分块
        let mut sealed_public = self.sealed_chunk_paths.clone();
        sealed_public.sort_by_key(|(first_tick, _, _)| *first_tick);
        let mut sealed_debug = self.sealed_debug_chunk_paths.clone();
        sealed_debug.sort_by_key(|(first_tick, _, _)| *first_tick);

        let replay = HReplayContainer {
            manifest: ReplayManifest {
                init_spec: (*self.init_spec).clone(),
                applied_commands: self.applied_commands.clone(),
                verification: Some(ReplayVerificationV1 {
                    state_hash_checkpoints: self.state_hash_checkpoints.clone(),
                    final_tick: self.engine.state.sim_tick,
                    final_state_hash: self.engine.last_hashes.0,
                }),
            },
            hash_trace: None,
        };
        let replay_bytes = replay.to_bytes();
        let replay_uri = format!("{}/epoch_{}.hreplay", self.sim_id, self.lease_epoch);
        let art_uri = format!(
            "{}/epoch_{}_gen_0.hframes.zst",
            self.sim_id, self.lease_epoch
        );
        let dbg_uri = format!("{}/epoch_{}_gen_0.hdbg.raw", self.sim_id, self.lease_epoch);

        let to_io_error =
            |e: hoops_output::OutputError| io::Error::new(io::ErrorKind::Other, e.to_string());
        let archive_written =
            self.archive_store
                .write_atomic_artifact_with(&art_uri, |file| {
                    let mut writer = BufWriter::new(file);
                    let mut assembler =
                        SoaArchiveAssembler::new(&self.header_pub, &mut writer)
                            .map_err(to_io_error)?;
                    for (_, _, path) in &sealed_public {
                        let bytes = self.archive_store.read_artifact(path)?;
                        assembler
                            .append_chunk_archive(&bytes)
                            .map_err(to_io_error)?;
                    }
                    let tail_first_tick = self
                        .open_tail_frames
                        .first()
                        .map_or(u32::MAX, |frame| frame.tick);
                    let tail_events: Vec<GameRecordEventV1> = self
                        .events_all
                        .iter()
                        .filter(|event| event.tick >= tail_first_tick)
                        .cloned()
                        .collect();
                    let tail_archive = encode_soa_zstd_archive(
                        &self.header_pub,
                        &self.open_tail_frames,
                        &tail_events,
                        3,
                    )
                    .map_err(to_io_error)?;
                    assembler
                        .append_chunk_archive(&tail_archive)
                        .map_err(to_io_error)?;
                    assembler.finish().map_err(to_io_error)?;
                    writer.flush()
                });

        match (
            self.archive_store
                .write_atomic_artifact(&replay_uri, &replay_bytes),
            archive_written,
        ) {
            (Ok(r_uri), Ok(a_uri)) => {
                let debug_saved = self.reassemble_and_write_debug_archive(
                    &dbg_uri,
                    &sealed_public,
                    &sealed_debug,
                );
                if debug_saved.is_some() {
                    self.open_tail_debug.clear();
                    self.open_tail_debug.shrink_to_fit();
                }
                let _ = self.archive_store.cas_publish_complete(
                    &self.sim_id,
                    self.lease_epoch,
                    a_uri,
                    r_uri,
                    debug_saved,
                );
            }
            (Ok(r_uri), Err(_)) => {
                let _ = self.archive_store.cas_set_archive_status(
                    &self.sim_id,
                    self.lease_epoch,
                    ArchiveStatus::ArchiveFinalizingRetry {
                        origin_path: FinalizeOriginPath::HealthyTail,
                        candidate_artifact_uri: art_uri,
                        spool_uri: None,
                        replay_uri: r_uri,
                        attempt_count: 1,
                        retry_started_wall_ms: now_wall_ms(),
                    },
                );
            }
            (Err(e), _) => {
                let _ = self.archive_store.cas_set_archive_status(
                    &self.sim_id,
                    self.lease_epoch,
                    ArchiveStatus::ArchiveFailed {
                        reason: e.to_string(),
                    },
                );
            }
        }
        let _ = self
            .wal
            .deregister_terminal_sim(&self.sim_id, self.lease_epoch);
        let _ = self.stream_tx.send(LiveStreamItem::Close {
            code: 1000,
            reason: "MatchFinished".to_owned(),
        });
        let _ = FINALIZE_FLUSH_TIMEOUT_MS;
    }

    fn reassemble_and_write_debug_archive(
        &self,
        dbg_uri: &str,
        sealed_public: &[(u32, u16, String)],
        sealed_debug: &[(u32, u16, String)],
    ) -> Option<String> {
        let play_dict_len = self.header_auth.play_dict.len() as u16;
        let def_dict_len = self.header_auth.def_dict.len() as u16;
        let total_frames = sealed_debug
            .iter()
            .map(|(_, count, _)| usize::from(*count))
            .sum::<usize>()
            + self.open_tail_debug.len();
        self.archive_store.write_atomic_artifact_with(dbg_uri, |file| {
            let invalid = || {
                io::Error::new(io::ErrorKind::InvalidData, "debug and public frames do not match")
            };
            let mut writer = BufWriter::new(file);
            let mut wire = Vec::with_capacity(512);
            write_raw_stream_header(
                &mut wire,
                total_frames as u32,
                true,
                self.events_all.len() as u32,
            );
            writer.write_all(&wire)?;
            let mut public = SealedPublicFrameCursor::new(
                &self.archive_store,
                sealed_public,
                &self.open_tail_frames,
            );
            for &(first_tick, count, ref path) in sealed_debug {
                let count = usize::from(count);
                if count == 0 || count > CHUNK_FRAMES {
                    return Err(invalid());
                }
                let bytes = self.archive_store.read_artifact(path)?;
                let mut decoder = zstd::stream::read::Decoder::new(&bytes[..])?;
                let mut row = [0; DEBUG_FRAME_BYTES];
                for i in 0..count {
                    decoder.read_exact(&mut row)?;
                    let frame = public.next_frame()?.ok_or_else(invalid)?;
                    if i == 0 && frame.tick != first_tick {
                        return Err(invalid());
                    }
                    let mut reader = CanonicalReader::new(&row);
                    DebugOverlayFrameV1::read_le(&mut reader, play_dict_len, def_dict_len, &frame)
                        .map_err(|_| invalid())?;
                    wire.clear();
                    frame.write_le(&mut wire);
                    wire.extend_from_slice(&row);
                    writer.write_all(&wire)?;
                }
                if decoder.read(&mut row[..1])? != 0 {
                    return Err(invalid());
                }
            }
            for debug in &self.open_tail_debug {
                let frame = public
                    .next_frame()?
                    .filter(|frame| frame.tick == debug.tick)
                    .ok_or_else(invalid)?;
                wire.clear();
                frame.write_le(&mut wire);
                debug.write_le(&mut wire);
                writer.write_all(&wire)?;
            }
            if public.next_frame()?.is_some() {
                return Err(invalid());
            }
            for event in &self.events_all {
                wire.clear();
                event.write_le(&mut wire);
                writer.write_all(&wire)?;
            }
            writer.flush()
        })
        .ok()
    }

    async fn abort_storage(&mut self, reason: &str) {
        self.finished = true;
        let _ = self.stream_tx.send(LiveStreamItem::Close {
            code: 4011,
            reason: "MatchStorageAborted".to_owned(),
        });
        let _ = self
            .archive_store
            .mark_aborted(&self.sim_id, self.lease_epoch, reason.to_owned());
        let _ = self
            .wal
            .deregister_terminal_sim(&self.sim_id, self.lease_epoch);
    }

    async fn abort_determinism(&mut self) {
        self.finished = true;
        let _ = self.stream_tx.send(LiveStreamItem::Close {
            code: 4012,
            reason: "MatchDeterminismAborted".to_owned(),
        });
        let _ = self.archive_store.mark_aborted(
            &self.sim_id,
            self.lease_epoch,
            "DeterminismInvariantViolation".to_owned(),
        );
        let _ = self
            .wal
            .deregister_terminal_sim(&self.sim_id, self.lease_epoch);
    }
}

/// 终场写调试归档时按需解码已封口公共分块，耗尽后接未封口尾部帧，内存只驻留当前分块
struct SealedPublicFrameCursor<'a> {
    store: &'a ArchiveStore,
    chunks: &'a [(u32, u16, String)],
    tail: &'a [PublicFrameV1],
    next_chunk: usize,
    frames: Vec<PublicFrameV1>,
    pos: usize,
    tail_pos: usize,
}

impl<'a> SealedPublicFrameCursor<'a> {
    fn new(
        store: &'a ArchiveStore,
        chunks: &'a [(u32, u16, String)],
        tail: &'a [PublicFrameV1],
    ) -> Self {
        Self {
            store,
            chunks,
            tail,
            next_chunk: 0,
            frames: Vec::new(),
            pos: 0,
            tail_pos: 0,
        }
    }

    fn next_frame(&mut self) -> io::Result<Option<PublicFrameV1>> {
        if self.pos >= self.frames.len() {
            let Some((_, count, path)) = self.chunks.get(self.next_chunk) else {
                let frame = self.tail.get(self.tail_pos).copied();
                if frame.is_some() {
                    self.tail_pos += 1;
                }
                return Ok(frame);
            };
            let bytes = self.store.read_artifact(path)?;
            let decoded = decode_soa_zstd_archive_range(&bytes, 0, u32::MAX).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "sealed public chunk is not decodable")
            })?;
            if decoded.frames.len() != usize::from(*count) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "sealed public chunk frame count mismatch",
                ));
            }
            self.frames = decoded.frames;
            self.pos = 0;
            self.next_chunk += 1;
        }
        let frame = self.frames.get(self.pos).copied();
        self.pos += 1;
        Ok(frame)
    }
}

pub fn decode_raw_bundle_for_browser(
    header: &Hps1Header,
    frames: &[PublicFrameV1],
    debug_frames: Option<&[DebugOverlayFrameV1]>,
    events: &[GameRecordEventV1],
) -> Vec<u8> {
    let hdr_bytes = header.to_bytes();
    let raw = encode_raw_stream(frames, debug_frames, events);
    let mut out = Vec::with_capacity(4 + hdr_bytes.len() + raw.len());
    out.extend_from_slice(&(hdr_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&hdr_bytes);
    out.extend_from_slice(&raw);
    out
}

pub fn _unused_shot_clock_guard(sc: u16) -> bool {
    decode_shot_clock_opt(sc).is_ok()
}
