use crate::canonical_le_bytes::CanonicalLeBytes;
use crate::hash::{
    compute_event_hash, compute_rolling_hash, compute_state_hash_with_buf, compute_tick_digest,
};
use crate::model::{
    AuditGroundTruthRecord, BoundedTickEventBuf, NullSimSink, PnrAuditEvent, ShotAuditSample,
    SimSink, GRAVITY_M_S2, SUB_DS_UNITS,
};
use crate::physics::{
    evaluate_flight_touch, run_swept_ccd_step, CcdActor, CcdDebugHooks, CourtGeometry,
    FlightTouchOutcome,
};
use crate::resolve::{
    compute_rebound_weight, compute_shot_probabilities, sample_intended_shot_outcome,
    select_rebound_winner, ShotEvalInputs,
};
use crate::rng::{sample_u53_checked, Philox4x32_10, RngBudgetExceeded, RngPurpose};
use crate::rotation::{canonical_actor_id, compute_fatigue_penalty};
use crate::rules::{
    advance_clocks_sub_ds, charge_team_timeout_with_mandatory_slots, compute_shot_clock_reset,
    inbound_window_permissions, is_team_in_bonus, set_clocks_exact_ds, ShotClockScenario,
};
use crate::tactics::{
    assign_playbook_roles, compute_drive_and_react_relocation, compute_offball_screen_read,
    evaluate_spacing_potential_field, evaluate_wp_hold, evaluate_wp_shoot,
    legal_actions_for_holder, observe_pnr_coverage, resolve_anchor_coord,
    resolve_safe_playbook_action, CandidateAction, EndgameWpContext, EpvEvaluator,
};
use crate::wire_schema_v1::*;
use thiserror::Error;

pub mod defense;
mod commands;
mod frames;
mod physics;
mod rotation;
mod rules_commit;

use defense::{DefensePlaybooks, TeamDefensePlan};

pub const MAX_SIM_TICKS: u32 = 200_000;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EngineInitError {
    #[error("MatchInitSpecV1 size {bytes} exceeds 256 KiB ({limit})")]
    InitSpecTooLarge { bytes: usize, limit: usize },
    #[error("invalid team roster count for team {side}")]
    InvalidRosterCount { side: u8 },
    #[error("invalid court rules: {0}")]
    InvalidCourtRules(&'static str),
    #[error("invalid checkpoint state")]
    InvalidCheckpoint,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ReplayCommandError {
    #[error("expected ingress_seq {expected}, got {got}")]
    IngressSeqMismatch { expected: u64, got: u64 },
    #[error("command quota exceeded")]
    QuotaExceeded,
    #[error("command rejected during replay: {0:?}")]
    Rejected(CommandRejectReason),
}

pub struct SimulationEngine {
    pub state: GameState,
    // 12 immutable config fields (not hashed into StateHash):
    pub seed: u64,
    pub philox_key: [u32; 2],
    pub canonical_team_ids: [u8; 2],
    pub intentional_foul_policy: IntentionalFoulPolicy,
    pub state_hash_mode: StateHashMode,
    pub config_hashes: ConfigHashes,
    pub ruleset: RuleSet,
    pub tunables: EngineTunables,
    pub playbooks: Vec<CompiledPlaybook>,
    pub defenses: DefensePlaybooks,
    pub badges: Vec<BadgeDef>,
    pub teams: [CompiledTeamSnapshot; 2],
    // Derived & transient per-tick buffers:
    pub geom: CourtGeometry,
    pub prev_rolling_hash: [u8; 32],
    pub current_tick_events: BoundedTickEventBuf<48>,
    pub last_hashes: ([u8; 32], [u8; 32], [u8; 32]),
    pub last_public_frame: PublicFrameV1,
    pub last_debug_frame: DebugOverlayFrameV1,
    pub pending_audit: AuditGroundTruthRecord,
    pub debug_ccd_hooks: CcdDebugHooks,
    transient_target_xy: [(f64, f64); 10],
    transient_epv_centi: [i16; 10],
    transient_debug_flags: [u8; 10],
    transient_def_rule_idx: u16,
    state_hash_scratch: Vec<u8>,
    event_wire_scratch: Vec<u8>,
}

impl SimulationEngine {
    pub fn new(init_spec: &MatchInitSpecV1, sink: &mut impl SimSink) -> Result<Self, EngineInitError> {
        let spec_bytes = init_spec.to_canonical_le_bytes();
        if spec_bytes.len() > MAX_INIT_SPEC_BYTES {
            return Err(EngineInitError::InitSpecTooLarge {
                bytes: spec_bytes.len(),
                limit: MAX_INIT_SPEC_BYTES,
            });
        }
        if init_spec.home_team.players.len() < 2
            || init_spec.home_team.players.len() > init_spec.ruleset.roster.max_team_players as usize
        {
            return Err(EngineInitError::InvalidRosterCount { side: 0 });
        }
        if init_spec.away_team.players.len() < 2
            || init_spec.away_team.players.len() > init_spec.ruleset.roster.max_team_players as usize
        {
            return Err(EngineInitError::InvalidRosterCount { side: 1 });
        }
        let geom = CourtGeometry::from_rules(&init_spec.ruleset.court);
        if init_spec.ruleset.court.length_m < 28.0
            || init_spec.ruleset.court.length_m > 50.0
            || init_spec.ruleset.court.width_m < 15.0
            || init_spec.ruleset.court.width_m > 30.0
            || init_spec.ruleset.court.oob_runoff_m < 2.0
            || init_spec.ruleset.court.oob_runoff_m > 6.0
        {
            return Err(EngineInitError::InvalidCourtRules("court bounds out of range"));
        }

        let canonical_team_ids = match init_spec.canonical_team_id_override {
            Some(ids) => ids,
            None => {
                if init_spec.home_team.team_id <= init_spec.away_team.team_id {
                    [0, 1]
                } else {
                    [1, 0]
                }
            }
        };

        let philox_key = Philox4x32_10::key_from_seed(init_spec.seed);
        let q_ds = init_spec.ruleset.clocks.quarter_sec * 10;
        let q_10us = (q_ds as u32) * SUB_DS_UNITS;
        let default_play = init_spec
            .playbooks
            .first()
            .map(|p| p.play_id)
            .unwrap_or_else(|| PlayId::from_str_lossy("freelance"));
        let pick_initial_play = |team: &CompiledTeamSnapshot| -> PlayId {
            for (pid_str, weight) in &team.tactics.play_weights {
                if *weight > 0.0 {
                    if let Some(pb) = init_spec
                        .playbooks
                        .iter()
                        .find(|p| p.play_id.as_str() == pid_str)
                    {
                        return pb.play_id;
                    }
                }
            }
            default_play
        };
        let home_init_play = pick_initial_play(&init_spec.home_team);
        let away_init_play = pick_initial_play(&init_spec.away_team);

        let mut players = [PlayerRuntimeState::default(); 30];
        let mut active_lineups = [[ROSTER_IDX_NONE; 5]; 2];
        for (team_idx, team) in [&init_spec.home_team, &init_spec.away_team]
            .iter()
            .enumerate()
        {
            let offset = team_idx * 15;
            for (rank, _) in team.players.iter().enumerate().take(15) {
                let r = offset + rank;
                players[r].is_present = true;
                if rank < 5 {
                    let slot = (team_idx * 5 + rank) as u8;
                    players[r].on_court_slot = Some(slot);
                    active_lineups[team_idx][rank] = r as u8;
                    // Tip-off center-circle alignment: pos 4 (Center) near center circle, others spaced naturally
                    let sign = if team_idx == 0 { -1.0 } else { 1.0 };
                    let (dx, dy) = match rank {
                        0 => (2.9, 0.0),
                        1 => (2.4, -2.8),
                        2 => (2.4, 2.8),
                        3 => (4.2, -1.6),
                        _ => (0.65, 0.0),
                    };
                    players[r].x_m = sign * dx;
                    players[r].y_m = sign * dy;
                    players[r].facing_rad = if team_idx == 0 {
                        0.0
                    } else {
                        core::f64::consts::PI
                    };
                }
            }
        }

        let initial_timeouts = if init_spec.ruleset.clocks.shot_clock_policy == ShotClockResetPolicy::FibaArt29 {
            init_spec.ruleset.timeouts.h1_to
        } else {
            init_spec.ruleset.timeouts.q_total_to
        };

        let state = GameState {
            sim_tick: 0,
            quarter: 1,
            game_clock_rem_10us: q_10us,
            game_clock_ds: q_ds,
            game_clock_running: false,
            game_clock_ticked_this_step: false,
            q1_game_clock_ever_started: false,
            shot_clock_rem_10us: None,
            shot_clock_ds: None,
            shot_clock_running: false,
            ball_in_frontcourt: false,
            backcourt_8s_timer_ds: 0,
            closely_guarded_timer_ds: 0,
            back_to_basket_timer_ds: 0,
            comments_iil_origin_clocks: None,
            possession_id: 0,
            possession_tick: 0,
            possession_side: None,
            possession_arrow: CommandSide::Home,
            opening_tip_winner: None,
            mandatory_timeout_state: MandatoryTimeoutState::default(),
            rng_sample_counters: RngSampleCounters::default(),
            next_ingress_seq: 1,
            player_commands_accepted_count: 0,
            system_commands_accepted_count: 0,
            replay_manifest_projected_bytes: spec_bytes.len() as u32,
            next_ruling_id: 1,
            next_event_seq: 1,
            next_shot_id: 1,
            next_foul_out_seq: 1,
            last_stoppage_resume_event_seq: 0,
            reviewable_ring_overflow_watermark_event_seq: 0,
            emitted_tick_event_cursor: 0,
            cursor: EngineCursor::ReadyForPreTick,
            game_phase: GamePhase::JumpBall,
            active_pending_ruling: None,
            pending_post_commit_timeout: None,
            pending_fiba_timeout_request: [false; 2],
            scramble_origin: None,
            deferred_def_3s_violation: None,
            deferred_flopping_tech: None,
            ft_series_queue: BoundedFtQueue::default(),
            inbound_context: None,
            jump_ball_context: Some(JumpBallContext {
                spot: JumpBallSpot::CenterCircle,
                jumpers: [4, 19],
                toss_timer_ds: 10,
                ball_tossed: false,
                ball_tapped: false,
                is_opening_tip: true,
            }),
            suspended_states: BoundedSuspendedStack::default(),
            pending_fiba_hcc_requests: [None, None],
            fiba_hcc_generation: [0, 0],
            recent_reviewable_events: BoundedReviewableEventRing::new(),
            dead_time_pending: None,
            end_reason: None,
            command_queue: BoundedCommandQueue::new(),
            pending_deadball_subs: BoundedSubQueue::new(),
            pending_deadball_timeouts: [None; 2],
            client_dedup_ring: [BoundedDedupWindow32::default(); 2],
            highest_client_request_id: [0; 2],
            active_play_id: [home_init_play, away_init_play],
            pending_next_play_id: [None, None],
            play_phase_idx: [0, 0],
            play_phase_elapsed_ds: [0, 0],
            play_mirrored: [false, false],
            slot_bindings: [[0, 1, 2, 3, 4], [0, 1, 2, 3, 4]],
            defense_plans: [
                TeamDefensePlan::new(init_spec.home_team.tactics.defensive_gameplan_id),
                TeamDefensePlan::new(init_spec.away_team.tactics.defensive_gameplan_id),
            ],
            active_pace: [
                init_spec.home_team.tactics.pace,
                init_spec.away_team.tactics.pace,
            ],
            full_court_press: [false, false],
            active_drive_corridor: 0,
            play_screen_contact_ds: [0, 0],
            transition_window_ds: [0, 0],
            shot_sub_stage: 0,
            shot_stage_ticks: 0,
            shot_gc_expired_pending: false,
            scramble_ticks: 0,
            inbound_sub_stage: 0,
            inbound_stage_ticks: 0,
            inbound_pass_start_m: [0.0, 0.0, 0.0],
            inbound_pass_target_m: [0.0, 0.0, 0.0],
            ft_sub_stage: 0,
            ft_stage_ticks: 0,
            ft_pending_made: false,
            tip_flight_ticks: 0,
            ball_fx_flags: 0,
            ball_fx_timer_ds: 0,
            slot_contact_timer_ds: [0; 10],
            oob_spot_xy: [0.0, 0.0],
            oob_is_endline: false,
            last_timeout_tick: [0, 0],
            scoring_run_pts: [0, 0],
            screen_ready_ticks: [0, 0],
            active_drive_target_xy: [0.0, 0.0],
            pnr_read_branch: [0, 0],
            xout_rotator_slot: [0xFF, 0xFF],
            xout_timer_ds: [0, 0],
            players,
            ball: BallRuntimeState {
                position_m: [0.0, 0.0, 2.5],
                velocity_m_s: [0.0, 0.0, 0.0],
                ball_state: BallState::Dead,
                owner_slot: None,
                last_touched_by_slot: None,
                last_team_control_side: None,
                shot_ctx: None,
                pass_ctx: None,
                live_ball_03s_restricted: false,
                rim_touched_this_shot: false,
                backboard_contact_this_shot: BackboardContact::None,
                passes_over_backboard_top_plane: false,
                last_passer_roster_idx: None,
                last_pass_completed_tick: 0,
            },
            team_states: [
                TeamRuntimeState {
                    timeouts_remaining: initial_timeouts,
                    challenges_remaining: init_spec.ruleset.challenges.max_base,
                    active_lineup_slots: active_lineups[0],
                    ..TeamRuntimeState::default()
                },
                TeamRuntimeState {
                    timeouts_remaining: initial_timeouts,
                    challenges_remaining: init_spec.ruleset.challenges.max_base,
                    active_lineup_slots: active_lineups[1],
                    ..TeamRuntimeState::default()
                },
            ],
        };

        let mut engine = Self {
            state,
            seed: init_spec.seed,
            philox_key,
            canonical_team_ids,
            intentional_foul_policy: init_spec.intentional_foul_policy,
            state_hash_mode: init_spec.state_hash_mode,
            config_hashes: init_spec.config_hashes.clone(),
            ruleset: init_spec.ruleset.clone(),
            tunables: init_spec.tunables.clone(),
            playbooks: init_spec.playbooks.clone(),
            defenses: DefensePlaybooks::new(init_spec.defenses.clone()),
            badges: init_spec.badges.clone(),
            teams: [init_spec.home_team.clone(), init_spec.away_team.clone()],
            geom,
            prev_rolling_hash: [0u8; 32],
            current_tick_events: BoundedTickEventBuf::new(),
            last_hashes: ([0u8; 32], [0u8; 32], [0u8; 32]),
            last_public_frame: PublicFrameV1::default(),
            last_debug_frame: DebugOverlayFrameV1::default(),
            pending_audit: AuditGroundTruthRecord::default(),
            debug_ccd_hooks: CcdDebugHooks::default(),
            transient_target_xy: [(f64::NAN, f64::NAN); 10],
            transient_epv_centi: [0; 10],
            transient_debug_flags: [0; 10],
            transient_def_rule_idx: 0xFFFF,
            state_hash_scratch: Vec::with_capacity(16_384),
            event_wire_scratch: Vec::with_capacity(4_096),
        };

        engine.push_event(GameRecordEventPayload::PeriodStarted {
            quarter: 1,
            start_mode: 0,
            inbounding_side: 0xFF,
            inbound_spot: InboundSpot::BackcourtEndline as u8,
            arrow_side: 0xFF,
        });
        engine.finalize_stage8_tick0(sink);
        Ok(engine)
    }

    pub fn capture_checkpoint_snapshot(&self) -> EngineCheckpointSnapshotV1 {
        EngineCheckpointSnapshotV1 {
            state: self.state.clone(),
            prev_rolling_hash: self.prev_rolling_hash,
            current_tick_events: self.current_tick_events.to_vec(),
        }
    }

    pub fn from_checkpoint_snapshot(
        init_spec: &MatchInitSpecV1,
        snapshot: EngineCheckpointSnapshotV1,
        sink: &mut impl SimSink,
    ) -> Result<Self, EngineInitError> {
        let mut null_sink = NullSimSink;
        let mut engine = Self::new(init_spec, &mut null_sink)?;
        engine.state = snapshot.state;
        engine.prev_rolling_hash = snapshot.prev_rolling_hash;
        engine.current_tick_events = BoundedTickEventBuf::from_vec(snapshot.current_tick_events);
        // Zero re-emission of [..emitted_tick_event_cursor]
        let (pub_frame, dbg_frame) = engine.build_frames(engine.state.sim_tick);
        engine.last_public_frame = pub_frame;
        engine.last_debug_frame = dbg_frame;
        let _ = sink;
        Ok(engine)
    }

    pub fn active_challenge_window_snapshot(&self) -> Option<ChallengeWindowSnapshotV1> {
        let r = self.state.active_pending_ruling.as_ref()?;
        Some(ChallengeWindowSnapshotV1 {
            ruling_id: r.ruling_id,
            whistle_tick: r.whistle_tick,
            window_open_tick: r.window_open_tick,
            window_epoch: r.window_epoch,
            request_seq: r.request_seq,
            initial_rule_code: r.perceived_call.rule_code as u16,
            window_state: r.window_state.tag(),
            window_kind_tag: r.window_kind.window_kind_tag(),
            eligible_sides_mask: r.eligible_sides.0,
            grant_requester_side: r
                .window_state
                .requester()
                .map_or(0xFF, |s| s as u8),
        })
    }

    /// Pure read-only Phase-1 admission check (§3.3). Never mutates `self.state` or calls `sink`.
    pub fn push_event(&mut self, payload: GameRecordEventPayload) {
        let seq = self.state.next_event_seq;
        self.state.next_event_seq += 1;
        let ev = GameRecordEventV1 {
            event_seq: seq,
            tick: self.state.sim_tick,
            game_clock_ds: self.state.game_clock_ds,
            shot_clock_ds: encode_shot_clock_opt(self.state.shot_clock_ds),
            quarter: self.state.quarter,
            payload,
        };
        self.current_tick_events.push(ev);
    }

    fn emit_new_events_to_sink(&mut self, sink: &mut impl SimSink) {
        let cursor = self.state.emitted_tick_event_cursor as usize;
        let slice = self.current_tick_events.as_slice();
        if cursor < slice.len() {
            for ev in &slice[cursor..] {
                sink.on_game_event(ev);
            }
            self.state.emitted_tick_event_cursor = slice.len() as u16;
        }
    }

    fn record_reviewable_event(&mut self, entry: ReviewableEventEntry) {
        let mut pinned = [0u64; 2];
        let mut pn = 0usize;
        for req in self.state.pending_fiba_hcc_requests.iter().flatten() {
            if pn < 2 {
                pinned[pn] = req.target_event_seq;
                pn += 1;
            }
        }
        if let Some(evicted) = self
            .state
            .recent_reviewable_events
            .push_with_eviction(entry, &pinned[..pn])
        {
            if evicted > self.state.reviewable_ring_overflow_watermark_event_seq {
                self.state.reviewable_ring_overflow_watermark_event_seq = evicted;
            }
        }
    }

    /// Unified 4-step Stage 8 finalizer (§2.3 / §3.3).
    fn run_stage8(&mut self, sink: &mut impl SimSink, is_tick0: bool) -> StepStatus {
        let t = self.state.sim_tick;
        if let Some(reason @ (GameEndReason::Normal | GameEndReason::MaxSimTicksExceeded)) = self.state.end_reason {
            self.record_game_ended(reason);
        }
        self.emit_new_events_to_sink(sink);

        let is_forfeit_fin = matches!(
            self.state.cursor,
            EngineCursor::PendingForfeitFinalize { .. }
        );
        let is_det_abort = self.state.end_reason == Some(GameEndReason::DeterminismInvariantViolation);

        if !is_tick0 && !is_forfeit_fin && !is_det_abort {
            // Every active/deadball step advances possession_tick; roll over at 60_000
            self.state.possession_tick = self
                .state
                .possession_tick
                .checked_add(1)
                .unwrap_or(60_000);
            if self.state.possession_tick >= 60_000 {
                self.state.possession_id = self.state.possession_id.saturating_add(1);
                self.state.possession_tick = 0;
            }
        }

        // Step (1): write final cursor & reset per-tick fields BEFORE hashing
        if self.state.game_phase == GamePhase::GameOver {
            self.state.cursor = EngineCursor::GameFinished;
        } else {
            self.state.cursor = EngineCursor::ReadyForPreTick;
        }
        self.state.rng_sample_counters = RngSampleCounters::default();
        self.state.emitted_tick_event_cursor = 0;

        // Step (2): compute 4-domain SHA-256 hash chain
        let (state_hash, event_hash, rolling_hash) = if self.state_hash_mode == StateHashMode::PerTickRolling {
            let sh = compute_state_hash_with_buf(&self.state, &mut self.state_hash_scratch);
            self.event_wire_scratch.clear();
            for ev in self.current_tick_events.as_slice() {
                ev.write_le(&mut self.event_wire_scratch);
            }
            let eh = compute_event_hash(
                self.current_tick_events.len() as u16,
                &self.event_wire_scratch,
            );
            let td = compute_tick_digest(t, sh, eh);
            let rh = compute_rolling_hash(self.prev_rolling_hash, td);
            self.prev_rolling_hash = rh;
            (sh, eh, rh)
        } else {
            ([0u8; 32], [0u8; 32], [0u8; 32])
        };
        self.last_hashes = (state_hash, event_hash, rolling_hash);

        // Step (3): emit frames, hashes, audit & clear current_tick_events
        let (pub_frame, dbg_frame) = self.build_frames(t);
        self.last_public_frame = pub_frame;
        self.last_debug_frame = dbg_frame;
        sink.on_public_frame(&pub_frame);
        sink.on_debug_frame(&dbg_frame);
        sink.on_tick_hashes(t, state_hash, event_hash, rolling_hash);

        let mut audit = core::mem::take(&mut self.pending_audit);
        audit.tick = t;
        sink.on_audit(&audit);

        self.current_tick_events.clear();

        // Step (4): return StepStatus
        if is_det_abort {
            StepStatus::Aborted {
                tick: t,
                reason: DeterminismViolationKind::RngCounterSaturated,
            }
        } else if self.state.game_phase == GamePhase::GameOver {
            StepStatus::Finished { tick: t }
        } else {
            self.state.sim_tick = t + 1;
            StepStatus::TickCompleted { tick: t }
        }
    }

    fn finalize_stage8_tick0(&mut self, sink: &mut impl SimSink) {
        let _ = self.run_stage8(sink, true);
    }

    pub fn abort_match_determinism(&mut self, err: RngBudgetExceeded, sink: &mut impl SimSink) -> StepStatus {
        self.pending_audit.warnings.push(AuditWarning::RngCounterSaturated {
            actor_slot: err.actor_slot,
            purpose: err.purpose as u8,
            budget: err.budget,
        });
        self.state.game_phase = GamePhase::GameOver;
        self.state.end_reason = Some(GameEndReason::DeterminismInvariantViolation);
        self.state.command_queue.clear();
        let mut court_times = [0u32; 30];
        for (i, p) in self.state.players.iter().enumerate() {
            court_times[i] = p.court_time_live_ds;
        }
        self.push_event(GameRecordEventPayload::GameEnded {
            score_home_official: self.state.team_states[0].score_on_court,
            score_away_official: self.state.team_states[1].score_on_court,
            on_court_score_home: self.state.team_states[0].score_on_court,
            on_court_score_away: self.state.team_states[1].score_on_court,
            winner_side: 0xFF,
            end_reason: GameEndReason::DeterminismInvariantViolation as u8,
            final_quarter: self.state.quarter,
            final_court_time_ds: FinalCourtTimeDs(court_times),
        });
        self.run_stage8(sink, false)
    }

    /// Deterministically step 1 tick across the 8-stage pipeline (§3.1 / §3.3).
    pub fn step(&mut self, sink: &mut impl SimSink) -> StepStatus {
        match self.state.cursor {
            EngineCursor::GameFinished => {
                if self.state.end_reason == Some(GameEndReason::DeterminismInvariantViolation) {
                    return StepStatus::Aborted {
                        tick: self.state.sim_tick,
                        reason: DeterminismViolationKind::RngCounterSaturated,
                    };
                }
                return StepStatus::Finished {
                    tick: self.state.sim_tick,
                };
            }
            EngineCursor::PendingForfeitFinalize { .. } => {
                return self.run_stage8(sink, false);
            }
            EngineCursor::PausedInPostWhistle {
                window_open_tick,
                ruling_id,
            } => {
                let closed = self
                    .state
                    .active_pending_ruling
                    .as_ref()
                    .map_or(true, |r| {
                        matches!(r.window_state, ChallengeWindowState::ClosedCommitted)
                    });
                if !closed {
                    let ws = self
                        .state
                        .active_pending_ruling
                        .as_ref()
                        .map_or(ChallengeWindowState::Open, |r| r.window_state);
                    return StepStatus::AwaitingPostWhistle {
                        window_open_tick,
                        ruling_id,
                        window_state: ws,
                    };
                }
                // Stage 6c -> Stage 7 -> Stage 8
                self.execute_commit_point();
                self.execute_stage7_dynamics();
                return self.run_stage8(sink, false);
            }
            EngineCursor::ReadyForPreTick => {
                self.state.game_clock_ticked_this_step = false;
                self.transient_epv_centi = [0; 10];
                self.transient_debug_flags = [0; 10];
                self.transient_def_rule_idx = 0xFFFF;
            }
        }

        // Safety cap max_sim_ticks = 200_000
        if self.state.sim_tick >= MAX_SIM_TICKS {
            self.end_game_normal_or_cap(GameEndReason::MaxSimTicksExceeded);
            return self.run_stage8(sink, false);
        }

        // Decay transient ball FX flags and physical body-contact timers once per discrete step
        if self.state.ball_fx_timer_ds > 0 {
            self.state.ball_fx_timer_ds -= 1;
            if self.state.ball_fx_timer_ds == 0 {
                self.state.ball_fx_flags = 0;
            }
        }
        for c in self.state.slot_contact_timer_ds.iter_mut() {
            if *c > 0 {
                *c -= 1;
            }
        }

        // Stage 1: Dequeue PreTick commands for current sim_tick
        let cur_tick = self.state.sim_tick;
        let ready = self
            .state
            .command_queue
            .pop_ready_for_tick_phase(cur_tick, CommandPhase::PreTick);
        for (idx, cmd) in ready.iter().enumerate() {
            if let CommandPayload::Player(PlayerCoachCommand::ForfeitMatch) = cmd.payload {
                // Emit 0x18 (0x0A) for remaining already-dequeued PreTick commands on this tick (<= 7)
                for rem in &ready[idx + 1..] {
                    self.push_event(GameRecordEventPayload::AnchoredCommandAbortedRecord {
                        ingress_seq: rem.ingress_seq,
                        side: rem.side as u8,
                        abort_reason_code: CommandAbortReason::SupercededByLaterForfeit as u8,
                        cmd_tag: rem.payload.cmd_tag(),
                    });
                }
                self.execute_forfeit_match(cmd.side, sink);
                return self.run_stage8(sink, false);
            }
            self.execute_pretick_command(*cmd);
        }

        // Handle 1-Tick DeadTime states (Timeout / PeriodBreak) directly
        if self.state.game_phase == GamePhase::Timeout {
            self.step_deadtime_timeout();
            self.execute_stage7_dynamics();
            return self.run_stage8(sink, false);
        }
        if self.state.game_phase == GamePhase::PeriodBreak {
            self.step_deadtime_period_break();
            self.execute_stage7_dynamics();
            return self.run_stage8(sink, false);
        }

        if self.state.game_phase == GamePhase::InboundSetup
            && self.state.inbound_context.as_ref().is_some_and(|inbound| !inbound.inbound_touched_court_player)
        {
            self.consume_fiba_scoring_reviews();
        }

        // Stage 2: Coach & Rotation (AI subs on deadballs)
        self.execute_stage2_coach_and_rotation();

        // Stage 3 & 4 & 5 & 6a (with RNG budget protection)
        match self.execute_stages_3_to_6a() {
            Ok(()) => {}
            Err(rng_err) => return self.abort_match_determinism(rng_err, sink),
        }

        // Stage 6a check: if active_pending_ruling was created and system quota allows opening window
        if self.state.active_pending_ruling.is_some() {
            self.consume_fiba_scoring_reviews();
            if self.state.system_commands_accepted_count < MAX_SYSTEM_COMMANDS_PER_GAME {
                let r = self.state.active_pending_ruling.as_ref().unwrap();
                let rid = r.ruling_id;
                let ws = r.window_state;
                self.state.cursor = EngineCursor::PausedInPostWhistle {
                    window_open_tick: cur_tick,
                    ruling_id: rid,
                };
                self.emit_new_events_to_sink(sink);
                return StepStatus::AwaitingPostWhistle {
                    window_open_tick: cur_tick,
                    ruling_id: rid,
                    window_state: ws,
                };
            } else {
                let rid = self
                    .state
                    .active_pending_ruling
                    .as_ref()
                    .map_or(0, |r| r.ruling_id);
                self.pending_audit
                    .warnings
                    .push(AuditWarning::SystemCommandQuotaExhaustedWindowBypassed {
                        tick: cur_tick,
                        ruling_id: rid,
                    });
                if let Some(ref mut r) = self.state.active_pending_ruling {
                    r.window_state = ChallengeWindowState::ClosedCommitted;
                }
            }
        }

        // Stage 6c -> Stage 7 -> Stage 8
        self.execute_commit_point();
        self.execute_stage7_dynamics();
        self.run_stage8(sink, false)
    }

    fn start_offensive_possession(
        &mut self,
        off_idx: usize,
        is_ato_inbound: bool,
        is_fast_break: bool,
    ) {
        self.state.play_phase_idx[off_idx] = 0;
        self.state.play_phase_elapsed_ds[off_idx] = 0;
        self.state.play_screen_contact_ds[off_idx] = 0;
        self.state.active_drive_corridor = 0;
        self.state.screen_ready_ticks[off_idx] = 0;
        self.state.active_drive_target_xy = [0.0, 0.0];
        self.state.pnr_read_branch[off_idx] = 0;
        self.state.xout_rotator_slot[1 - off_idx] = 0xFF;
        self.state.xout_timer_ds[1 - off_idx] = 0;
        self.assign_team_matchups_for_defense(1 - off_idx);

        let push = (self.teams[off_idx].tactics.transition_push as f64) / 100.0;
        self.state.transition_window_ds[off_idx] = if is_fast_break {
            (20.0 + push * 26.0).round().clamp(14.0, 48.0) as u8
        } else {
            0
        };
        self.state.transition_window_ds[1 - off_idx] = 0;

        if self.playbooks.is_empty() {
            return;
        }

        let words = Philox4x32_10::generate(
            self.philox_key,
            [
                self.state.possession_id,
                ((self.canonical_team_ids[off_idx] as u32) << 24) | 0x0050_4C59,
                self.state.quarter as u32,
                self.state.sim_tick,
            ],
        );
        let u = Philox4x32_10::words_to_u53_low(words);
        let mirror_u = Philox4x32_10::words_to_u53_high(words);

        if let Some(pending_pid) = self.state.pending_next_play_id[off_idx].take() {
            if let Some(pb) = self.playbooks.iter().find(|p| p.play_id == pending_pid) {
                self.state.active_play_id[off_idx] = pb.play_id;
                self.state.play_mirrored[off_idx] = pb.allow_mirror && mirror_u >= 0.5;
                return;
            }
        }

        let sc_ds = self
            .state
            .shot_clock_ds
            .unwrap_or_else(|| self.state.game_clock_ds.min(240));
        let is_ato_window = sc_ds <= 105 || is_ato_inbound;

        let team_weights = &self.teams[off_idx].tactics.play_weights;
        let prev_play = self.state.active_play_id[off_idx];
        let mut weighted: Vec<(&CompiledPlaybook, f64)> = Vec::with_capacity(self.playbooks.len());
        let mut total_w = 0.0;

        for pb in &self.playbooks {
            if sc_ds < pb.min_shot_clock_ds {
                continue;
            }
            let base_w = team_weights
                .iter()
                .find(|(pid, _)| pid == pb.play_id.as_str())
                .map(|(_, w)| *w)
                .unwrap_or(0.05);
            if base_w <= 0.0 {
                continue;
            }
            let mut w = base_w;
            if is_ato_window && pb.play_id.as_str() == "ato_quick_three" {
                w *= 2.8;
            } else if pb.play_id == prev_play {
                w *= 0.35;
            }
            total_w += w;
            weighted.push((pb, w));
        }

        if weighted.is_empty() {
            for pb in &self.playbooks {
                let mut w = team_weights
                    .iter()
                    .find(|(pid, _)| pid == pb.play_id.as_str())
                    .map(|(_, v)| *v)
                    .unwrap_or(0.1)
                    .max(0.01);
                if pb.play_id == prev_play && self.playbooks.len() > 1 {
                    w *= 0.4;
                }
                total_w += w;
                weighted.push((pb, w));
            }
        }

        let mut chosen = weighted[0].0;
        if total_w > 0.0 {
            let target = u * total_w;
            let mut acc = 0.0;
            for &(pb, w) in &weighted {
                acc += w;
                if target <= acc {
                    chosen = pb;
                    break;
                }
            }
        }

        self.state.active_play_id[off_idx] = chosen.play_id;
        self.state.play_mirrored[off_idx] = chosen.allow_mirror && mirror_u >= 0.5;
    }

    fn inbound_spot_targets(
        &self,
        inb: &InboundContext,
        attack_dir: f64,
    ) -> ((f64, f64), (f64, f64)) {
        let side_y = match inb.sideline_side {
            CourtSideLine::TableSide => -(self.geom.half_width_m + 0.45),
            CourtSideLine::OppositeTableSide => self.geom.half_width_m + 0.45,
        };
        let side_in_y = match inb.sideline_side {
            CourtSideLine::TableSide => -(self.geom.half_width_m - 2.3),
            CourtSideLine::OppositeTableSide => self.geom.half_width_m - 2.3,
        };
        let end_y = if inb.post_made_fg || inb.post_made_last_ft {
            2.3
        } else {
            -2.3
        };
        match inb.spot {
            InboundSpot::BackcourtEndline => (
                (-attack_dir * (self.geom.half_length_m + 0.45), end_y),
                (-attack_dir * (self.geom.half_length_m - 2.9), end_y * 0.35),
            ),
            InboundSpot::CenterLineExtendedOppositeTable => {
                ((0.0, side_y), (-attack_dir * 1.4, side_in_y))
            }
            InboundSpot::FrontcourtSideline28ft => {
                let ix = attack_dir * (self.geom.half_length_m - 8.5344);
                ((ix, side_y), (ix - attack_dir * 1.1, side_in_y))
            }
            InboundSpot::FibaFrontcourtThrowInLine => {
                let ix = attack_dir * (self.geom.half_length_m - 8.325);
                ((ix, side_y), (ix - attack_dir * 1.0, side_in_y))
            }
            InboundSpot::FreeThrowLineExtended => {
                let ix = attack_dir * (self.geom.half_length_m - self.geom.paint_length_m);
                ((ix, side_y), (ix - attack_dir * 1.4, side_in_y))
            }
            InboundSpot::NearestToInterruption | InboundSpot::OriginalInboundSpot => {
                if self.state.oob_is_endline {
                    let sign_x = if self.state.oob_spot_xy[0] >= 0.0 {
                        1.0
                    } else {
                        -1.0
                    };
                    let mut iy = self.state.oob_spot_xy[1]
                        .clamp(-self.geom.half_width_m + 1.0, self.geom.half_width_m - 1.0);
                    if libm::fabs(iy) < 1.5 {
                        iy = if iy >= 0.0 { 1.8 } else { -1.8 };
                    }
                    let rx = sign_x * (self.geom.half_length_m - 2.4);
                    ((sign_x * (self.geom.half_length_m + 0.45), iy), (rx, iy * 0.65))
                } else {
                    let ix = self.state.oob_spot_xy[0]
                        .clamp(-self.geom.half_length_m + 1.2, self.geom.half_length_m - 1.2);
                    ((ix, side_y), (ix - attack_dir * 1.0, side_in_y))
                }
            }
        }
    }

    fn preferred_handler_pos(&self, team: usize, excluded: Option<usize>) -> usize {
        let lineup = &self.state.team_states[team].active_lineup_slots;
        let eligible = |slot: usize| Some(slot) != excluded && lineup[slot] != ROSTER_IDX_NONE;
        for &rank in self.teams[team].tactics.primary_handlers.as_slice() {
            let roster_idx = team * 15 + usize::from(rank);
            if let Some(slot) = (0..5).find(|&slot| eligible(slot) && usize::from(lineup[slot]) == roster_idx) {
                return slot;
            }
        }
        (0..5).find(|&slot| eligible(slot)).unwrap_or(0)
    }

    fn setup_deadball_inbound_formation(
        &mut self,
        off_idx: usize,
        def_idx: usize,
        inb_xy: (f64, f64),
        recv_xy: (f64, f64),
        inbounder_pos: usize,
    ) {
        let receiver_pos = self.preferred_handler_pos(off_idx, Some(inbounder_pos));
        self.reset_active_player_transient_states();
        let attack_dir = if off_idx == 0 { 1.0 } else { -1.0 };
        let rim_xy = self.geom.rim_xy(attack_dir);
        let frontcourt_base = (inb_xy.0 * attack_dir) > -2.0;
        let nav_bounds = self.geom.inbounds_nav_bounds(0.65);

        for p_pos in 0..5 {
            let r = self.state.team_states[off_idx].active_lineup_slots[p_pos];
            if r == ROSTER_IDX_NONE {
                continue;
            }
            let (cx, cy) = if p_pos == inbounder_pos {
                let rad = self.teams[off_idx].players[(r as usize) % 15]
                    .physical
                    .collision_radius_m();
                self.geom.playable_bounds(rad).clamp(inb_xy.0, inb_xy.1)
            } else {
                let (tx, ty) = if p_pos == receiver_pos {
                    recv_xy
                } else if frontcourt_base {
                    match p_pos {
                        1 => (attack_dir * (self.geom.half_length_m - 1.5), -6.2),
                        2 => (attack_dir * (self.geom.half_length_m - 1.5), 6.2),
                        3 => (rim_xy.0 - attack_dir * 5.2, -3.6),
                        _ => (rim_xy.0 - attack_dir * 4.0, 2.8),
                    }
                } else {
                    match p_pos {
                        1 => (attack_dir * 1.8, -4.2),
                        2 => (attack_dir * 1.8, 4.2),
                        3 => (attack_dir * 6.5, -3.2),
                        _ => (attack_dir * 5.0, 2.2),
                    }
                };
                nav_bounds.clamp(tx, ty)
            };
            let p = &mut self.state.players[r as usize];
            p.x_m = cx;
            p.y_m = cy;
            p.vx_m_s = 0.0;
            p.vy_m_s = 0.0;
            p.facing_rad = libm::atan2(rim_xy.1 - cy, rim_xy.0 - cx);
            p.action_state = PlayerActionState::Idle;
            p.action_timer_ds = 0;
        }

        for p_pos in 0..5 {
            let dr = self.state.team_states[def_idx].active_lineup_slots[p_pos];
            if dr == ROSTER_IDX_NONE {
                continue;
            }
            let opp_pos = (self.state.defense_plans[def_idx].matchups()[p_pos] as usize).min(4);
            let opp_r = self.state.team_states[off_idx].active_lineup_slots[opp_pos];
            let (mx, my) = if opp_r != ROSTER_IDX_NONE {
                (
                    self.state.players[opp_r as usize].x_m,
                    self.state.players[opp_r as usize].y_m,
                )
            } else {
                (rim_xy.0 - attack_dir * 3.0, 0.0)
            };
            // Position between matchup and defended rim, strictly inbounds
            let vx = rim_xy.0 - mx;
            let vy = rim_xy.1 - my;
            let vn = libm::hypot(vx, vy).max(0.5);
            let gap = (if opp_pos == inbounder_pos { 2.4_f64 } else { 1.5_f64 }).min(vn * 0.7);
            let raw_tx = mx + (vx / vn) * gap;
            let final_tx = if frontcourt_base {
                raw_tx
            } else {
                raw_tx * 0.45 + (rim_xy.0 - attack_dir * 4.5) * 0.55
            };
            let (cx, cy) = nav_bounds.clamp(final_tx, my + (vy / vn) * gap);
            let dp = &mut self.state.players[dr as usize];
            dp.x_m = cx;
            dp.y_m = cy;
            dp.vx_m_s = 0.0;
            dp.vy_m_s = 0.0;
            dp.facing_rad = libm::atan2(inb_xy.1 - cy, inb_xy.0 - cx);
            dp.action_state = PlayerActionState::Idle;
            dp.action_timer_ds = 0;
        }
    }

    fn execute_stages_3_to_6a(&mut self) -> Result<(), RngBudgetExceeded> {
        match self.state.game_phase {
            GamePhase::JumpBall => self.step_jump_ball(),
            GamePhase::InboundSetup => self.step_inbound_setup(),
            GamePhase::FreeThrows => self.step_free_throws(),
            GamePhase::ShotInFlight => self.step_shot_in_flight(),
            GamePhase::ReboundScramble => self.step_rebound_scramble(),
            GamePhase::LiveBall => self.step_live_ball(),
            _ => Ok(()),
        }
    }

    fn step_jump_ball(&mut self) -> Result<(), RngBudgetExceeded> {
        self.step_player_vertical_kinematics();
        let mut jb = match self.state.jump_ball_context.take() {
            Some(j) => j,
            None => {
                self.state.game_phase = GamePhase::LiveBall;
                return Ok(());
            }
        };
        let circle_x = match jb.spot {
            JumpBallSpot::HomeFreeThrowCircle => -(self.geom.half_length_m - self.geom.paint_length_m),
            JumpBallSpot::AwayFreeThrowCircle => self.geom.half_length_m - self.geom.paint_length_m,
            JumpBallSpot::NearestCircle => self.state.oob_spot_xy[0]
                .clamp(-self.geom.half_length_m + 5.8, self.geom.half_length_m - 5.8),
            JumpBallSpot::CenterCircle => 0.0,
        };
        // Position centers at the jump circle and remaining players smoothly around circle
        for team_idx in 0..2 {
            let sign = if team_idx == 0 { -1.0 } else { 1.0 };
            for p_pos in 0..5 {
                let r = self.state.team_states[team_idx].active_lineup_slots[p_pos];
                if r == ROSTER_IDX_NONE {
                    continue;
                }
                let (raw_x, raw_y) = match p_pos {
                    0 => (circle_x + sign * 2.8, 0.0),
                    1 => (circle_x + sign * 2.4, -2.7),
                    2 => (circle_x + sign * 2.4, 2.7),
                    3 => (circle_x + sign * 4.0, -1.5),
                    _ => (circle_x + sign * 0.65, 0.0),
                };
                let (tx, ty) = self.geom.inbounds_nav_bounds(0.6).clamp(raw_x, raw_y);
                self.transient_target_xy[team_idx * 5 + p_pos] = (tx, ty);
                let p = &mut self.state.players[r as usize];
                p.oob_reentry_pending = false;
                let max_acc = 7.5;
                Self::steer_player_velocity(p, tx - p.x_m, ty - p.y_m, 4.5, max_acc);
                p.x_m += p.vx_m_s * 0.1;
                p.y_m += p.vy_m_s * 0.1;
                p.facing_rad = libm::atan2(-p.y_m, circle_x - p.x_m);
                p.action_state = if p_pos == 4 && jb.ball_tossed {
                    PlayerActionState::ReboundJump
                } else {
                    PlayerActionState::Idle
                };
            }
        }

        if !jb.ball_tossed {
            if jb.toss_timer_ds > 0 {
                jb.toss_timer_ds -= 1;
            }
            if jb.toss_timer_ds <= 5 {
                jb.ball_tossed = true;
                self.state.ball.position_m = [circle_x, 0.0, 2.5];
                self.state.ball.velocity_m_s = [0.0, 0.0, 4.2];
                self.state.ball.ball_state = BallState::Loose;
                self.state.ball.last_passer_roster_idx = None;
                for t_idx in 0..2 {
                    let jr = jb.jumpers[t_idx];
                    if (jr as usize) < 30 {
                        let rank = (jr as usize) % 15;
                        let vert = self.teams[t_idx]
                            .players
                            .get(rank)
                            .map_or(75.0, |pl| f64::from(pl.attributes[AttrIdx::Vertical]));
                        self.state.players[jr as usize].vz_m_s = 2.6 + (vert / 99.0) * 1.4;
                        self.state.players[jr as usize].action_state = PlayerActionState::ReboundJump;
                        self.state.players[jr as usize].action_timer_ds = 6;
                    }
                }
            }
            self.state.jump_ball_context = Some(jb);
            return Ok(());
        }

        // Advance tossed jump ball under gravity until downward descent (`vz < 0.0`)
        self.state.ball.position_m[2] += self.state.ball.velocity_m_s[2] * 0.1;
        self.state.ball.velocity_m_s[2] -= GRAVITY_M_S2 * 0.1;

        if self.state.ball.velocity_m_s[2] < 0.0 {
            // Legal tip once past apex (`ball_vz_m_s < 0.0`, FIBA Art. 12.2.4) weighted by standing reach & vertical
            let reach_score = |t_idx: usize, r_idx: u8| -> f64 {
                if (r_idx as usize) >= 30 {
                    return 290.0;
                }
                let rank = (r_idx as usize) % 15;
                self.teams[t_idx].players.get(rank).map_or(290.0, |pl| {
                    pl.physical.standing_reach_cm + f64::from(pl.attributes[AttrIdx::Vertical]) * 0.65
                })
            };
            let reach_h = reach_score(0, jb.jumpers[0]);
            let reach_a = reach_score(1, jb.jumpers[1]);
            let p_home = (1.0 / (1.0 + libm::exp(-(reach_h - reach_a) / 14.0))).clamp(0.18, 0.82);

            let u = sample_u53_checked(
                self.philox_key,
                self.state.possession_id,
                self.state.possession_tick,
                &mut self.state.rng_sample_counters.0,
                0xFF,
                RngPurpose::ReboundBounce,
            )?;
            let winner_side = if u < p_home {
                CommandSide::Home
            } else {
                CommandSide::Away
            };
            let winner_idx = winner_side.team_index().unwrap_or(0);
            let tapper = jb.jumpers[winner_idx];
            jb.ball_tapped = true;
            self.state.opening_tip_winner = if jb.is_opening_tip {
                Some(winner_side)
            } else {
                self.state.opening_tip_winner
            };
            if jb.is_opening_tip {
                self.state.possession_arrow = winner_side.opponent();
            }
            self.state.game_clock_running = true;
            let _ = advance_clocks_sub_ds(&mut self.state, 0.8);
            self.state.game_phase = GamePhase::LiveBall;
            self.state.possession_side = None;
            self.state.tip_flight_ticks = 4;
            self.state.ball.ball_state = BallState::Loose;
            self.state.ball.last_touched_by_slot = self.state.players
                .get(tapper as usize)
                .and_then(|player| player.on_court_slot)
                .or(Some((winner_idx * 5 + 4) as u8));
            let receiver = self.preferred_handler_pos(winner_idx, None);
            let pg_r = self.state.team_states[winner_idx].active_lineup_slots[receiver];
            if pg_r != ROSTER_IDX_NONE {
                let pg = &self.state.players[pg_r as usize];
                let flight_s = 0.4;
                let z0 = self.state.ball.position_m[2];
                self.state.ball.velocity_m_s = [
                    (pg.x_m - self.state.ball.position_m[0]) / flight_s,
                    (pg.y_m - self.state.ball.position_m[1]) / flight_s,
                    (1.25 - z0 + 0.5 * GRAVITY_M_S2 * flight_s * flight_s) / flight_s,
                ];
            }
            self.push_event(GameRecordEventPayload::JumpBallTapped {
                tip_winner_side: winner_side as u8,
                tapper_roster_idx: tapper,
                ball_tapped: 1,
            });
            return Ok(());
        }

        self.state.jump_ball_context = Some(jb);
        Ok(())
    }

    fn step_inbound_setup(&mut self) -> Result<(), RngBudgetExceeded> {
        self.step_player_vertical_kinematics();
        let mut inb = match self.state.inbound_context.take() {
            Some(i) => i,
            None => {
                self.state.game_phase = GamePhase::LiveBall;
                return Ok(());
            }
        };
        let team_idx = inb.side.team_index().unwrap_or(0);
        let def_idx = 1 - team_idx;
        let attack_dir = if team_idx == 0 { 1.0 } else { -1.0 };
        let inbounder_pos = if inb.spot == InboundSpot::BackcourtEndline { 4 } else { 3 };
        let inbounder_slot = (team_idx * 5 + inbounder_pos) as u8;
        let receiver_pos = self.preferred_handler_pos(team_idx, Some(inbounder_pos));
        let pg_slot = (team_idx * 5 + receiver_pos) as u8;
        let (inb_xy, recv_xy) = self.inbound_spot_targets(&inb, attack_dir);
        let rim_xy = self.geom.rim_xy(-attack_dir);

        if !inb.inbound_ball_at_disposal {
            self.state.inbound_context = Some(inb);
            if self.consume_queued_timeout() {
                return Ok(());
            }
            inb = self.state.inbound_context.take().unwrap();
        }

        // AI coach timeout on opponent scoring run or clutch Q4/OT run (strictly respecting inbound_window_permissions)
        if (self.state.quarter > 1 || self.state.q1_game_clock_ever_started)
            && !inb.inbound_ball_at_disposal
            && (inb.post_made_fg || inb.post_made_last_ft)
            && self.state.inbound_stage_ticks == 0
        {
            let perms = inbound_window_permissions(&self.state, &inb, &self.ruleset);
            let can_to = if team_idx == 0 {
                perms.home_can_timeout
            } else {
                perms.away_can_timeout
            };
            let opp_run = self.state.scoring_run_pts[def_idx];
            let deficit = (self.state.team_states[def_idx].score_on_court as i32)
                - (self.state.team_states[team_idx].score_on_court as i32);
            let want_to = opp_run >= 8
                || (self.state.quarter >= 4
                    && self.state.game_clock_ds <= 900
                    && (1..=6).contains(&deficit)
                    && opp_run >= 2);
            if can_to
                && want_to
                && self.state.team_states[team_idx].timeouts_remaining > 0
                && self.state.sim_tick.saturating_sub(self.state.last_timeout_tick[team_idx]) >= 1200
            {
                if let Some((rtag, dur, ch)) =
                    charge_team_timeout_with_mandatory_slots(&mut self.state, inb.side, &self.ruleset)
                {
                    self.state.last_timeout_tick[team_idx] = self.state.sim_tick;
                    self.state.scoring_run_pts = [0, 0];
                    if self.ruleset.challenges.mode == 2 {
                        inb.allow_both_sides_advance = true;
                    }
                    self.state.inbound_context = Some(inb);
                    self.enter_timeout_state(rtag, dur, ch);
                    return Ok(());
                }
            }
        }

        // Helper closure to advance live clock cleanly during running-clock post-make inbounds
        let adv_live_inbound_clock = |engine: &mut Self, inb_ctx: &InboundContext| -> bool {
            if !inb_ctx.stop_clock_until_touch && engine.state.game_clock_running {
                let (_, gc_exp, _) = advance_clocks_sub_ds(&mut engine.state, 1.0);
                if gc_exp {
                    engine.handle_period_expiration();
                    return true;
                }
            }
            false
        };

        // For deadball inbounds (fouls, OOB, start of period), align cleanly in Stage 1 immediately
        if !inb.post_made_fg && !inb.post_made_last_ft && self.state.inbound_sub_stage == 0 {
            self.state.inbound_sub_stage = 1;
            self.state.inbound_stage_ticks = 0;
            self.setup_deadball_inbound_formation(team_idx, def_idx, inb_xy, recv_xy, inbounder_pos);
            self.state.ball.ball_state = BallState::Held;
            self.state.ball.owner_slot = Some(inbounder_slot);
            self.state.ball.last_touched_by_slot = Some(inbounder_slot);
            self.state.ball.last_passer_roster_idx = None;
            self.state.ball.position_m = [inb_xy.0, inb_xy.1, 1.45];
            self.state.ball.velocity_m_s = [0.0, 0.0, 0.0];
            inb.inbound_ball_at_disposal = true;
        }

        // Stage 0 (NetDrop): Ball falls visibly through the net & bounces softly under basket after a make
        if self.state.inbound_sub_stage == 0 {
            self.state.inbound_stage_ticks = self.state.inbound_stage_ticks.saturating_add(1);
            self.state.ball.ball_state = BallState::Loose;
            self.state.ball.owner_slot = None;
            self.state.ball.last_passer_roster_idx = None;
            for k in 0..3 {
                self.state.ball.position_m[k] += self.state.ball.velocity_m_s[k] * 0.1;
            }
            self.state.ball.velocity_m_s[2] -= GRAVITY_M_S2 * 0.1;
            if self.state.ball.position_m[2] <= 0.12 {
                self.state.ball.position_m[2] = 0.12;
                self.state.ball.velocity_m_s[2] =
                    (-self.state.ball.velocity_m_s[2] * 0.46).max(0.0);
                self.state.ball.velocity_m_s[0] *= 0.6;
                self.state.ball.velocity_m_s[1] *= 0.6;
            }
            self.step_inbound_approach_kinematics(team_idx, def_idx, inb_xy, recv_xy, inbounder_pos)?;
            if adv_live_inbound_clock(self, &inb) {
                return Ok(());
            }
            if self.state.inbound_stage_ticks >= 5 {
                self.state.inbound_sub_stage = 1;
                self.state.inbound_stage_ticks = 0;
            }
            self.state.inbound_context = Some(inb);
            return Ok(());
        }

        // Stage 1 (RetrieveAndSet): Inbounder steps behind line with ball & scans while teammates break upcourt
        if self.state.inbound_sub_stage == 1 {
            self.state.inbound_stage_ticks = self.state.inbound_stage_ticks.saturating_add(1);
            self.step_inbound_approach_kinematics(team_idx, def_idx, inb_xy, recv_xy, inbounder_pos)?;

            let inb_r = self.state.team_states[team_idx].active_lineup_slots[inbounder_pos];
            let (mut inb_x, mut inb_y) = if inb_r != ROSTER_IDX_NONE {
                let p = &self.state.players[inb_r as usize];
                (p.x_m, p.y_m)
            } else {
                inb_xy
            };
            let is_endline_spot = inb.spot == InboundSpot::BackcourtEndline
                || (self.state.oob_is_endline
                    && matches!(
                        inb.spot,
                        InboundSpot::NearestToInterruption | InboundSpot::OriginalInboundSpot
                    ));
            let mut inbounder_behind_line = if is_endline_spot {
                libm::fabs(inb_x) >= self.geom.half_length_m + 0.10
            } else {
                libm::fabs(inb_x) >= self.geom.half_length_m + 0.10
                    || libm::fabs(inb_y) >= self.geom.half_width_m + 0.10
            };
            if !inbounder_behind_line && self.state.inbound_stage_ticks >= 16 && inb_r != ROSTER_IDX_NONE {
                let ip = &mut self.state.players[inb_r as usize];
                ip.x_m = inb_xy.0;
                ip.y_m = inb_xy.1;
                ip.vx_m_s = 0.0;
                ip.vy_m_s = 0.0;
                inb_x = inb_xy.0;
                inb_y = inb_xy.1;
                inbounder_behind_line = true;
            }

            let d_to_spot = libm::hypot(inb_x - inb_xy.0, inb_y - inb_xy.1);
            let d_to_rim = libm::hypot(inb_x - rim_xy.0, inb_y - rim_xy.1);

            if inbounder_behind_line && (d_to_spot <= 0.95 || self.state.inbound_stage_ticks >= 8 || (!inb.post_made_fg && !inb.post_made_last_ft)) {
                inb.inbound_ball_at_disposal = true;
                self.state.ball.ball_state = BallState::Held;
                self.state.ball.owner_slot = Some(inbounder_slot);
                self.state.ball.last_touched_by_slot = Some(inbounder_slot);
                self.state.ball.position_m = [inb_x, inb_y, 1.45];
                self.state.ball.velocity_m_s = [0.0, 0.0, 0.0];
            } else if d_to_rim <= 1.35 || self.state.inbound_stage_ticks >= 3 {
                self.state.ball.ball_state = BallState::Held;
                self.state.ball.owner_slot = Some(inbounder_slot);
                self.state.ball.position_m = [inb_x, inb_y, 1.25];
            } else {
                // Ball waits gently under rim until inbounder reaches it
                self.state.ball.ball_state = BallState::Loose;
                self.state.ball.owner_slot = None;
                self.state.ball.position_m[0] += self.state.ball.velocity_m_s[0] * 0.1;
                self.state.ball.position_m[1] += self.state.ball.velocity_m_s[1] * 0.1;
                self.state.ball.position_m[2] =
                    (self.state.ball.position_m[2] + self.state.ball.velocity_m_s[2] * 0.1).max(0.12);
                self.state.ball.velocity_m_s[2] -= GRAVITY_M_S2 * 0.1;
                if self.state.ball.position_m[2] <= 0.12 {
                    self.state.ball.velocity_m_s = [0.0, 0.0, 0.0];
                }
            }

            if adv_live_inbound_clock(self, &inb) {
                return Ok(());
            }

            if inb.inbound_ball_at_disposal {
                inb.inbound_5s_timer_ds = inb.inbound_5s_timer_ds.saturating_add(1);
                if inb.inbound_5s_timer_ds >= 50 {
                    if inb.is_period_start_arrow_inbound
                        && self.ruleset.fouls.possession_start_mode == 1
                    {
                        self.state.possession_arrow = inb.side.opponent();
                    }
                    self.trigger_turnover_stoppage(RuleCode::VIO_5S_INBOUND, inb.side, inb_r);
                    return Ok(());
                }
            }

            let pg_r = self.state.team_states[team_idx].active_lineup_slots[receiver_pos];
            let (pg_x, pg_y) = if pg_r != ROSTER_IDX_NONE {
                let p = &self.state.players[pg_r as usize];
                (p.x_m, p.y_m)
            } else {
                recv_xy
            };
            let pg_dist = libm::hypot(pg_x - recv_xy.0, pg_y - recv_xy.1);
            let ready_to_pass = if !inb.post_made_fg && !inb.post_made_last_ft {
                inbounder_behind_line
                    && inb.inbound_ball_at_disposal
                    && inb.inbound_5s_timer_ds >= 2
                    && self.state.inbound_stage_ticks >= 3
            } else {
                inbounder_behind_line
                    && inb.inbound_ball_at_disposal
                    && inb.inbound_5s_timer_ds >= 2
                    && ((self.state.inbound_stage_ticks >= 4 && pg_dist <= 2.4)
                        || self.state.inbound_stage_ticks >= 10)
            };

            if ready_to_pass {
                if inb_r != ROSTER_IDX_NONE {
                    let ip = &mut self.state.players[inb_r as usize];
                    ip.action_state = PlayerActionState::Pass;
                    ip.action_timer_ds = 4;
                    ip.facing_rad = libm::atan2(pg_y - inb_y, pg_x - inb_x);
                }
                self.state.inbound_pass_start_m = [inb_x, inb_y, 1.45];
                self.state.inbound_pass_target_m = [pg_x, pg_y, 1.25];
                self.state.ball.ball_state = BallState::PassFlight;
                self.state.ball.owner_slot = None;
                self.state.ball.last_touched_by_slot = Some(inbounder_slot);
                self.state.inbound_sub_stage = 2;
                self.state.inbound_stage_ticks = 0;
            }
            self.state.inbound_context = Some(inb);
            return Ok(());
        }

        // Stage 2 (InboundPassFlight): Real inbound pass flight arc from inbounder to PG
        self.state.inbound_stage_ticks = self.state.inbound_stage_ticks.saturating_add(1);
        self.step_inbound_approach_kinematics(team_idx, def_idx, inb_xy, recv_xy, inbounder_pos)?;

        let pg_roster = self.state.team_states[team_idx].active_lineup_slots[receiver_pos];
        let (pg_cur_x, pg_cur_y) = if pg_roster != ROSTER_IDX_NONE {
            let p = &self.state.players[pg_roster as usize];
            self.geom.inbounds_nav_bounds(0.60).clamp(p.x_m, p.y_m)
        } else {
            recv_xy
        };
        self.state.inbound_pass_target_m = [pg_cur_x, pg_cur_y, 1.25];
        let pass_dist = libm::hypot(
            self.state.inbound_pass_target_m[0] - self.state.inbound_pass_start_m[0],
            self.state.inbound_pass_target_m[1] - self.state.inbound_pass_start_m[1],
        );
        let total_flight = ((pass_dist / 1.35).round().clamp(3.0, 5.0)) as u8;
        let frac = (self.state.inbound_stage_ticks as f64 / total_flight.max(1) as f64).min(1.0);
        for k in 0..2 {
            self.state.ball.position_m[k] = self.state.inbound_pass_start_m[k]
                + (self.state.inbound_pass_target_m[k] - self.state.inbound_pass_start_m[k]) * frac;
        }
        self.state.ball.position_m[2] = self.state.inbound_pass_start_m[2]
            + (self.state.inbound_pass_target_m[2] - self.state.inbound_pass_start_m[2]) * frac
            + 0.28 * 4.0 * frac * (1.0 - frac);
        self.state.ball.ball_state = BallState::PassFlight;
        self.state.ball.owner_slot = None;

        if self.state.inbound_stage_ticks < total_flight {
            if adv_live_inbound_clock(self, &inb) {
                return Ok(());
            }
            self.state.inbound_context = Some(inb);
            return Ok(());
        }

        // Complete inbound catch by PG cleanly inside court lines
        inb.inbound_touched_court_player = true;
        let nav_in = self.geom.inbounds_nav_bounds(0.60);
        let (pg_x, pg_y) = if pg_roster != ROSTER_IDX_NONE {
            let p = &mut self.state.players[pg_roster as usize];
            if !inb.post_made_fg && !inb.post_made_last_ft {
                let d_recv = libm::hypot(p.x_m - recv_xy.0, p.y_m - recv_xy.1);
                if d_recv > 3.5 {
                    p.x_m = recv_xy.0;
                    p.y_m = recv_xy.1;
                }
            }
            let (cl_x, cl_y) = nav_in.clamp(p.x_m, p.y_m);
            p.x_m = cl_x;
            p.y_m = cl_y;
            p.oob_reentry_pending = false;
            p.dribbles_since_catch = 0;
            p.caught_ball_inside_ldb = self.geom.is_in_ldb(cl_x, cl_y, attack_dir);
            p.vx_m_s = 0.0;
            p.vy_m_s = 0.0;
            p.action_state = PlayerActionState::Catch;
            p.action_timer_ds = 4;
            p.feet_set_ticks = 0;
            p.footwork = FootworkRecord {
                dribble_status: DribbleStatus::LiveTripleThreat,
                ..FootworkRecord::default()
            };
            (p.x_m, p.y_m)
        } else {
            (pg_cur_x, pg_cur_y)
        };
        // Inbounder steps smoothly just inside court after releasing pass
        let inb_r = self.state.team_states[team_idx].active_lineup_slots[inbounder_pos];
        if inb_r != ROSTER_IDX_NONE {
            let ip = &mut self.state.players[inb_r as usize];
            let (ix, iy) = self.geom.inbounds_nav_bounds(0.55).clamp(ip.x_m, ip.y_m);
            ip.x_m = ix;
            ip.y_m = iy;
            ip.oob_reentry_pending = false;
            ip.action_state = PlayerActionState::Jog;
        }
        self.state.oob_is_endline = false;

        self.state.ball.position_m = [pg_x, pg_y, 1.25];
        self.state.ball.velocity_m_s = [0.0, 0.0, 0.0];
        self.state.ball.ball_state = BallState::Held;
        self.state.ball.owner_slot = Some(pg_slot);
        self.state.ball.last_touched_by_slot = Some(pg_slot);
        let retained_control = self.state.ball.last_team_control_side == Some(inb.side);
        self.state.ball.last_team_control_side = Some(inb.side);
        self.state.ball.rim_touched_this_shot = false;
        self.state.ball.backboard_contact_this_shot = BackboardContact::None;
        self.state.possession_side = Some(inb.side);
        self.state.ball_in_frontcourt = (pg_x * attack_dir) > 0.0;
        if !retained_control || self.state.ball_in_frontcourt || inb.post_made_fg || inb.post_made_last_ft {
            self.state.backcourt_8s_timer_ds = 0;
        }

        if inb.is_period_start_arrow_inbound {
            // Flip alternating arrow upon inbound completion
            self.state.possession_arrow = inb.side.opponent();
        }

        // Clocks start upon legal touch on court
        self.state.game_clock_running = true;
        if !inb.is_period_start_arrow_inbound
            || matches!(
                self.ruleset.clocks.shot_clock_policy,
                ShotClockResetPolicy::NbaRule7
            )
        {
            let target_sc = inb.pending_shot_clock_ds;
            let new_sc = target_sc.and_then(|s| {
                if self.state.game_clock_ds <= s {
                    None
                } else {
                    Some(s)
                }
            });
            self.state.shot_clock_ds = new_sc;
            self.state.shot_clock_rem_10us = new_sc.map(|s| (s as u32) * SUB_DS_UNITS);
            self.state.shot_clock_running = new_sc.is_some();
        } else {
            // FIBA Art. 29.2.1 period-start inbound: game_clock runs on first touch, shot_clock starts on control
            let new_sc = inb.pending_shot_clock_ds.and_then(|s| {
                if self.state.game_clock_ds <= s {
                    None
                } else {
                    Some(s)
                }
            });
            self.state.shot_clock_ds = new_sc;
            self.state.shot_clock_rem_10us = new_sc.map(|s| (s as u32) * SUB_DS_UNITS);
            self.state.shot_clock_running = true;
        }

        if self.ruleset.clocks.min_catch_and_shoot_ds > 0
            && (self.state.game_clock_ds < self.ruleset.clocks.min_catch_and_shoot_ds
                || self
                    .state
                    .shot_clock_ds
                    .map_or(false, |sc| sc < self.ruleset.clocks.min_catch_and_shoot_ds))
        {
            self.state.ball.live_ball_03s_restricted = true;
        } else {
            self.state.ball.live_ball_03s_restricted = false;
        }

        // Record Comments II-L baseline on NBA stopped-clock inbound
        if inb.stop_clock_until_touch
            && matches!(
                self.ruleset.clocks.shot_clock_policy,
                ShotClockResetPolicy::NbaRule7
            )
        {
            self.state.comments_iil_origin_clocks = Some(CommentsIILOrigin {
                kind: 0,
                start_gc_rem_10us: self.state.game_clock_rem_10us,
                start_sc_rem_10us: self.state.shot_clock_rem_10us,
            });
        }

        self.reset_possession_epoch();
        self.reset_substage_counters();
        let inb_team_idx = inb.side.team_index().unwrap_or(0);
        let is_ato_inb = inb.allow_both_sides_advance
            || (!inb.post_made_fg && !inb.post_made_last_ft && self.state.ball_in_frontcourt);
        self.start_offensive_possession(inb_team_idx, is_ato_inb, false);
        self.state.last_stoppage_resume_event_seq = self.state.next_event_seq.saturating_sub(1);
        self.state
            .recent_reviewable_events
            .mark_stoppage_occurred(self.state.last_stoppage_resume_event_seq);
        self.state.team_states[0].consecutive_timeouts_used_in_deadball = 0;
        self.state.team_states[1].consecutive_timeouts_used_in_deadball = 0;

        let (_, gc_exp, sc_exp) = advance_clocks_sub_ds(&mut self.state, 0.5);
        self.state.game_phase = GamePhase::LiveBall;
        self.push_event(GameRecordEventPayload::PossessionEstablished {
            possession_id: self.state.possession_id,
            side: inb.side as u8,
            handler_roster_idx: pg_roster,
            trigger_kind: 1,
            new_shot_clock_ds: match self.state.shot_clock_ds {
                Some(v) => v,
                None => 0xFFFF,
            },
        });
        if gc_exp {
            self.handle_period_expiration();
        } else if sc_exp {
            self.trigger_shot_clock_violation();
        }
        Ok(())
    }

    fn step_free_throws(&mut self) -> Result<(), RngBudgetExceeded> {
        self.step_player_vertical_kinematics();
        let top = match self.state.ft_series_queue.top_mut() {
            Some(t) => *t,
            None => {
                self.state.game_phase = GamePhase::LiveBall;
                return Ok(());
            }
        };
        let shooter_idx = top.shooter_roster_idx;
        let team_idx = if (shooter_idx as usize) < 15 { 0 } else { 1 };
        let def_idx = 1 - team_idx;
        let attack_dir = if team_idx == 0 { 1.0 } else { -1.0 };
        let rim_xy = self.geom.rim_xy(attack_dir);
        let ft_x = attack_dir * (self.geom.half_length_m - self.geom.paint_length_m - 0.15);
        let live_lane = top.shots_remaining == 1
            && matches!(top.on_complete, PostFtOutcome::LiveReboundOnLastMiss)
            && !top.is_technical_or_flagrant;
        let shooter_side = if team_idx == 0 {
            CommandSide::Home
        } else {
            CommandSide::Away
        };
        let shooter_slot = self.state.team_states[team_idx]
            .active_lineup_slots
            .iter()
            .position(|&r| r == shooter_idx)
            .map(|p| (team_idx * 5 + p) as u8);

        // Stage 0 (Prep, 5 ticks = 0.5s): Line up (live rebound lane vs cleared tech/non-last FT), bounce routine, release
        if self.state.ft_sub_stage == 0 {
            if self.state.ft_stage_ticks == 0 {
                self.reset_active_player_transient_states();
                self.state.ball.rim_touched_this_shot = false;
                self.state.ball.backboard_contact_this_shot = BackboardContact::None;
                let nav_b = self.geom.inbounds_nav_bounds(0.75);
                let mut off_lane_slot = 0usize;
                for p_pos in 0..5 {
                    let r = self.state.team_states[team_idx].active_lineup_slots[p_pos];
                    if r == ROSTER_IDX_NONE {
                        continue;
                    }
                    let (raw_tx, raw_ty) = if r == shooter_idx {
                        (ft_x, 0.0)
                    } else if live_lane {
                        off_lane_slot += 1;
                        match off_lane_slot {
                            1 => (rim_xy.0 - attack_dir * 2.15, 2.70),
                            2 => (rim_xy.0 - attack_dir * 2.15, -2.70),
                            3 => (ft_x - attack_dir * 2.6, -3.5),
                            _ => (ft_x - attack_dir * 2.6, 3.5),
                        }
                    } else {
                        off_lane_slot += 1;
                        match off_lane_slot {
                            1 => (ft_x - attack_dir * 2.6, -3.6),
                            2 => (ft_x - attack_dir * 2.6, 3.6),
                            3 => (ft_x - attack_dir * 3.8, -1.8),
                            _ => (ft_x - attack_dir * 3.8, 1.8),
                        }
                    };
                    let (tx, ty) = nav_b.clamp(raw_tx, raw_ty);
                    self.transient_target_xy[team_idx * 5 + p_pos] = (tx, ty);
                    let p = &mut self.state.players[r as usize];
                    p.x_m = tx;
                    p.y_m = ty;
                    p.vx_m_s = 0.0;
                    p.vy_m_s = 0.0;
                    p.facing_rad = libm::atan2(rim_xy.1 - ty, rim_xy.0 - tx);
                    p.action_state = if r == shooter_idx {
                        PlayerActionState::Dribble
                    } else if live_lane && off_lane_slot <= 2 {
                        PlayerActionState::BoxOut
                    } else {
                        PlayerActionState::Idle
                    };
                    p.action_timer_ds = 0;
                }
                for p_pos in 0..5 {
                    let dr = self.state.team_states[def_idx].active_lineup_slots[p_pos];
                    if dr == ROSTER_IDX_NONE {
                        continue;
                    }
                    let (raw_tx, raw_ty) = if live_lane {
                        match p_pos {
                            4 => (rim_xy.0 - attack_dir * 1.15, 2.70),
                            3 => (rim_xy.0 - attack_dir * 1.15, -2.70),
                            2 => (rim_xy.0 - attack_dir * 3.10, 2.70),
                            1 => (rim_xy.0 - attack_dir * 3.10, -2.70),
                            _ => (ft_x - attack_dir * 3.2, 0.0),
                        }
                    } else {
                        match p_pos {
                            0 => (ft_x - attack_dir * 4.2, 0.0),
                            1 => (ft_x - attack_dir * 3.6, 4.2),
                            2 => (ft_x - attack_dir * 3.6, -4.2),
                            3 => (ft_x - attack_dir * 4.8, 2.4),
                            _ => (ft_x - attack_dir * 4.8, -2.4),
                        }
                    };
                    let (tx, ty) = nav_b.clamp(raw_tx, raw_ty);
                    self.transient_target_xy[def_idx * 5 + p_pos] = (tx, ty);
                    let dp = &mut self.state.players[dr as usize];
                    dp.x_m = tx;
                    dp.y_m = ty;
                    dp.vx_m_s = 0.0;
                    dp.vy_m_s = 0.0;
                    dp.facing_rad = libm::atan2(rim_xy.1 - ty, rim_xy.0 - tx);
                    dp.action_state = if live_lane && p_pos >= 1 {
                        PlayerActionState::BoxOut
                    } else {
                        PlayerActionState::Idle
                    };
                    dp.action_timer_ds = 0;
                }
                self.state.ball.last_passer_roster_idx = None;
            }

            self.state.ft_stage_ticks = self.state.ft_stage_ticks.saturating_add(1);
            let bounce_z =
                1.05 + 0.25 * libm::fabs(libm::sin((self.state.ft_stage_ticks as f64) * 1.55));
            self.state.ball.ball_state = BallState::Held;
            self.state.ball.owner_slot = shooter_slot;
            self.state.ball.position_m = [ft_x + attack_dir * 0.28, 0.0, bounce_z];
            self.state.ball.velocity_m_s = [0.0, 0.0, 0.0];

            if (self.state.ft_stage_ticks as u16) >= self.ruleset.clocks.free_throw_timer_ds {
                self.trigger_turnover_stoppage(RuleCode::VIO_FT_VIOLATION, shooter_side, shooter_idx);
                return Ok(());
            }
            if self.state.ft_stage_ticks < 5 {
                return Ok(());
            }

            // Release FT on tick 5: sample outcome & launch 0.7s parabolic arc toward rim
            let rank = (shooter_idx as usize) % 15;
            let ft_attr = self.teams[team_idx]
                .players
                .get(rank)
                .map_or(75, |p| p.attributes[AttrIdx::FreeThrow]);
            let p_make = (0.50 + 0.42 * (ft_attr as f64 / 99.0)).clamp(0.45, 0.96);
            let cid = canonical_actor_id(self.canonical_team_ids[team_idx], rank);
            let u = sample_u53_checked(
                self.philox_key,
                self.state.possession_id,
                self.state.possession_tick,
                &mut self.state.rng_sample_counters.0,
                cid,
                RngPurpose::ShotOutcome,
            )?;
            self.state.ft_pending_made = u < p_make;
            let z0 = 2.25;
            let flight_s = 0.7;
            let miss_off = if self.state.ft_pending_made { 0.0 } else { -0.14 };
            self.state.ball.position_m = [ft_x + attack_dir * 0.25, 0.0, z0];
            self.state.ball.velocity_m_s = [
                (rim_xy.0 + attack_dir * miss_off - self.state.ball.position_m[0]) / flight_s,
                0.0,
                (self.geom.rim_height_m - z0 + 0.5 * GRAVITY_M_S2 * flight_s * flight_s) / flight_s,
            ];
            self.state.ball.ball_state = BallState::ShotFlight;
            self.state.ball.owner_slot = None;
            if shooter_idx != ROSTER_IDX_NONE {
                let sp = &mut self.state.players[shooter_idx as usize];
                sp.action_state = PlayerActionState::ShootJumper;
                sp.action_timer_ds = 6;
            }
            self.state.ft_sub_stage = 1;
            self.state.ft_stage_ticks = 0;
            return Ok(());
        }

        // Stage 1 (Flight, 7 ticks = 0.7s): Ball travels cleanly along parabola to rim
        if self.state.ft_sub_stage == 1 {
            self.tick_player_action_timers();
            self.state.ft_stage_ticks = self.state.ft_stage_ticks.saturating_add(1);
            for k in 0..3 {
                self.state.ball.position_m[k] += self.state.ball.velocity_m_s[k] * 0.1;
            }
            self.state.ball.velocity_m_s[2] -= GRAVITY_M_S2 * 0.1;
            if self.state.ft_stage_ticks < 7 {
                return Ok(());
            }

            let made = self.state.ft_pending_made;
            if made {
                self.state.team_states[team_idx].score_on_court += 1;
                self.state.scoring_run_pts[team_idx] =
                    self.state.scoring_run_pts[team_idx].saturating_add(1);
                self.state.scoring_run_pts[def_idx] = 0;
                self.state.ball_fx_flags = 0x10;
                self.state.ball_fx_timer_ds = 5;
                self.state.ball.rim_touched_this_shot = false;
                self.state.ball.position_m = [rim_xy.0, rim_xy.1, self.geom.rim_height_m - 0.12];
                self.state.ball.velocity_m_s = [0.0, 0.0, -2.35];
                self.state.ball.ball_state = BallState::Loose;
            } else {
                self.state.ball_fx_flags = 0x01;
                self.state.ball_fx_timer_ds = 5;
                self.state.ball.rim_touched_this_shot = true;
                self.state.ball.position_m = [rim_xy.0 - attack_dir * 0.25, 0.0, self.geom.rim_height_m];
                self.state.ball.velocity_m_s = [-attack_dir * 2.1, 0.2, 2.6];
                self.state.ball.ball_state = BallState::Loose;
            }

            let shot_number = top.total_shots.saturating_sub(top.shots_remaining) + 1;
            let mut queue_copy = self.state.ft_series_queue;
            let mut popped_on_complete = top.on_complete;
            let remaining_after = top.shots_remaining.saturating_sub(1);
            if remaining_after == 0 {
                let _ = queue_copy.pop();
            } else if let Some(t) = queue_copy.top_mut() {
                t.shots_remaining = remaining_after;
            }
            self.state.ft_series_queue = queue_copy;

            let post_tag = if remaining_after > 0
                || matches!(popped_on_complete, PostFtOutcome::ResumeInterruptedFtSeries)
                || !self.state.ft_series_queue.is_empty()
            {
                0u8
            } else if self.state.game_clock_ds == 0 {
                4u8
            } else {
                match popped_on_complete {
                    PostFtOutcome::LiveReboundOnLastMiss => {
                        if made { 2 } else { 1 }
                    }
                    PostFtOutcome::RetainedInbound { side, .. } => {
                        if side == shooter_side { 3 } else { 2 }
                    }
                    PostFtOutcome::ResumeJumpBall { .. } => 5,
                    PostFtOutcome::ResumeInterruptedFtSeries => 0,
                }
            };

            let ft_seq_id = self.state.next_event_seq as u32;
            self.push_event(GameRecordEventPayload::FreeThrowResolved {
                ft_seq_id,
                shooter_roster_idx: shooter_idx,
                team_side: team_idx as u8,
                made: if made { 1 } else { 0 },
                shot_number,
                total_shots: top.total_shots,
                is_technical_or_flagrant: if top.is_technical_or_flagrant { 1 } else { 0 },
                post_ft_outcome_tag: post_tag,
                ft_flags: 0x01,
            });

            // Last live FT miss immediately triggers ReboundScramble while ball bounces off rim!
            if !made
                && remaining_after == 0
                && self.state.ft_series_queue.is_empty()
                && self.state.game_clock_ds > 0
                && matches!(popped_on_complete, PostFtOutcome::LiveReboundOnLastMiss)
            {
                self.reset_possession_epoch();
                self.reset_substage_counters();
                self.state.game_phase = GamePhase::ReboundScramble;
                self.state.game_clock_running = false;
                self.state.shot_clock_running = false;
                self.state.shot_clock_ds = None;
                self.state.shot_clock_rem_10us = None;
                self.state.scramble_origin = Some(ScrambleOrigin::FromFreeThrowMiss {
                    prior_offense: shooter_side,
                });
                self.state.ball.ball_state = BallState::Loose;
                self.state.ball.owner_slot = None;
                return Ok(());
            }

            // Otherwise hold PostResolve briefly (0.3s) for visible net drop before next FT/inbound
            if remaining_after == 0
                && !matches!(popped_on_complete, PostFtOutcome::ResumeInterruptedFtSeries)
                && self.state.ft_series_queue.is_empty()
            {
                // Store final outcome tag implicitly by popping queue; re-push sentinel only if needed?
                // Even simpler: resolve non-live final makes/retained immediately after PostResolve?
                // Keep exact outcome handling cleanly by resolving series completion here for non-made/retained,
                // or run 3-tick net drop before next FT!
            }
            let series_still_active = remaining_after > 0
                || (matches!(popped_on_complete, PostFtOutcome::ResumeInterruptedFtSeries)
                    && !self.state.ft_series_queue.is_empty())
                || !self.state.ft_series_queue.is_empty();
            if series_still_active {
                self.state.ft_sub_stage = 2;
                self.state.ft_stage_ticks = 0;
                return Ok(());
            }

            if self.state.game_clock_ds == 0 {
                self.reset_substage_counters();
                self.handle_period_expiration();
                return Ok(());
            }

            // Final FT complete (`made == true` or `RetainedInbound` or `ResumeJumpBall`)
            self.reset_possession_epoch();
            self.reset_substage_counters();
            match popped_on_complete {
                PostFtOutcome::LiveReboundOnLastMiss => {
                    let opp = shooter_side.opponent();
                    self.state.game_phase = GamePhase::InboundSetup;
                    let sc = compute_shot_clock_reset(
                        ShotClockScenario::TurnoverOpponentToOwnBackcourt,
                        self.state.shot_clock_ds,
                        self.state.game_clock_ds,
                        &self.ruleset,
                    );
                    self.state.inbound_context = Some(InboundContext {
                        side: opp,
                        spot: InboundSpot::BackcourtEndline,
                        sideline_side: CourtSideLine::TableSide,
                        allow_endline_run: true,
                        stop_clock_until_touch: self.state.quarter >= 4 && self.state.game_clock_ds <= 1200,
                        inbound_ball_at_disposal: false,
                        inbound_touched_court_player: false,
                        inbound_5s_timer_ds: 0,
                        original_spot: InboundSpot::BackcourtEndline,
                        pending_shot_clock_ds: sc,
                        is_period_start_arrow_inbound: false,
                        allow_both_sides_advance: false,
                        post_made_fg: false,
                        post_made_last_ft: true,
                    });
                }
                PostFtOutcome::RetainedInbound { side, spot } => {
                    let sc = self.state.shot_clock_ds;
                    self.state.game_phase = GamePhase::InboundSetup;
                    self.state.inbound_context = Some(InboundContext {
                        side,
                        spot,
                        sideline_side: CourtSideLine::OppositeTableSide,
                        allow_endline_run: false,
                        stop_clock_until_touch: true,
                        inbound_ball_at_disposal: false,
                        inbound_touched_court_player: false,
                        inbound_5s_timer_ds: 0,
                        original_spot: spot,
                        pending_shot_clock_ds: sc,
                        is_period_start_arrow_inbound: false,
                        allow_both_sides_advance: false,
                        post_made_fg: false,
                        post_made_last_ft: false,
                    });
                }
                PostFtOutcome::ResumeJumpBall { spot } => {
                    self.state.game_phase = GamePhase::JumpBall;
                    self.state.jump_ball_context = Some(JumpBallContext {
                        spot,
                        jumpers: [
                            self.state.team_states[0].active_lineup_slots[4],
                            self.state.team_states[1].active_lineup_slots[4],
                        ],
                        toss_timer_ds: 5,
                        ball_tossed: false,
                        ball_tapped: false,
                        is_opening_tip: false,
                    });
                }
                PostFtOutcome::ResumeInterruptedFtSeries => {}
            }
            let _ = &mut popped_on_complete;
            return Ok(());
        }

        // Stage 2 (PostResolve, 3 ticks = 0.3s): Net drop between consecutive FTs
        self.tick_player_action_timers();
        self.state.ft_stage_ticks = self.state.ft_stage_ticks.saturating_add(1);
        for k in 0..3 {
            self.state.ball.position_m[k] += self.state.ball.velocity_m_s[k] * 0.1;
        }
        self.state.ball.velocity_m_s[2] -= GRAVITY_M_S2 * 0.1;
        if self.state.ball.position_m[2] <= 0.12 {
            self.state.ball.position_m[2] = 0.12;
            self.state.ball.velocity_m_s[2] =
                (-self.state.ball.velocity_m_s[2] * 0.5).max(0.0);
        }
        if self.state.ft_stage_ticks >= 3 {
            self.state.ft_sub_stage = 0;
            self.state.ft_stage_ticks = 0;
        }
        Ok(())
    }

    fn step_shot_in_flight(&mut self) -> Result<(), RngBudgetExceeded> {
        self.step_player_vertical_kinematics();
        let mut sctx = match self.state.ball.shot_ctx.take() {
            Some(s) => s,
            None => {
                self.state.game_phase = GamePhase::LiveBall;
                return Ok(());
            }
        };

        let shooter_team_idx = if (sctx.shooter_roster_idx as usize) < 15 { 0 } else { 1 };
        let attack_dir = if shooter_team_idx == 0 { 1.0 } else { -1.0 };
        let rim_xy = self.geom.rim_xy(attack_dir);
        let shooter_side = if shooter_team_idx == 0 {
            CommandSide::Home
        } else {
            CommandSide::Away
        };
        let shooter_slot = self.state.team_states[shooter_team_idx]
            .active_lineup_slots
            .iter()
            .position(|&r| r == sctx.shooter_roster_idx)
            .map_or((shooter_team_idx * 5) as u8, |p| (shooter_team_idx * 5 + p) as u8);

        // Advance ball 3D parabola for 1 tick (0.1 s)
        for k in 0..3 {
            self.state.ball.position_m[k] += self.state.ball.velocity_m_s[k] * 0.1;
        }
        self.state.ball.velocity_m_s[2] -= GRAVITY_M_S2 * 0.1;

        // Keep players moving actively (boxing out & crashing) during shot flight
        self.step_shot_or_rebound_kinematics(shooter_team_idx, 1 - shooter_team_idx, true)?;

        // Advance clocks unless deferred foul whistle already stopped clocks
        let mut gc_exp = false;
        let mut sc_exp = false;
        if sctx.whistle_tick.is_none() {
            let (_, g, s) = advance_clocks_sub_ds(&mut self.state, 1.0);
            gc_exp = g;
            sc_exp = s;
        } else {
            self.state.game_clock_ticked_this_step = false;
        }

        let elapsed_ticks = self.state.sim_tick.saturating_sub(sctx.release_tick);
        let rel_x = (sctx.release_x_mm as f64) / 1000.0;
        let rel_y = (sctx.release_y_mm as f64) / 1000.0;
        let rel_dist = libm::hypot(rim_xy.0 - rel_x, rim_xy.1 - rel_y);
        let expected_ticks = if rel_dist <= 1.3 {
            3u32
        } else if rel_dist <= 2.3 {
            ((0.52 + 0.04 * rel_dist) * 10.0).round().clamp(5.0, 7.0) as u32
        } else {
            ((0.68 + 0.082 * rel_dist) * 10.0).round().clamp(7.0, 14.0) as u32
        };
        let target_z = match sctx.intended_outcome {
            IntendedOutcomeClass::BankMake
            | IntendedOutcomeClass::BankMiss
            | IntendedOutcomeClass::BackboardOnlyMiss => self.geom.rim_height_m + 0.42,
            IntendedOutcomeClass::Airball => self.geom.rim_height_m - 0.30,
            _ => self.geom.rim_height_m,
        };
        let flight_done = elapsed_ticks >= expected_ticks
            || (elapsed_ticks >= 3
                && self.state.ball.velocity_m_s[2] < 0.0
                && self.state.ball.position_m[2] <= target_z + 0.05);

        self.state.shot_gc_expired_pending |= gc_exp;

        // Stage 0 (AirFlight): Parabolic flight to rim / backboard
        if self.state.shot_sub_stage == 0 {
            if !flight_done {
                let bx = self.state.ball.position_m[0];
                let by = self.state.ball.position_m[1];
                let bz = self.state.ball.position_m[2];
                let is_nba = self.ruleset.fouls.goaltend_mode == 0;

                // 3D aerial block / goaltend / interference sweep against close jumping defenders (§7.3)
                if elapsed_ticks >= 2 && sctx.deferred_foul_call.is_none() {
                    let def_idx = 1 - shooter_team_idx;
                    let dist_rim_ball = libm::hypot(bx - rim_xy.0, by - rim_xy.1);
                    let mut def_cands: [(u8, u8, u8, f64); 5] = [(0xFF, 0xFF, 0, 99.0); 5];
                    let mut dn = 0usize;
                    for p_pos in 0..5 {
                        let dr = self.state.team_states[def_idx].active_lineup_slots[p_pos];
                        if dr != ROSTER_IDX_NONE {
                            let rank = (dr as usize).saturating_sub(def_idx * 15);
                            let cid = canonical_actor_id(self.canonical_team_ids[def_idx], rank);
                            let dp = &self.state.players[dr as usize];
                            let d2 = libm::hypot(dp.x_m - bx, dp.y_m - by);
                            def_cands[dn] = ((def_idx * 5 + p_pos) as u8, dr, cid, d2);
                            dn += 1;
                        }
                    }
                    if dn > 1 {
                        def_cands[..dn].sort_by_key(|x| x.2);
                    }
                    for &(d_slot, dr, d_cid, d2) in &def_cands[..dn] {
                        if d2 > 1.25 {
                            continue;
                        }
                        let rank = (dr as usize).saturating_sub(def_idx * 15);
                        let snap = &self.teams[def_idx].players[rank];
                        if self.state.players[dr as usize].z_m <= 0.01
                            && dist_rim_ball <= 2.6
                            && self.state.ball.velocity_m_s[2] >= -1.0
                        {
                            let vert = f64::from(snap.attributes[AttrIdx::Vertical]);
                            let dp_mut = &mut self.state.players[dr as usize];
                            dp_mut.vz_m_s = 2.5 + (vert / 99.0) * 1.3;
                            dp_mut.action_state = PlayerActionState::ContestJump;
                            dp_mut.action_timer_ds = 5;
                        }
                        let dp = &self.state.players[dr as usize];
                        let hand_reach_z = (snap.physical.standing_reach_cm / 100.0) + dp.z_m;
                        if d2 <= 0.92
                            && dp.z_m > 0.05
                            && bz <= hand_reach_z + 0.14
                            && bz >= hand_reach_z - 0.80
                        {
                            let ft = evaluate_flight_touch(
                                self.ruleset.fouls.goaltend_mode,
                                false,
                                0,
                                None,
                                false,
                                false,
                                sctx.rim_touched,
                                sctx.backboard_contact,
                                self.state.ball.position_m,
                                self.state.ball.velocity_m_s,
                                rim_xy,
                                self.geom.rim_height_m,
                                sctx.whistle_tick.is_some(),
                                self.state.game_clock_rem_10us == 0,
                                false,
                            );
                            let blk_attr = f64::from(snap.attributes[AttrIdx::Block]);
                            let prox = (1.0 - d2 / 0.95).clamp(0.0, 1.0);
                            let p_touch = match ft {
                                FlightTouchOutcome::Blocked => (0.03 + 0.11 * (blk_attr / 99.0)) * prox,
                                FlightTouchOutcome::Goaltending
                                | FlightTouchOutcome::BasketInterferenceDefenseScore { .. } => {
                                    0.028 * prox
                                }
                                FlightTouchOutcome::InterceptedLive => 0.25 * prox,
                                _ => 0.0,
                            };
                            if p_touch > 0.0 {
                                let u_t = sample_u53_checked(
                                    self.philox_key,
                                    self.state.possession_id,
                                    self.state.possession_tick,
                                    &mut self.state.rng_sample_counters.0,
                                    d_cid,
                                    RngPurpose::ContactAndFoulRoll,
                                )?;
                                if u_t < p_touch {
                                    match ft {
                                        FlightTouchOutcome::Blocked => {
                                            self.reset_substage_counters();
                                            let ev_seq = self.state.next_event_seq;
                                            self.push_event(GameRecordEventPayload::ShotResolved {
                                                shot_id: sctx.shot_id,
                                                shooter_roster_idx: sctx.shooter_roster_idx,
                                                scorer_team: 0xFF,
                                                points_awarded: 0,
                                                outcome_class: IntendedOutcomeClass::Blocked as u8,
                                                rim_touched: 0,
                                                backboard_contact: sctx.backboard_contact as u8,
                                                is_blocked: 1,
                                                blocker_roster_idx: dr,
                                                is_goaltend: 0,
                                                assist_roster_idx: 0xFF,
                                                foul_on_shot: 0,
                                                telemetry: ShotTelemetryPublic {
                                                    release_tick: sctx.release_tick,
                                                    release_game_clock_ds: sctx.release_game_clock_ds,
                                                    whistle_game_clock_ds: 0xFFFF,
                                                    whistle_tick: 0xFFFF_FFFF,
                                                    flight_resolved_tick: self.state.sim_tick,
                                                    basket_scored_tick: 0xFFFF_FFFF,
                                                },
                                            });
                                            self.record_reviewable_event(ReviewableEventEntry {
                                                event_seq: ev_seq,
                                                tick: self.state.sim_tick,
                                                quarter: self.state.quarter,
                                                game_clock_ds: self.state.game_clock_ds,
                                                points_awarded: 0,
                                                original_rule_code: RuleCode::NONE as u16,
                                                scorer_side: None,
                                                shooter_roster_idx: Some(sctx.shooter_roster_idx),
                                                assist_roster_idx: None,
                                                blocker_roster_idx: Some(dr),
                                                original_is_three_pointer: sctx.shot_value == 3,
                                                original_fga_counted: true,
                                                stoppage_occurred_since: false,
                                                shot_clock_expired_before_release: !sctx.released_before_shot_clock_zero,
                                                ground_truth_is_three_pointer: sctx.ground_truth_is_three_pointer,
                                                ground_truth_oob_by_side: None,
                                                ground_truth_goaltend_violation: false,
                                                ground_truth_def_foul_after_motion_start: false,
                                                official_not_reasonably_certain: false,
                                            });
                                            self.state.ball.ball_state = BallState::Loose;
                                            self.state.ball.owner_slot = None;
                                            self.state.ball.last_touched_by_slot = Some(d_slot);
                                            self.state.ball.last_passer_roster_idx = None;
                                            self.state.ball.velocity_m_s = [
                                                -attack_dir * 2.0,
                                                if by >= 0.0 { 4.4 } else { -4.4 },
                                                2.0,
                                            ];
                                            self.state.ball_fx_flags |= 0x04;
                                            self.state.ball_fx_timer_ds = 5;
                                            self.state.scramble_origin =
                                                Some(ScrambleOrigin::FromNoRimMissOrBlock {
                                                    prior_offense: shooter_side,
                                                });
                                            self.state.game_phase = GamePhase::ReboundScramble;
                                            self.state.scramble_ticks = 0;
                                            return Ok(());
                                        }
                                        FlightTouchOutcome::Goaltending
                                        | FlightTouchOutcome::BasketInterferenceDefenseScore { .. } => {
                                            let rule_code = if matches!(ft, FlightTouchOutcome::Goaltending) {
                                                RuleCode::VIO_GOALTEND
                                            } else {
                                                RuleCode::VIO_BASKET_INTERFERENCE
                                            };
                                            let points = sctx.shot_value;
                                            let def_side = shooter_side.opponent();
                                            self.reset_substage_counters();
                                            self.state.team_states[shooter_team_idx].score_on_court +=
                                                points as u16;
                                            self.state.scoring_run_pts[shooter_team_idx] = self
                                                .state
                                                .scoring_run_pts[shooter_team_idx]
                                                .saturating_add(points);
                                            self.state.scoring_run_pts[def_idx] = 0;
                                            self.record_defense_possession_outcome(
                                                def_idx,
                                                shooter_team_idx,
                                                points,
                                            );
                                            self.state.ball.position_m =
                                                [rim_xy.0, rim_xy.1, self.geom.rim_height_m - 0.12];
                                            self.state.ball.velocity_m_s = [0.0, 0.0, -2.35];
                                            self.state.ball.ball_state = BallState::Loose;
                                            self.state.ball_fx_flags |= 0x08 | 0x10;
                                            self.state.ball_fx_timer_ds = 6;
                                            self.push_event(GameRecordEventPayload::ViolationTriggered {
                                                offender_roster_idx: dr,
                                                team_side: def_side as u8,
                                                rule_code: rule_code as u16,
                                                footwork: FootworkTelemetryPublic::default(),
                                            });
                                            let ev_seq = self.state.next_event_seq;
                                            self.push_event(GameRecordEventPayload::ShotResolved {
                                                shot_id: sctx.shot_id,
                                                shooter_roster_idx: sctx.shooter_roster_idx,
                                                scorer_team: shooter_team_idx as u8,
                                                points_awarded: points,
                                                outcome_class: IntendedOutcomeClass::Goaltended as u8,
                                                rim_touched: if sctx.rim_touched { 1 } else { 0 },
                                                backboard_contact: sctx.backboard_contact as u8,
                                                is_blocked: 0,
                                                blocker_roster_idx: dr,
                                                is_goaltend: 1,
                                                assist_roster_idx: sctx
                                                    .assist_roster_idx
                                                    .unwrap_or(0xFF),
                                                foul_on_shot: 0,
                                                telemetry: ShotTelemetryPublic {
                                                    release_tick: sctx.release_tick,
                                                    release_game_clock_ds: sctx.release_game_clock_ds,
                                                    whistle_game_clock_ds: self.state.game_clock_ds,
                                                    whistle_tick: self.state.sim_tick,
                                                    flight_resolved_tick: self.state.sim_tick,
                                                    basket_scored_tick: self.state.sim_tick,
                                                },
                                            });
                                            self.record_reviewable_event(ReviewableEventEntry {
                                                event_seq: ev_seq,
                                                tick: self.state.sim_tick,
                                                quarter: self.state.quarter,
                                                game_clock_ds: self.state.game_clock_ds,
                                                points_awarded: points,
                                                original_rule_code: rule_code as u16,
                                                scorer_side: Some(shooter_side),
                                                shooter_roster_idx: Some(sctx.shooter_roster_idx),
                                                assist_roster_idx: sctx.assist_roster_idx,
                                                blocker_roster_idx: Some(dr),
                                                original_is_three_pointer: sctx.shot_value == 3,
                                                original_fga_counted: true,
                                                stoppage_occurred_since: false,
                                                shot_clock_expired_before_release: !sctx.released_before_shot_clock_zero,
                                                ground_truth_is_three_pointer: sctx.ground_truth_is_three_pointer,
                                                ground_truth_oob_by_side: None,
                                                ground_truth_goaltend_violation: true,
                                                ground_truth_def_foul_after_motion_start: false,
                                                official_not_reasonably_certain: false,
                                            });
                                            if self.state.game_clock_rem_10us == 0
                                                || self.state.shot_gc_expired_pending
                                            {
                                                self.handle_period_expiration();
                                                return Ok(());
                                            }
                                            let stop_last_sec = if self.state.quarter >= 4 {
                                                self.ruleset.clocks.stop_on_made_final_ot_sec
                                            } else {
                                                self.ruleset.clocks.stop_on_made_reg_last_sec
                                            };
                                            let stop_clock =
                                                self.state.game_clock_ds <= stop_last_sec * 10;
                                            if stop_clock {
                                                self.state.game_clock_running = false;
                                            }
                                            self.state.shot_clock_running = false;
                                            let sc = compute_shot_clock_reset(
                                                ShotClockScenario::TurnoverOpponentToOwnBackcourt,
                                                self.state.shot_clock_ds,
                                                self.state.game_clock_ds,
                                                &self.ruleset,
                                            );
                                            self.state.game_phase = GamePhase::InboundSetup;
                                            self.state.inbound_context = Some(InboundContext {
                                                side: def_side,
                                                spot: InboundSpot::BackcourtEndline,
                                                sideline_side: CourtSideLine::TableSide,
                                                allow_endline_run: true,
                                                stop_clock_until_touch: stop_clock,
                                                inbound_ball_at_disposal: false,
                                                inbound_touched_court_player: false,
                                                inbound_5s_timer_ds: 0,
                                                original_spot: InboundSpot::BackcourtEndline,
                                                pending_shot_clock_ds: sc,
                                                is_period_start_arrow_inbound: false,
                                                allow_both_sides_advance: false,
                                                post_made_fg: true,
                                                post_made_last_ft: false,
                                            });
                                            return Ok(());
                                        }
                                        FlightTouchOutcome::InterceptedLive => {
                                            self.state.ball.ball_state = BallState::Held;
                                            self.state.ball.owner_slot = Some(d_slot);
                                            self.state.ball.last_touched_by_slot = Some(d_slot);
                                            self.state.ball.last_passer_roster_idx = None;
                                            self.state.possession_side =
                                                Some(shooter_side.opponent());
                                            self.reset_possession_epoch();
                                            self.reset_substage_counters();
                                            self.record_defense_possession_outcome(
                                                def_idx,
                                                shooter_team_idx,
                                                0,
                                            );
                                            self.start_offensive_possession(def_idx, false, true);
                                            self.state.game_phase = GamePhase::LiveBall;
                                            let dr_p = &self.state.players[dr as usize];
                                            self.state.ball_in_frontcourt =
                                                (dr_p.x_m * (-attack_dir)) > 0.0;
                                            self.state.backcourt_8s_timer_ds = 0;
                                            let sc = compute_shot_clock_reset(
                                                ShotClockScenario::LiveStealOrDefRebound,
                                                self.state.shot_clock_ds,
                                                self.state.game_clock_ds,
                                                &self.ruleset,
                                            );
                                            let gc_ds = self.state.game_clock_ds;
                                            set_clocks_exact_ds(&mut self.state, gc_ds, sc);
                                            self.state.shot_clock_running = sc.is_some();
                                            self.push_event(GameRecordEventPayload::ShotResolved {
                                                shot_id: sctx.shot_id,
                                                shooter_roster_idx: sctx.shooter_roster_idx,
                                                scorer_team: 0xFF,
                                                points_awarded: 0,
                                                outcome_class: IntendedOutcomeClass::InterceptedLive
                                                    as u8,
                                                rim_touched: 0,
                                                backboard_contact: sctx.backboard_contact as u8,
                                                is_blocked: 0,
                                                blocker_roster_idx: dr,
                                                is_goaltend: 0,
                                                assist_roster_idx: 0xFF,
                                                foul_on_shot: 0,
                                                telemetry: ShotTelemetryPublic {
                                                    release_tick: sctx.release_tick,
                                                    release_game_clock_ds: sctx.release_game_clock_ds,
                                                    whistle_game_clock_ds: 0xFFFF,
                                                    whistle_tick: 0xFFFF_FFFF,
                                                    flight_resolved_tick: self.state.sim_tick,
                                                    basket_scored_tick: 0xFFFF_FFFF,
                                                },
                                            });
                                            return Ok(());
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }

                // OOB airball / behind backboard before reaching target
                if !sctx.intended_outcome.is_make()
                    && (self
                        .geom
                        .is_over_or_behind_backboard(self.state.ball.position_m, attack_dir, is_nba)
                        || (bz <= 0.14 && self.geom.is_out_of_bounds(bx, by)))
                {
                    let combined_gc_exp = gc_exp || self.state.shot_gc_expired_pending;
                    self.reset_substage_counters();
                    self.state.ball.last_passer_roster_idx = None;
                    let ev_seq = self.state.next_event_seq;
                    self.push_event(GameRecordEventPayload::ShotResolved {
                        shot_id: sctx.shot_id,
                        shooter_roster_idx: sctx.shooter_roster_idx,
                        scorer_team: 0xFF,
                        points_awarded: 0,
                        outcome_class: sctx.intended_outcome as u8,
                        rim_touched: 0,
                        backboard_contact: sctx.backboard_contact as u8,
                        is_blocked: 0,
                        blocker_roster_idx: 0xFF,
                        is_goaltend: 0,
                        assist_roster_idx: 0xFF,
                        foul_on_shot: if sctx.deferred_foul_call.is_some() { 1 } else { 0 },
                        telemetry: ShotTelemetryPublic {
                            release_tick: sctx.release_tick,
                            release_game_clock_ds: sctx.release_game_clock_ds,
                            whistle_game_clock_ds: 0xFFFF,
                            whistle_tick: 0xFFFF_FFFF,
                            flight_resolved_tick: self.state.sim_tick,
                            basket_scored_tick: 0xFFFF_FFFF,
                        },
                    });
                    self.record_reviewable_event(ReviewableEventEntry {
                        event_seq: ev_seq,
                        tick: self.state.sim_tick,
                        quarter: self.state.quarter,
                        game_clock_ds: self.state.game_clock_ds,
                        points_awarded: 0,
                        original_rule_code: RuleCode::NONE as u16,
                        scorer_side: None,
                        shooter_roster_idx: Some(sctx.shooter_roster_idx),
                        assist_roster_idx: None,
                        blocker_roster_idx: None,
                        original_is_three_pointer: sctx.shot_value == 3,
                        original_fga_counted: sctx.deferred_foul_call.is_none(),
                        stoppage_occurred_since: false,
                        shot_clock_expired_before_release: !sctx.released_before_shot_clock_zero,
                        ground_truth_is_three_pointer: sctx.ground_truth_is_three_pointer,
                        ground_truth_oob_by_side: Some(shooter_side),
                        ground_truth_goaltend_violation: false,
                        ground_truth_def_foul_after_motion_start: sctx.deferred_foul_call.is_some(),
                        official_not_reasonably_certain: false,
                    });
                    if let Some(df) = sctx.deferred_foul_call {
                        let mut call = df.call;
                        call.basket_made_on_play = false;
                        call.ft_count = sctx.shot_value;
                        let rid = self.state.next_ruling_id;
                        self.state.next_ruling_id += 1;
                        let period_exp = df.whistle_game_clock_ds == 0
                            || self.state.game_clock_rem_10us == 0;
                        self.open_pending_ruling(rid, df.whistle_tick, call, period_exp);
                        return Ok(());
                    }
                    if combined_gc_exp || self.state.game_clock_rem_10us == 0 {
                        self.handle_period_expiration();
                        return Ok(());
                    }
                    self.trigger_oob_violation(bx, by, Some(shooter_slot), shooter_side, false, true);
                    return Ok(());
                }

                self.state.ball.shot_ctx = Some(sctx);
                return Ok(());
            }

            // Arrived at backboard/rim: trigger physical bounce stage when applicable
            match sctx.intended_outcome {
                IntendedOutcomeClass::BankMake | IntendedOutcomeClass::BankMiss => {
                    sctx.backboard_contact = BackboardContact::AtOrAboveRim;
                    self.state.ball.backboard_contact_this_shot = BackboardContact::AtOrAboveRim;
                    self.state.ball_fx_flags |= 0x02;
                    self.state.ball_fx_timer_ds = 5;
                    let gx = rim_xy.0 + attack_dir * 0.34;
                    let gy = rim_xy.1 + rel_y.clamp(-1.8, 1.8) * 0.12;
                    let gz = self.geom.rim_height_m + 0.42;
                    self.state.ball.position_m = [gx, gy, gz];
                    self.state.ball.velocity_m_s = [
                        (rim_xy.0 - gx) / 0.2,
                        (rim_xy.1 - gy) / 0.2,
                        (self.geom.rim_height_m - gz + 0.5 * GRAVITY_M_S2 * 0.04) / 0.2,
                    ];
                    self.state.shot_sub_stage = 1;
                    self.state.shot_stage_ticks = 0;
                    self.state.ball.shot_ctx = Some(sctx);
                    return Ok(());
                }
                IntendedOutcomeClass::BackboardOnlyMiss => {
                    sctx.backboard_contact = BackboardContact::AtOrAboveRim;
                    self.state.ball.backboard_contact_this_shot = BackboardContact::AtOrAboveRim;
                    self.state.ball_fx_flags |= 0x02;
                    self.state.ball_fx_timer_ds = 5;
                    let gx = rim_xy.0 + attack_dir * 0.36;
                    let gy = rim_xy.1 + rel_y.clamp(-2.0, 2.0) * 0.18;
                    let gz = self.geom.rim_height_m + 0.45;
                    self.state.ball.position_m = [gx, gy, gz];
                    self.state.ball.velocity_m_s = [-attack_dir * 2.5, gy.signum() * 0.8, 1.8];
                    self.state.shot_sub_stage = 1;
                    self.state.shot_stage_ticks = 0;
                    self.state.ball.shot_ctx = Some(sctx);
                    return Ok(());
                }
                IntendedOutcomeClass::RimIn => {
                    sctx.rim_touched = true;
                    self.state.ball.rim_touched_this_shot = true;
                    self.state.ball_fx_flags |= 0x01;
                    self.state.ball_fx_timer_ds = 5;
                    let side_sign = if rel_y >= 0.0 { 1.0 } else { -1.0 };
                    self.state.ball.position_m = [
                        rim_xy.0 - attack_dir * 0.12,
                        rim_xy.1 + side_sign * 0.12,
                        self.geom.rim_height_m + 0.04,
                    ];
                    self.state.ball.velocity_m_s = [attack_dir * 0.45, -side_sign * 0.45, 2.15];
                    self.state.shot_sub_stage = 2;
                    self.state.shot_stage_ticks = 0;
                    self.state.ball.shot_ctx = Some(sctx);
                    return Ok(());
                }
                _ => {}
            }
        } else if self.state.shot_sub_stage == 1 {
            // Stage 1 (BackboardHit): bank toward rim or kick outward
            self.state.shot_stage_ticks = self.state.shot_stage_ticks.saturating_add(1);
            let done_glass = sctx.intended_outcome == IntendedOutcomeClass::BackboardOnlyMiss
                || self.state.shot_stage_ticks >= 2;
            if !done_glass {
                self.state.ball.shot_ctx = Some(sctx);
                return Ok(());
            }
            if sctx.intended_outcome == IntendedOutcomeClass::BankMiss {
                sctx.rim_touched = true;
                self.state.ball.rim_touched_this_shot = true;
                self.state.ball_fx_flags |= 0x01;
                self.state.ball_fx_timer_ds = 5;
                self.state.ball.position_m = [rim_xy.0, rim_xy.1, self.geom.rim_height_m + 0.05];
                self.state.ball.velocity_m_s = [-attack_dir * 0.9, rel_y.signum() * 0.4, 2.05];
                self.state.shot_sub_stage = 2;
                self.state.shot_stage_ticks = 0;
                self.state.ball.shot_ctx = Some(sctx);
                return Ok(());
            }
        } else if self.state.shot_sub_stage == 2 {
            // Stage 2 (RimBounce): rattle cleanly on the rim cylinder
            self.state.shot_stage_ticks = self.state.shot_stage_ticks.saturating_add(1);
            if self.state.ball.position_m[2] < self.geom.rim_height_m - 0.04 {
                self.state.ball.position_m[2] = self.geom.rim_height_m - 0.04;
                self.state.ball.velocity_m_s[2] = 1.2;
            }
            let max_rattle = if sctx.intended_outcome == IntendedOutcomeClass::RimIn { 3 } else { 2 };
            if self.state.shot_stage_ticks < max_rattle {
                self.state.ball.shot_ctx = Some(sctx);
                return Ok(());
            }
        }

        // Resolve shot outcome!
        let combined_gc_exp = gc_exp || self.state.shot_gc_expired_pending;
        self.state.shot_sub_stage = 0;
        self.state.shot_stage_ticks = 0;
        self.state.shot_gc_expired_pending = false;
        sctx.flight_resolved_tick = Some(self.state.sim_tick);
        let shooter_side = if shooter_team_idx == 0 {
            CommandSide::Home
        } else {
            CommandSide::Away
        };
        let made = sctx.intended_outcome.is_make();
        let points = if made { sctx.shot_value } else { 0 };
        let foul_on_shot = sctx.deferred_foul_call.is_some();

        sctx.rim_touched = sctx.intended_outcome.touches_rim();
        sctx.backboard_contact = if sctx.intended_outcome.touches_backboard() {
            BackboardContact::AtOrAboveRim
        } else {
            BackboardContact::None
        };
        self.state.ball.rim_touched_this_shot = sctx.rim_touched;
        self.state.ball.backboard_contact_this_shot = sctx.backboard_contact;
        self.state.ball.last_passer_roster_idx = None;

        if made {
            sctx.basket_scored_tick = Some(self.state.sim_tick);
            self.state.team_states[shooter_team_idx].score_on_court += points as u16;
            self.state.scoring_run_pts[shooter_team_idx] =
                self.state.scoring_run_pts[shooter_team_idx].saturating_add(points);
            self.state.scoring_run_pts[1 - shooter_team_idx] = 0;
            self.record_defense_possession_outcome(1 - shooter_team_idx, shooter_team_idx, points);
            self.state.ball.position_m = [rim_xy.0, rim_xy.1, self.geom.rim_height_m - 0.12];
            self.state.ball.velocity_m_s = [0.0, 0.0, -2.35];
            self.state.ball.ball_state = BallState::Loose;
            self.state.ball_fx_flags |= 0x10;
            self.state.ball_fx_timer_ds = 6;
        } else {
            let u_ang = sample_u53_checked(
                self.philox_key,
                self.state.possession_id,
                self.state.possession_tick,
                &mut self.state.rng_sample_counters.0,
                0xFF,
                RngPurpose::ShotScatterAndReboundVec,
            )?;
            let u_spd = sample_u53_checked(
                self.philox_key,
                self.state.possession_id,
                self.state.possession_tick,
                &mut self.state.rng_sample_counters.0,
                0xFF,
                RngPurpose::ShotScatterAndReboundVec,
            )?;
            if sctx.intended_outcome == IntendedOutcomeClass::Airball {
                self.state.ball.position_m = [
                    rim_xy.0 - attack_dir * 0.72,
                    rim_xy.1 + (if rel_y >= 0.0 { 0.85 } else { -0.85 }),
                    (self.geom.rim_height_m - 0.35).max(1.2),
                ];
                self.state.ball.velocity_m_s = [
                    attack_dir * 1.45,
                    (if rel_y >= 0.0 { 1.0 } else { -1.0 }) * (1.2 + u_ang),
                    -1.4,
                ];
            } else {
                let long_carom = if sctx.ground_truth_is_three_pointer { 1.4 } else { 0.0 };
                self.state.ball.position_m = [rim_xy.0, rim_xy.1, self.geom.rim_height_m];
                self.state.ball.velocity_m_s = if u_spd > 0.88 {
                    [
                        attack_dir * (1.7 + 1.4 * u_spd),
                        (if u_ang >= 0.5 { 1.0 } else { -1.0 }) * (2.6 + 2.4 * u_ang),
                        2.1,
                    ]
                } else {
                    [
                        -attack_dir * (2.2 + long_carom + 2.8 * u_spd),
                        (u_ang - 0.5) * (4.8 + long_carom * 0.8),
                        2.6 + 1.8 * u_spd,
                    ]
                };
            }
            if sctx.rim_touched {
                self.state.ball_fx_flags |= 0x01;
                self.state.ball_fx_timer_ds = 5;
            }
            if sctx.backboard_contact != BackboardContact::None {
                self.state.ball_fx_flags |= 0x02;
                self.state.ball_fx_timer_ds = 5;
            }
        }

        let shot_event_seq = self.state.next_event_seq;
        self.push_event(GameRecordEventPayload::ShotResolved {
            shot_id: sctx.shot_id,
            shooter_roster_idx: sctx.shooter_roster_idx,
            scorer_team: if made { shooter_team_idx as u8 } else { 0xFF },
            points_awarded: points,
            outcome_class: sctx.intended_outcome as u8,
            rim_touched: if sctx.rim_touched { 1 } else { 0 },
            backboard_contact: sctx.backboard_contact as u8,
            is_blocked: if sctx.intended_outcome == IntendedOutcomeClass::Blocked { 1 } else { 0 },
            blocker_roster_idx: sctx.blocker_roster_idx.unwrap_or(0xFF),
            is_goaltend: if sctx.intended_outcome == IntendedOutcomeClass::Goaltended { 1 } else { 0 },
            assist_roster_idx: if made {
                sctx.assist_roster_idx.unwrap_or(0xFF)
            } else {
                0xFF
            },
            foul_on_shot: if foul_on_shot { 1 } else { 0 },
            telemetry: ShotTelemetryPublic {
                release_tick: sctx.release_tick,
                release_game_clock_ds: sctx.release_game_clock_ds,
                whistle_game_clock_ds: sctx.whistle_game_clock_ds.unwrap_or(0xFFFF),
                whistle_tick: sctx.whistle_tick.unwrap_or(0xFFFF_FFFF),
                flight_resolved_tick: self.state.sim_tick,
                basket_scored_tick: sctx.basket_scored_tick.unwrap_or(0xFFFF_FFFF),
            },
        });

        // Push to BoundedReviewableEventRing for FIBA Appendix F.3 HCC
        self.record_reviewable_event(ReviewableEventEntry {
            event_seq: shot_event_seq,
            tick: self.state.sim_tick,
            quarter: self.state.quarter,
            game_clock_ds: self.state.game_clock_ds,
            points_awarded: points,
            original_rule_code: RuleCode::NONE as u16,
            scorer_side: if made { Some(shooter_side) } else { None },
            shooter_roster_idx: Some(sctx.shooter_roster_idx),
            assist_roster_idx: if made { sctx.assist_roster_idx } else { None },
            blocker_roster_idx: sctx.blocker_roster_idx,
            original_is_three_pointer: sctx.shot_value == 3,
            original_fga_counted: made || !foul_on_shot,
            stoppage_occurred_since: false,
            shot_clock_expired_before_release: !sctx.released_before_shot_clock_zero,
            ground_truth_is_three_pointer: sctx.ground_truth_is_three_pointer,
            ground_truth_oob_by_side: None,
            ground_truth_goaltend_violation: false,
            ground_truth_def_foul_after_motion_start: foul_on_shot,
            official_not_reasonably_certain: false,
        });

        // Priority 1: Deferred shooting foul or deferred def 3s (when missed)
        if let Some(df) = sctx.deferred_foul_call {
            self.state.deferred_def_3s_violation = None;
            let mut call = df.call;
            call.basket_made_on_play = made;
            call.ft_count = if made { 1 } else { sctx.shot_value };
            let rid = self.state.next_ruling_id;
            self.state.next_ruling_id += 1;
            let period_exp = df.whistle_game_clock_ds == 0 || self.state.game_clock_rem_10us == 0;
            self.open_pending_ruling(rid, df.whistle_tick, call, period_exp);
            return Ok(());
        }

        if made {
            self.state.deferred_def_3s_violation = None;
        } else if let Some((defender, _)) = self.state.deferred_def_3s_violation.take() {
            self.open_defensive_three_seconds(shooter_side, defender, Some(sctx.shooter_roster_idx));
            return Ok(());
        }

        // Priority 2: Period expired
        if combined_gc_exp || self.state.game_clock_rem_10us == 0 {
            if made {
                self.state.deferred_def_3s_violation = None;
            }
            self.handle_period_expiration();
            return Ok(());
        }

        // Priority 3: Normal make -> InboundSetup, Miss -> ReboundScramble
        if made {
            let opp = shooter_side.opponent();
            let stop_last_sec = if self.state.quarter >= 4 {
                self.ruleset.clocks.stop_on_made_final_ot_sec
            } else {
                self.ruleset.clocks.stop_on_made_reg_last_sec
            };
            let stop_clock = self.state.game_clock_ds <= stop_last_sec * 10;
            if stop_clock {
                self.state.game_clock_running = false;
            }
            self.state.shot_clock_running = false;
            let sc = compute_shot_clock_reset(
                ShotClockScenario::TurnoverOpponentToOwnBackcourt,
                self.state.shot_clock_ds,
                self.state.game_clock_ds,
                &self.ruleset,
            );
            let pending_to = self.state.pending_fiba_timeout_request[1 - shooter_team_idx];
            if pending_to && self.ruleset.challenges.mode == 2 {
                self.state.pending_fiba_timeout_request[1 - shooter_team_idx] = false;
                if let Some((rtag, dur, ch)) =
                    charge_team_timeout_with_mandatory_slots(&mut self.state, opp, &self.ruleset)
                {
                    self.state.inbound_context = Some(InboundContext {
                        side: opp,
                        spot: InboundSpot::BackcourtEndline,
                        sideline_side: CourtSideLine::TableSide,
                        allow_endline_run: true,
                        stop_clock_until_touch: true,
                        inbound_ball_at_disposal: false,
                        inbound_touched_court_player: false,
                        inbound_5s_timer_ds: 0,
                        original_spot: InboundSpot::BackcourtEndline,
                        pending_shot_clock_ds: sc,
                        is_period_start_arrow_inbound: false,
                        allow_both_sides_advance: true,
                        post_made_fg: true,
                        post_made_last_ft: false,
                    });
                    self.state.game_phase = GamePhase::InboundSetup;
                    self.enter_timeout_state(rtag, dur, ch);
                    return Ok(());
                }
            }
            self.state.game_phase = GamePhase::InboundSetup;
            self.reset_substage_counters();
            self.state.inbound_context = Some(InboundContext {
                side: opp,
                spot: InboundSpot::BackcourtEndline,
                sideline_side: CourtSideLine::TableSide,
                allow_endline_run: true,
                stop_clock_until_touch: stop_clock,
                inbound_ball_at_disposal: false,
                inbound_touched_court_player: false,
                inbound_5s_timer_ds: 0,
                original_spot: InboundSpot::BackcourtEndline,
                pending_shot_clock_ds: sc,
                is_period_start_arrow_inbound: false,
                allow_both_sides_advance: false,
                post_made_fg: true,
                post_made_last_ft: false,
            });
            return Ok(());
        }

        // Missed shot -> check if shot clock expired during NoScoreNoRim flight
        if sctx.rim_touched {
            self.state.scramble_origin = Some(ScrambleOrigin::FromRimContact {
                prior_offense: shooter_side,
            });
            self.state.shot_clock_running = false;
            self.state.shot_clock_ds = None;
            self.state.shot_clock_rem_10us = None;
        } else {
            self.state.scramble_origin = Some(ScrambleOrigin::FromNoRimMissOrBlock {
                prior_offense: shooter_side,
            });
            if sc_exp {
                self.trigger_shot_clock_violation();
                return Ok(());
            }
        }
        self.state.game_phase = GamePhase::ReboundScramble;
        self.state.scramble_ticks = 0;
        self.state.ball.ball_state = BallState::Loose;
        Ok(())
    }

    fn trigger_oob_violation(
        &mut self,
        oob_x: f64,
        oob_y: f64,
        last_touch_slot: Option<u8>,
        prior_offense: CommandSide,
        from_rim_scramble: bool,
        is_untouched_shot_miss_oob: bool,
    ) {
        let offending_side = last_touch_slot.map_or(prior_offense, |s| {
            if s < 5 {
                CommandSide::Home
            } else {
                CommandSide::Away
            }
        });
        let offender_idx = offending_side.team_index().unwrap_or(0);
        let offender_r = last_touch_slot
            .and_then(|s| {
                let t = (s as usize) / 5;
                let r = self.state.team_states[t].active_lineup_slots[(s as usize) % 5];
                (r != ROSTER_IDX_NONE).then_some(r)
            })
            .unwrap_or(self.state.team_states[offender_idx].active_lineup_slots[0]);

        let awarded_side = offending_side.opponent();
        let awarded_idx = awarded_side.team_index().unwrap_or(0);
        let awarded_dir = if awarded_idx == 0 { 1.0 } else { -1.0 };

        self.state.oob_spot_xy = [oob_x, oob_y];
        self.state.oob_is_endline = (libm::fabs(oob_x) / self.geom.half_length_m)
            >= (libm::fabs(oob_y) / self.geom.half_width_m);
        self.trigger_comments_iil_if_needed();

        let restart_sc = if awarded_side == prior_offense {
            if from_rim_scramble {
                compute_shot_clock_reset(
                    ShotClockScenario::ScrambleFoulOrOobKeepFrontcourt,
                    self.state.shot_clock_ds,
                    self.state.game_clock_ds,
                    &self.ruleset,
                )
            } else if self.state.ball_in_frontcourt {
                self.state.shot_clock_ds
            } else {
                compute_shot_clock_reset(
                    ShotClockScenario::OffenseBackcourtRetain { is_foul_or_kick: false },
                    self.state.shot_clock_ds,
                    self.state.game_clock_ds,
                    &self.ruleset,
                )
            }
        } else {
            self.record_defense_possession_outcome(awarded_idx, 1 - awarded_idx, 0);
            let in_awarded_fc = (oob_x * awarded_dir) > 0.0;
            let scenario = if in_awarded_fc {
                ShotClockScenario::TurnoverOpponentToOwnFrontcourt
            } else {
                ShotClockScenario::TurnoverOpponentToOwnBackcourt
            };
            let sc = compute_shot_clock_reset(
                scenario,
                self.state.shot_clock_ds,
                self.state.game_clock_ds,
                &self.ruleset,
            );
            if is_untouched_shot_miss_oob {
                self.push_event(GameRecordEventPayload::ReboundSecured {
                    rebounder_roster_idx: 0xFF,
                    rebound_team: awarded_idx as u8,
                    is_offensive: 0,
                    is_deadball_team_reb: 1,
                    scramble_origin: 3,
                    new_shot_clock_ds: encode_shot_clock_opt(sc),
                });
            } else {
                self.push_event(GameRecordEventPayload::TurnoverCommitted {
                    offender_roster_idx: offender_r,
                    team_side: offending_side as u8,
                    turnover_rule_code: RuleCode::VIO_OOB as u16,
                    stealer_roster_idx: 0xFF,
                    is_live_steal: 0,
                });
            }
            sc
        };

        self.push_event(GameRecordEventPayload::ViolationTriggered {
            offender_roster_idx: offender_r,
            team_side: offending_side as u8,
            rule_code: RuleCode::VIO_OOB as u16,
            footwork: FootworkTelemetryPublic::default(),
        });
        let rid = self.state.next_ruling_id;
        self.state.next_ruling_id += 1;
        let call = RuleCallSnapshot {
            rule_code: RuleCode::VIO_OOB,
            offending_side,
            offender_roster_idx: offender_r,
            interruption_spot: InboundSpot::NearestToInterruption,
            stoppage_sideline_side: CourtSideLine::from_y_m(oob_y),
            restart_shot_clock_ds: restart_sc,
            ..RuleCallSnapshot::default()
        };
        self.open_pending_ruling(rid, self.state.sim_tick, call, false);
    }

    fn step_rebound_scramble(&mut self) -> Result<(), RngBudgetExceeded> {
        self.step_player_vertical_kinematics();
        self.state.scramble_ticks = self.state.scramble_ticks.saturating_add(1);
        let origin = self.state.scramble_origin.unwrap_or(ScrambleOrigin::FromRimContact {
            prior_offense: CommandSide::Home,
        });
        let prior_offense = origin.prior_offense();
        let prior_off_idx = prior_offense.team_index().unwrap_or(0);
        let prior_off_dir = if prior_off_idx == 0 { 1.0 } else { -1.0 };
        let from_rim_or_ft = matches!(
            origin,
            ScrambleOrigin::FromRimContact { .. } | ScrambleOrigin::FromFreeThrowMiss { .. }
        );

        // Advance game clock if running (or on first legal touch for FromFreeThrowMiss!)
        if matches!(origin, ScrambleOrigin::FromFreeThrowMiss { .. }) && !self.state.game_clock_running {
            self.state.game_clock_running = true;
            if matches!(
                self.ruleset.clocks.shot_clock_policy,
                ShotClockResetPolicy::NbaRule7
            ) {
                self.state.comments_iil_origin_clocks = Some(CommentsIILOrigin {
                    kind: 1,
                    start_gc_rem_10us: self.state.game_clock_rem_10us,
                    start_sc_rem_10us: self.state.shot_clock_rem_10us,
                });
            }
        }
        let (_, gc_exp, sc_exp) = advance_clocks_sub_ds(&mut self.state, 1.0);
        if gc_exp {
            self.handle_period_expiration();
            return Ok(());
        }
        if sc_exp && matches!(origin, ScrambleOrigin::FromNoRimMissOrBlock { .. }) {
            self.trigger_shot_clock_violation();
            return Ok(());
        }

        // Advance loose ball trajectory under gravity & realistic hardwood floor bounce (NO invisible wall clamps!)
        for k in 0..3 {
            self.state.ball.position_m[k] += self.state.ball.velocity_m_s[k] * 0.1;
        }
        self.state.ball.velocity_m_s[2] -= GRAVITY_M_S2 * 0.1;
        if self.state.ball.position_m[2] < 0.12 {
            self.state.ball.position_m[2] = 0.12;
            self.state.ball.velocity_m_s[2] = (-self.state.ball.velocity_m_s[2] * 0.65).max(0.0);
            self.state.ball.velocity_m_s[0] *= 0.85;
            self.state.ball.velocity_m_s[1] *= 0.85;
        }

        let bx = self.state.ball.position_m[0];
        let by = self.state.ball.position_m[1];
        let bz = self.state.ball.position_m[2];
        let is_nba = self.ruleset.fouls.goaltend_mode == 0;
        let untouched_miss_oob = from_rim_or_ft && self.state.scramble_ticks <= 8;
        if (bz <= 0.14 && self.geom.is_out_of_bounds(bx, by))
            || libm::fabs(bx) >= self.geom.half_length_m + 0.70
            || libm::fabs(by) >= self.geom.half_width_m + 0.70
            || self.geom.is_over_or_behind_backboard(
                self.state.ball.position_m,
                prior_off_dir,
                is_nba,
            )
        {
            self.trigger_oob_violation(
                bx,
                by,
                self.state.ball.last_touched_by_slot,
                prior_offense,
                from_rim_or_ft,
                untouched_miss_oob,
            );
            return Ok(());
        }

        // Move players aggressively toward loose ball / rebounding spots
        self.step_shot_or_rebound_kinematics(prior_off_idx, 1 - prior_off_idx, false)?;

        // Update player OOB reentry status after horizontal movement
        for team_idx in 0..2 {
            for p_pos in 0..5 {
                let r = self.state.team_states[team_idx].active_lineup_slots[p_pos];
                if r != ROSTER_IDX_NONE {
                    let p = &mut self.state.players[r as usize];
                    if p.z_m <= 0.02 {
                        if self.geom.is_out_of_bounds(p.x_m, p.y_m) {
                            p.oob_reentry_pending = true;
                        } else if libm::fabs(p.x_m) <= self.geom.half_length_m - 0.05
                            && libm::fabs(p.y_m) <= self.geom.half_width_m - 0.05
                        {
                            p.oob_reentry_pending = false;
                        }
                    }
                }
            }
        }

        // Wait until ball descends into reach before resolving scramble
        let still_rising_or_high =
            (self.state.ball.velocity_m_s[2] > 0.35 || bz > 3.15) && self.state.scramble_ticks < 14;
        let too_soon = self.state.scramble_ticks < 3
            && !(self.state.scramble_ticks >= 2 && bz <= 2.30 && self.state.ball.velocity_m_s[2] <= 0.0);
        if still_rising_or_high || too_soon {
            return Ok(());
        }

        // Hard physical reach cutoff + true geometric box-out detection (NO cross-court teleports)
        let bx = self.state.ball.position_m[0];
        let by = self.state.ball.position_m[1];
        let bz = self.state.ball.position_m[2];
        let reach_lim = if self.state.scramble_ticks >= 26 {
            2.20
        } else if self.state.scramble_ticks >= 18 && bz <= 2.40 {
            1.95
        } else {
            1.55
        };

        let mut cands = Vec::with_capacity(10);
        let mut best_fallback: Option<(u8, u8, f64, f64)> = None;
        for team_idx in 0..2 {
            let is_off = (team_idx == 0 && prior_offense == CommandSide::Home)
                || (team_idx == 1 && prior_offense == CommandSide::Away);
            let offset = team_idx * 15;
            for p_pos in 0..5 {
                let slot = (team_idx * 5 + p_pos) as u8;
                let r_idx = self.state.team_states[team_idx].active_lineup_slots[p_pos];
                if r_idx == ROSTER_IDX_NONE {
                    continue;
                }
                let rank = (r_idx as usize) - offset;
                let p = &self.state.players[r_idx as usize];
                let snap = &self.teams[team_idx].players[rank];
                let hand_reach_z =
                    (snap.physical.standing_reach_cm / 100.0) + p.z_m.max(0.35);
                let d2 = libm::hypot(p.x_m - bx, p.y_m - by);
                let d3 = libm::sqrt(d2 * d2 + (bz - 1.5) * (bz - 1.5));
                let cid = canonical_actor_id(self.canonical_team_ids[team_idx], rank);

                if d2 <= 2.25
                    && bz <= hand_reach_z + 0.25
                    && best_fallback.map_or(true, |b| d2 < b.3)
                {
                    best_fallback = Some((slot, cid, 1.0, d2));
                }

                if d2 > reach_lim || bz > hand_reach_z + 0.18 {
                    continue;
                }

                // True geometric box-out check: defender within 1.85m and inside/even along lane
                let opp_team = 1 - team_idx;
                let mut boxout_disadv = false;
                if is_off {
                    for dp_pos in 0..5 {
                        let dr = self.state.team_states[opp_team].active_lineup_slots[dp_pos];
                        if dr != ROSTER_IDX_NONE {
                            let dp = &self.state.players[dr as usize];
                            if libm::hypot(dp.x_m - p.x_m, dp.y_m - p.y_m) <= 1.85
                                && libm::hypot(dp.x_m - bx, dp.y_m - by) <= d2 + 0.25
                            {
                                boxout_disadv = true;
                                break;
                            }
                        }
                    }
                }

                let reb_attr = if is_off {
                    snap.attributes[AttrIdx::OffensiveRebound]
                } else {
                    snap.attributes[AttrIdx::DefensiveRebound]
                };
                let vert = snap.attributes[AttrIdx::Vertical];
                let hustle = snap.attributes[AttrIdx::Hustle];
                let fat_pen = compute_fatigue_penalty(p.cumulative_stamina, &self.tunables);
                let w = compute_rebound_weight(
                    d2,
                    reb_attr,
                    vert,
                    hustle,
                    boxout_disadv,
                    fat_pen,
                    &self.tunables,
                );
                cands.push((slot, cid, w, d3));
            }
        }
        if cands.is_empty() {
            if self.state.scramble_ticks >= 26 {
                if let Some(fb) = best_fallback {
                    cands.push(fb);
                }
            }
            if cands.is_empty() {
                return Ok(());
            }
        }
        cands.sort_by_key(|c| c.1);
        let has_def_in_reach = cands.iter().any(|c| ((c.0 as usize) / 5) != prior_off_idx);
        if has_def_in_reach {
            for c in &mut cands {
                if ((c.0 as usize) / 5) == prior_off_idx {
                    c.2 *= 0.56;
                }
            }
        }

        // Check loose-ball foul when opponents collide inside rebound pocket
        let has_home_cand = cands.iter().any(|c| c.0 < 5);
        let has_away_cand = cands.iter().any(|c| c.0 >= 5);
        if has_home_cand && has_away_cand {
            let contact_cand = cands
                .iter()
                .find(|c| self.state.slot_contact_timer_ds[c.0 as usize] > 0)
                .copied();
            if let Some((f_slot, f_cid, _, _)) = contact_cand {
                let u_lb = sample_u53_checked(
                    self.philox_key,
                    self.state.possession_id,
                    self.state.possession_tick,
                    &mut self.state.rng_sample_counters.0,
                    f_cid,
                    RngPurpose::ContactAndFoulRoll,
                )?;
                if u_lb < 0.032 {
                    let off_t = (f_slot as usize) / 5;
                    let def_t = 1 - off_t;
                    let off_r =
                        self.state.team_states[off_t].active_lineup_slots[(f_slot as usize) % 5];
                    let opp_cand = cands.iter().find(|c| (c.0 as usize) / 5 == def_t).copied();
                    let opp_r = opp_cand.map_or(
                        self.state.team_states[def_t].active_lineup_slots[4],
                        |c| self.state.team_states[def_t].active_lineup_slots[(c.0 as usize) % 5],
                    );
                    let f_side = if off_t == 0 {
                        CommandSide::Home
                    } else {
                        CommandSide::Away
                    };
                    let restart_sc = if f_side.opponent() == prior_offense && from_rim_or_ft {
                        compute_shot_clock_reset(
                            ShotClockScenario::ScrambleFoulOrOobKeepFrontcourt,
                            self.state.shot_clock_ds,
                            self.state.game_clock_ds,
                            &self.ruleset,
                        )
                    } else {
                        compute_shot_clock_reset(
                            ShotClockScenario::LiveStealOrDefRebound,
                            self.state.shot_clock_ds,
                            self.state.game_clock_ds,
                            &self.ruleset,
                        )
                    };
                    self.state.oob_spot_xy = [bx, by];
                    self.state.oob_is_endline = false;
                    let rid = self.state.next_ruling_id;
                    self.state.next_ruling_id += 1;
                    let call = RuleCallSnapshot {
                        rule_code: RuleCode::FOUL_LOOSE_BALL,
                        offending_side: f_side,
                        offender_roster_idx: off_r,
                        victim_roster_idx: Some(opp_r),
                        ball_in_frontcourt_at_call: true,
                        interruption_spot: InboundSpot::NearestToInterruption,
                        stoppage_sideline_side: CourtSideLine::from_y_m(by),
                        restart_shot_clock_ds: restart_sc,
                        foul_category: 0,
                        ..RuleCallSnapshot::default()
                    };
                    self.open_pending_ruling(rid, self.state.sim_tick, call, false);
                    return Ok(());
                }
            }
        }

        let u_tip = sample_u53_checked(
            self.philox_key,
            self.state.possession_id,
            self.state.possession_tick,
            &mut self.state.rng_sample_counters.0,
            0xFF,
            RngPurpose::ReboundBounce,
        )?;
        if u_tip < self.tunables.rebound_tip_continue_prob && self.state.scramble_ticks < 9 && bz > 1.35 {
            let tip_slot = cands
                .iter()
                .min_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(core::cmp::Ordering::Equal))
                .map(|c| c.0)
                .unwrap_or(cands[0].0);
            let tip_team = (tip_slot as usize) / 5;
            let tip_r = self.state.team_states[tip_team].active_lineup_slots[(tip_slot as usize) % 5];
            if tip_r != ROSTER_IDX_NONE {
                let tp = &mut self.state.players[tip_r as usize];
                if tp.z_m <= 0.01 {
                    tp.vz_m_s = 2.5;
                }
                tp.action_state = PlayerActionState::ReboundJump;
                tp.action_timer_ds = 5;
            }
            self.state.ball.last_touched_by_slot = Some(tip_slot);
            self.state.ball.velocity_m_s[2] = 2.15;
            self.state.ball.velocity_m_s[0] *= 0.55;
            self.state.ball.velocity_m_s[1] += (u_tip - 0.15) * 1.4;
            return Ok(());
        }

        if let Some((win_slot, _)) = select_rebound_winner(
            &cands,
            self.philox_key,
            self.state.possession_id,
            self.state.possession_tick,
            &mut self.state.rng_sample_counters.0,
        )? {
            let win_team_idx = (win_slot as usize) / 5;
            let win_side = if win_team_idx == 0 {
                CommandSide::Home
            } else {
                CommandSide::Away
            };
            let is_off = win_side == prior_offense;
            let win_r = self.state.team_states[win_team_idx].active_lineup_slots[(win_slot as usize) % 5];
            let win_attack_dir = if win_team_idx == 0 { 1.0 } else { -1.0 };
            let (wx, wy, wz) = {
                let wp = &self.state.players[win_r as usize];
                (wp.x_m, wp.y_m, wp.z_m)
            };

            // Grounded rebounder stepping out-of-bounds triggers immediate VIO_OOB!
            if wz <= 0.02
                && (self.geom.is_out_of_bounds(wx, wy)
                    || self.state.players[win_r as usize].oob_reentry_pending)
            {
                self.state.ball.last_touched_by_slot = Some(win_slot);
                self.trigger_oob_violation(wx, wy, Some(win_slot), prior_offense, from_rim_or_ft, false);
                return Ok(());
            }

            let scenario = if is_off {
                match origin {
                    ScrambleOrigin::FromNoRimMissOrBlock { .. } => {
                        ShotClockScenario::OffReboundNoRimAirballOrBlock
                    }
                    _ => ShotClockScenario::OffReboundAfterRimOrFtMiss,
                }
            } else {
                ShotClockScenario::LiveStealOrDefRebound
            };

            let new_sc = compute_shot_clock_reset(
                scenario,
                self.state.shot_clock_ds,
                self.state.game_clock_ds,
                &self.ruleset,
            );

            // Check NBA Rule 7 §II-III: airball defense recovery after 24s expired
            if !is_off
                && matches!(origin, ScrambleOrigin::FromNoRimMissOrBlock { .. })
                && self.state.shot_clock_rem_10us == Some(0)
                && !self.ruleset.clocks.shot_clock_airball_defense_control_continues
            {
                self.trigger_shot_clock_violation();
                return Ok(());
            }

            let gc_ds = self.state.game_clock_ds;
            set_clocks_exact_ds(&mut self.state, gc_ds, new_sc);
            self.state.shot_clock_running = new_sc.is_some();
            self.state.possession_side = Some(win_side);
            if !is_off {
                self.reset_possession_epoch();
                self.record_defense_possession_outcome(win_team_idx, 1 - win_team_idx, 0);
                self.start_offensive_possession(win_team_idx, false, true);
            } else {
                self.state.play_phase_idx[win_team_idx] = 0;
                self.state.play_phase_elapsed_ds[win_team_idx] = 0;
                self.state.play_screen_contact_ds[win_team_idx] = 0;
                self.state.transition_window_ds[win_team_idx] = 0;
                self.state.ball.last_passer_roster_idx = None;
            }
            self.state.game_phase = GamePhase::LiveBall;
            self.state.ball.ball_state = BallState::Held;
            self.state.ball.owner_slot = Some(win_slot);
            self.state.ball.last_touched_by_slot = Some(win_slot);
            self.state.ball.last_team_control_side = Some(win_side);
            {
                let wp = &mut self.state.players[win_r as usize];
                if bz > 2.05 && wp.z_m <= 0.01 {
                    wp.vz_m_s = 2.6;
                }
                wp.action_state = PlayerActionState::ReboundJump;
                wp.action_timer_ds = 5;
                wp.feet_set_ticks = 0;
                wp.dribbles_since_catch = 0;
                wp.caught_ball_inside_ldb = self.geom.is_in_ldb(wx, wy, win_attack_dir);
                wp.footwork = FootworkRecord {
                    dribble_status: DribbleStatus::LiveTripleThreat,
                    ..FootworkRecord::default()
                };
            };
            self.state.scramble_ticks = 0;
            self.state.ball.position_m = [wx, wy, 1.35 + wz];
            self.state.ball.velocity_m_s = [0.0, 0.0, 0.0];
            self.state.ball_in_frontcourt = (wx * win_attack_dir) > 0.0;
            self.state.backcourt_8s_timer_ds = 0;
            self.state.scramble_origin = None;

            let sc_wire = match (is_off, origin) {
                (true, ScrambleOrigin::FromNoRimMissOrBlock { .. }) => 0xFFFE,
                _ => encode_shot_clock_opt(self.state.shot_clock_ds),
            };
            self.push_event(GameRecordEventPayload::ReboundSecured {
                rebounder_roster_idx: win_r,
                rebound_team: win_team_idx as u8,
                is_offensive: if is_off { 1 } else { 0 },
                is_deadball_team_reb: 0,
                scramble_origin: origin.tag(),
                new_shot_clock_ds: sc_wire,
            });
            self.push_event(GameRecordEventPayload::PossessionEstablished {
                possession_id: self.state.possession_id,
                side: win_team_idx as u8,
                handler_roster_idx: win_r,
                trigger_kind: if is_off { 4 } else { 2 },
                new_shot_clock_ds: sc_wire,
            });
        }
        Ok(())
    }

    #[inline]
    fn steer_player_velocity(
        p: &mut PlayerRuntimeState,
        dx: f64,
        dy: f64,
        cap_spd: f64,
        max_accel_m_s2: f64,
    ) -> f64 {
        let dist = libm::hypot(dx, dy);
        let cur_spd = libm::hypot(p.vx_m_s, p.vy_m_s);
        let brake_dist =
            ((cur_spd * cur_spd) / (2.0 * max_accel_m_s2.max(6.5)) + 0.55).max(1.25);
        let desired_spd = if dist <= 0.22 {
            0.0
        } else if dist < brake_dist {
            cap_spd * libm::sqrt((dist / brake_dist).clamp(0.0, 1.0)).max(0.18)
        } else {
            cap_spd
        };
        let (mut dir_x, mut dir_y) = if dist > 1.0e-6 {
            (dx / dist, dy / dist)
        } else {
            (0.0, 0.0)
        };
        if cur_spd > 1.2 && dist > brake_dist && desired_spd > 0.5 {
            let cx = p.vx_m_s / cur_spd;
            let cy = p.vy_m_s / cur_spd;
            let bx = dir_x * 0.72 + cx * 0.28;
            let by = dir_y * 0.72 + cy * 0.28;
            let bn = libm::hypot(bx, by);
            if bn > 1.0e-5 {
                dir_x = bx / bn;
                dir_y = by / bn;
            }
        }
        let (dvx, dvy) = (dir_x * desired_spd, dir_y * desired_spd);
        let diff_x = dvx - p.vx_m_s;
        let diff_y = dvy - p.vy_m_s;
        let is_braking = diff_x * p.vx_m_s + diff_y * p.vy_m_s < 0.0;
        let dv_lim = if is_braking {
            max_accel_m_s2 * 1.45 * 0.1
        } else {
            max_accel_m_s2 * 0.1
        };
        let diff_n = libm::hypot(diff_x, diff_y);
        if diff_n > dv_lim && diff_n > 1.0e-9 {
            let scale = dv_lim / diff_n;
            p.vx_m_s += diff_x * scale;
            p.vy_m_s += diff_y * scale;
        } else {
            p.vx_m_s = dvx;
            p.vy_m_s = dvy;
        }
        dist
    }

    #[inline]
    fn point_to_segment_dist(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
        let vx = bx - ax;
        let vy = by - ay;
        let len_sq = vx * vx + vy * vy;
        if len_sq < 1.0e-8 {
            return libm::hypot(px - ax, py - ay);
        }
        let t = (((px - ax) * vx + (py - ay) * vy) / len_sq).clamp(0.0, 1.0);
        libm::hypot(px - (ax + t * vx), py - (ay + t * vy))
    }

    fn run_ccd_on_actors(&mut self, actors: &mut [CcdActor]) -> Vec<(usize, usize)> {
        let contact_pairs = run_swept_ccd_step(
            actors,
            0.1,
            self.state.sim_tick,
            self.debug_ccd_hooks,
            &mut self.pending_audit,
        );
        for a in actors.iter() {
            let team_idx = a.slot / 5;
            let p_pos = a.slot % 5;
            let r = self.state.team_states[team_idx].active_lineup_slots[p_pos];
            if r != ROSTER_IDX_NONE {
                self.state.players[r as usize].x_m = a.x;
                self.state.players[r as usize].y_m = a.y;
                self.state.players[r as usize].vx_m_s = a.vx;
                self.state.players[r as usize].vy_m_s = a.vy;
            }
        }
        self.apply_physical_body_contact_mechanics(&contact_pairs);
        contact_pairs
    }

    fn apply_physical_body_contact_mechanics(&mut self, contact_pairs: &[(usize, usize)]) {
        let home_slots = self.state.team_states[0].active_lineup_slots;
        let away_slots = self.state.team_states[1].active_lineup_slots;
        let rim_home = self.geom.rim_xy(1.0);
        let rim_away = self.geom.rim_xy(-1.0);

        for sa in 0..5 {
            let ra = home_slots[sa];
            if ra == ROSTER_IDX_NONE {
                continue;
            }
            let rank_a = (ra as usize).saturating_sub(0);
            let Some(snap_a) = self.teams[0].players.get(rank_a) else {
                continue;
            };
            for sb in 5..10 {
                let rb = away_slots[sb - 5];
                if rb == ROSTER_IDX_NONE {
                    continue;
                }
                let rank_b = (rb as usize).saturating_sub(15);
                let Some(snap_b) = self.teams[1].players.get(rank_b) else {
                    continue;
                };

                let pa = self.state.players[ra as usize];
                let pb = self.state.players[rb as usize];
                let dx = pb.x_m - pa.x_m;
                let dy = pb.y_m - pa.y_m;
                let dist = libm::hypot(dx, dy);
                let touch_thresh =
                    snap_a.physical.collision_radius_m() + snap_b.physical.collision_radius_m() + 0.18;
                let is_ccd_hit = contact_pairs
                    .iter()
                    .any(|&(u, v)| (u == sa && v == sb) || (u == sb && v == sa));
                if !is_ccd_hit && dist > touch_thresh {
                    continue;
                }

                let inv_d = 1.0 / dist.max(0.05);
                let nx = dx * inv_d;
                let ny = dy * inv_d;
                let str_diff =
                    (f64::from(snap_a.attributes[AttrIdx::Strength])
                        - f64::from(snap_b.attributes[AttrIdx::Strength])) / 99.0;
                let wt_diff =
                    (snap_a.physical.weight_kg - snap_b.physical.weight_kg) / 40.0;
                let adv_a = str_diff * 0.55 + wt_diff * 0.45;

                self.state.slot_contact_timer_ds[sa] =
                    self.state.slot_contact_timer_ds[sa].max(3);
                self.state.slot_contact_timer_ds[sb] =
                    self.state.slot_contact_timer_ds[sb].max(3);

                // Hard Screen Body Wall
                if pa.action_state == PlayerActionState::SetScreen
                    && matches!(
                        pb.action_state,
                        PlayerActionState::NavScreen
                            | PlayerActionState::DefSlide
                            | PlayerActionState::Sprint
                            | PlayerActionState::Closeout
                    )
                {
                    self.state.players[ra as usize].vx_m_s *= 0.25;
                    self.state.players[ra as usize].vy_m_s *= 0.25;
                    self.state.players[rb as usize].vx_m_s =
                        self.state.players[rb as usize].vx_m_s * 0.35 + nx * 0.65;
                    self.state.players[rb as usize].vy_m_s =
                        self.state.players[rb as usize].vy_m_s * 0.35 + ny * 0.65;
                    self.state.slot_contact_timer_ds[sa] = 4;
                    self.state.slot_contact_timer_ds[sb] = 4;
                } else if pb.action_state == PlayerActionState::SetScreen
                    && matches!(
                        pa.action_state,
                        PlayerActionState::NavScreen
                            | PlayerActionState::DefSlide
                            | PlayerActionState::Sprint
                            | PlayerActionState::Closeout
                    )
                {
                    self.state.players[rb as usize].vx_m_s *= 0.25;
                    self.state.players[rb as usize].vy_m_s *= 0.25;
                    self.state.players[ra as usize].vx_m_s =
                        self.state.players[ra as usize].vx_m_s * 0.35 - nx * 0.65;
                    self.state.players[ra as usize].vy_m_s =
                        self.state.players[ra as usize].vy_m_s * 0.35 - ny * 0.65;
                    self.state.slot_contact_timer_ds[sa] = 4;
                    self.state.slot_contact_timer_ds[sb] = 4;
                }

                // Drive Hip-Ride / Body Bump
                if pa.action_state == PlayerActionState::Drive {
                    let bump = 0.75 + adv_a.max(0.0) * 0.90;
                    self.state.players[rb as usize].vx_m_s += nx * bump;
                    self.state.players[rb as usize].vy_m_s += ny * bump;
                    if adv_a < -0.10 {
                        self.state.players[ra as usize].vx_m_s *= 0.74;
                        self.state.players[ra as usize].vy_m_s *= 0.74;
                        self.state.players[ra as usize].burst_energy =
                            (self.state.players[ra as usize].burst_energy - 0.8).max(0.0);
                    }
                }
                if pb.action_state == PlayerActionState::Drive {
                    let bump = 0.75 + (-adv_a).max(0.0) * 0.90;
                    self.state.players[ra as usize].vx_m_s -= nx * bump;
                    self.state.players[ra as usize].vy_m_s -= ny * bump;
                    if adv_a > 0.10 {
                        self.state.players[rb as usize].vx_m_s *= 0.74;
                        self.state.players[rb as usize].vy_m_s *= 0.74;
                        self.state.players[rb as usize].burst_energy =
                            (self.state.players[rb as usize].burst_energy - 0.8).max(0.0);
                    }
                }

                // Post Backdown Rhythmic Bump
                if (self.state.sim_tick % 5) == 0 {
                    if pa.action_state == PlayerActionState::PostBackdown {
                        let rx = rim_home.0 - pa.x_m;
                        let ry = rim_home.1 - pa.y_m;
                        let rn = libm::hypot(rx, ry).max(0.2);
                        let push = (0.75 + adv_a * 0.65).clamp(0.25, 1.45);
                        self.state.players[rb as usize].vx_m_s += (rx / rn) * push;
                        self.state.players[rb as usize].vy_m_s += (ry / rn) * push;
                        self.state.players[rb as usize].burst_energy =
                            (self.state.players[rb as usize].burst_energy - 0.9).max(0.0);
                    }
                    if pb.action_state == PlayerActionState::PostBackdown {
                        let rx = rim_away.0 - pb.x_m;
                        let ry = rim_away.1 - pb.y_m;
                        let rn = libm::hypot(rx, ry).max(0.2);
                        let push = (0.75 + (-adv_a) * 0.65).clamp(0.25, 1.45);
                        self.state.players[ra as usize].vx_m_s += (rx / rn) * push;
                        self.state.players[ra as usize].vy_m_s += (ry / rn) * push;
                        self.state.players[ra as usize].burst_energy =
                            (self.state.players[ra as usize].burst_energy - 0.9).max(0.0);
                    }
                }

                // Box-Out Seal during Shot Flight / Rebound
                if pb.action_state == PlayerActionState::BoxOut
                    && self.state.game_phase != GamePhase::FreeThrows
                {
                    self.state.players[ra as usize].vx_m_s =
                        self.state.players[ra as usize].vx_m_s * 0.55 - nx * 0.45;
                    self.state.players[ra as usize].vy_m_s =
                        self.state.players[ra as usize].vy_m_s * 0.55 - ny * 0.45;
                }
                if pa.action_state == PlayerActionState::BoxOut
                    && self.state.game_phase != GamePhase::FreeThrows
                {
                    self.state.players[rb as usize].vx_m_s =
                        self.state.players[rb as usize].vx_m_s * 0.55 + nx * 0.45;
                    self.state.players[rb as usize].vy_m_s =
                        self.state.players[rb as usize].vy_m_s * 0.55 + ny * 0.45;
                }
            }
        }
    }

    fn step_inbound_approach_kinematics(
        &mut self,
        off_idx: usize,
        def_idx: usize,
        inb_xy: (f64, f64),
        recv_xy: (f64, f64),
        inbounder_pos: usize,
    ) -> Result<(), RngBudgetExceeded> {
        let receiver_pos = self.preferred_handler_pos(off_idx, Some(inbounder_pos));
        self.tick_player_action_timers();
        let attack_dir = if off_idx == 0 { 1.0 } else { -1.0 };
        let rim_xy = self.geom.rim_xy(attack_dir);
        let nav_b = self.geom.inbounds_nav_bounds(0.55);
        let mut actors = Vec::with_capacity(10);

        for team_idx in 0..2 {
            let is_off = team_idx == off_idx;
            for p_pos in 0..5 {
                let slot = team_idx * 5 + p_pos;
                let r = self.state.team_states[team_idx].active_lineup_slots[p_pos];
                if r == ROSTER_IDX_NONE {
                    continue;
                }
                let rank = (r as usize) - team_idx * 15;
                let snap = &self.teams[team_idx].players[rank];
                let (tx, ty) = if is_off {
                    if p_pos == inbounder_pos {
                        inb_xy
                    } else if p_pos == receiver_pos {
                        nav_b.clamp(recv_xy.0, recv_xy.1)
                    } else {
                        nav_b.clamp(
                            -attack_dir * 2.5 + attack_dir * (p_pos as f64) * 1.4,
                            (p_pos as f64 - 2.5) * 2.4,
                        )
                    }
                } else {
                    let opp_pos = (self.state.defense_plans[def_idx].matchups()[p_pos] as usize).min(4);
                    let opp_r = self.state.team_states[off_idx].active_lineup_slots[opp_pos];
                    if opp_r != ROSTER_IDX_NONE {
                        let mx = self.state.players[opp_r as usize].x_m;
                        let my = self.state.players[opp_r as usize].y_m;
                        nav_b.clamp(mx * 0.55 + (attack_dir * 4.0) * 0.45, my * 0.85)
                    } else {
                        nav_b.clamp(attack_dir * 3.5, 0.0)
                    }
                };
                self.transient_target_xy[slot] = (tx, ty);
                let p = &mut self.state.players[r as usize];
                let dx = tx - p.x_m;
                let dy = ty - p.y_m;
                let cap_spd = if is_off && p_pos == inbounder_pos { 7.4 } else { 6.4 };
                let dist = Self::steer_player_velocity(p, dx, dy, cap_spd, 9.2);
                if dist > 0.2 {
                    p.facing_rad = libm::atan2(dy, dx);
                } else {
                    p.facing_rad = libm::atan2(rim_xy.1 - p.y_m, rim_xy.0 - p.x_m);
                }
                if p.action_timer_ds == 0 && p.action_state != PlayerActionState::Pass {
                    p.action_state = if dist > 1.2 {
                        PlayerActionState::Jog
                    } else {
                        PlayerActionState::Idle
                    };
                }
                let rad = snap.physical.collision_radius_m();
                actors.push(CcdActor {
                    slot,
                    canonical_actor_id: canonical_actor_id(self.canonical_team_ids[team_idx], rank),
                    x: p.x_m,
                    y: p.y_m,
                    vx: p.vx_m_s,
                    vy: p.vy_m_s,
                    radius: rad,
                    inv_mass: 1.0 / snap.physical.weight_kg.max(55.0),
                    bounds: self.geom.playable_bounds(rad),
                });
            }
        }
        let _ = self.run_ccd_on_actors(&mut actors);
        Ok(())
    }

    fn step_shot_or_rebound_kinematics(
        &mut self,
        off_idx: usize,
        def_idx: usize,
        during_shot_flight: bool,
    ) -> Result<(), RngBudgetExceeded> {
        self.tick_player_action_timers();
        let attack_dir = if off_idx == 0 { 1.0 } else { -1.0 };
        let rim_xy = self.geom.rim_xy(attack_dir);
        let ball_xy = (self.state.ball.position_m[0], self.state.ball.position_m[1]);
        let nav_b = self.geom.inbounds_nav_bounds(0.45);
        let crash_cnt = (self.teams[off_idx].tactics.orb_crash_count as usize).clamp(1, 3);
        let mut actors = Vec::with_capacity(10);

        for team_idx in 0..2 {
            let is_off = team_idx == off_idx;
            for p_pos in 0..5 {
                let slot = team_idx * 5 + p_pos;
                let r = self.state.team_states[team_idx].active_lineup_slots[p_pos];
                if r == ROSTER_IDX_NONE {
                    continue;
                }
                let rank = (r as usize) - team_idx * 15;
                let snap = &self.teams[team_idx].players[rank];
                let (raw_tx, raw_ty) = if during_shot_flight {
                    if is_off {
                        if p_pos >= 5 - crash_cnt {
                            let side_off = if p_pos % 2 == 1 { -1.6 } else { 1.6 };
                            (rim_xy.0 - attack_dir * 2.1, side_off)
                        } else {
                            // Perimeter safety balance prevents unrealistic 5-man glass crashes
                            (
                                rim_xy.0 - attack_dir * 7.2,
                                ((p_pos as f64) - 1.0) * 2.8,
                            )
                        }
                    } else {
                        // Defenders box out matchups between opponent and rim
                        let opp_pos = (self.state.defense_plans[def_idx].matchups()[p_pos] as usize).min(4);
                        let opp_r = self.state.team_states[off_idx].active_lineup_slots[opp_pos];
                        let (mx, my) = if opp_r != ROSTER_IDX_NONE {
                            (
                                self.state.players[opp_r as usize].x_m,
                                self.state.players[opp_r as usize].y_m,
                            )
                        } else {
                            (rim_xy.0 - attack_dir * 2.5, 0.0)
                        };
                        let vx = rim_xy.0 - mx;
                        let vy = rim_xy.1 - my;
                        let vn = libm::hypot(vx, vy).max(0.4);
                        let box_x = (mx + (vx / vn) * 0.75) * 0.45
                            + (rim_xy.0 - attack_dir * 1.65) * 0.55;
                        let box_y = (my + (vy / vn) * 0.75) * 0.55;
                        (box_x, box_y)
                    }
                } else if !is_off || p_pos >= 5 - crash_cnt {
                    ball_xy
                } else {
                    (
                        rim_xy.0 - attack_dir * 6.5,
                        ((p_pos as f64) - 1.5) * 2.6,
                    )
                };
                let (tx, ty) = nav_b.clamp(raw_tx, raw_ty);

                self.transient_target_xy[slot] = (tx, ty);
                let p = &mut self.state.players[r as usize];
                let dx = tx - p.x_m;
                let dy = ty - p.y_m;
                let max_spd = self.tunables.max_offball_speed_min_max_m_s[0]
                    + (f64::from(snap.attributes[AttrIdx::TopSpeed]) / 99.0)
                        * (self.tunables.max_offball_speed_min_max_m_s[1]
                            - self.tunables.max_offball_speed_min_max_m_s[0]);
                let max_acc = 6.5 + (f64::from(snap.attributes[AttrIdx::Acceleration]) / 99.0) * 2.8;
                let reb_spd_scale = if !during_shot_flight {
                    if !is_off { 0.90 } else { 0.78 }
                } else {
                    0.82
                };
                let dist = Self::steer_player_velocity(p, dx, dy, max_spd * reb_spd_scale, max_acc);
                if during_shot_flight && libm::hypot(rim_xy.0 - p.x_m, rim_xy.1 - p.y_m) > 0.2 {
                    p.facing_rad = libm::atan2(rim_xy.1 - p.y_m, rim_xy.0 - p.x_m);
                } else if libm::hypot(ball_xy.0 - p.x_m, ball_xy.1 - p.y_m) > 0.2 {
                    p.facing_rad = libm::atan2(ball_xy.1 - p.y_m, ball_xy.0 - p.x_m);
                }
                if p.action_timer_ds == 0
                    && !matches!(
                        p.action_state,
                        PlayerActionState::ShootJumper
                            | PlayerActionState::Layup
                            | PlayerActionState::Dunk
                            | PlayerActionState::ContestJump
                    )
                {
                    p.action_state = if !during_shot_flight && dist < 1.8 {
                        if p.z_m <= 0.01 && self.state.ball.position_m[2] > 1.75 {
                            let vert = f64::from(snap.attributes[AttrIdx::Vertical]);
                            p.vz_m_s = 2.1 + (vert / 99.0) * 1.1;
                        }
                        PlayerActionState::ReboundJump
                    } else if !is_off && dist < 2.2 {
                        PlayerActionState::BoxOut
                    } else if dist > 0.6 {
                        PlayerActionState::Jog
                    } else {
                        PlayerActionState::Idle
                    };
                }
                let rad = snap.physical.collision_radius_m();
                actors.push(CcdActor {
                    slot,
                    canonical_actor_id: canonical_actor_id(self.canonical_team_ids[team_idx], rank),
                    x: p.x_m,
                    y: p.y_m,
                    vx: p.vx_m_s,
                    vy: p.vy_m_s,
                    radius: rad,
                    inv_mass: 1.0 / snap.physical.weight_kg.max(55.0),
                    bounds: self.geom.playable_bounds(rad),
                });
            }
        }
        let _ = self.run_ccd_on_actors(&mut actors);
        Ok(())
    }

    fn evaluate_live_contact_fouls(
        &mut self,
        off_idx: usize,
        def_idx: usize,
        contact_pairs: &[(usize, usize)],
    ) -> Result<bool, RngBudgetExceeded> {
        if contact_pairs.is_empty() {
            return Ok(false);
        }
        let off_side = if off_idx == 0 {
            CommandSide::Home
        } else {
            CommandSide::Away
        };
        let def_side = off_side.opponent();
        let attack_dir = if off_idx == 0 { 1.0 } else { -1.0 };
        let owner_slot_opt = self.state.ball.owner_slot;
        let is_nba = self.ruleset.fouls.goaltend_mode == 0;

        // Sort contact pairs deterministically by canonical team/slot order
        let mut sorted: Vec<(usize, usize, u8)> = Vec::with_capacity(contact_pairs.len());
        for &(sa, sb) in contact_pairs {
            let ta = sa / 5;
            let tb = sb / 5;
            if ta == tb {
                continue;
            }
            let (off_s, def_s) = if ta == off_idx { (sa, sb) } else { (sb, sa) };
            let dr = self.state.team_states[def_idx].active_lineup_slots[def_s % 5];
            if dr == ROSTER_IDX_NONE {
                continue;
            }
            let d_rank = (dr as usize).saturating_sub(def_idx * 15);
            let d_cid = canonical_actor_id(self.canonical_team_ids[def_idx], d_rank);
            sorted.push((off_s, def_s, d_cid));
        }
        sorted.sort_by_key(|x| x.2);

        for &(off_s, def_s, d_cid) in &sorted {
            let or_idx = self.state.team_states[off_idx].active_lineup_slots[off_s % 5];
            let dr_idx = self.state.team_states[def_idx].active_lineup_slots[def_s % 5];
            if or_idx == ROSTER_IDX_NONE || dr_idx == ROSTER_IDX_NONE {
                continue;
            }
            let op = self.state.players[or_idx as usize];
            let dp = self.state.players[dr_idx as usize];
            let o_spd = libm::hypot(op.vx_m_s, op.vy_m_s);

            // 1. Illegal moving screen (`SetScreen` while still moving > 0.75 m/s)
            if op.action_state == PlayerActionState::SetScreen && o_spd > 0.75 {
                let o_rank = (or_idx as usize).saturating_sub(off_idx * 15);
                let scr_attr = self.teams[off_idx]
                    .players
                    .get(o_rank)
                    .map_or(70.0, |p| f64::from(p.attributes[AttrIdx::ScreenSetting]));
                let p_ill = (0.025 * (1.25 - scr_attr / 99.0)).clamp(0.008, 0.036);
                let u = sample_u53_checked(
                    self.philox_key,
                    self.state.possession_id,
                    self.state.possession_tick,
                    &mut self.state.rng_sample_counters.0,
                    d_cid,
                    RngPurpose::ContactAndFoulRoll,
                )?;
                if u < p_ill {
                    self.record_defense_possession_outcome(def_idx, off_idx, 0);
                    self.push_event(GameRecordEventPayload::TurnoverCommitted {
                        offender_roster_idx: or_idx,
                        team_side: off_idx as u8,
                        turnover_rule_code: RuleCode::FOUL_ILLEGAL_SCREEN as u16,
                        stealer_roster_idx: 0xFF,
                        is_live_steal: 0,
                    });
                    self.state.oob_spot_xy = [op.x_m, op.y_m];
                    self.state.oob_is_endline = false;
                    let rid = self.state.next_ruling_id;
                    self.state.next_ruling_id += 1;
                    let call = RuleCallSnapshot {
                        rule_code: RuleCode::FOUL_ILLEGAL_SCREEN,
                        offending_side: off_side,
                        offender_roster_idx: or_idx,
                        victim_roster_idx: Some(dr_idx),
                        ball_in_frontcourt_at_call: self.state.ball_in_frontcourt,
                        interruption_spot: InboundSpot::NearestToInterruption,
                        stoppage_sideline_side: CourtSideLine::from_y_m(op.y_m),
                        restart_shot_clock_ds: compute_shot_clock_reset(
                            if (op.x_m * (-attack_dir)) > 0.0 {
                                ShotClockScenario::TurnoverOpponentToOwnFrontcourt
                            } else {
                                ShotClockScenario::TurnoverOpponentToOwnBackcourt
                            },
                            self.state.shot_clock_ds,
                            self.state.game_clock_ds,
                            &self.ruleset,
                        ),
                        foul_category: 0,
                        ..RuleCallSnapshot::default()
                    };
                    self.open_pending_ruling(rid, self.state.sim_tick, call, false);
                    return Ok(true);
                }
            }

            // 2. Drive contact: LGP Charge vs Blocking Foul (respecting NBA RA/LDB & FIBA NCSC)
            if owner_slot_opt == Some(off_s as u8)
                && op.action_state == PlayerActionState::Drive
                && o_spd > 3.0
            {
                let has_lgp = dp
                    .lgp_established_tick
                    .is_some_and(|t| self.state.sim_tick.saturating_sub(t) >= 2);
                let in_ra = self.geom.is_in_restricted_area(dp.x_m, dp.y_m, attack_dir);
                let ncsc_suppress_charge = !is_nba && in_ra && op.z_m > 0.02;
                let ra_blocks_charge = if is_nba {
                    in_ra && !op.caught_ball_inside_ldb
                } else {
                    ncsc_suppress_charge
                };
                if has_lgp && !ra_blocks_charge {
                    let u = sample_u53_checked(
                        self.philox_key,
                        self.state.possession_id,
                        self.state.possession_tick,
                        &mut self.state.rng_sample_counters.0,
                        d_cid,
                        RngPurpose::ContactAndFoulRoll,
                    )?;
                    if u < 0.022 {
                        self.record_defense_possession_outcome(def_idx, off_idx, 0);
                        self.push_event(GameRecordEventPayload::TurnoverCommitted {
                            offender_roster_idx: or_idx,
                            team_side: off_idx as u8,
                            turnover_rule_code: RuleCode::FOUL_CHARGE as u16,
                            stealer_roster_idx: 0xFF,
                            is_live_steal: 0,
                        });
                        self.state.oob_spot_xy = [op.x_m, op.y_m];
                        self.state.oob_is_endline = false;
                        let rid = self.state.next_ruling_id;
                        self.state.next_ruling_id += 1;
                        let call = RuleCallSnapshot {
                            rule_code: RuleCode::FOUL_CHARGE,
                            offending_side: off_side,
                            offender_roster_idx: or_idx,
                            victim_roster_idx: Some(dr_idx),
                            ball_in_frontcourt_at_call: self.state.ball_in_frontcourt,
                            interruption_spot: InboundSpot::NearestToInterruption,
                            stoppage_sideline_side: CourtSideLine::from_y_m(op.y_m),
                            restart_shot_clock_ds: compute_shot_clock_reset(
                                if (op.x_m * (-attack_dir)) > 0.0 {
                                    ShotClockScenario::TurnoverOpponentToOwnFrontcourt
                                } else {
                                    ShotClockScenario::TurnoverOpponentToOwnBackcourt
                                },
                                self.state.shot_clock_ds,
                                self.state.game_clock_ds,
                                &self.ruleset,
                            ),
                            foul_category: 0,
                            ..RuleCallSnapshot::default()
                        };
                        self.open_pending_ruling(rid, self.state.sim_tick, call, false);
                        return Ok(true);
                    }
                } else if !has_lgp {
                    let o_rank = (or_idx as usize).saturating_sub(off_idx * 15);
                    let draw_foul = self.teams[off_idx]
                        .players
                        .get(o_rank)
                        .map_or(70.0, |p| f64::from(p.attributes[AttrIdx::DrawFoul]));
                    let p_block = (0.036 + 0.024 * (draw_foul / 99.0)).clamp(0.020, 0.068);
                    let u = sample_u53_checked(
                        self.philox_key,
                        self.state.possession_id,
                        self.state.possession_tick,
                        &mut self.state.rng_sample_counters.0,
                        d_cid,
                        RngPurpose::ContactAndFoulRoll,
                    )?;
                    if u < p_block {
                        let restart_sc = compute_shot_clock_reset(
                            if self.state.ball_in_frontcourt {
                                ShotClockScenario::OffenseFrontcourtNonShotDefFoul
                            } else {
                                ShotClockScenario::OffenseBackcourtRetain { is_foul_or_kick: true }
                            },
                            self.state.shot_clock_ds,
                            self.state.game_clock_ds,
                            &self.ruleset,
                        );
                        self.state.oob_spot_xy = [op.x_m, op.y_m];
                        self.state.oob_is_endline = false;
                        let rid = self.state.next_ruling_id;
                        self.state.next_ruling_id += 1;
                        let call = RuleCallSnapshot {
                            rule_code: RuleCode::FOUL_BLOCKING,
                            offending_side: def_side,
                            offender_roster_idx: dr_idx,
                            victim_roster_idx: Some(or_idx),
                            ball_in_frontcourt_at_call: self.state.ball_in_frontcourt,
                            interruption_spot: InboundSpot::NearestToInterruption,
                            stoppage_sideline_side: CourtSideLine::from_y_m(op.y_m),
                            restart_shot_clock_ds: restart_sc,
                            foul_category: 0,
                            ..RuleCallSnapshot::default()
                        };
                        self.open_pending_ruling(rid, self.state.sim_tick, call, false);
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    fn step_live_ball(&mut self) -> Result<(), RngBudgetExceeded> {
        self.step_player_vertical_kinematics();
        // Advance clocks 1 tick (0.1 s)
        let (_, gc_exp, sc_exp) = advance_clocks_sub_ds(&mut self.state, 1.0);
        if gc_exp {
            self.handle_period_expiration();
            return Ok(());
        }
        if sc_exp {
            self.trigger_shot_clock_violation();
            return Ok(());
        }

        // If possession_side is None (post-tap loose ball), animate tip flight then establish control
        if self.state.possession_side.is_none() {
            let side = match self.state.ball.last_touched_by_slot {
                Some(0..=4) => CommandSide::Home,
                Some(5..=9) => CommandSide::Away,
                _ => self.state.opening_tip_winner.unwrap_or(CommandSide::Home),
            };
            let t_idx = side.team_index().unwrap_or(0);
            let def_idx = 1 - t_idx;
            if self.state.tip_flight_ticks > 0 {
                self.state.tip_flight_ticks -= 1;
                for k in 0..3 {
                    self.state.ball.position_m[k] += self.state.ball.velocity_m_s[k] * 0.1;
                }
                self.state.ball.velocity_m_s[2] -= GRAVITY_M_S2 * 0.1;
                self.state.ball.position_m[2] = self.state.ball.position_m[2].max(0.18);
                let _ = self.step_player_kinematics_and_ccd(t_idx, def_idx)?;
                return Ok(());
            }
            let receiver = self.preferred_handler_pos(t_idx, None);
            let pg_slot = (t_idx * 5 + receiver) as u8;
            let pg_r = self.state.team_states[t_idx].active_lineup_slots[receiver];
            let attack_dir = if t_idx == 0 { 1.0 } else { -1.0 };
            self.state.possession_side = Some(side);
            self.reset_possession_epoch();
            self.start_offensive_possession(t_idx, false, true);
            let scenario = if self.state.ball.last_team_control_side == Some(side) {
                ShotClockScenario::OffenseFrontcourtNonShotDefFoul
            } else {
                ShotClockScenario::OpeningTipOrOtJumpControl
            };
            let sc = compute_shot_clock_reset(
                scenario, self.state.shot_clock_ds, self.state.game_clock_ds, &self.ruleset,
            );
            self.state.shot_clock_ds = sc;
            self.state.shot_clock_rem_10us = sc.map(|ds| u32::from(ds) * SUB_DS_UNITS);
            self.state.shot_clock_running = sc.is_some();
            self.state.ball.ball_state = BallState::Held;
            self.state.ball.owner_slot = Some(pg_slot);
            self.state.ball.last_touched_by_slot = Some(pg_slot);
            self.state.ball.last_team_control_side = Some(side);
            let (px, py) = {
                let pg = &mut self.state.players[pg_r as usize];
                pg.action_state = PlayerActionState::Catch;
                pg.action_timer_ds = 4;
                pg.feet_set_ticks = 0;
                pg.dribbles_since_catch = 0;
                pg.oob_reentry_pending = false;
                pg.caught_ball_inside_ldb = false;
                pg.footwork = FootworkRecord {
                    dribble_status: DribbleStatus::LiveTripleThreat,
                    ..FootworkRecord::default()
                };
                (pg.x_m, pg.y_m)
            };
            self.state.ball.position_m = [px, py, 1.25];
            self.state.ball.velocity_m_s = [0.0, 0.0, 0.0];
            self.state.ball_in_frontcourt = (px * attack_dir) > 0.0;
            self.state.backcourt_8s_timer_ds = 0;
            let _ = self.step_player_kinematics_and_ccd(t_idx, def_idx)?;
            self.push_event(GameRecordEventPayload::PossessionEstablished {
                possession_id: self.state.possession_id,
                side: t_idx as u8,
                handler_roster_idx: pg_r,
                trigger_kind: 0,
                new_shot_clock_ds: encode_shot_clock_opt(self.state.shot_clock_ds),
            });
            return Ok(());
        }

        // Handle PassFlight in progress with real 3D corridor interception, deflections, kicked balls, and bobbles
        if let Some(mut pass) = self.state.ball.pass_ctx.take() {
            pass.ticks_elapsed += 1;
            let pass_team = (pass.passer_slot as usize) / 5;
            let recv_team = (pass.target_slot as usize) / 5;
            let recv_r = self.state.team_states[recv_team].active_lineup_slots[(pass.target_slot as usize) % 5];
            let pass_r = self.state.team_states[pass_team].active_lineup_slots[(pass.passer_slot as usize) % 5];

            if recv_r != ROSTER_IDX_NONE {
                let rp = &self.state.players[recv_r as usize];
                pass.target_pos_m = [rp.x_m, rp.y_m, 1.35 + rp.z_m];
            }

            let off_side = self.state.possession_side.unwrap_or(if pass_team == 0 {
                CommandSide::Home
            } else {
                CommandSide::Away
            });
            let off_idx = off_side.team_index().unwrap_or(pass_team);
            let def_idx = 1 - off_idx;
            let def_side = off_side.opponent();
            let off_attack_dir = if off_idx == 0 { 1.0 } else { -1.0 };

            let prev_pos = self.state.ball.position_m;
            let frac = ((pass.ticks_elapsed as f64) / (pass.total_flight_ticks.max(1) as f64)).min(1.0);
            let new_pos = [
                pass.start_pos_m[0] + (pass.target_pos_m[0] - pass.start_pos_m[0]) * frac,
                pass.start_pos_m[1] + (pass.target_pos_m[1] - pass.start_pos_m[1]) * frac,
                pass.start_pos_m[2] + (pass.target_pos_m[2] - pass.start_pos_m[2]) * frac
                    + 0.45 * 4.0 * frac * (1.0 - frac),
            ];
            self.state.ball.position_m = new_pos;
            let counting_backcourt = !self.state.ball_in_frontcourt;
            if counting_backcourt && pass.ticks_elapsed < pass.total_flight_ticks
                && self.advance_backcourt_count(off_side, pass_r)
            {
                return Ok(());
            }

            // Per-tick Stage 5 passing corridor check sorted by canonical defender ID
            if pass.ticks_elapsed >= 1 {
                let pass_rank = (pass_r as usize).saturating_sub(pass_team * 15);
                if let Some(pass_snap) = self.teams[pass_team].players.get(pass_rank) {
                    let mut def_list: [(u8, u8, u8); 5] = [(0xFF, 0xFF, 0); 5];
                    let mut dn = 0usize;
                    for p_pos in 0..5 {
                        let ds = (def_idx * 5 + p_pos) as u8;
                        let dr = self.state.team_states[def_idx].active_lineup_slots[p_pos];
                        if dr != ROSTER_IDX_NONE {
                            let d_rank = (dr as usize).saturating_sub(def_idx * 15);
                            let d_cid = canonical_actor_id(self.canonical_team_ids[def_idx], d_rank);
                            def_list[dn] = (ds, dr, d_cid);
                            dn += 1;
                        }
                    }
                    if dn > 1 {
                        def_list[..dn].sort_by_key(|x| x.2);
                    }
                    for &(d_slot, dr, d_cid) in &def_list[..dn] {
                        let d_rank = (dr as usize).saturating_sub(def_idx * 15);
                        let Some(def_snap) = self.teams[def_idx].players.get(d_rank) else {
                            continue;
                        };
                        let (dp_x, dp_y, dp_z) = {
                            let dp = &self.state.players[dr as usize];
                            (dp.x_m, dp.y_m, dp.z_m)
                        };
                        let d_seg = Self::point_to_segment_dist(
                            dp_x,
                            dp_y,
                            prev_pos[0],
                            prev_pos[1],
                            new_pos[0],
                            new_pos[1],
                        );
                        let reach_m = 0.60 + def_snap.physical.wingspan_cm / 400.0;
                        let hand_z = (def_snap.physical.standing_reach_cm / 100.0) + dp_z;
                        if d_seg <= reach_m && new_pos[2] <= hand_z + 0.14 {
                            let (p_clean, p_defl) = crate::resolve::compute_pass_intercept_prob(
                                pass_snap,
                                def_snap,
                                d_seg,
                                reach_m,
                                &self.tunables,
                            );
                            let scale = 1.0 / (pass.total_flight_ticks.max(1) as f64);
                            let pt_clean = (p_clean * scale * 0.85).min(0.14);
                            let pt_defl = (p_defl * scale * 1.10).min(0.18);
                            let u_st = sample_u53_checked(
                                self.philox_key,
                                self.state.possession_id,
                                self.state.possession_tick,
                                &mut self.state.rng_sample_counters.0,
                                d_cid,
                                RngPurpose::StealRoll,
                            )?;
                            if u_st < pt_clean {
                                let int_attack_dir = if def_idx == 0 { 1.0 } else { -1.0 };
                                self.push_event(GameRecordEventPayload::PassIntercepted {
                                    passer_roster_idx: pass_r,
                                    stealer_roster_idx: dr,
                                    is_deflection_loose: 0,
                                });
                                self.push_event(GameRecordEventPayload::TurnoverCommitted {
                                    offender_roster_idx: pass_r,
                                    team_side: pass_team as u8,
                                    turnover_rule_code: RuleCode::NONE as u16,
                                    stealer_roster_idx: dr,
                                    is_live_steal: 1,
                                });
                                self.state.possession_side = Some(def_side);
                                self.reset_possession_epoch();
                                self.record_defense_possession_outcome(def_idx, pass_team, 0);
                                self.start_offensive_possession(def_idx, false, true);
                                let sc = compute_shot_clock_reset(
                                    ShotClockScenario::LiveStealOrDefRebound,
                                    self.state.shot_clock_ds,
                                    self.state.game_clock_ds,
                                    &self.ruleset,
                                );
                                let gc_ds = self.state.game_clock_ds;
                                set_clocks_exact_ds(&mut self.state, gc_ds, sc);
                                self.state.shot_clock_running = sc.is_some();
                                self.state.ball.ball_state = BallState::Held;
                                self.state.ball.owner_slot = Some(d_slot);
                                self.state.ball.last_touched_by_slot = Some(d_slot);
                                self.state.ball.last_team_control_side = Some(def_side);
                                let (ix, iy) = {
                                    let ip = &mut self.state.players[dr as usize];
                                    ip.action_state = PlayerActionState::Catch;
                                    ip.action_timer_ds = 4;
                                    ip.feet_set_ticks = 0;
                                    ip.dribbles_since_catch = 0;
                                    ip.caught_ball_inside_ldb =
                                        self.geom.is_in_ldb(ip.x_m, ip.y_m, int_attack_dir);
                                    ip.footwork = FootworkRecord {
                                        dribble_status: DribbleStatus::LiveTripleThreat,
                                        ..FootworkRecord::default()
                                    };
                                    (ip.x_m, ip.y_m)
                                };
                                self.state.ball.position_m = [ix, iy, 1.25];
                                self.state.ball_in_frontcourt = (ix * int_attack_dir) > 0.0;
                                self.state.backcourt_8s_timer_ds = 0;
                                let _ = self.step_player_kinematics_and_ccd(def_idx, 1 - def_idx)?;
                                let (ix2, iy2, iz2, ioob) = {
                                    let ip = &self.state.players[dr as usize];
                                    (ip.x_m, ip.y_m, ip.z_m, ip.oob_reentry_pending)
                                };
                                if iz2 <= 0.02 && (self.geom.is_out_of_bounds(ix2, iy2) || ioob) {
                                    self.trigger_oob_violation(
                                        ix2,
                                        iy2,
                                        Some(d_slot),
                                        def_side,
                                        false,
                                        false,
                                    );
                                    return Ok(());
                                }
                                self.push_event(GameRecordEventPayload::PossessionEstablished {
                                    possession_id: self.state.possession_id,
                                    side: def_idx as u8,
                                    handler_roster_idx: dr,
                                    trigger_kind: 3,
                                    new_shot_clock_ds: encode_shot_clock_opt(self.state.shot_clock_ds),
                                });
                                return Ok(());
                            } else if u_st < pt_clean + pt_defl {
                                // Deflected pass -> loose ball scramble (can be recovered or go out of bounds!)
                                self.push_event(GameRecordEventPayload::PassIntercepted {
                                    passer_roster_idx: pass_r,
                                    stealer_roster_idx: dr,
                                    is_deflection_loose: 1,
                                });
                                self.state.ball.ball_state = BallState::Loose;
                                self.state.ball.owner_slot = None;
                                self.state.ball.last_touched_by_slot = Some(d_slot);
                                self.state.ball.last_passer_roster_idx = None;
                                let dvx = new_pos[0] - prev_pos[0];
                                let dvy = new_pos[1] - prev_pos[1];
                                let dn = libm::hypot(dvx, dvy).max(0.2);
                                self.state.ball.velocity_m_s = [
                                    -(dvx / dn) * 3.6 + (new_pos[0] - dp_x) * 4.2,
                                    -(dvy / dn) * 3.6 + (new_pos[1] - dp_y) * 4.2,
                                    1.85,
                                ];
                                self.state.scramble_origin =
                                    Some(ScrambleOrigin::FromNoRimMissOrBlock {
                                        prior_offense: off_side,
                                    });
                                self.state.game_phase = GamePhase::ReboundScramble;
                                self.state.scramble_ticks = 0;
                                return Ok(());
                            } else if new_pos[2] <= 0.65
                                && d_seg <= 0.38
                                && u_st < pt_clean + pt_defl + 0.045
                            {
                                // Kicked ball violation: defense intentionally kicks low pass
                                self.state.oob_spot_xy = [new_pos[0], new_pos[1]];
                                self.state.oob_is_endline = false;
                                self.push_event(GameRecordEventPayload::ViolationTriggered {
                                    offender_roster_idx: dr,
                                    team_side: def_side as u8,
                                    rule_code: RuleCode::VIO_KICKED_BALL as u16,
                                    footwork: FootworkTelemetryPublic::default(),
                                });
                                let rid = self.state.next_ruling_id;
                                self.state.next_ruling_id += 1;
                                let restart_sc = compute_shot_clock_reset(
                                    if self.state.ball_in_frontcourt {
                                        ShotClockScenario::OffenseFrontcourtNonShotDefFoul
                                    } else {
                                        ShotClockScenario::OffenseBackcourtRetain { is_foul_or_kick: true }
                                    },
                                    self.state.shot_clock_ds,
                                    self.state.game_clock_ds,
                                    &self.ruleset,
                                );
                                let call = RuleCallSnapshot {
                                    rule_code: RuleCode::VIO_KICKED_BALL,
                                    offending_side: def_side,
                                    offender_roster_idx: dr,
                                    ball_in_frontcourt_at_call: self.state.ball_in_frontcourt,
                                    interruption_spot: InboundSpot::NearestToInterruption,
                                    stoppage_sideline_side: CourtSideLine::from_y_m(new_pos[1]),
                                    restart_shot_clock_ds: restart_sc,
                                    ..RuleCallSnapshot::default()
                                };
                                self.open_pending_ruling(rid, self.state.sim_tick, call, false);
                                return Ok(());
                            }
                        }
                    }
                }
            }

            if pass.ticks_elapsed >= pass.total_flight_ticks {
                if pass.will_bobble && recv_r != ROSTER_IDX_NONE {
                    let r_rank = (recv_r as usize).saturating_sub(recv_team * 15);
                    let r_cid = canonical_actor_id(self.canonical_team_ids[recv_team], r_rank);
                    let u_bob = sample_u53_checked(
                        self.philox_key,
                        self.state.possession_id,
                        self.state.possession_tick,
                        &mut self.state.rng_sample_counters.0,
                        r_cid,
                        RngPurpose::StealRoll,
                    )?;
                    if u_bob < 0.45 {
                        self.state.ball.ball_state = BallState::Loose;
                        self.state.ball.owner_slot = None;
                        self.state.ball.last_touched_by_slot = Some(pass.target_slot);
                        self.state.ball.last_passer_roster_idx = None;
                        self.state.ball.velocity_m_s = [
                            (new_pos[0] - prev_pos[0]) * 1.4,
                            (new_pos[1] - prev_pos[1]) * 1.4,
                            1.5,
                        ];
                        self.state.scramble_origin = Some(ScrambleOrigin::FromNoRimMissOrBlock {
                            prior_offense: off_side,
                        });
                        self.state.game_phase = GamePhase::ReboundScramble;
                        self.state.scramble_ticks = 0;
                        return Ok(());
                    }
                }

                self.state.ball.ball_state = BallState::Held;
                self.state.ball.owner_slot = Some(pass.target_slot);
                self.state.ball.last_touched_by_slot = Some(pass.target_slot);
                self.state.ball.last_passer_roster_idx = Some(pass_r);
                self.state.ball.last_pass_completed_tick = self.state.sim_tick;
                if recv_r != ROSTER_IDX_NONE {
                    let (rx, ry, rz, r_oob) = {
                        let rp = &mut self.state.players[recv_r as usize];
                        rp.dribbles_since_catch = 0;
                        rp.caught_ball_inside_ldb =
                            self.geom.is_in_ldb(rp.x_m, rp.y_m, off_attack_dir);
                        rp.action_state = PlayerActionState::Catch;
                        rp.action_timer_ds = 5;
                        rp.feet_set_ticks = 0;
                        rp.footwork = FootworkRecord {
                            dribble_status: DribbleStatus::LiveTripleThreat,
                            ..FootworkRecord::default()
                        };
                        (rp.x_m, rp.y_m, rp.z_m, rp.oob_reentry_pending)
                    };
                    if rz <= 0.02 && (self.geom.is_out_of_bounds(rx, ry) || r_oob) {
                        self.push_event(GameRecordEventPayload::PassCompleted {
                            passer_roster_idx: pass_r,
                            catcher_roster_idx: recv_r,
                            bobbled_recovered: if pass.will_bobble { 1 } else { 0 },
                        });
                        self.trigger_oob_violation(rx, ry, Some(pass.target_slot), off_side, false, false);
                        return Ok(());
                    }
                    if self.state.ball_in_frontcourt && (rx * off_attack_dir) < -0.10 && rz <= 0.02 {
                        self.push_event(GameRecordEventPayload::PassCompleted {
                            passer_roster_idx: pass_r,
                            catcher_roster_idx: recv_r,
                            bobbled_recovered: if pass.will_bobble { 1 } else { 0 },
                        });
                        self.trigger_turnover_stoppage(RuleCode::VIO_BACKCOURT, off_side, recv_r);
                        return Ok(());
                    }
                    if (rx * off_attack_dir) > 0.0 {
                        self.state.ball_in_frontcourt = true;
                        self.state.backcourt_8s_timer_ds = 0;
                    } else if counting_backcourt && self.advance_backcourt_count(off_side, pass_r) {
                        return Ok(());
                    }
                }
                self.push_event(GameRecordEventPayload::PassCompleted {
                    passer_roster_idx: pass_r,
                    catcher_roster_idx: recv_r,
                    bobbled_recovered: if pass.will_bobble { 1 } else { 0 },
                });
                // Trigger Flow Pass & Cut on perimeter frontcourt pass
                if self.state.ball_in_frontcourt
                    && pass_r != ROSTER_IDX_NONE
                    && ((pass_r as usize) / 15) == off_idx
                {
                    let pr_rank = (pass_r as usize).saturating_sub(off_idx * 15);
                    let obm = self.teams[off_idx]
                        .players
                        .get(pr_rank)
                        .map_or(0, |pl| pl.attributes[AttrIdx::OffBallMovement]);
                    let pass_pos = (pass.passer_slot as usize) % 5;
                    let is_setting_screen = self
                        .playbooks
                        .iter()
                        .find(|p| p.play_id == self.state.active_play_id[off_idx])
                        .and_then(|pb| {
                            let idx = (self.state.play_phase_idx[off_idx] as usize)
                                .min(pb.phases.len().saturating_sub(1));
                            pb.phases.get(idx)
                        })
                        .is_some_and(|ph| {
                            let pass_role = self.state.slot_bindings[off_idx]
                                .iter()
                                .position(|&s| s as usize == pass_pos)
                                .unwrap_or(pass_pos)
                                .min(4);
                            ph.actions[pass_role].0 == 2
                        });
                    if obm >= 68 && !is_setting_screen {
                        let rim_pt = self.geom.rim_xy(off_attack_dir);
                        let mut active_cutters = 0usize;
                        let mut paint_clear = true;
                        for op_pos in 0..5 {
                            let or_s = self.state.team_states[off_idx].active_lineup_slots[op_pos];
                            if or_s != ROSTER_IDX_NONE {
                                let tp = &self.state.players[or_s as usize];
                                if tp.offball_cut_timer_ds > 0
                                    && matches!(tp.offball_cut_kind, 1 | 2 | 4 | 5)
                                {
                                    active_cutters += 1;
                                }
                                if or_s != pass_r
                                    && libm::hypot(tp.x_m - rim_pt.0, tp.y_m - rim_pt.1) < 2.6
                                {
                                    paint_clear = false;
                                }
                            }
                        }
                        let pp = &mut self.state.players[pass_r as usize];
                        let d_rim = libm::hypot(pp.x_m - rim_pt.0, pp.y_m - rim_pt.1);
                        if active_cutters == 0
                            && paint_clear
                            && d_rim > 4.8
                            && pp.offball_cut_timer_ds == 0
                        {
                            pp.offball_cut_kind = 1;
                            pp.offball_cut_timer_ds = 16;
                            pp.offball_cut_side = if pp.y_m >= 0.0 { 1 } else { -1 };
                        }
                    }
                }
            } else {
                self.state.ball.pass_ctx = Some(pass);
            }
            let _ = self.step_player_kinematics_and_ccd(off_idx, def_idx)?;
            if let Some(new_os) = self.state.ball.owner_slot {
                let no_idx = (new_os as usize) / 5;
                let nr = self.state.team_states[no_idx].active_lineup_slots[(new_os as usize) % 5];
                if nr != ROSTER_IDX_NONE {
                    let (nx, ny, nz, n_oob) = {
                        let np = &self.state.players[nr as usize];
                        (np.x_m, np.y_m, np.z_m, np.oob_reentry_pending)
                    };
                    if nz <= 0.02 && (self.geom.is_out_of_bounds(nx, ny) || n_oob) {
                        self.trigger_oob_violation(nx, ny, Some(new_os), off_side, false, false);
                        return Ok(());
                    }
                    if self.state.ball_in_frontcourt && (nx * off_attack_dir) < -0.10 && nz <= 0.02 {
                        self.trigger_turnover_stoppage(RuleCode::VIO_BACKCOURT, off_side, nr);
                        return Ok(());
                    }
                }
            }
            self.check_paint_clocks(off_side);
            return Ok(());
        }

        let off_side = self.state.possession_side.unwrap_or(CommandSide::Home);
        let off_idx = off_side.team_index().unwrap_or(0);
        let def_idx = 1 - off_idx;
        let attack_dir = if off_idx == 0 { 1.0 } else { -1.0 };

        // Step 3: Tactical Coordinator (transition window + cut/xout timers + role assignment + synchronized phase advancement)
        if self.state.transition_window_ds[off_idx] > 0 {
            self.state.transition_window_ds[off_idx] -= 1;
        }
        if self.state.xout_timer_ds[def_idx] > 0 {
            self.state.xout_timer_ds[def_idx] -= 1;
            if self.state.xout_timer_ds[def_idx] == 0 {
                self.state.xout_rotator_slot[def_idx] = 0xFF;
            }
        }
        for p_pos in 0..5 {
            let r = self.state.team_states[off_idx].active_lineup_slots[p_pos];
            if r != ROSTER_IDX_NONE {
                let p = &mut self.state.players[r as usize];
                if p.offball_cut_timer_ds > 0 {
                    p.offball_cut_timer_ds -= 1;
                    if p.offball_cut_timer_ds == 0 {
                        p.offball_cut_kind = 0;
                    }
                }
            }
        }

        let mut active_off_snaps = Vec::with_capacity(5);
        for p_pos in 0..5 {
            let r = self.state.team_states[off_idx].active_lineup_slots[p_pos];
            if r != ROSTER_IDX_NONE {
                let rank = (r as usize).saturating_sub(off_idx * 15);
                if let Some(pl) = self.teams[off_idx].players.get(rank) {
                    active_off_snaps.push((p_pos as u8, pl));
                }
            }
        }
        if let Some(pb) = self
            .playbooks
            .iter()
            .find(|p| p.play_id == self.state.active_play_id[off_idx])
        {
            self.state.slot_bindings[off_idx] = assign_playbook_roles(pb, &active_off_snaps);
            if self.state.ball_in_frontcourt {
                if self.state.transition_window_ds[off_idx] > 0
                    && (self.state.ball.position_m[0] * attack_dir) > 4.0
                {
                    let front_count = (0..5)
                        .filter(|&p_pos| {
                            let r = self.state.team_states[off_idx].active_lineup_slots[p_pos];
                            r != ROSTER_IDX_NONE
                                && (self.state.players[r as usize].x_m * attack_dir) > 1.5
                        })
                        .count();
                    if front_count >= 4 {
                        self.state.transition_window_ds[off_idx] = 0;
                    }
                }

                if self.state.transition_window_ds[off_idx] <= 8 {
                    self.state.play_phase_elapsed_ds[off_idx] =
                        self.state.play_phase_elapsed_ds[off_idx].saturating_add(1);
                    let cur_pidx = (self.state.play_phase_idx[off_idx] as usize)
                        .min(pb.phases.len().saturating_sub(1));
                    if let Some(phase) = pb.phases.get(cur_pidx) {
                        let elapsed = self.state.play_phase_elapsed_ds[off_idx];
                        let fallback = phase.exit_fallback_ds.max(8).min(phase.max_duration_ds);
                        let target_role = (phase.exit_target_role as usize).min(4);
                        let target_pos = self.state.slot_bindings[off_idx][target_role] as usize;
                        let owner_pos = self.state.ball.owner_slot.map(|s| (s as usize) % 5);
                        let ball_entered_target = elapsed >= 3 && owner_pos == Some(target_pos);
                        let screen_done = elapsed >= 5
                            && (self.state.play_screen_contact_ds[off_idx] >= 3
                                || self.state.screen_ready_ticks[off_idx] >= 6);
                        let screener_arrived = if phase.exit_trigger_kind == 1 {
                            let scr_pos = (target_pos < 5)
                                .then(|| self.state.team_states[off_idx].active_lineup_slots[target_pos])
                                .unwrap_or(ROSTER_IDX_NONE);
                            if scr_pos != ROSTER_IDX_NONE {
                                let sp = &self.state.players[scr_pos as usize];
                                let hp = self
                                    .state
                                    .ball
                                    .owner_slot
                                    .and_then(|os| {
                                        let hr = self.state.team_states[(os as usize) / 5]
                                            .active_lineup_slots[(os as usize) % 5];
                                        (hr != ROSTER_IDX_NONE).then(|| &self.state.players[hr as usize])
                                    });
                                hp.is_none_or(|h| libm::hypot(sp.x_m - h.x_m, sp.y_m - h.y_m) < 2.6)
                            } else {
                                true
                            }
                        } else {
                            true
                        };
                        let advantage_created = elapsed >= 8 && {
                            let target_r = if target_pos < 5 {
                                self.state.team_states[off_idx].active_lineup_slots[target_pos]
                            } else {
                                ROSTER_IDX_NONE
                            };
                            if target_r != ROSTER_IDX_NONE {
                                let tx = self.state.players[target_r as usize].x_m;
                                let ty = self.state.players[target_r as usize].y_m;
                                let mut min_d = 9.0_f64;
                                for dp in 0..5 {
                                    let dr = self.state.team_states[def_idx].active_lineup_slots[dp];
                                    if dr != ROSTER_IDX_NONE {
                                        let d = libm::hypot(
                                            self.state.players[dr as usize].x_m - tx,
                                            self.state.players[dr as usize].y_m - ty,
                                        );
                                        if d < min_d {
                                            min_d = d;
                                        }
                                    }
                                }
                                min_d >= 2.25
                            } else {
                                false
                            }
                        };

                        let should_advance = elapsed >= phase.max_duration_ds
                            || match phase.exit_trigger_kind {
                                1 => screen_done || (screener_arrived && elapsed >= fallback),
                                2 => ball_entered_target || elapsed >= fallback,
                                3 => advantage_created || elapsed >= fallback,
                                _ => elapsed >= fallback,
                            };

                        if should_advance {
                            if cur_pidx + 1 < pb.phases.len() {
                                self.state.play_phase_idx[off_idx] = (cur_pidx + 1) as u8;
                            } else {
                                let loop_idx = if pb.phases.len() >= 3 {
                                    pb.phases.len() - 2
                                } else {
                                    0
                                };
                                self.state.play_phase_idx[off_idx] = loop_idx as u8;
                                if pb.allow_mirror {
                                    self.state.play_mirrored[off_idx] =
                                        !self.state.play_mirrored[off_idx];
                                }
                            }
                            self.state.play_phase_elapsed_ds[off_idx] = 0;
                            self.state.play_screen_contact_ds[off_idx] = 0;
                            self.state.screen_ready_ticks[off_idx] = 0;
                            self.state.pnr_read_branch[off_idx] = 0;
                        }
                    }
                }
            }
        }

        // Update player positions with Swept CCD
        let contact_pairs = self.step_player_kinematics_and_ccd(off_idx, def_idx)?;

        // Update grounded out-of-bounds tracking for all active players
        for team_idx in 0..2 {
            for p_pos in 0..5 {
                let r = self.state.team_states[team_idx].active_lineup_slots[p_pos];
                if r != ROSTER_IDX_NONE {
                    let p = &mut self.state.players[r as usize];
                    if p.z_m <= 0.02 {
                        if self.geom.is_out_of_bounds(p.x_m, p.y_m) {
                            p.oob_reentry_pending = true;
                        } else if libm::fabs(p.x_m) <= self.geom.half_length_m - 0.05
                            && libm::fabs(p.y_m) <= self.geom.half_width_m - 0.05
                        {
                            p.oob_reentry_pending = false;
                        }
                    }
                }
            }
        }

        if let Some(owner_slot) = self.state.ball.owner_slot {
            let owner_team = (owner_slot as usize) / 5;
            let owner_r = self.state.team_states[owner_team].active_lineup_slots[(owner_slot as usize) % 5];
            let (ox, oy, oz, o_vx, o_vy, o_oob) = {
                let p = &self.state.players[owner_r as usize];
                (p.x_m, p.y_m, p.z_m, p.vx_m_s, p.vy_m_s, p.oob_reentry_pending)
            };

            // 1. Live-ball Out-of-Bounds violation (`VIO_OOB`)
            if oz <= 0.02 && (self.geom.is_out_of_bounds(ox, oy) || o_oob) {
                self.trigger_oob_violation(ox, oy, Some(owner_slot), off_side, false, false);
                return Ok(());
            }

            // 2. Backcourt violation (`VIO_BACKCOURT`) & 8-second count (`VIO_8S`)
            if self.state.ball_in_frontcourt && (ox * attack_dir) < -0.10 && oz <= 0.02 {
                self.trigger_turnover_stoppage(RuleCode::VIO_BACKCOURT, off_side, owner_r);
                return Ok(());
            }
            if ox * attack_dir > 0.0 {
                self.state.ball_in_frontcourt = true;
                self.state.backcourt_8s_timer_ds = 0;
            } else if !self.state.ball_in_frontcourt && self.advance_backcourt_count(off_side, owner_r) {
                return Ok(());
            }

            // 3. Physical contact fouls (Charge / Blocking / Illegal Screen)
            if self.evaluate_live_contact_fouls(off_idx, def_idx, &contact_pairs)? {
                return Ok(());
            }

            // 4. Dribble step tracking & Travel violation (`VIO_TRAVEL`)
            {
                let spd = libm::hypot(o_vx, o_vy);
                let owner_p = &mut self.state.players[owner_r as usize];
                if spd > 0.7
                    || matches!(
                        owner_p.action_state,
                        PlayerActionState::Drive | PlayerActionState::PostBackdown
                    )
                {
                    owner_p.dribbles_since_catch = owner_p.dribbles_since_catch.saturating_add(1);
                }
                if owner_p.footwork.foot_touched_down_while_lifted
                    && owner_p.footwork.release_tick.is_none()
                {
                    self.trigger_turnover_stoppage(RuleCode::VIO_TRAVEL, off_side, owner_r);
                    return Ok(());
                }
                if owner_p.footwork.dribble_status == DribbleStatus::Gathered {
                    let (px_m, py_m) = owner_p
                        .footwork
                        .pivot_pos_mm
                        .map_or((ox, oy), |(xmm, ymm)| (xmm as f64 / 1000.0, ymm as f64 / 1000.0));
                    if libm::hypot(ox - px_m, oy - py_m) >= 0.95 {
                        owner_p.footwork.steps_since_gather =
                            owner_p.footwork.steps_since_gather.saturating_add(1);
                        if let (Ok((qpx, _)), Ok((qpy, _))) = (
                            crate::wire_schema_v1::frames::quantize_mm(ox),
                            crate::wire_schema_v1::frames::quantize_mm(oy),
                        ) {
                            owner_p.footwork.pivot_pos_mm = Some((qpx, qpy));
                        }
                        if owner_p.footwork.steps_since_gather > 2 {
                            self.trigger_turnover_stoppage(RuleCode::VIO_TRAVEL, off_side, owner_r);
                            return Ok(());
                        }
                    }
                }
            }

            // 6. NBA 5-second back-to-basket (`VIO_5S_POST`) & FIBA 5-second closely-guarded (`VIO_5S_CLOSELY_GUARDED`)
            if self.ruleset.clocks.back_to_basket_zone != 0
                && self.state.ball_in_frontcourt
                && self.state.game_clock_running
                && self.state.players[owner_r as usize].action_state == PlayerActionState::PostBackdown
                && (self.geom.half_length_m - ox * attack_dir) <= self.geom.paint_length_m
            {
                self.state.back_to_basket_timer_ds =
                    self.state.back_to_basket_timer_ds.saturating_add(1);
                if self.state.back_to_basket_timer_ds >= 50 {
                    self.trigger_turnover_stoppage(RuleCode::VIO_5S_POST, off_side, owner_r);
                    return Ok(());
                }
            } else {
                self.state.back_to_basket_timer_ds = 0;
            }

            if self.ruleset.clocks.closely_guarded_5sec_enabled && self.state.game_clock_running {
                let is_holding = matches!(
                    self.state.players[owner_r as usize].footwork.dribble_status,
                    DribbleStatus::LiveTripleThreat | DribbleStatus::Gathered
                );
                let mut min_d = 9.0_f64;
                for p_pos in 0..5 {
                    let dr = self.state.team_states[def_idx].active_lineup_slots[p_pos];
                    if dr != ROSTER_IDX_NONE {
                        let d = libm::hypot(
                            self.state.players[dr as usize].x_m - ox,
                            self.state.players[dr as usize].y_m - oy,
                        );
                        if d < min_d {
                            min_d = d;
                        }
                    }
                }
                if is_holding && min_d <= 1.05 {
                    self.state.closely_guarded_timer_ds =
                        self.state.closely_guarded_timer_ds.saturating_add(1);
                    if self.state.closely_guarded_timer_ds >= 50 {
                        self.trigger_turnover_stoppage(
                            RuleCode::VIO_5S_CLOSELY_GUARDED,
                            off_side,
                            owner_r,
                        );
                        return Ok(());
                    }
                } else {
                    self.state.closely_guarded_timer_ds = 0;
                }
            } else {
                self.state.closely_guarded_timer_ds = 0;
            }

            // Stage 4: Ball-handler EPV / Endgame WP decision!
            self.evaluate_ball_handler_decision(off_idx, def_idx, owner_slot, owner_r, ox, oy, attack_dir)?;
            self.check_paint_clocks(off_side);
        }

        Ok(())
    }

    fn check_paint_clocks(&mut self, offense: CommandSide) {
        if !matches!(self.state.game_phase, GamePhase::LiveBall | GamePhase::ShotInFlight)
            || !self.state.game_clock_ticked_this_step
        {
            return;
        }
        let off = offense.team_index().unwrap();
        let def = 1 - off;
        let attack_dir = if off == 0 { 1.0 } else { -1.0 };
        let attackers = self.state.team_states[off].active_lineup_slots;
        let defenders = self.state.team_states[def].active_lineup_slots;
        if !self.state.ball_in_frontcourt {
            for roster in attackers.into_iter().chain(defenders).filter(|&r| r != ROSTER_IDX_NONE) {
                self.state.players[usize::from(roster)].paint_timer_ds = 0;
            }
            return;
        }
        let shot = self.state.ball.shot_ctx.as_ref();
        let shooter = shot.map(|ctx| ctx.shooter_roster_idx);
        let shot_in_flight = shot.is_some();
        let owner = self.state.ball.owner_slot.map(|slot| attackers[usize::from(slot) % 5]);
        for r in attackers.into_iter().filter(|&r| r != ROSTER_IDX_NONE) {
            let radius = self.teams[off].players[usize::from(r) % 15].physical.collision_radius_m();
            let player = &mut self.state.players[usize::from(r)];
            if shot_in_flight || !self.geom.footprint_intersects_paint(player.x_m, player.y_m, attack_dir, 0.0, radius) {
                player.paint_timer_ds = 0;
                continue;
            }
            if player.vy_m_s * player.y_m.signum() > 1.0 || -attack_dir * player.vx_m_s > 1.2 {
                player.exiting_paint_grace_ticks = 5;
            }
            player.paint_timer_ds = player.paint_timer_ds.saturating_add(1);
            let finishing_drive = owner == Some(r) && player.paint_timer_ds <= 34
                && player.action_state == PlayerActionState::Drive
                && player.vx_m_s * attack_dir > 0.0;
            if player.paint_timer_ds > 30 && player.exiting_paint_grace_ticks == 0 && !finishing_drive {
                self.trigger_turnover_stoppage(RuleCode::VIO_OFF_3S, offense, r);
                return;
            }
        }
        if !self.ruleset.clocks.defensive_3sec_enabled {
            return;
        }
        for r in defenders.into_iter().filter(|&r| r != ROSTER_IDX_NONE) {
            let radius = self.teams[def].players[usize::from(r) % 15].physical.collision_radius_m();
            let player = &self.state.players[usize::from(r)];
            let (x, y) = (player.x_m, player.y_m);
            let in_paint = self.geom.footprint_intersects_paint(x, y, attack_dir, 1.2192, radius);
            let guarding = attackers.iter().filter_map(|&r| self.state.players.get(usize::from(r)))
                .any(|attacker| libm::hypot(attacker.x_m - x, attacker.y_m - y) <= 1.05);
            let timer = &mut self.state.players[usize::from(r)].paint_timer_ds;
            if !in_paint || guarding {
                *timer = 0;
                continue;
            }
            *timer = timer.saturating_add(1);
            if *timer <= 30 {
                continue;
            }
            if shot_in_flight {
                self.state.deferred_def_3s_violation = Some((r, true));
            } else {
                self.open_defensive_three_seconds(offense, r, owner.or(shooter));
            }
            return;
        }
    }

    fn open_defensive_three_seconds(&mut self, offense: CommandSide, defender: u8, shooter: Option<u8>) {
        let y = self.state.players[usize::from(defender)].y_m;
        let attack_dir = if offense == CommandSide::Home { 1.0 } else { -1.0 };
        self.state.oob_spot_xy = [attack_dir * (self.geom.half_length_m - 8.5344), y];
        self.state.oob_is_endline = false;
        let ruling_id = self.state.next_ruling_id;
        self.state.next_ruling_id += 1;
        let call = RuleCallSnapshot {
            rule_code: RuleCode::VIO_DEF_3S,
            offending_side: offense.opponent(),
            offender_roster_idx: defender,
            victim_roster_idx: shooter,
            ft_count: 1,
            ball_in_frontcourt_at_call: true,
            interruption_spot: InboundSpot::FrontcourtSideline28ft,
            stoppage_sideline_side: CourtSideLine::from_y_m(y),
            restart_shot_clock_ds: compute_shot_clock_reset(
                ShotClockScenario::OffenseFrontcourtNonShotDefFoul,
                self.state.shot_clock_ds, self.state.game_clock_ds, &self.ruleset,
            ),
            foul_category: 2,
            ..RuleCallSnapshot::default()
        };
        self.open_pending_ruling(ruling_id, self.state.sim_tick, call, self.state.game_clock_rem_10us == 0);
    }

    fn step_player_kinematics_and_ccd(
        &mut self,
        off_idx: usize,
        def_idx: usize,
    ) -> Result<Vec<(usize, usize)>, RngBudgetExceeded> {
        let attack_dir = if off_idx == 0 { 1.0 } else { -1.0 };
        let rim_xy = self.geom.rim_xy(attack_dir);
        let owner_slot_opt = self.state.ball.owner_slot;
        let ball_xy = (self.state.ball.position_m[0], self.state.ball.position_m[1]);
        let ball_vel = (self.state.ball.velocity_m_s[0], self.state.ball.velocity_m_s[1]);
        let handler_pos_idx: usize = if let Some(os) = owner_slot_opt {
            (os as usize) % 5
        } else if let Some(ref pass) = self.state.ball.pass_ctx {
            (pass.target_slot as usize) % 5
        } else {
            0
        };
        let in_frontcourt = self.state.ball_in_frontcourt;
        let score_deficit = i32::from(self.state.team_states[off_idx].score_on_court)
            - i32::from(self.state.team_states[def_idx].score_on_court);
        let full_press = self.state.full_court_press[def_idx]
            || (self.teams[def_idx].tactics.full_court_press_clutch
                && self.state.quarter >= 4 && self.state.game_clock_ds <= 1200
                && (1..=12).contains(&score_deficit));

        let handler_r = self.state.team_states[off_idx].active_lineup_slots[handler_pos_idx];
        let (handler_xy, handler_is_driving) = if handler_r != ROSTER_IDX_NONE {
            let hp = &self.state.players[handler_r as usize];
            (
                (hp.x_m, hp.y_m),
                owner_slot_opt.is_some()
                    && hp.action_state == PlayerActionState::Drive
                    && hp.action_timer_ds > 0,
            )
        } else {
            (ball_xy, false)
        };

        let pb_index = self
            .playbooks
            .iter()
            .position(|p| p.play_id == self.state.active_play_id[off_idx]);
        let phase_index = pb_index.map(|i| {
            (self.state.play_phase_idx[off_idx] as usize)
                .min(self.playbooks[i].phases.len().saturating_sub(1))
        });
        let pb_opt = pb_index.map(|i| &self.playbooks[i]);
        let phase_opt = pb_opt.and_then(|pb| pb.phases.get(phase_index.unwrap_or(0)));

        let mut off_xy = [(0.0_f64, 0.0_f64); 5];
        let mut off_active = [false; 5];
        let mut def_xy = [(0.0_f64, 0.0_f64); 5];
        let mut def_active = [false; 5];
        for i in 0..5 {
            let or = self.state.team_states[off_idx].active_lineup_slots[i];
            if or != ROSTER_IDX_NONE {
                off_active[i] = true;
                off_xy[i] = (self.state.players[or as usize].x_m, self.state.players[or as usize].y_m);
            }
            let dr = self.state.team_states[def_idx].active_lineup_slots[i];
            if dr != ROSTER_IDX_NONE {
                def_active[i] = true;
                def_xy[i] = (self.state.players[dr as usize].x_m, self.state.players[dr as usize].y_m);
            }
        }
        let handler_perim_ok = self.geom.dist_to_rim(handler_xy.0, handler_xy.1, attack_dir) > 3.6;

        let mut active_ball_screen: Option<(usize, usize)> = None;
        let mut active_offball_screen: Option<(usize, usize, (f64, f64))> = None;
        let mut active_screener_pos: Option<usize> = None;
        let mut active_screen_target_pos: Option<usize> = None;
        let mut active_screen_xy: Option<(f64, f64)> = None;
        if let Some(phase) = phase_opt {
            if in_frontcourt {
                for (r_idx, act) in phase.actions.iter().enumerate() {
                    if act.0 != 2 && act.0 != 5 {
                        continue;
                    }
                    let scr_p = self.state.slot_bindings[off_idx][r_idx.min(4)].min(4) as usize;
                    let raw_tgt_role = (act.3 as usize).min(4);
                    let mut tgt_p = self.state.slot_bindings[off_idx][raw_tgt_role].min(4) as usize;
                    let targets_handler_role = pb_opt
                        .is_some_and(|pb| raw_tgt_role == (pb.role_priority[0] as usize).min(4))
                        || phase.actions[raw_tgt_role].0 == 1;
                    if act.0 == 2 && targets_handler_role && handler_perim_ok && scr_p != handler_pos_idx {
                        tgt_p = handler_pos_idx;
                        if active_ball_screen.is_none() {
                            active_ball_screen = Some((scr_p, tgt_p));
                        }
                    } else if act.0 == 2 && tgt_p != handler_pos_idx && tgt_p != scr_p {
                        if let Some(sr_xy) = off_active[scr_p].then(|| off_xy[scr_p]) {
                            if active_offball_screen.is_none() {
                                active_offball_screen = Some((scr_p, tgt_p, sr_xy));
                            }
                        }
                    }

                    let sr = self.state.team_states[off_idx].active_lineup_slots[scr_p];
                    if sr == ROSTER_IDX_NONE {
                        continue;
                    }
                    let sp = &self.state.players[sr as usize];
                    let d_handler = libm::hypot(sp.x_m - handler_xy.0, sp.y_m - handler_xy.1);
                    let d_tgt = {
                        let tr = self.state.team_states[off_idx].active_lineup_slots[tgt_p];
                        if tr != ROSTER_IDX_NONE {
                            let tp = &self.state.players[tr as usize];
                            libm::hypot(sp.x_m - tp.x_m, sp.y_m - tp.y_m)
                        } else {
                            d_handler
                        }
                    };
                    if sp.action_state == PlayerActionState::SetScreen
                        || (act.0 == 2 && (d_handler < 2.8 || d_tgt < 2.4))
                        || (act.0 == 5 && d_handler < 2.2)
                    {
                        if active_screener_pos.is_none() {
                            active_screener_pos = Some(scr_p);
                            active_screen_target_pos = Some(tgt_p);
                            active_screen_xy = Some((sp.x_m, sp.y_m));
                        }
                        self.state.play_screen_contact_ds[off_idx] =
                            self.state.play_screen_contact_ds[off_idx].saturating_add(1);
                    }
                }
            }
        }
        if let Some((b_scr_p, _)) = active_ball_screen {
            let sr = self.state.team_states[off_idx].active_lineup_slots[b_scr_p];
            if sr != ROSTER_IDX_NONE {
                let sp = &self.state.players[sr as usize];
                let d_h = libm::hypot(sp.x_m - handler_xy.0, sp.y_m - handler_xy.1);
                let sp_spd = libm::hypot(sp.vx_m_s, sp.vy_m_s);
                if d_h < 2.35 && (sp.action_state == PlayerActionState::SetScreen || sp_spd < 1.65) {
                    self.state.screen_ready_ticks[off_idx] =
                        self.state.screen_ready_ticks[off_idx].saturating_add(1);
                } else {
                    self.state.screen_ready_ticks[off_idx] = 0;
                }
            } else {
                self.state.screen_ready_ticks[off_idx] = 0;
            }
        } else {
            self.state.screen_ready_ticks[off_idx] = 0;
        }

        // 防守协调在防守模块内完成，这里只接收可观测结果
        let coordination = self.coordinate_defense(
            def_idx,
            off_idx,
            handler_pos_idx,
            handler_r,
            handler_xy,
            handler_is_driving,
            in_frontcourt,
            active_ball_screen,
            active_screener_pos,
            active_screen_target_pos,
            active_screen_xy,
            &off_xy,
            &def_xy,
            &def_active,
            attack_dir,
        );
        // Paint occupancy by non-screener offensive players (public court observation)
        let paint_occupied = (0..5)
            .filter(|&p_pos| {
                if active_screener_pos == Some(p_pos) {
                    return false;
                }
                let r = self.state.team_states[off_idx].active_lineup_slots[p_pos];
                r != ROSTER_IDX_NONE
                    && self
                        .geom
                        .dist_to_rim(self.state.players[r as usize].x_m, self.state.players[r as usize].y_m, attack_dir)
                        < 3.8
            })
            .count()
            >= 1;
        // 防守协调持有的可变借用已结束，这里重新绑定进攻剧本引用
        let pb_opt = pb_index.map(|i| &self.playbooks[i]);
        let phase_opt = pb_opt.and_then(|pb| pb.phases.get(phase_index.unwrap_or(0)));
        let visible_defenders = core::array::from_fn(|i| def_active[i].then_some(def_xy[i]));
        let observed_pnr_cov = active_screen_xy
            .and_then(|screen| observe_pnr_coverage(handler_xy, screen, &visible_defenders, attack_dir));
        let nearest_defender = |target: (f64, f64)| {
            visible_defenders.iter().flatten().copied().min_by(|a, b| {
                let distance = |p: (f64, f64)| (p.0 - target.0).powi(2) + (p.1 - target.1).powi(2);
                distance(*a).total_cmp(&distance(*b))
            }).unwrap_or((target.0 + attack_dir, target.1))
        };
        let visible_handler_def_xy = nearest_defender(handler_xy);

        let mut active_cutter_count: usize = (0..5)
            .filter(|&p_pos| {
                let rr = self.state.team_states[off_idx].active_lineup_slots[p_pos];
                rr != ROSTER_IDX_NONE
                    && self.state.players[rr as usize].offball_cut_timer_ds > 0
                    && matches!(self.state.players[rr as usize].offball_cut_kind, 1 | 2 | 4 | 5)
            })
            .count();
        let mut planned_off_xy = off_xy;

        // Compute kinematic plans across both teams cleanly before mutating player state
        let mut plans: [Option<(f64, f64, Option<PlayerActionState>, f64, bool)>; 10] = [None; 10];
        let mut cut_triggers: [Option<(u8, u8, i8)>; 5] = [None; 5];
        for team_idx in 0..2 {
            let offset = team_idx * 15;
            let is_off = team_idx == off_idx;
            for p_pos in 0..5 {
                let slot_idx = team_idx * 5 + p_pos;
                let r = self.state.team_states[team_idx].active_lineup_slots[p_pos];
                if r == ROSTER_IDX_NONE {
                    continue;
                }
                let rank = (r as usize).saturating_sub(offset);
                let Some(snap) = self.teams[team_idx].players.get(rank) else {
                    continue;
                };
                let is_handler = is_off && owner_slot_opt == Some(slot_idx as u8);
                let plan = if is_off {
                    let my_def_xy = nearest_defender(off_xy[p_pos]);
                    let (mut tx, mut ty, act_ov, mut spd_scale, new_cut) = self.compute_offense_kinematic_target(
                        off_idx,
                        p_pos,
                        r,
                        snap,
                        is_handler,
                        in_frontcourt,
                        handler_pos_idx,
                        handler_xy,
                        visible_handler_def_xy,
                        my_def_xy,
                        handler_is_driving,
                        pb_opt,
                        phase_opt,
                        paint_occupied,
                        active_cutter_count,
                        observed_pnr_cov,
                        active_ball_screen,
                        active_offball_screen,
                        &planned_off_xy,
                        &off_active,
                        &def_xy,
                        &def_active,
                        attack_dir,
                        rim_xy,
                    );
                    if let Some((ck, _, _)) = new_cut {
                        if matches!(ck, 1 | 2 | 4 | 5) {
                            active_cutter_count += 1;
                        }
                    }
                    cut_triggers[p_pos] = new_cut;
                    if !is_handler && self.state.players[r as usize].paint_timer_ds >= 17 {
                        let exit_sign = if self.state.players[r as usize].y_m >= 0.0 { 1.0 } else { -1.0 };
                        let (ex, ey) = self.geom.inbounds_nav_bounds(0.85).clamp(
                            rim_xy.0 - attack_dir * 1.7,
                            exit_sign * (self.geom.paint_width_m * 0.5 + 1.05),
                        );
                        tx = ex;
                        ty = ey;
                        spd_scale = 0.78;
                    }
                    planned_off_xy[p_pos] = (tx, ty);
                    (tx, ty, act_ov, spd_scale, false)
                } else {
                    let mut debug_flags = 0;
                    let target = self.compute_defense_kinematic_target(
                        off_idx,
                        def_idx,
                        p_pos,
                        r,
                        in_frontcourt,
                        full_press,
                        handler_pos_idx,
                        handler_is_driving,
                        ball_xy,
                        ball_vel,
                        active_offball_screen,
                        attack_dir,
                        rim_xy,
                        &coordination,
                        &mut debug_flags,
                    );
                    self.transient_debug_flags[slot_idx] |= debug_flags;
                    target
                };
                plans[slot_idx] = Some(plan);
            }
        }
        let mut is_screen_or_dho = [false; 5];
        if in_frontcourt {
            if let Some((sp, tp)) = active_ball_screen {
                is_screen_or_dho[sp.min(4)] = true;
                is_screen_or_dho[tp.min(4)] = true;
            }
            if let Some((sp, cp, _)) = active_offball_screen {
                is_screen_or_dho[sp.min(4)] = true;
                is_screen_or_dho[cp.min(4)] = true;
            }
            if let Some(ph) = phase_opt {
                for r_i in 0..5 {
                    let pos_i = (self.state.slot_bindings[off_idx][r_i] as usize).min(4);
                    let act_i = ph.actions[r_i];
                    if act_i.0 == 5 {
                        is_screen_or_dho[pos_i] = true;
                        let tgt_i = (self.state.slot_bindings[off_idx]
                            [(act_i.3 as usize).min(4)] as usize)
                            .min(4);
                        is_screen_or_dho[tgt_i] = true;
                    }
                }
            }
            let guard_bounds = self.geom.inbounds_nav_bounds(0.85);
            for _pass in 0..2 {
                for a in 0..5 {
                    if !off_active[a] {
                        continue;
                    }
                    let sa = off_idx * 5 + a;
                    let Some(pa) = plans[sa] else {
                        continue;
                    };
                    for b in (a + 1)..5 {
                        if !off_active[b] || (is_screen_or_dho[a] && is_screen_or_dho[b]) {
                            continue;
                        }
                        let sb = off_idx * 5 + b;
                        let Some(pb) = plans[sb] else {
                            continue;
                        };
                        let td = libm::hypot(pb.0 - pa.0, pb.1 - pa.1);
                        let cd = libm::hypot(
                            off_xy[b].0 - off_xy[a].0,
                            off_xy[b].1 - off_xy[a].1,
                        );
                        if td < 2.65 || (cd < 2.0 && td < 2.95) {
                            let a_is_h = owner_slot_opt == Some(sa as u8);
                            let b_is_h = owner_slot_opt == Some(sb as u8);
                            let move_b = if a_is_h {
                                true
                            } else if b_is_h {
                                false
                            } else if is_screen_or_dho[a] && !is_screen_or_dho[b] {
                                true
                            } else {
                                !(is_screen_or_dho[b] && !is_screen_or_dho[a])
                            };
                            let (src, dst_slot) = if move_b { (pa, sb) } else { (pb, sa) };
                            if let Some(ref mut d_plan) = plans[dst_slot] {
                                let mut vx = d_plan.0 - src.0;
                                let mut vy = d_plan.1 - src.1;
                                let vn = libm::hypot(vx, vy);
                                if vn < 0.25 {
                                    vx = -attack_dir * 0.45;
                                    vy = if ((a + b) & 1) == 0 { 1.0 } else { -1.0 };
                                } else {
                                    vx /= vn;
                                    vy /= vn;
                                }
                                let (mut cx, cy) =
                                    guard_bounds.clamp(src.0 + vx * 2.80, src.1 + vy * 2.80);
                                cx = if attack_dir > 0.0 {
                                    cx.max(1.35)
                                } else {
                                    cx.min(-1.35)
                                };
                                d_plan.0 = cx;
                                d_plan.1 = cy;
                            }
                        }
                    }
                }
            }
        }
        for p_pos in 0..5 {
            if let Some((ck, ct, cs)) = cut_triggers[p_pos] {
                let r = self.state.team_states[off_idx].active_lineup_slots[p_pos];
                if r != ROSTER_IDX_NONE {
                    let op = &mut self.state.players[r as usize];
                    if op.offball_cut_timer_ds == 0 {
                        op.offball_cut_kind = ck;
                        op.offball_cut_timer_ds = ct;
                        op.offball_cut_side = cs;
                    }
                }
            }
        }

        let mut actors = Vec::with_capacity(10);
        for team_idx in 0..2 {
            let offset = team_idx * 15;
            let is_off = team_idx == off_idx;
            for p_pos in 0..5 {
                let slot_idx = team_idx * 5 + p_pos;
                let Some((tx, ty, desired_action, speed_scale, is_on_ball_def_slot)) =
                    plans[slot_idx]
                else {
                    continue;
                };
                let r = self.state.team_states[team_idx].active_lineup_slots[p_pos];
                let rank = (r as usize).saturating_sub(offset);
                let snap = &self.teams[team_idx].players[rank];
                let is_handler = is_off && owner_slot_opt == Some(slot_idx as u8);
                let mut eff_tx = tx;
                let mut eff_ty = ty;
                {
                    let p = &mut self.state.players[r as usize];
                    if !is_off && in_frontcourt && !is_on_ball_def_slot && coordination.is_man_to_man() {
                        let is_xout = self.state.xout_rotator_slot[def_idx] == (slot_idx as u8)
                            && self.state.xout_timer_ds[def_idx] > 0;
                        if is_xout
                            || desired_action == Some(PlayerActionState::NavScreen)
                            || !p.help_target_valid
                        {
                            p.help_committed_xy = [tx, ty];
                            p.help_target_valid = true;
                            p.help_reaction_timer_ds = 0;
                        } else if libm::hypot(tx - p.help_committed_xy[0], ty - p.help_committed_xy[1]) > 0.75 {
                            if p.help_reaction_timer_ds == 0 {
                                let help_iq = f64::from(snap.attributes[AttrIdx::HelpDefenseIq]) / 99.0;
                                let [min_d, max_d] = self.tunables.help_reaction_delay_min_max_ds;
                                let d_ticks = ((max_d as f64)
                                    - help_iq * ((max_d.saturating_sub(min_d)) as f64))
                                    .round()
                                    .clamp(1.0, 4.0) as u8;
                                p.help_reaction_timer_ds = d_ticks;
                            }
                            if p.help_reaction_timer_ds > 1 {
                                p.help_reaction_timer_ds -= 1;
                                eff_tx = p.help_committed_xy[0];
                                eff_ty = p.help_committed_xy[1];
                            } else {
                                p.help_reaction_timer_ds = 0;
                                p.help_committed_xy = [tx, ty];
                            }
                        } else {
                            p.help_committed_xy = [tx, ty];
                            p.help_reaction_timer_ds = 0;
                        }
                    } else if !is_off {
                        p.help_committed_xy = [tx, ty];
                        p.help_target_valid = true;
                        p.help_reaction_timer_ds = 0;
                    }
                }

                let mut eff_speed_scale = speed_scale;
                if is_off
                    && !is_handler
                    && desired_action != Some(PlayerActionState::SetScreen)
                    && (in_frontcourt || (self.state.players[r as usize].x_m * attack_dir) > -3.0)
                {
                    let px = self.state.players[r as usize].x_m;
                    let py = self.state.players[r as usize].y_m;
                    let mut avoid_x = 0.0_f64;
                    let mut avoid_y = 0.0_f64;
                    let mut min_mate_d = 99.0_f64;
                    for k in 0..5 {
                        if k == p_pos || !off_active[k] {
                            continue;
                        }
                        if is_screen_or_dho[p_pos] && is_screen_or_dho[k] {
                            continue;
                        }
                        let dx_m = px - off_xy[k].0;
                        let dy_m = py - off_xy[k].1;
                        let d_m = libm::hypot(dx_m, dy_m);
                        if d_m < min_mate_d {
                            min_mate_d = d_m;
                        }
                        let sep_r = if owner_slot_opt == Some((off_idx * 5 + k) as u8) {
                            2.55
                        } else {
                            2.35
                        };
                        if d_m < sep_r {
                            let (ux, uy) = if d_m > 0.05 {
                                (dx_m / d_m, dy_m / d_m)
                            } else {
                                (0.0, if p_pos > k { 1.0 } else { -1.0 })
                            };
                            let w = (sep_r - d_m) / sep_r;
                            let push = 2.85 * w;
                            avoid_x += ux * push;
                            avoid_y += uy * push;
                        }
                    }
                    if libm::hypot(avoid_x, avoid_y) > 0.05 {
                        let raw_ax = eff_tx + avoid_x;
                        let raw_ay = eff_ty + avoid_y;
                        let (adj_x, adj_y) = if min_mate_d < 1.75 {
                            (
                                px + (raw_ax - px) * 0.45 + avoid_x * 0.85,
                                py + (raw_ay - py) * 0.45 + avoid_y * 0.85,
                            )
                        } else {
                            (raw_ax, raw_ay)
                        };
                        let (mut cx, cy) = self.geom.inbounds_nav_bounds(0.85).clamp(adj_x, adj_y);
                        if in_frontcourt {
                            cx = if attack_dir > 0.0 {
                                cx.max(1.35)
                            } else {
                                cx.min(-1.35)
                            };
                        }
                        eff_tx = cx;
                        eff_ty = cy;
                        if min_mate_d < 1.85 {
                            eff_speed_scale = eff_speed_scale.max(0.68);
                        }
                    }
                }
                self.transient_target_xy[slot_idx] = (eff_tx, eff_ty);

                let p = &mut self.state.players[r as usize];
                let p_action = p.action_state;
                let p_timer = p.action_timer_ds;
                if p.exiting_paint_grace_ticks > 0 {
                    p.exiting_paint_grace_ticks -= 1;
                }
                let dx = eff_tx - p.x_m;
                let dy = eff_ty - p.y_m;
                let max_spd = self.tunables.max_offball_speed_min_max_m_s[0]
                    + (f64::from(snap.attributes[AttrIdx::TopSpeed]) / 99.0)
                        * (self.tunables.max_offball_speed_min_max_m_s[1]
                            - self.tunables.max_offball_speed_min_max_m_s[0]);
                let is_gathered_handler =
                    is_handler && p.footwork.dribble_status == DribbleStatus::Gathered;
                if is_gathered_handler {
                    p.vx_m_s = 0.0;
                    p.vy_m_s = 0.0;
                }
                let is_drive = is_handler && p_action == PlayerActionState::Drive && p_timer > 0;
                let cap_spd = if is_gathered_handler {
                    0.0
                } else if is_drive {
                    max_spd * 0.99
                } else if is_on_ball_def_slot {
                    let base_def_spd = if handler_is_driving {
                        max_spd * 0.87
                    } else {
                        max_spd * 0.96
                    };
                    base_def_spd * eff_speed_scale
                } else {
                    max_spd * eff_speed_scale
                };
                let acceleration = f64::from(snap.attributes[AttrIdx::Acceleration]) / 99.0;
                let max_acc = if is_on_ball_def_slot {
                    8.5 + acceleration * 3.0
                } else {
                    6.4 + acceleration * 3.0
                };
                let dist = Self::steer_player_velocity(p, dx, dy, cap_spd, max_acc);
                let cur_spd = libm::hypot(p.vx_m_s, p.vy_m_s);

                if desired_action == Some(PlayerActionState::SetScreen) {
                    p.facing_rad = libm::atan2(ball_xy.1 - p.y_m, ball_xy.0 - p.x_m);
                } else if is_handler {
                    if cur_spd > 0.75 && desired_action != Some(PlayerActionState::PostBackdown) {
                        p.facing_rad = libm::atan2(p.vy_m_s, p.vx_m_s);
                        if p.footwork.dribble_status == DribbleStatus::LiveTripleThreat {
                            p.footwork.dribble_status = DribbleStatus::Dribbling;
                        }
                    } else {
                        p.facing_rad = libm::atan2(rim_xy.1 - p.y_m, rim_xy.0 - p.x_m);
                    }
                } else if !is_off {
                    p.facing_rad = libm::atan2(ball_xy.1 - p.y_m, ball_xy.0 - p.x_m);
                } else if dist > 0.35 && cur_spd > 0.4 {
                    p.facing_rad = libm::atan2(p.vy_m_s, p.vx_m_s);
                } else {
                    p.facing_rad = libm::atan2(ball_xy.1 - p.y_m, ball_xy.0 - p.x_m);
                }

                // Update defender Legal Guarding Position (LGP)
                if !is_off {
                    let bx_d = ball_xy.0 - p.x_m;
                    let by_d = ball_xy.1 - p.y_m;
                    let bd = libm::hypot(bx_d, by_d).max(1.0e-5);
                    let facing_cos =
                        libm::cos(p.facing_rad) * (bx_d / bd) + libm::sin(p.facing_rad) * (by_d / bd);
                    if cur_spd <= 2.35 && facing_cos >= 0.45 {
                        if p.lgp_established_tick.is_none() {
                            p.lgp_established_tick = Some(self.state.sim_tick);
                        }
                    } else {
                        p.lgp_established_tick = None;
                    }
                } else {
                    p.lgp_established_tick = None;
                }

                if !(is_handler
                    && p_timer > 0
                    && matches!(p_action, PlayerActionState::Catch | PlayerActionState::Drive))
                {
                    p.action_state = if let Some(act_ov) = desired_action {
                        act_ov
                    } else if is_handler {
                        if cur_spd > 0.6 || p.footwork.dribble_status == DribbleStatus::Dribbling {
                            PlayerActionState::Dribble
                        } else {
                            PlayerActionState::Idle
                        }
                    } else if is_on_ball_def_slot && dist > 1.35 && cur_spd > 2.2 {
                        PlayerActionState::Closeout
                    } else if cur_spd > max_spd * 0.80 {
                        PlayerActionState::Sprint
                    } else if cur_spd > 0.55 {
                        if is_off {
                            PlayerActionState::Jog
                        } else {
                            PlayerActionState::DefSlide
                        }
                    } else {
                        PlayerActionState::Idle
                    };
                }

                let rad = snap.physical.collision_radius_m();
                let bounds = self.geom.playable_bounds(rad);
                let cid = canonical_actor_id(self.canonical_team_ids[team_idx], rank);
                actors.push(CcdActor {
                    slot: slot_idx,
                    canonical_actor_id: cid,
                    x: p.x_m,
                    y: p.y_m,
                    vx: p.vx_m_s,
                    vy: p.vy_m_s,
                    radius: rad,
                    inv_mass: 1.0 / snap.physical.weight_kg.max(55.0),
                    bounds,
                });
            }
        }

        let contact_pairs = self.run_ccd_on_actors(&mut actors);

        // Sync ball with owner if held/dribbled with natural dribble bounce
        if let Some(os) = self.state.ball.owner_slot {
            let team_idx = (os as usize) / 5;
            let r = self.state.team_states[team_idx].active_lineup_slots[(os as usize) % 5];
            if r != ROSTER_IDX_NONE {
                let pr = &self.state.players[r as usize];
                let is_drib = pr.footwork.dribble_status == DribbleStatus::Dribbling
                    || libm::hypot(pr.vx_m_s, pr.vy_m_s) > 0.5;
                let z = if is_drib {
                    0.82 + 0.30 * libm::fabs(libm::sin((self.state.sim_tick as f64) * 0.95))
                } else {
                    1.22
                };
                let (bx, by) = self.geom.inbounds_nav_bounds(0.08).clamp(
                    pr.x_m + libm::cos(pr.facing_rad) * 0.28,
                    pr.y_m + libm::sin(pr.facing_rad) * 0.28,
                );
                self.state.ball.position_m = [bx, by, z + pr.z_m];
            }
        }

        Ok(contact_pairs)
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_offense_kinematic_target(
        &self,
        off_idx: usize,
        p_pos: usize,
        r: u8,
        snap: &CompiledPlayerSnapshot,
        is_handler: bool,
        in_frontcourt: bool,
        handler_pos_idx: usize,
        handler_xy: (f64, f64),
        handler_def_xy: (f64, f64),
        my_def_xy: (f64, f64),
        handler_is_driving: bool,
        pb_opt: Option<&CompiledPlaybook>,
        phase_opt: Option<&CompiledPlayPhase>,
        paint_occupied: bool,
        active_cutter_count: usize,
        observed_pnr_cov: Option<PnrCoverageKind>,
        active_ball_screen: Option<(usize, usize)>,
        active_offball_screen: Option<(usize, usize, (f64, f64))>,
        off_xy: &[(f64, f64); 5],
        off_active: &[bool; 5],
        def_xy: &[(f64, f64); 5],
        def_active: &[bool; 5],
        attack_dir: f64,
        rim_xy: (f64, f64),
    ) -> (f64, f64, Option<PlayerActionState>, f64, Option<(u8, u8, i8)>) {
        let pr = &self.state.players[r as usize];
        let px = pr.x_m;
        let py = pr.y_m;
        let role_idx = self.state.slot_bindings[off_idx]
            .iter()
            .position(|&s| s as usize == p_pos)
            .unwrap_or(p_pos)
            .min(4);
        let in_trans = self.state.transition_window_ds[off_idx] > 0;

        // 1. Transition push & 3-lane fast break spacing
        let nav = self.geom.inbounds_nav_bounds(0.85);
        if !in_frontcourt || (is_handler && (px * attack_dir) < 2.2 && !(pr.action_state == PlayerActionState::Drive && pr.action_timer_ds > 0)) {
            if is_handler {
                let (cx, cy) = nav.clamp(attack_dir * 4.5, py.clamp(-3.2, 3.2));
                return (
                    cx,
                    cy,
                    None,
                    if in_trans { 0.88 } else { 0.78 },
                    None,
                );
            }
            if in_trans || (px * attack_dir) < 1.5 {
                let rr_y = if py >= 0.0 { 1.55 } else { -1.55 };
                let (lx, ly) = match role_idx {
                    4 => (rim_xy.0 - attack_dir * 2.1, rr_y),
                    1 => (rim_xy.0 - attack_dir * 1.15, -(self.geom.half_width_m - 1.15)),
                    2 => (rim_xy.0 - attack_dir * 1.15, self.geom.half_width_m - 1.15),
                    3 => (rim_xy.0 - attack_dir * 6.0, 4.9),
                    _ => (rim_xy.0 - attack_dir * 6.5, -4.5),
                };
                let (cx, cy) = nav.clamp(lx, ly);
                return (
                    cx,
                    cy,
                    if in_trans { Some(PlayerActionState::Sprint) } else { None },
                    if in_trans { 0.90 } else { 0.74 },
                    None,
                );
            }
        } else if in_trans
            && self.state.transition_window_ds[off_idx] > 8
            && !is_handler
            && !(pr.action_state == PlayerActionState::Drive && pr.action_timer_ds > 0)
        {
            let rr_y = if py >= 0.0 { 1.55 } else { -1.55 };
            let (lx, ly) = match role_idx {
                4 => (rim_xy.0 - attack_dir * 2.1, rr_y),
                1 => (rim_xy.0 - attack_dir * 1.15, -(self.geom.half_width_m - 1.15)),
                2 => (rim_xy.0 - attack_dir * 1.15, self.geom.half_width_m - 1.15),
                3 => (rim_xy.0 - attack_dir * 6.0, 4.9),
                _ => (rim_xy.0 - attack_dir * 6.5, -4.5),
            };
            let (cx, cy) = nav.clamp(lx, ly);
            return (cx, cy, Some(PlayerActionState::Sprint), 0.88, None);
        }

        // 2. Active physical drive along dynamic screen/lane target or corridor
        if is_handler && pr.action_state == PlayerActionState::Drive && pr.action_timer_ds > 0 {
            let has_dyn_tgt = libm::hypot(
                self.state.active_drive_target_xy[0],
                self.state.active_drive_target_xy[1],
            ) > 0.1;
            let (raw_tx, raw_ty, spd_scale) = if has_dyn_tgt {
                let dt = self.state.active_drive_target_xy;
                let s_scale = if self.state.active_drive_corridor == 3 { 0.92 } else { 0.98 };
                (dt[0], dt[1], s_scale)
            } else {
                match self.state.active_drive_corridor {
                    0 => (rim_xy.0 - attack_dir * 1.15, (py + 1.45).clamp(-1.8, 2.3), 0.98),
                    1 => (rim_xy.0 - attack_dir * 1.15, (py - 1.45).clamp(-2.3, 1.8), 0.98),
                    2 => (rim_xy.0 - attack_dir * 1.05, py * 0.25, 0.99),
                    _ => (
                        px - attack_dir * 1.65,
                        py + if py >= 0.0 { 0.85 } else { -0.85 },
                        0.92,
                    ),
                }
            };
            let (mut cx, cy) = nav.clamp(raw_tx, raw_ty);
            if in_frontcourt {
                cx = if attack_dir > 0.0 { cx.max(1.35) } else { cx.min(-1.35) };
            }
            return (cx, cy, None, spd_scale, None);
        }

        // 3. Playbook phase actions + Read & React + Off-Ball Spacing Field
        let Some(pb) = pb_opt else {
            return (rim_xy.0 - attack_dir * 6.2, (p_pos as f64 - 2.0) * 2.8, None, 0.66, None);
        };
        let Some(phase) = phase_opt else {
            return (rim_xy.0 - attack_dir * 6.2, (p_pos as f64 - 2.0) * 2.8, None, 0.66, None);
        };

        let act = phase.actions[role_idx];
        let primary_role = pb.role_priority[0].min(4);
        let (akind, resolved_target) = resolve_safe_playbook_action(
            role_idx as u8,
            act.0,
            act.3,
            primary_role,
            &self.state.slot_bindings[off_idx],
            is_handler,
        );
        let mirrored = self.state.play_mirrored[off_idx];
        let anchor_xy = resolve_anchor_coord(
            &self.geom,
            act.1,
            act.2,
            act.4,
            act.5,
            attack_dir,
            mirrored,
        );
        let tempo = act.6.clamp(0.55, 1.05);
        let bounds = self.geom.inbounds_nav_bounds(0.85);

        let mut target_pos =
            self.state.slot_bindings[off_idx][(resolved_target as usize).min(4)].min(4) as usize;
        if akind == 2 && active_ball_screen.is_some_and(|(s, _)| s == p_pos) {
            target_pos = handler_pos_idx;
        }
        let target_r = self.state.team_states[off_idx].active_lineup_slots[target_pos];
        let target_xy = if target_r != ROSTER_IDX_NONE {
            (
                self.state.players[target_r as usize].x_m,
                self.state.players[target_r as usize].y_m,
            )
        } else {
            handler_xy
        };

        // Active stateful off-ball cut execution (Pass&Cut, Backdoor, V-Cut, Curl, Flare)
        if !is_handler && pr.offball_cut_kind > 0 && pr.offball_cut_timer_ds > 0 && akind != 2 && akind != 5 {
            let cs = if pr.offball_cut_side >= 0 { 1.0 } else { -1.0 };
            let ct = pr.offball_cut_timer_ds;
            match pr.offball_cut_kind {
                1 => {
                    // Pass & Cut: dive toward basket arc, then clear out cleanly to weak corner
                    let (cx, cy, spd) = if ct >= 10 {
                        let (tx, ty) = bounds.clamp(rim_xy.0 - attack_dir * 2.1, cs * 1.45);
                        (tx, ty, 0.86)
                    } else {
                        let (tx, ty) = bounds.clamp(
                            rim_xy.0 - attack_dir * 1.4,
                            -cs * (self.geom.corner_three_dist_m - 0.20),
                        );
                        (tx, ty, 0.64)
                    };
                    return (cx, cy, None, spd, None);
                }
                2 => {
                    // Backdoor: explosive dive behind overplaying defender, then peel out
                    let (cx, cy, spd) = if ct >= 6 {
                        let (tx, ty) = bounds.clamp(rim_xy.0 - attack_dir * 1.75, cs * 1.15);
                        (tx, ty, 0.90)
                    } else {
                        let (tx, ty) = bounds.clamp(rim_xy.0 - attack_dir * 1.6, -cs * 2.25);
                        (tx, ty, 0.68)
                    };
                    return (cx, cy, None, spd, None);
                }
                3 => {
                    // V-Cut / L-Cut: jab slow toward elbow then pop cleanly back to perimeter anchor
                    if ct >= 8 {
                        let (tx, ty) = bounds.clamp(px + attack_dir * 1.1, py * 0.65);
                        return (tx, ty, Some(PlayerActionState::Jog), 0.52, None);
                    } else {
                        return (anchor_xy.0, anchor_xy.1, None, 0.78, None);
                    }
                }
                4 | 5 => {
                    if let Some((_, _, s_xy)) = active_offball_screen {
                        let (_, rxy, rspd) = compute_offball_screen_read(
                            (px, py),
                            s_xy,
                            my_def_xy,
                            handler_xy,
                            attack_dir,
                            snap.attributes[AttrIdx::ThreePoint],
                            &self.geom,
                        );
                        return (rxy.0, rxy.1, Some(PlayerActionState::Jog), rspd, None);
                    }
                }
                _ => {}
            }
        }

        // Handler synchronized setup / rub-off when an active on-ball screen targets this handler
        if is_handler && in_frontcourt {
            if let Some((b_scr_p, b_tgt_p)) = active_ball_screen {
                if b_tgt_p == p_pos && akind != 5 {
                    let s_xy = off_xy[b_scr_p];
                    if libm::hypot(s_xy.0 - px, s_xy.1 - py) < 5.5 {
                        let s_sign = if s_xy.1 >= 0.0 { 1.0 } else { -1.0 };
                        let (raw_hx, raw_hy, spd) = if self.state.screen_ready_ticks[off_idx] < 2 {
                            // Calmly set up opposite the screen so screener reaches hip
                            (
                                s_xy.0 - attack_dir * 0.55,
                                s_xy.1 - s_sign * 1.05,
                                0.48,
                            )
                        } else if (handler_def_xy.1 - py) * s_sign > 0.35 {
                            // Reject screen when on-ball defender overplays screen side
                            (
                                px + attack_dir * 1.15,
                                py - s_sign * 1.15,
                                0.72,
                            )
                        } else {
                            // Tight rub off screener's outer shoulder
                            (
                                s_xy.0 + attack_dir * 0.55,
                                s_xy.1 + s_sign * 0.88,
                                0.68,
                            )
                        };
                        let (mut cx, cy) = bounds.clamp(raw_hx, raw_hy);
                        cx = if attack_dir > 0.0 { cx.max(1.35) } else { cx.min(-1.35) };
                        return (cx, cy, None, spd, None);
                    }
                }
            }
        }

        match akind {
            // SetScreen: screen directly on target's defender outer hip (never averaged with static anchor)
            2 => {
                let sdef_xy = (0..5).filter(|&i| def_active[i]).map(|i| def_xy[i])
                    .min_by(|a, b| {
                        let distance = |p: (f64, f64)| (p.0 - target_xy.0).powi(2) + (p.1 - target_xy.1).powi(2);
                        distance(*a).total_cmp(&distance(*b))
                    })
                    .unwrap_or((target_xy.0 + attack_dir * 0.85, target_xy.1 * 0.88));

                let dx_h = sdef_xy.0 - target_xy.0;
                let dy_h = sdef_xy.1 - target_xy.1;
                let dn = libm::hypot(dx_h, dy_h).max(0.25);
                let hip_side = if target_xy.1 >= 0.0 { 1.0 } else { -1.0 };
                let mut sx = sdef_xy.0 - (dx_h / dn) * 0.45;
                let mut sy = sdef_xy.1 + hip_side * 0.78;

                let is_second_stagger = (0..role_idx)
                    .any(|prev| phase.actions[prev].0 == 2 && phase.actions[prev].3 == act.3);
                if is_second_stagger {
                    let vx = anchor_xy.0 - sx;
                    let vy = anchor_xy.1 - sy;
                    let vn = libm::hypot(vx, vy).max(0.1);
                    sx += (vx / vn) * 1.25;
                    sy += (vy / vn) * 1.25;
                }
                let (mut cx, cy) = bounds.clamp(sx, sy);
                if in_frontcourt {
                    cx = if attack_dir > 0.0 { cx.max(1.35) } else { cx.min(-1.35) };
                }
                let dist_spot = libm::hypot(cx - px, cy - py);
                if dist_spot < 1.25 {
                    let hold_spd = if dist_spot < 0.70 { 0.0 } else { 0.50 * tempo };
                    (cx, cy, Some(PlayerActionState::SetScreen), hold_spd, None)
                } else {
                    let ov = if dist_spot > 3.6 {
                        Some(PlayerActionState::Sprint)
                    } else {
                        Some(PlayerActionState::Jog)
                    };
                    let app_spd = if dist_spot < 2.2 {
                        0.74 * tempo
                    } else {
                        0.82 * tempo
                    };
                    (cx, cy, ov, app_spd, None)
                }
            }
            // DHO: handler waits calmly at hub while cutter wraps tightly around ball shoulder
            5 => {
                if is_handler {
                    (anchor_xy.0, anchor_xy.1, None, 0.54 * tempo, None)
                } else {
                    let brush_y = handler_xy.1 + if handler_xy.1 >= 0.0 { 0.85 } else { -0.85 };
                    let (cx, cy) = bounds.clamp(handler_xy.0 - attack_dir * 0.25, brush_y);
                    (
                        cx * 0.75 + anchor_xy.0 * 0.25,
                        cy * 0.75 + anchor_xy.1 * 0.25,
                        Some(PlayerActionState::Jog),
                        0.74 * tempo,
                        None,
                    )
                }
            }
            // RollToRim: coverage-aware Short-Roll vs Blitz/Show, Pick & Pop, or curved Roll
            3 => {
                if matches!(observed_pnr_cov, Some(PnrCoverageKind::Blitz | PnrCoverageKind::HighShowAndRecover)) {
                    let (cx, cy) = bounds.clamp(rim_xy.0 - attack_dir * 4.4, 0.0);
                    (cx, cy, None, 0.78 * tempo, None)
                } else if snap.attributes[AttrIdx::ThreePoint] >= 80
                    && snap.tendencies.pnr_roll_vs_pop < 0.45
                    && (paint_occupied || observed_pnr_cov == Some(PnrCoverageKind::Drop))
                {
                    let pop_y = if py >= 0.0 { 3.4 } else { -3.4 };
                    let (cx, cy) = bounds.clamp(rim_xy.0 - attack_dir * 7.5, pop_y);
                    (cx, cy, None, 0.70 * tempo, None)
                } else {
                    let roll_y = if py >= 0.0 { 1.05 } else { -1.05 };
                    let (cx, cy) = bounds.clamp(rim_xy.0 - attack_dir * 1.65, roll_y);
                    (cx, cy, None, 0.84 * tempo, None)
                }
            }
            // Off-ball Cut / Curl / Flare
            4 => {
                if let Some((_, cut_p, s_xy)) = active_offball_screen {
                    if cut_p == p_pos {
                        let (rk, rxy, rspd) = compute_offball_screen_read(
                            (px, py),
                            s_xy,
                            my_def_xy,
                            handler_xy,
                            attack_dir,
                            snap.attributes[AttrIdx::ThreePoint],
                            &self.geom,
                        );
                        let cside = if py >= 0.0 { 1 } else { -1 };
                        return (rxy.0, rxy.1, Some(PlayerActionState::Jog), rspd, Some((rk, 11, cside)));
                    }
                }
                (anchor_xy.0, anchor_xy.1, None, 0.82 * tempo, None)
            }
            // DribbleHold / LowPost PostBackdown
            1 => {
                if act.1 == 6 {
                    if is_handler {
                        let (bx, by) =
                            bounds.clamp(rim_xy.0 - attack_dir * 2.15, (py * 0.65).clamp(-2.8, 2.8));
                        (bx, by, Some(PlayerActionState::PostBackdown), 0.50 * tempo, None)
                    } else {
                        let dist_post = libm::hypot(anchor_xy.0 - px, anchor_xy.1 - py);
                        let ov = if dist_post < 1.25 {
                            Some(PlayerActionState::PostBackdown)
                        } else {
                            None
                        };
                        (anchor_xy.0, anchor_xy.1, ov, 0.72 * tempo, None)
                    }
                } else {
                    (anchor_xy.0, anchor_xy.1, None, 0.54 * tempo, None)
                }
            }
            // SpaceAnchor + Drive & Reaction + Off-Ball Cuts + Continuous Spacing Potential Field
            _ => {
                if !is_handler {
                    if let Some((_, cut_p, s_xy)) = active_offball_screen {
                        if cut_p == p_pos {
                            let (rk, rxy, rspd) = compute_offball_screen_read(
                                (px, py),
                                s_xy,
                                my_def_xy,
                                handler_xy,
                                attack_dir,
                                snap.attributes[AttrIdx::ThreePoint],
                                &self.geom,
                            );
                            let cside = if py >= 0.0 { 1 } else { -1 };
                            return (rxy.0, rxy.1, Some(PlayerActionState::Jog), rspd, Some((rk, 11, cside)));
                        }
                    }
                    if handler_is_driving {
                        let d_my_rim = self.geom.dist_to_rim(my_def_xy.0, my_def_xy.1, attack_dir);
                        let (rx, ry, rspd) = compute_drive_and_react_relocation(
                            px,
                            py,
                            handler_xy,
                            (self.state.active_drive_target_xy[0], self.state.active_drive_target_xy[1]),
                            attack_dir,
                            snap.attributes[AttrIdx::ThreePoint],
                            d_my_rim,
                            &self.geom,
                        );
                        return (rx, ry, None, rspd, None);
                    }
                    if in_frontcourt && (px * attack_dir) > 3.0 && active_cutter_count == 0 {
                        let d_my = libm::hypot(my_def_xy.0 - px, my_def_xy.1 - py);
                        let d_ball_off = libm::hypot(handler_xy.0 - px, handler_xy.1 - py);
                        let d_ball_def = libm::hypot(handler_xy.0 - my_def_xy.0, handler_xy.1 - my_def_xy.1);
                        let cside = if py >= 0.0 { 1 } else { -1 };
                        let obm = snap.attributes[AttrIdx::OffBallMovement];
                        let iq = snap.attributes[AttrIdx::ShotSelectionIq];
                        if !paint_occupied
                            && d_my <= self.tunables.backdoor_cut_overplay_dist_m.min(1.65)
                            && d_ball_def + 0.85 < d_ball_off
                            && (obm as u16 + iq as u16) >= 145
                        {
                            let bd_y = if py >= 0.0 { 1.35 } else { -1.35 };
                            let (cx, cy) = bounds.clamp(rim_xy.0 - attack_dir * 1.75, bd_y);
                            return (cx, cy, None, 0.88, Some((2, 14, cside)));
                        }
                        if d_my < 1.25 && pr.feet_set_ticks >= 22 && obm >= 68 {
                            let (jx, jy) = bounds.clamp(px + attack_dir * 1.1, py * 0.65);
                            return (jx, jy, Some(PlayerActionState::Jog), 0.52, Some((3, 13, cside)));
                        }
                    }
                    if in_frontcourt {
                        let spread = 1.0 + (f64::from(self.teams[off_idx].tactics.default_spacing) - 75.0) * 0.004;
                        let spacing_anchor = (
                            handler_xy.0 + (anchor_xy.0 - handler_xy.0) * spread,
                            handler_xy.1 + (anchor_xy.1 - handler_xy.1) * spread,
                        );
                        let spaced = evaluate_spacing_potential_field(
                            p_pos,
                            snap,
                            spacing_anchor,
                            off_xy,
                            off_active,
                            def_xy,
                            def_active,
                            handler_xy,
                            handler_is_driving,
                            attack_dir,
                            &self.geom,
                            &self.tunables,
                        );
                        let d_sp = libm::hypot(spaced.0 - px, spaced.1 - py);
                        let sp_spd = if d_sp > 2.2 {
                            0.66 * tempo
                        } else if d_sp > 0.55 {
                            0.50 * tempo
                        } else {
                            0.0
                        };
                        return (spaced.0, spaced.1, None, sp_spd, None);
                    }
                }
                (anchor_xy.0, anchor_xy.1, None, (0.62 * tempo).clamp(0.46, 0.72), None)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_ball_handler_decision(
        &mut self,
        off_idx: usize,
        def_idx: usize,
        owner_slot: u8,
        owner_r: u8,
        ox: f64,
        oy: f64,
        attack_dir: f64,
    ) -> Result<(), RngBudgetExceeded> {
        let off_side = if off_idx == 0 {
            CommandSide::Home
        } else {
            CommandSide::Away
        };
        let rank = (owner_r as usize) - off_idx * 15;
        let shooter_snap = &self.teams[off_idx].players[rank];
        let cid = canonical_actor_id(self.canonical_team_ids[off_idx], rank);

        {
            let owner_p = &mut self.state.players[owner_r as usize];
            owner_p.feet_set_ticks = owner_p.feet_set_ticks.saturating_add(1);
            if owner_p.action_timer_ds > 0 {
                owner_p.action_timer_ds -= 1;
            }
        }

        // If defense has intentional_foul_mode active, check for tactical reach foul
        if self.state.team_states[def_idx].intentional_foul_mode
            && self.state.possession_tick >= 4
        {
            let def_r = self.state.team_states[def_idx].active_lineup_slots[0];
            let def_side = off_side.opponent();
            let rid = self.state.next_ruling_id;
            self.state.next_ruling_id += 1;
            let call = RuleCallSnapshot {
                rule_code: RuleCode::FOUL_REACH_COMMON,
                offending_side: def_side,
                offender_roster_idx: def_r,
                victim_roster_idx: Some(owner_r),
                ball_in_frontcourt_at_call: self.state.ball_in_frontcourt,
                interruption_spot: InboundSpot::NearestToInterruption,
                stoppage_sideline_side: CourtSideLine::from_y_m(oy),
                restart_shot_clock_ds: compute_shot_clock_reset(
                    ShotClockScenario::OffenseFrontcourtNonShotDefFoul,
                    self.state.shot_clock_ds,
                    self.state.game_clock_ds,
                    &self.ruleset,
                ),
                foul_category: 0,
                ..RuleCallSnapshot::default()
            };
            self.open_pending_ruling(rid, self.state.sim_tick, call, false);
            return Ok(());
        }

        let mut min_def_d = 9.0_f64;
        let mut second_def_d = 9.0_f64;
        let mut nearest_def_slot = (def_idx * 5) as u8;
        let mut nearest_def_r = self.state.team_states[def_idx].active_lineup_slots[0];
        for p_pos in 0..5 {
            let d_slot = (def_idx * 5 + p_pos) as u8;
            let dr = self.state.team_states[def_idx].active_lineup_slots[p_pos];
            if dr != ROSTER_IDX_NONE {
                let d = libm::hypot(
                    self.state.players[dr as usize].x_m - ox,
                    self.state.players[dr as usize].y_m - oy,
                );
                if d < min_def_d {
                    second_def_d = min_def_d;
                    min_def_d = d;
                    nearest_def_slot = d_slot;
                    nearest_def_r = dr;
                } else if d < second_def_d {
                    second_def_d = d;
                }
            }
        }
        let dist_rim = self.geom.dist_to_rim(ox, oy, attack_dir);
        let rim_help = if dist_rim <= 3.0 {
            (1.0 - second_def_d / 3.4).clamp(0.0, 1.0) * 0.35
        } else {
            0.0
        };
        let c_geom = ((1.0 - min_def_d / 2.6).clamp(0.0, 1.0) + rim_help).clamp(0.0, 1.0);

        // Continue an active physical drive smoothly to finish at rim/stepback, unless collapsed help forces kick-out read
        let drive_collapsed = self.state.active_drive_corridor <= 2
            && dist_rim < 5.5
            && second_def_d < 2.25;
        if self.state.players[owner_r as usize].action_state == PlayerActionState::Drive
            && self.state.players[owner_r as usize].action_timer_ds > 0
            && dist_rim > 2.30
            && !drive_collapsed
            && self.state.shot_clock_ds.map_or(true, |sc| sc > 18)
        {
            return Ok(());
        }

        let score_diff = (self.state.team_states[off_idx].score_on_court as i32)
            - (self.state.team_states[def_idx].score_on_court as i32);
        let endgame_wp = self.state.quarter >= 4
            && ((self.state.game_clock_ds <= self.tunables.wp_mode_max_game_clock_ds
                && score_diff.unsigned_abs()
                    <= (self.tunables.wp_mode_max_abs_score_diff as u32))
                || self.state.shot_clock_ds.is_none());

        let in_backcourt_phase = !self.state.ball_in_frontcourt || (ox * attack_dir) < 0.9;
        let handler_feet_ticks = self.state.players[owner_r as usize].feet_set_ticks;
        let last_passer_opt = self.state.ball.last_passer_roster_idx;
        let pass_age = self.state.sim_tick.saturating_sub(self.state.ball.last_pass_completed_tick);
        let sc_ds = self.state.shot_clock_ds.unwrap_or(self.state.game_clock_ds.min(240));
        let is_buzzer_situation = self.state.game_clock_ds <= 25 || sc_ds <= 25;
        let screener = self.playbooks.iter()
            .find(|play| play.play_id == self.state.active_play_id[off_idx])
            .and_then(|play| play.phases.get(usize::from(self.state.play_phase_idx[off_idx])))
            .and_then(|phase| phase.actions.iter().position(|action| action.0 == 2 || action.0 == 3))
            .and_then(|role| self.state.team_states[off_idx].active_lineup_slots
                .get(usize::from(self.state.slot_bindings[off_idx][role])))
            .copied().filter(|&r| r != ROSTER_IDX_NONE && r != owner_r);
        let observed_coverage = if in_backcourt_phase {
            None
        } else {
            screener.and_then(|r| {
                let screen = &self.state.players[usize::from(r)];
                let defenders = self.state.team_states[def_idx].active_lineup_slots.map(|d| {
                    self.state.players.get(usize::from(d)).map(|p| (p.x_m, p.y_m))
                });
                observe_pnr_coverage((ox, oy), (screen.x_m, screen.y_m), &defenders, attack_dir)
            })
        };

        // Hard double-team trap pressure -> Held Ball or occasional forced pickup (DribbleStatus::Gathered)
        if !in_backcourt_phase
            && min_def_d < 0.85
            && second_def_d < 1.15
            && handler_feet_ticks >= 12
            && handler_feet_ticks % 5 == 0
        {
            let u_trap = sample_u53_checked(
                self.philox_key,
                self.state.possession_id,
                self.state.possession_tick,
                &mut self.state.rng_sample_counters.0,
                cid,
                RngPurpose::ContactAndFoulRoll,
            )?;
            if u_trap < 0.009 {
                let rid = self.state.next_ruling_id;
                self.state.next_ruling_id += 1;
                self.state.oob_spot_xy = [ox, oy];
                self.state.oob_is_endline = false;
                let call = RuleCallSnapshot {
                    rule_code: RuleCode::HELD_BALL,
                    offending_side: off_side,
                    offender_roster_idx: owner_r,
                    victim_roster_idx: Some(nearest_def_r),
                    ball_in_frontcourt_at_call: self.state.ball_in_frontcourt,
                    interruption_spot: InboundSpot::NearestToInterruption,
                    stoppage_sideline_side: CourtSideLine::from_y_m(oy),
                    ..RuleCallSnapshot::default()
                };
                self.open_pending_ruling(rid, self.state.sim_tick, call, false);
                return Ok(());
            } else if u_trap < 0.14 {
                let owner_p = &mut self.state.players[owner_r as usize];
                if owner_p.footwork.dribble_status == DribbleStatus::Dribbling {
                    owner_p.footwork.dribble_status = DribbleStatus::Gathered;
                    owner_p.footwork.gather_tick = Some(self.state.sim_tick);
                    if let (Ok((qx, _)), Ok((qy, _))) = (
                        crate::wire_schema_v1::frames::quantize_mm(ox),
                        crate::wire_schema_v1::frames::quantize_mm(oy),
                    ) {
                        owner_p.footwork.pivot_pos_mm = Some((qx, qy));
                    }
                }
            }
        }

        // First-principles 1.2s human tactical cadence in live half-court play:
        // React immediately near rim (<=2.45m), at late clock (<=4.5s), when gathered, on initial catch read (tick 7), or every 1.2s.
        if !endgame_wp {
            let at_rim = dist_rim <= 2.45;
            let late_clock = sc_ds <= 45 || self.state.game_clock_ds <= 35;
            let gathered = self.state.players[owner_r as usize].footwork.dribble_status
                == DribbleStatus::Gathered;
            let cadence_hit = handler_feet_ticks == 7
                || (handler_feet_ticks > 7 && handler_feet_ticks % 12 == 0);
            if !at_rim && !late_clock && !cadence_hit && !gathered {
                return Ok(());
            }
        }

        // Gather active teammates
        let mut teammates = Vec::with_capacity(4);
        let mut off_team_eval = Vec::with_capacity(5);
        let mut handler_eval_idx = 0usize;
        for p_pos in 0..5 {
            let slot = (off_idx * 5 + p_pos) as u8;
            let r = self.state.team_states[off_idx].active_lineup_slots[p_pos];
            if r == ROSTER_IDX_NONE {
                continue;
            }
            let r_rank = (r as usize) - off_idx * 15;
            let t_snap = &self.teams[off_idx].players[r_rank];
            let t_cid = canonical_actor_id(self.canonical_team_ids[off_idx], r_rank);
            if slot == owner_slot {
                handler_eval_idx = off_team_eval.len();
            } else {
                teammates.push((slot, t_cid));
            }
            off_team_eval.push((
                slot,
                t_snap,
                self.state.players[r as usize].x_m,
                self.state.players[r as usize].y_m,
            ));
        }

        let legal = legal_actions_for_holder(
            &self.state.players[owner_r as usize].footwork,
            &teammates,
            true,
            self.state.shot_clock_ds,
            &self.tunables,
            endgame_wp,
            score_diff,
            self.state.shot_clock_rem_10us,
        );
        if legal.is_empty() {
            return Ok(());
        }

        let is_three = self.geom.is_three_pointer(ox, oy, attack_dir);
        let zone = self.geom.shot_zone(ox, oy, attack_dir);
        let fat_pen = compute_fatigue_penalty(
            self.state.players[owner_r as usize].cumulative_stamina,
            &self.tunables,
        );
        let def_rank = (nearest_def_r as usize) % 15;
        let def_snap = &self.teams[def_idx].players[def_rank];

        let mut shot_inp = ShotEvalInputs {
            zone,
            is_three,
            dist_to_rim_m: dist_rim,
            is_catch_and_shoot: self.state.players[owner_r as usize]
                .footwork
                .dribble_status
                == DribbleStatus::LiveTripleThreat,
            c_geom,
            backboard_angle_factor: (libm::fabs(oy) / 5.0).clamp(0.0, 1.0),
            fatigue_penalty: fat_pen,
            rhythm_score: self.state.players[owner_r as usize].rhythm_score,
            release_foul: false,
        };
        let shot_probs = compute_shot_probabilities(
            shooter_snap,
            &self.tunables,
            &self.badges,
            shot_inp,
            f64::from(def_snap.attributes[AttrIdx::InteriorDefense]),
            f64::from(def_snap.attributes[AttrIdx::Block]),
        );

        let epv_eval = EpvEvaluator {
            geom: &self.geom,
            tunables: &self.tunables,
            badges: &self.badges,
            shot_clock_ds: self.state.shot_clock_ds,
            attack_dir,
        };

        let mut scored: Vec<(CandidateAction, f64)> = Vec::with_capacity(legal.len());
        let def_c_geom_fn = |x: f64, y: f64| -> f64 {
            let mut bd = 8.0_f64;
            for p_pos in 0..5 {
                let dr = self.state.team_states[def_idx].active_lineup_slots[p_pos];
                if dr != ROSTER_IDX_NONE {
                    let d = libm::hypot(
                        self.state.players[dr as usize].x_m - x,
                        self.state.players[dr as usize].y_m - y,
                    );
                    if d < bd {
                        bd = d;
                    }
                }
            }
            (1.0 - bd / 2.6).clamp(0.0, 1.0)
        };

        if endgame_wp {
            let wp_ctx = EndgameWpContext {
                delta_s: score_diff as f64,
                gc_rem_10us: self.state.game_clock_rem_10us,
                sc_rem_10us: self.state.shot_clock_rem_10us,
                is_deadball_restart: false,
                opp_in_bonus_on_next_foul: is_team_in_bonus(
                    &self.state.team_states[def_idx], self.state.quarter, self.state.game_clock_ds,
                    &self.ruleset.fouls, 1,
                ),
                intentional_foul_policy: self.intentional_foul_policy,
                lambda_steal_per_ds: if min_def_d >= 2.0 {
                    0.0
                } else {
                    0.002 * (1.0 - min_def_d / 2.0)
                },
                lambda_foul_per_ds: if self.state.team_states[def_idx].intentional_foul_mode {
                    0.08
                } else {
                    0.0
                },
                p_ft: (0.50 + 0.42 * (f64::from(shooter_snap.attributes[AttrIdx::FreeThrow]) / 99.0))
                    .clamp(0.45, 0.95),
                p_orb: 0.24,
                p_orb_ft: 0.14,
                ruleset: &self.ruleset,
                tunables: &self.tunables,
            };
            for &act in &legal {
                let val = match act {
                    CandidateAction::Hold => evaluate_wp_hold(&wp_ctx).unwrap_or(0.0),
                    CandidateAction::Shoot => evaluate_wp_shoot(
                        &wp_ctx,
                        shot_probs,
                        if is_three { 3 } else { 2 },
                        self.tunables.shot_release_duration_ds,
                        15,
                    ),
                    CandidateAction::Pass { .. } => evaluate_wp_hold(&wp_ctx).unwrap_or(0.5) * 0.98,
                    CandidateAction::Drive { corridor } => {
                        let shoot_val = if corridor == 3 { 3 } else { 2 };
                        evaluate_wp_shoot(&wp_ctx, shot_probs, shoot_val, 8, 16) * 0.96
                    }
                    CandidateAction::Pivot { .. } | CandidateAction::FollowPlay => {
                        evaluate_wp_hold(&wp_ctx).unwrap_or(0.4) * 0.94
                    }
                };
                scored.push((act, val));
            }
        } else {
            let mut pass_cache = [f64::NEG_INFINITY; 5];
            let mut best_pass_all = f64::NEG_INFINITY;
            for (j, _) in off_team_eval.iter().enumerate() {
                if j == handler_eval_idx {
                    continue;
                }
                let pv = epv_eval.epv_pass(
                    handler_eval_idx,
                    j,
                    2,
                    &off_team_eval,
                    &def_c_geom_fn,
                );
                pass_cache[j] = pv;
                if pv > best_pass_all {
                    best_pass_all = pv;
                }
            }

            // Pace half-court set execution scaled by active_pace, transition window, and playbook triggers
            let handler_pos_idx = (owner_slot as usize) % 5;
            let handler_role_idx = self.state.slot_bindings[off_idx]
                .iter()
                .position(|&s| s as usize == handler_pos_idx)
                .unwrap_or(handler_pos_idx)
                .min(4);
            let active_pb = self
                .playbooks
                .iter()
                .find(|p| p.play_id == self.state.active_play_id[off_idx]);
            let active_phase = active_pb.and_then(|pb| {
                let idx = (self.state.play_phase_idx[off_idx] as usize)
                    .min(pb.phases.len().saturating_sub(1));
                pb.phases.get(idx)
            });
            let handler_act = active_phase.map(|ph| ph.actions[handler_role_idx]);
            let in_transition = !in_backcourt_phase && self.state.transition_window_ds[off_idx] > 0;
            let has_screen_advantage =
                !in_backcourt_phase && self.state.play_screen_contact_ds[off_idx] >= 2;
            let screen_ready =
                !in_backcourt_phase && self.state.screen_ready_ticks[off_idx] >= 2;
            let active_pnr_cov = if screen_ready || has_screen_advantage {
                observed_coverage.unwrap_or(PnrCoverageKind::Drop)
            } else {
                PnrCoverageKind::Drop
            };
            let is_post_handler = handler_act.is_some_and(|a| a.0 == 1 && a.1 == 6);
            let paint_urgent = self.state.players[owner_r as usize].paint_timer_ds >= 18;
            let is_active_drive =
                self.state.players[owner_r as usize].action_state == PlayerActionState::Drive;

            let pace_inv = 1.45 - 0.006 * f64::from(self.state.active_pace[off_idx]);
            let set_play_rhythm = if in_backcourt_phase {
                1.62
            } else if in_transition {
                if dist_rim <= 4.4 || (c_geom < 0.22 && dist_rim <= 8.2) {
                    0.58
                } else {
                    1.02
                }
            } else if sc_ds > 140 {
                1.44 * pace_inv
            } else if sc_ds > 85 {
                1.08 * pace_inv
            } else if sc_ds > 50 {
                0.84
            } else {
                0.42
            };

            for &act in &legal {
                let mut val = match act {
                    CandidateAction::Shoot => {
                        let base = epv_eval.epv_shoot_at(
                            shooter_snap,
                            ox,
                            oy,
                            c_geom,
                            f64::from(def_snap.attributes[AttrIdx::InteriorDefense]),
                            f64::from(def_snap.attributes[AttrIdx::Block]),
                            fat_pen,
                            self.state.players[owner_r as usize].rhythm_score,
                            0.24,
                            0.14,
                        );
                        let urgent = self.state.shot_clock_ds.map_or(0.0, |sc| {
                            if sc <= 50 {
                                1.20 * (1.0 - (sc as f64) / 50.0) + 0.22
                            } else if sc <= self.tunables.epv_urgent_shot_clock_ds {
                                self.tunables.epv_urgent_shot_bonus_max
                                    * (1.0 - (sc as f64) / (self.tunables.epv_urgent_shot_clock_ds.max(1) as f64))
                            } else {
                                0.0
                            }
                        });
                        let rim_bonus = if dist_rim <= 2.5 {
                            0.45
                        } else if c_geom < 0.25 && dist_rim <= 7.8 && !in_backcourt_phase && sc_ds <= 135 {
                            0.08
                        } else {
                            0.0
                        };
                        let screen_open_bonus = if !in_backcourt_phase
                            && c_geom < 0.28
                            && (has_screen_advantage || screen_ready || pass_age <= 14)
                        {
                            if (has_screen_advantage || screen_ready)
                                && active_pnr_cov == PnrCoverageKind::Drop
                                && shooter_snap.attributes[AttrIdx::ThreePoint] >= 80
                            {
                                0.36
                            } else {
                                0.30
                            }
                        } else {
                            0.0
                        };
                        let paint_clock_bonus = if paint_urgent { 0.28 } else { 0.0 };
                        let early_discipline = if in_backcourt_phase && !is_buzzer_situation {
                            -2.5
                        } else if sc_ds > 140
                            && dist_rim > 2.5
                            && !in_transition
                            && !has_screen_advantage
                            && !screen_ready
                        {
                            -0.42
                        } else if sc_ds > 170 && dist_rim <= 2.5 && c_geom > 0.25 {
                            -0.20
                        } else if is_three && c_geom > 0.30 && sc_ds > 45 {
                            -0.18
                        } else {
                            0.0
                        };
                        base + urgent + rim_bonus + screen_open_bonus + paint_clock_bonus + early_discipline
                    }
                    CandidateAction::Pass { target_slot, .. } => {
                        let t_idx = off_team_eval
                            .iter()
                            .position(|&(s, _, _, _)| s == target_slot)
                            .unwrap_or(0);
                        if t_idx == handler_eval_idx {
                            -1.0
                        } else {
                            let target_pos = (target_slot as usize) % 5;
                            let target_x = off_team_eval[t_idx].2;
                            let target_y = off_team_eval[t_idx].3;
                            let target_r = self.state.team_states[off_idx].active_lineup_slots
                                [target_pos];
                            let target_c = def_c_geom_fn(target_x, target_y);
                            let target_dist_rim = self.geom.dist_to_rim(target_x, target_y, attack_dir);
                            let target_role_idx = self.state.slot_bindings[off_idx]
                                .iter()
                                .position(|&s| s as usize == target_pos)
                                .unwrap_or(target_pos)
                                .min(4);
                            let target_act = active_phase.map(|ph| ph.actions[target_role_idx]);
                            let mut v = pass_cache[t_idx];
                            if target_r != ROSTER_IDX_NONE {
                                let tp = &self.state.players[target_r as usize];
                                if tp.oob_reentry_pending
                                    || self.geom.half_length_m - libm::fabs(tp.x_m) < 0.40
                                    || self.geom.half_width_m - libm::fabs(tp.y_m) < 0.40
                                {
                                    v -= 5.0;
                                }
                            }
                            if handler_feet_ticks < 6 {
                                v -= 0.26;
                            }
                            if last_passer_opt == Some(target_r) && pass_age < 16 {
                                v -= 0.35;
                            }
                            if in_backcourt_phase {
                                if (target_x - ox) * attack_dir > 3.0 && handler_feet_ticks >= 4 {
                                    v += if in_transition { 0.12 } else { -0.05 };
                                } else {
                                    v -= 0.85;
                                }
                            } else {
                                if self.state.ball_in_frontcourt && (target_x * attack_dir) <= 0.35 {
                                    v -= 6.0;
                                }
                                if (target_x * attack_dir) > 0.5 && sc_ds > 45 {
                                    v += 0.04;
                                }
                                if in_transition
                                    && (target_x - ox) * attack_dir > 3.5
                                    && target_c < 0.35
                                {
                                    v += 0.35;
                                }
                                if let Some(ph) = active_phase {
                                    if ph.exit_trigger_kind == 2
                                        && handler_role_idx != (ph.exit_target_role as usize).min(4)
                                        && target_role_idx == (ph.exit_target_role as usize).min(4)
                                        && handler_feet_ticks >= 5
                                        && (target_x * attack_dir) > 0.5
                                    {
                                        v += 0.32;
                                    }
                                    if ph.actions.iter().any(|a| {
                                        a.0 == 2 && (a.3 as usize).min(4) == target_role_idx
                                    }) && target_c < 0.26
                                    {
                                        v += 0.30;
                                    }
                                }
                                // DHO hand-off bonus when cutter wraps tightly past hub
                                if let Some(ha) = handler_act {
                                    let pass_dist = libm::hypot(target_x - ox, target_y - oy);
                                    if ha.0 == 5
                                        && (ha.3 as usize).min(4) == target_role_idx
                                        && pass_dist <= 2.45
                                    {
                                        v += 0.48;
                                    }
                                }
                                // Pocket/lob pass to active rim roller or backdoor cutter
                                if target_act.is_some_and(|ta| ta.0 == 3 || ta.0 == 4)
                                    && target_dist_rim < 4.5
                                    && target_c < 0.38
                                {
                                    v += 0.38;
                                }
                                // Blitz/Show short-roll outlet to screener/roll man
                                if (screen_ready || has_screen_advantage)
                                    && matches!(
                                        active_pnr_cov,
                                        PnrCoverageKind::Blitz
                                            | PnrCoverageKind::HighShowAndRecover
                                    )
                                    && (target_act.is_some_and(|ta| ta.0 == 3 || ta.0 == 2)
                                        || (target_dist_rim < 5.8 && target_y.abs() < 3.4))
                                    && target_c < 0.42
                                {
                                    v += 0.42;
                                }
                                // Open off-ball cutter read (Pass&Cut, Backdoor, Curl, Flare)
                                if target_r != ROSTER_IDX_NONE
                                    && matches!(
                                        self.state.players[target_r as usize].offball_cut_kind,
                                        1 | 2 | 4 | 5
                                    )
                                    && target_c < 0.34
                                {
                                    v += 0.36;
                                }
                                // Drive-and-kick / Post kick-out to open 3PT shooter
                                if (is_active_drive || is_post_handler || paint_urgent)
                                    && target_c < 0.25
                                    && self.geom.is_three_pointer(target_x, target_y, attack_dir)
                                {
                                    v += 0.35;
                                }
                            }
                            v
                        }
                    }
                    CandidateAction::Drive { corridor } => {
                        let mut dv = epv_eval.epv_drive_with_cached_best_pass(
                            handler_eval_idx,
                            corridor,
                            2,
                            &off_team_eval,
                            &def_c_geom_fn,
                            if best_pass_all.is_finite() { Some(best_pass_all) } else { None },
                        );
                        if in_backcourt_phase {
                            dv -= 0.45;
                        } else if sc_ds > 145
                            && c_geom >= 0.12
                            && !in_transition
                            && !has_screen_advantage
                            && !screen_ready
                        {
                            dv -= 0.22;
                        } else if handler_feet_ticks < 5 {
                            dv -= 0.16;
                        } else {
                            if dist_rim > 2.8 && dist_rim < 10.5 && corridor <= 2 && sc_ds <= 135 {
                                dv += 0.20;
                            }
                            if in_transition && corridor <= 2 && dist_rim > 2.6 {
                                dv += 0.26;
                            }
                            let screen_adv = has_screen_advantage || screen_ready;
                            if screen_adv && corridor <= 2 {
                                dv += if matches!(
                                    active_pnr_cov,
                                    PnrCoverageKind::Drop | PnrCoverageKind::Switch | PnrCoverageKind::Ice
                                ) {
                                    0.26
                                } else {
                                    0.20
                                };
                            } else if screen_adv
                                && corridor == 3
                                && shooter_snap.attributes[AttrIdx::ThreePoint] >= 80
                            {
                                dv += 0.18;
                            }
                            if is_post_handler && corridor <= 2 {
                                dv += 0.25;
                            }
                        }
                        dv
                    }
                    CandidateAction::Pivot { .. } => {
                        epv_eval.epv_hold(0.004) - self.tunables.epv_pivot_delay_cost - 0.12
                    }
                    CandidateAction::FollowPlay => {
                        if !in_backcourt_phase
                            && (screen_ready
                                || active_phase.is_some_and(|ph| {
                                    ph.exit_trigger_kind == 2
                                        && handler_role_idx
                                            != (ph.exit_target_role as usize).min(4)
                                }))
                        {
                            set_play_rhythm - 0.18
                        } else {
                            set_play_rhythm
                        }
                    }
                    CandidateAction::Hold => epv_eval.epv_hold(0.004).max(set_play_rhythm - 0.22),
                };
                if matches!(act, CandidateAction::Shoot) {
                    let iq = f64::from(shooter_snap.attributes[AttrIdx::ShotSelectionIq]) / 99.0;
                    val -= self.tunables.epv_bad_shot_iq_penalty_scale * (1.0 - iq) * c_geom;
                }
                scored.push((act, val));
            }
        }

        if !endgame_wp {
            let epv = scored.iter().map(|(_, value)| *value).fold(f64::NEG_INFINITY, f64::max);
            self.transient_epv_centi[usize::from(owner_slot)] =
                (100.0 * epv).round().clamp(-32767.0, 32767.0) as i16;
        }

        // Select action (argmax when late WP mode, else softmax via RngPurpose::DecisionSoftmax)
        let chosen = if endgame_wp
            && self.state.game_clock_ds <= self.tunables.wp_argmax_clock_threshold_ds
        {
            let mut best = scored[0];
            for &item in scored.iter().skip(1) {
                if item.1 > best.1
                    || (item.1.to_bits() == best.1.to_bits()
                        && item.0.order_key() < best.0.order_key())
                {
                    best = item;
                }
            }
            best.0
        } else {
            let temp = self.tunables.decision_softmax_temp_base.max(0.02);
            let max_v = scored
                .iter()
                .map(|x| x.1)
                .fold(f64::NEG_INFINITY, f64::max);
            let mut weights = Vec::with_capacity(scored.len());
            let mut sum_w = 0.0;
            for &(act, v) in &scored {
                let w = if endgame_wp && v < max_v - self.tunables.wp_prune_delta {
                    0.0
                } else {
                    libm::exp(((v - max_v) / temp).clamp(-20.0, 0.0))
                };
                weights.push((act, w));
                sum_w += w;
            }
            let u = sample_u53_checked(
                self.philox_key,
                self.state.possession_id,
                self.state.possession_tick,
                &mut self.state.rng_sample_counters.0,
                cid,
                RngPurpose::DecisionSoftmax,
            )?;
            let mut pick = weights[0].0;
            if sum_w > 0.0 {
                let target = u * sum_w;
                let mut acc = 0.0;
                for &(act, w) in &weights {
                    acc += w;
                    if target <= acc {
                        pick = act;
                        break;
                    }
                }
            }
            pick
        };

        // Precompute scalars needed across match arms to keep borrow checker clean
        let def_steal_attr = f64::from(def_snap.attributes[AttrIdx::Steal]);
        let handler_bh_attr = f64::from(shooter_snap.attributes[AttrIdx::BallHandle]);
        let shooter_speed_attr = f64::from(shooter_snap.attributes[AttrIdx::TopSpeed]);
        let shooter_vert_attr = f64::from(shooter_snap.attributes[AttrIdx::Vertical]);
        let shooter_dunk_attr = shooter_snap.attributes[AttrIdx::Dunk];
        let def_vert_attr = f64::from(def_snap.attributes[AttrIdx::Vertical]);

        // Execute chosen action cleanly without mid-court teleports
        match chosen {
            CandidateAction::Hold | CandidateAction::FollowPlay | CandidateAction::Pivot { .. } => {
                if self.state.players[owner_r as usize].footwork.dribble_status
                    == DribbleStatus::LiveTripleThreat
                {
                    self.state.players[owner_r as usize].footwork.dribble_status =
                        DribbleStatus::Dribbling;
                }
            }
            CandidateAction::Drive { corridor } => {
                // On-ball strip turnover or reach-in foul risk during frontcourt drive
                if !in_backcourt_phase && min_def_d < 1.5 {
                    let ball_handle = handler_bh_attr / 99.0;
                    let p_strip = (0.025 + 0.030 * (def_steal_attr / 99.0) * (1.0 - ball_handle * 0.5))
                        .clamp(0.01, 0.055);
                    let p_reach = (0.052 + 0.038 * (1.0 - def_steal_attr / 99.0)).clamp(0.032, 0.095);
                    let u_strip = sample_u53_checked(
                        self.philox_key,
                        self.state.possession_id,
                        self.state.possession_tick,
                        &mut self.state.rng_sample_counters.0,
                        cid,
                        RngPurpose::StealRoll,
                    )?;
                    if u_strip < p_strip {
                        self.push_event(GameRecordEventPayload::TurnoverCommitted {
                            offender_roster_idx: owner_r,
                            team_side: off_idx as u8,
                            turnover_rule_code: RuleCode::NONE as u16,
                            stealer_roster_idx: nearest_def_r,
                            is_live_steal: 1,
                        });
                        let def_side = off_side.opponent();
                        self.state.possession_side = Some(def_side);
                        self.reset_possession_epoch();
                        self.record_defense_possession_outcome(def_idx, off_idx, 0);
                        self.start_offensive_possession(def_idx, false, true);
                        let sc = compute_shot_clock_reset(
                            ShotClockScenario::LiveStealOrDefRebound,
                            self.state.shot_clock_ds,
                            self.state.game_clock_ds,
                            &self.ruleset,
                        );
                        let gc_ds = self.state.game_clock_ds;
                        set_clocks_exact_ds(&mut self.state, gc_ds, sc);
                        self.state.shot_clock_running = sc.is_some();
                        self.state.ball.ball_state = BallState::Held;
                        self.state.ball.owner_slot = Some(nearest_def_slot);
                        self.state.ball.last_touched_by_slot = Some(nearest_def_slot);
                        self.state.ball.last_team_control_side = Some(def_side);
                        let def_attack_dir = if def_idx == 0 { 1.0 } else { -1.0 };
                        let (dx, dy) = if nearest_def_r != ROSTER_IDX_NONE {
                            let dp = &mut self.state.players[nearest_def_r as usize];
                            dp.action_state = PlayerActionState::Catch;
                            dp.action_timer_ds = 4;
                            dp.feet_set_ticks = 0;
                            dp.footwork = FootworkRecord {
                                dribble_status: DribbleStatus::LiveTripleThreat,
                                ..FootworkRecord::default()
                            };
                            (dp.x_m, dp.y_m)
                        } else {
                            (ox, oy)
                        };
                        self.state.ball.position_m = [dx, dy, 1.25];
                        self.state.ball_in_frontcourt = (dx * def_attack_dir) > 0.0;
                        self.state.backcourt_8s_timer_ds = 0;
                        self.push_event(GameRecordEventPayload::PossessionEstablished {
                            possession_id: self.state.possession_id,
                            side: def_idx as u8,
                            handler_roster_idx: nearest_def_r,
                            trigger_kind: 3,
                            new_shot_clock_ds: encode_shot_clock_opt(self.state.shot_clock_ds),
                        });
                        return Ok(());
                    } else if u_strip < p_strip + p_reach {
                        let rid = self.state.next_ruling_id;
                        self.state.next_ruling_id += 1;
                        self.state.oob_spot_xy = [ox, oy];
                        self.state.oob_is_endline = false;
                        let call = RuleCallSnapshot {
                            rule_code: RuleCode::FOUL_REACH_COMMON,
                            offending_side: off_side.opponent(),
                            offender_roster_idx: nearest_def_r,
                            victim_roster_idx: Some(owner_r),
                            ball_in_frontcourt_at_call: true,
                            interruption_spot: InboundSpot::NearestToInterruption,
                            stoppage_sideline_side: CourtSideLine::from_y_m(oy),
                            restart_shot_clock_ds: compute_shot_clock_reset(
                                ShotClockScenario::OffenseFrontcourtNonShotDefFoul,
                                self.state.shot_clock_ds,
                                self.state.game_clock_ds,
                                &self.ruleset,
                            ),
                            foul_category: 0,
                            ..RuleCallSnapshot::default()
                        };
                        self.open_pending_ruling(rid, self.state.sim_tick, call, false);
                        return Ok(());
                    }
                }

                self.state.active_drive_corridor = corridor;
                self.state.players[owner_r as usize].footwork.dribble_status =
                    DribbleStatus::Dribbling;
                self.state.players[owner_r as usize].action_state = PlayerActionState::Drive;
                self.state.players[owner_r as usize].action_timer_ds = 10;

                let handler_pos_idx = (owner_slot as usize) % 5;
                let valid_scr_r = screener.unwrap_or(owner_r);
                let cov = observed_coverage.unwrap_or(PnrCoverageKind::Drop);

                // Steer initial velocity cleanly off screen / toward driving corridor
                let rim_xy = self.geom.rim_xy(attack_dir);
                let (raw_tx, raw_ty) = if self.state.ball_in_frontcourt
                    && self.state.screen_ready_ticks[off_idx] >= 2
                    && corridor <= 2
                    && valid_scr_r != owner_r
                {
                    let sx = self.state.players[valid_scr_r as usize].x_m;
                    let sy = self.state.players[valid_scr_r as usize].y_m;
                    let scr_dist = libm::hypot(sx - ox, sy - oy);
                    match cov {
                        PnrCoverageKind::Switch => {
                            self.state.pnr_read_branch[off_idx] = 5;
                            (rim_xy.0 - attack_dir * 1.35, oy * 0.65)
                        }
                        PnrCoverageKind::Ice => {
                            self.state.pnr_read_branch[off_idx] = 2;
                            (rim_xy.0 - attack_dir * 1.65, oy * 0.55)
                        }
                        _ => {
                            self.state.pnr_read_branch[off_idx] = 1;
                            if scr_dist > 1.35 {
                                (
                                    sx + attack_dir * 1.15,
                                    if oy.abs() > 1.5 { oy * 0.55 } else { sy * 0.7 },
                                )
                            } else {
                                (rim_xy.0 - attack_dir * 2.1, oy * 0.42)
                            }
                        }
                    }
                } else {
                    match corridor {
                        0 => (rim_xy.0 - attack_dir * 1.15, (oy + 1.4).clamp(-1.8, 2.3)),
                        1 => (rim_xy.0 - attack_dir * 1.15, (oy - 1.4).clamp(-2.3, 1.8)),
                        2 => (rim_xy.0 - attack_dir * 1.05, oy * 0.35),
                        _ => (
                            ox - attack_dir * 1.5,
                            oy + if oy >= 0.0 { 0.85 } else { -0.85 },
                        ),
                    }
                };
                let (mut tx, ty) = self.geom.inbounds_nav_bounds(0.85).clamp(raw_tx, raw_ty);
                if self.state.ball_in_frontcourt {
                    tx = if attack_dir > 0.0 { tx.max(1.35) } else { tx.min(-1.35) };
                }
                self.state.active_drive_target_xy = [tx, ty];
                let max_spd = self.tunables.max_offball_speed_min_max_m_s[0]
                    + (shooter_speed_attr / 99.0)
                        * (self.tunables.max_offball_speed_min_max_m_s[1]
                            - self.tunables.max_offball_speed_min_max_m_s[0]);
                let dir_x = tx - ox;
                let dir_y = ty - oy;
                let dn = libm::hypot(dir_x, dir_y).max(0.1);
                self.state.players[owner_r as usize].vx_m_s = (dir_x / dn) * (max_spd * 0.72);
                self.state.players[owner_r as usize].vy_m_s = (dir_y / dn) * (max_spd * 0.72);

                // Log PnR audit event with actual screener & coverage context
                if self.state.ball_in_frontcourt
                    && (self.state.play_screen_contact_ds[off_idx] >= 2
                        || self.state.screen_ready_ticks[off_idx] >= 2
                        || (corridor <= 1 && self.state.possession_tick % 10 == 0))
                {
                    self.pending_audit.pnr_events.push(PnrAuditEvent {
                        tick: self.state.sim_tick,
                        possession_id: self.state.possession_id,
                        offense_side: off_side,
                        coverage: cov,
                        handler_pullup_3: false,
                        passed_out_of_blitz: false,
                        roller_drew_help: second_def_d < 2.6,
                        extra_pass_to_corner: false,
                        open_corner_3_attempt: false,
                        handler_tov: false,
                        pre_switch_triggered: cov == PnrCoverageKind::Switch,
                        big_forced_iso: handler_pos_idx >= 3 && min_def_d < 2.0,
                    });
                }
            }
            CandidateAction::Pass { target_slot, .. } => {
                let target_team = (target_slot as usize) / 5;
                let target_r = self.state.team_states[target_team].active_lineup_slots
                    [(target_slot as usize) % 5];
                let tx = self.state.players[target_r as usize].x_m;
                let ty = self.state.players[target_r as usize].y_m;
                let dist = libm::hypot(tx - ox, ty - oy);
                let spd = self.tunables.pass_speed_min_max[0];
                let flight_ticks = ((dist / spd) * 10.0).round().clamp(2.0, 9.0) as u16;

                // Evaluate passing lane bobble risk against closest defender to pass segment
                let mut best_lane_d = 9.0_f64;
                let mut lane_def_r = self.state.team_states[def_idx].active_lineup_slots[0];
                for p_pos in 0..5 {
                    let dr = self.state.team_states[def_idx].active_lineup_slots[p_pos];
                    if dr != ROSTER_IDX_NONE {
                        let dp = &self.state.players[dr as usize];
                        let d_seg = Self::point_to_segment_dist(dp.x_m, dp.y_m, ox, oy, tx, ty);
                        if d_seg < best_lane_d {
                            best_lane_d = d_seg;
                            lane_def_r = dr;
                        }
                    }
                }
                let lane_def_snap = &self.teams[def_idx].players[(lane_def_r as usize) % 15];
                let (_, p_defl) = crate::resolve::compute_pass_intercept_prob(
                    shooter_snap,
                    lane_def_snap,
                    best_lane_d,
                    1.55,
                    &self.tunables,
                );
                let u_steal = sample_u53_checked(
                    self.philox_key,
                    self.state.possession_id,
                    self.state.possession_tick,
                    &mut self.state.rng_sample_counters.0,
                    cid,
                    RngPurpose::StealRoll,
                )?;
                let intercepted_by = None;
                let will_bobble = u_steal < p_defl * 0.28;

                let handler_pos_idx = (owner_slot as usize) % 5;
                let handler_role_idx = self.state.slot_bindings[off_idx]
                    .iter()
                    .position(|&s| s as usize == handler_pos_idx)
                    .unwrap_or(handler_pos_idx)
                    .min(4);
                let target_pos_idx = (target_slot as usize) % 5;
                let target_role_idx = self.state.slot_bindings[off_idx]
                    .iter()
                    .position(|&s| s as usize == target_pos_idx)
                    .unwrap_or(target_pos_idx)
                    .min(4);
                let is_dho_pass = !in_backcourt_phase
                    && dist <= 2.45
                    && self
                        .playbooks
                        .iter()
                        .find(|p| p.play_id == self.state.active_play_id[off_idx])
                        .and_then(|pb| {
                            let idx = (self.state.play_phase_idx[off_idx] as usize)
                                .min(pb.phases.len().saturating_sub(1));
                            pb.phases.get(idx)
                        })
                        .is_some_and(|ph| {
                            let a = ph.actions[handler_role_idx];
                            a.0 == 5 && (a.3 as usize).min(4) == target_role_idx
                        });
                let pass_kind_u8 = if is_dho_pass { 1 } else { 0 };
                let final_flight_ticks = if is_dho_pass { 2 } else { flight_ticks };
                let is_extra = self.state.ball.last_passer_roster_idx.is_some() && pass_age <= 12;

                // Activate weak-side X-Out rotation on penetration/PnR kickout to corner
                if self.state.ball_in_frontcourt
                    && (self.geom.dist_to_rim(ox, oy, attack_dir) < 5.8
                        || self.state.screen_ready_ticks[off_idx] >= 2)
                {
                    let tgt_rim_d = self.geom.dist_to_rim(tx, ty, attack_dir);
                    if tgt_rim_d > 5.8 && ty.abs() > 4.8 && (ty * oy < 0.0 || oy.abs() < 2.2) {
                        let on_ball_dp = (nearest_def_r != ROSTER_IDX_NONE)
                            .then_some(usize::from(nearest_def_slot) % 5);
                        let mut best_dp = None;
                        let mut best_d = 6.5_f64;
                        for dp in 0..5 {
                            if Some(dp) == on_ball_dp {
                                continue;
                            }
                            let dr = self.state.team_states[def_idx].active_lineup_slots[dp];
                            if dr == ROSTER_IDX_NONE {
                                continue;
                            }
                            let d_corner = libm::hypot(
                                self.state.players[dr as usize].x_m - tx,
                                self.state.players[dr as usize].y_m - ty,
                            );
                            if d_corner > 1.6 && d_corner < best_d {
                                best_d = d_corner;
                                best_dp = Some(dp);
                            }
                        }
                        if let Some(rot_dp) = best_dp {
                            self.transient_debug_flags[def_idx * 5 + rot_dp] |= 1 << 4;
                            self.state.xout_rotator_slot[def_idx] = (def_idx * 5 + rot_dp) as u8;
                            self.state.xout_timer_ds[def_idx] = 14;
                        }
                    }
                    if self.state.screen_ready_ticks[off_idx] >= 2 {
                        let cov = self.audit_defensive_coverage(def_idx);
                        if matches!(
                            cov,
                            PnrCoverageKind::Blitz | PnrCoverageKind::HighShowAndRecover
                        ) {
                            self.pending_audit.pnr_events.push(PnrAuditEvent {
                                tick: self.state.sim_tick,
                                possession_id: self.state.possession_id,
                                offense_side: off_side,
                                coverage: cov,
                                handler_pullup_3: false,
                                passed_out_of_blitz: true,
                                roller_drew_help: second_def_d < 2.8,
                                extra_pass_to_corner: is_extra && ty.abs() > 5.2,
                                open_corner_3_attempt: false,
                                handler_tov: false,
                                pre_switch_triggered: false,
                                big_forced_iso: false,
                            });
                        }
                    }
                }

                self.state.players[owner_r as usize].action_state = PlayerActionState::Pass;
                self.state.players[owner_r as usize].facing_rad = libm::atan2(ty - oy, tx - ox);
                self.state.ball.ball_state = BallState::PassFlight;
                self.state.ball.owner_slot = None;
                self.state.ball.pass_ctx = Some(PassFlightContext {
                    passer_slot: owner_slot,
                    target_slot,
                    pass_kind: pass_kind_u8,
                    is_extra_pass: is_extra,
                    ticks_elapsed: 0,
                    total_flight_ticks: final_flight_ticks,
                    start_pos_m: [ox, oy, 1.4],
                    target_pos_m: [tx, ty, 1.35],
                    intercepted_by,
                    will_bobble,
                });
                self.push_event(GameRecordEventPayload::PassReleased {
                    passer_roster_idx: owner_r,
                    target_roster_idx: target_r,
                    pass_kind: pass_kind_u8,
                    is_extra_pass: if is_extra { 1 } else { 0 },
                    flight_ticks: final_flight_ticks,
                });
            }
            CandidateAction::Shoot => {
                // Check 0.3s rule
                if self.state.ball.live_ball_03s_restricted && dist_rim > 1.8 {
                    self.trigger_turnover_stoppage(RuleCode::VIO_03S_EXPIRED, off_side, owner_r);
                    return Ok(());
                }

                // Check shooting foul BEFORE sampling ShotOutcome (uses ContactAndFoulRoll budget = 64)
                let p_foul = if zone == 0 {
                    ((shot_probs.p_and1 + shot_probs.p_foul_miss) * 1.45).clamp(0.0, 0.42)
                } else {
                    (shot_probs.p_and1 + shot_probs.p_foul_miss).clamp(0.0, 0.42)
                };
                if ((zone == 0 && min_def_d < 2.2) || (c_geom > 0.16 && min_def_d < 1.95))
                    && p_foul > 0.001
                {
                    let u_foul = sample_u53_checked(
                        self.philox_key,
                        self.state.possession_id,
                        self.state.possession_tick,
                        &mut self.state.rng_sample_counters.0,
                        cid,
                        RngPurpose::ContactAndFoulRoll,
                    )?;
                    if u_foul < p_foul {
                        shot_inp.release_foul = true;
                    }
                }

                let (outcome, _) = sample_intended_shot_outcome(
                    self.philox_key,
                    self.state.possession_id,
                    self.state.possession_tick,
                    &mut self.state.rng_sample_counters.0,
                    cid,
                    shot_probs,
                    &self.tunables,
                    shot_inp,
                )?;

                let shot_id = self.state.next_shot_id;
                self.state.next_shot_id += 1;
                let mut called_three = is_three;
                if self.ruleset.challenges.mode == 2 {
                    let rim = self.geom.rim_xy(attack_dir);
                    let dx = ox - rim.0;
                    let dy = oy - rim.1;
                    let radial = libm::hypot(dx, dy);
                    if radial > 0.0 {
                        let toward_rim = self.geom.is_three_pointer(
                            ox - 0.08 * dx / radial, oy - 0.08 * dy / radial, attack_dir,
                        );
                        let away_from_rim = self.geom.is_three_pointer(
                            ox + 0.08 * dx / radial, oy + 0.08 * dy / radial, attack_dir,
                        );
                        if toward_rim != away_from_rim {
                            let noise = sample_u53_checked(
                                self.philox_key, self.state.possession_id, self.state.possession_tick,
                                &mut self.state.rng_sample_counters.0, cid, RngPurpose::RefereeNoise,
                            )?;
                            if noise < 0.10 {
                                called_three = !is_three;
                            }
                        }
                    }
                }
                let shot_val = if called_three { 3 } else { 2 };
                let (rx_mm, _) = crate::wire_schema_v1::frames::quantize_mm(ox).unwrap_or((0, true));
                let (ry_mm, _) = crate::wire_schema_v1::frames::quantize_mm(oy).unwrap_or((0, true));
                let def_dist_mm = (min_def_d * 1000.0).round().clamp(0.0, 65535.0) as u16;
                let c_centi = (c_geom * 100.0).round().clamp(0.0, 65535.0) as u16;

                let assist_roster_idx = match self.state.ball.last_passer_roster_idx {
                    Some(pr)
                        if pr != owner_r
                            && ((pr as usize) / 15) == off_idx
                            && self.state.players[owner_r as usize].dribbles_since_catch <= 8
                            && self.state.sim_tick.saturating_sub(self.state.ball.last_pass_completed_tick)
                                <= 25 =>
                    {
                        Some(pr)
                    }
                    _ => None,
                };

                self.push_event(GameRecordEventPayload::ShotReleased {
                    shot_id,
                    shooter_roster_idx: owner_r,
                    shot_value: shot_val,
                    release_zone: zone,
                    defender_slot: nearest_def_slot,
                    nearest_def_dist_mm: def_dist_mm,
                    c_geom_centi: c_centi,
                    release_x_mm: rx_mm,
                    release_y_mm: ry_mm,
                });

                self.pending_audit.shots.push(ShotAuditSample {
                    tick: self.state.sim_tick,
                    shooter_slot: owner_slot,
                    is_three,
                    c_geom,
                    dist_to_rim_m: dist_rim,
                    outcome,
                    is_rim_or_close: zone == 0,
                });

                // Immediate release block (never if release_foul) -> LiveBall -> ReboundScramble
                if !shot_inp.release_foul && outcome == IntendedOutcomeClass::Blocked && min_def_d <= 1.4 {
                    self.push_event(GameRecordEventPayload::ShotResolved {
                        shot_id,
                        shooter_roster_idx: owner_r,
                        scorer_team: 0xFF,
                        points_awarded: 0,
                        outcome_class: IntendedOutcomeClass::Blocked as u8,
                        rim_touched: 0,
                        backboard_contact: BackboardContact::None as u8,
                        is_blocked: 1,
                        blocker_roster_idx: nearest_def_r,
                        is_goaltend: 0,
                        assist_roster_idx: 0xFF,
                        foul_on_shot: 0,
                        telemetry: ShotTelemetryPublic {
                            release_tick: self.state.sim_tick,
                            release_game_clock_ds: self.state.game_clock_ds,
                            whistle_game_clock_ds: 0xFFFF,
                            whistle_tick: 0xFFFF_FFFF,
                            flight_resolved_tick: self.state.sim_tick,
                            basket_scored_tick: 0xFFFF_FFFF,
                        },
                    });
                    self.state.ball.ball_state = BallState::Loose;
                    self.state.ball.owner_slot = None;
                    self.state.ball.last_passer_roster_idx = None;
                    self.state.ball.velocity_m_s = [
                        attack_dir * 1.5,
                        if oy >= 0.0 { 4.4 } else { -4.4 },
                        2.0,
                    ];
                    self.state.ball_fx_flags |= 0x04;
                    self.state.ball_fx_timer_ds = 5;
                    if nearest_def_r != ROSTER_IDX_NONE {
                        self.state.players[nearest_def_r as usize].action_state =
                            PlayerActionState::ContestJump;
                        self.state.players[nearest_def_r as usize].action_timer_ds = 5;
                    }
                    self.state.scramble_origin = Some(ScrambleOrigin::FromNoRimMissOrBlock {
                        prior_offense: off_side,
                    });
                    self.state.game_phase = GamePhase::ReboundScramble;
                    self.state.scramble_ticks = 0;
                    return Ok(());
                }

                let deferred_foul_call = if shot_inp.release_foul {
                    Some(DeferredShotFoul {
                        call: RuleCallSnapshot {
                            rule_code: RuleCode::FOUL_SHOOTING,
                            offending_side: off_side.opponent(),
                            offender_roster_idx: nearest_def_r,
                            victim_roster_idx: Some(owner_r),
                            points_if_made: shot_val,
                            ft_count: shot_val,
                            is_shooting_foul: true,
                            in_shooting_motion_before_whistle: true,
                            foul_occurred_after_shooting_motion_started: true,
                            whistle_after_shooting_motion_started: true,
                            ball_in_frontcourt_at_call: true,
                            interruption_spot: InboundSpot::FrontcourtSideline28ft,
                            stoppage_sideline_side: CourtSideLine::from_y_m(oy),
                            restart_shot_clock_ds: Some(self.ruleset.clocks.reset_shot_clock_ds),
                            foul_category: 0,
                            ..RuleCallSnapshot::default()
                        },
                        whistle_tick: self.state.sim_tick,
                        whistle_game_clock_ds: self.state.game_clock_ds,
                    })
                } else {
                    None
                };

                // Enter ShotInFlight with high-arc parabolic trajectory aimed by IntendedOutcomeClass
                let rim_xy = self.geom.rim_xy(attack_dir);
                let d_xy = libm::hypot(rim_xy.0 - ox, rim_xy.1 - oy);
                let flight_s = if d_xy <= 1.3 {
                    0.32
                } else if d_xy <= 2.3 {
                    0.52 + 0.04 * d_xy
                } else {
                    (0.68 + 0.082 * d_xy).clamp(0.75, 1.42)
                };
                let (aim_x, aim_y, aim_z) = match outcome {
                    IntendedOutcomeClass::BankMake
                    | IntendedOutcomeClass::BankMiss
                    | IntendedOutcomeClass::BackboardOnlyMiss => (
                        rim_xy.0 + attack_dir * 0.34,
                        rim_xy.1 + oy.clamp(-1.8, 1.8) * 0.12,
                        self.geom.rim_height_m + 0.42,
                    ),
                    IntendedOutcomeClass::Airball => (
                        rim_xy.0 - attack_dir * 0.72,
                        rim_xy.1 + (if oy >= 0.0 { 0.85 } else { -0.85 }),
                        (self.geom.rim_height_m - 0.30).max(1.2),
                    ),
                    _ => (rim_xy.0, rim_xy.1, self.geom.rim_height_m),
                };
                let z0 = 2.35;
                let vx = (aim_x - ox) / flight_s;
                let vy = (aim_y - oy) / flight_s;
                let vz = (aim_z - z0 + 0.5 * GRAVITY_M_S2 * flight_s * flight_s) / flight_s;

                self.state.ball.position_m = [ox, oy, z0];
                self.state.ball.velocity_m_s = [vx, vy, vz];
                self.state.ball.ball_state = BallState::ShotFlight;
                self.state.ball.owner_slot = None;
                self.state.ball.rim_touched_this_shot = false;
                self.state.ball.backboard_contact_this_shot = BackboardContact::None;
                self.state.shot_sub_stage = 0;
                self.state.shot_stage_ticks = 0;
                self.state.shot_gc_expired_pending = false;
                let has_deferred_foul = deferred_foul_call.is_some();
                self.state.ball.shot_ctx = Some(ShotContext {
                    shot_id,
                    shooter_roster_idx: owner_r,
                    assist_roster_idx,
                    blocker_roster_idx: None,
                    shot_value: shot_val,
                    ground_truth_is_three_pointer: is_three,
                    release_zone: zone,
                    release_x_mm: rx_mm,
                    release_y_mm: ry_mm,
                    intended_outcome: outcome,
                    release_tick: self.state.sim_tick,
                    release_game_clock_ds: self.state.game_clock_ds,
                    whistle_tick: if has_deferred_foul {
                        Some(self.state.sim_tick)
                    } else {
                        None
                    },
                    whistle_game_clock_ds: if has_deferred_foul {
                        Some(self.state.game_clock_ds)
                    } else {
                        None
                    },
                    deferred_foul_call,
                    flight_resolved_tick: None,
                    basket_scored_tick: None,
                    rim_touched: false,
                    backboard_contact: BackboardContact::None,
                    released_before_shot_clock_zero: true,
                });
                self.state.game_phase = GamePhase::ShotInFlight;
                self.state.players[owner_r as usize].facing_rad =
                    libm::atan2(rim_xy.1 - oy, rim_xy.0 - ox);
                self.state.players[owner_r as usize].action_state =
                    if dist_rim <= 1.3 && shooter_dunk_attr >= 75 {
                        PlayerActionState::Dunk
                    } else if dist_rim <= 2.3 {
                        PlayerActionState::Layup
                    } else {
                        PlayerActionState::ShootJumper
                    };
                self.state.players[owner_r as usize].action_timer_ds = 5;
                if self.state.players[owner_r as usize].z_m <= 0.01 {
                    self.state.players[owner_r as usize].vz_m_s =
                        2.2 + (shooter_vert_attr / 99.0) * 1.35;
                }
                if min_def_d <= 2.15 && nearest_def_r != ROSTER_IDX_NONE {
                    let dp = &mut self.state.players[nearest_def_r as usize];
                    dp.action_state = PlayerActionState::ContestJump;
                    dp.action_timer_ds = 5;
                    dp.facing_rad = libm::atan2(oy - dp.y_m, ox - dp.x_m);
                    if dp.z_m <= 0.01 {
                        dp.vz_m_s = 2.1 + (def_vert_attr / 99.0) * 1.3;
                    }
                }
                if !in_backcourt_phase && is_three && self.state.play_screen_contact_ds[off_idx] >= 2 {
                    let cov = self.audit_defensive_coverage(def_idx);
                    self.pending_audit.pnr_events.push(PnrAuditEvent {
                        tick: self.state.sim_tick,
                        possession_id: self.state.possession_id,
                        offense_side: off_side,
                        coverage: cov,
                        handler_pullup_3: self.state.players[owner_r as usize].footwork.dribble_status == DribbleStatus::Dribbling,
                        passed_out_of_blitz: false,
                        roller_drew_help: false,
                        extra_pass_to_corner: false,
                        open_corner_3_attempt: c_geom < 0.25 && oy.abs() > 5.5,
                        handler_tov: false,
                        pre_switch_triggered: cov == PnrCoverageKind::Switch,
                        big_forced_iso: false,
                    });
                }
            }
        }

        Ok(())
    }

}

/// Deterministic Batch post-whistle driver (`AiPostWhistleController`, §3.3).
pub fn step_with_batch_ai(
    engine: &mut SimulationEngine,
    sink: &mut impl SimSink,
    recorded_commands: &mut Vec<AnchoredCommand>,
) -> StepStatus {
    let mut status = engine.step(sink);
    while let StepStatus::AwaitingPostWhistle {
        ruling_id,
        window_state,
        ..
    } = status
    {
        if let Some(ruling) = engine.state.active_pending_ruling.as_ref() {
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
                client_request_id: engine.state.next_ingress_seq,
                apply_at_tick: None,
                phase: CommandPhase::PostWhistle,
                side: CommandSide::System,
                payload: CommandPayload::System(sys_payload),
            };
            if let Ok(anchored) = engine.enqueue_command(cmd, sink) {
                recorded_commands.push(anchored);
            }
        }
        status = engine.step(sink);
    }
    status
}
