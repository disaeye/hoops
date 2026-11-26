// 指令模块：两阶段准入预检、定锚提交、重演、PreTick / PostWhistle 执行、挑战复核与得分改判。

use crate::canonical_le_bytes::CanonicalLeBytes;
use crate::hash::compute_request_fingerprint;
use crate::model::SimSink;
use crate::rules::{
    apply_nba_challenge_timeout_and_bonus, charge_team_timeout_with_mandatory_slots,
    compute_shot_clock_reset, inbound_window_permissions, nba_rule_14_vi_retains_made_basket,
    validate_substitution, ShotClockScenario,
};
use crate::wire_schema_v1::*;

use super::{ReplayCommandError, SimulationEngine};

impl SimulationEngine {
    /// Pure read-only Phase-1 admission check (§3.3). Never mutates `self.state` or calls `sink`.
    pub fn prepare_command(
        &self,
        cmd: IngressCommand,
    ) -> Result<PreparedOutcome, CommandRejectReason> {
        let encoded_ingress_len = cmd.to_canonical_le_bytes().len();
        if encoded_ingress_len > MAX_INGRESS_COMMAND_BYTES {
            return Err(CommandRejectReason::PayloadTooLarge);
        }

        // System commands bypass client_dedup_ring and player array bounds
        if cmd.side == CommandSide::System {
            let sys_cmd = match cmd.payload {
                CommandPayload::System(s) => s,
                CommandPayload::Player(_) => return Err(CommandRejectReason::InvalidPhase),
            };
            if cmd.apply_at_tick.is_some() {
                return Err(CommandRejectReason::InvalidPhase);
            }
            if self.state.game_phase == GamePhase::GameOver
                || matches!(
                    self.state.cursor,
                    EngineCursor::PendingForfeitFinalize { .. } | EngineCursor::GameFinished
                )
            {
                return Err(CommandRejectReason::MatchAlreadyEnded);
            }
            if self.state.system_commands_accepted_count >= MAX_SYSTEM_COMMANDS_PER_GAME {
                return Err(CommandRejectReason::GameCommandLimitExceeded);
            }
            let ruling = match (&self.state.cursor, &self.state.active_pending_ruling) {
                (EngineCursor::PausedInPostWhistle { ruling_id, .. }, Some(r))
                    if r.ruling_id == *ruling_id =>
                {
                    r
                }
                _ => return Err(CommandRejectReason::InvalidPhase),
            };
            match sys_cmd {
                SystemCommand::ConfirmChallengeGrant {
                    ruling_id,
                    request_seq,
                } => {
                    if ruling.ruling_id != ruling_id {
                        return Err(CommandRejectReason::RulingIdMismatch);
                    }
                    if !matches!(
                        ruling.window_state,
                        ChallengeWindowState::PendingRefereeGrant { .. }
                    ) {
                        return Err(CommandRejectReason::InvalidPhase);
                    }
                    if ruling.request_seq != request_seq {
                        return Err(CommandRejectReason::StaleRequestSeq);
                    }
                }
                SystemCommand::CloseChallengeWindow {
                    ruling_id,
                    window_epoch,
                } => {
                    if ruling.ruling_id != ruling_id {
                        return Err(CommandRejectReason::RulingIdMismatch);
                    }
                    if ruling.window_epoch != window_epoch {
                        return Err(CommandRejectReason::StaleWindowEpoch);
                    }
                    if !matches!(ruling.window_state, ChallengeWindowState::Open) {
                        return Err(CommandRejectReason::InvalidPhase);
                    }
                }
            }
            if self.current_tick_events.len() + cmd.payload.max_immediate_events()
                > MAX_EVENTS_PER_TICK
            {
                return Err(CommandRejectReason::TickEventCapacityReserved);
            }
            let anchored = AnchoredCommand {
                client_request_id: cmd.client_request_id,
                enqueued_at_tick: self.state.sim_tick,
                enqueued_phase: cmd.phase,
                requested_apply_at_tick: None,
                resolved_tick: self.state.sim_tick,
                resolved_phase: CommandPhase::PostWhistle,
                ingress_seq: self.state.next_ingress_seq,
                side: CommandSide::System,
                payload: cmd.payload,
            };
            let enc = anchored.to_canonical_le_bytes();
            if enc.len() > MAX_ANCHORED_PLAYER_BYTES {
                return Err(CommandRejectReason::PayloadTooLarge);
            }
            if (self.state.replay_manifest_projected_bytes as usize) + enc.len()
                > MAX_REPLAY_MANIFEST_BYTES
            {
                return Err(CommandRejectReason::ReplayManifestCapacityExceeded);
            }
            let mut arr = [0u8; 182];
            arr[..enc.len()].copy_from_slice(&enc);
            let fp = compute_request_fingerprint(&(
                cmd.side,
                cmd.phase,
                cmd.apply_at_tick,
                cmd.payload,
            ));
            return Ok(PreparedOutcome::NewCommand(PreparedCommand {
                ingress: cmd,
                expected_enqueued_at_tick: self.state.sim_tick,
                expected_enqueued_phase: cmd.phase,
                expected_ingress_seq: self.state.next_ingress_seq,
                candidate_anchored: anchored,
                request_fingerprint: fp,
                projected_manifest_delta_bytes: enc.len() as u32,
                encoded_bytes: EncodedAnchoredBytes(arr),
                encoded_len: enc.len() as u16,
            }));
        }

        // Player commands (Home | Away)
        let team_idx = cmd
            .side
            .team_index()
            .ok_or(CommandRejectReason::InvalidPhase)?;
        let p_cmd = match cmd.payload {
            CommandPayload::Player(p) => p,
            CommandPayload::System(_) => return Err(CommandRejectReason::ForbiddenSystemCommand),
        };

        // 1. Idempotency dedup check FIRST (even if GameOver!)
        let fp = compute_request_fingerprint(&(
            cmd.side,
            cmd.phase,
            cmd.apply_at_tick,
            cmd.payload,
        ));
        if let Some(existing) = self.state.client_dedup_ring[team_idx].find(cmd.client_request_id)
        {
            if existing.request_fingerprint == fp {
                let enc = existing.anchored.to_canonical_le_bytes();
                let mut arr = [0u8; 182];
                let len = enc.len().min(182);
                arr[..len].copy_from_slice(&enc[..len]);
                return Ok(PreparedOutcome::AlreadyAccepted {
                    existing_anchored: existing.anchored,
                    encoded_bytes: EncodedAnchoredBytes(arr),
                    encoded_len: len as u16,
                });
            } else {
                return Err(CommandRejectReason::IdempotencyPayloadMismatch);
            }
        }
        if cmd.client_request_id <= self.state.highest_client_request_id[team_idx]
            && self.state.highest_client_request_id[team_idx] > 0
        {
            return Err(CommandRejectReason::ClientRequestIdTooOld);
        }

        // 2. GameOver check for new requests
        if self.state.game_phase == GamePhase::GameOver
            || matches!(
                self.state.cursor,
                EngineCursor::PendingForfeitFinalize { .. } | EngineCursor::GameFinished
            )
        {
            return Err(CommandRejectReason::MatchAlreadyEnded);
        }

        // 3. Player quota & capacity
        if self.state.player_commands_accepted_count >= MAX_PLAYER_COMMANDS_PER_GAME {
            return Err(CommandRejectReason::GameCommandLimitExceeded);
        }

        // 4. Phase & timing check
        let resolved_tick = match (cmd.phase, cmd.apply_at_tick) {
            (CommandPhase::PreTick, None) => {
                if !matches!(self.state.cursor, EngineCursor::ReadyForPreTick) {
                    return Err(CommandRejectReason::InvalidPhase);
                }
                self.state.sim_tick
            }
            (CommandPhase::PreTick, Some(t)) => {
                if t < self.state.sim_tick
                    || (t == self.state.sim_tick
                        && !matches!(self.state.cursor, EngineCursor::ReadyForPreTick))
                {
                    return Err(CommandRejectReason::Expired);
                }
                if t > self.state.sim_tick.saturating_add(MAX_FUTURE_RESOLVE_HORIZON_TICKS) {
                    return Err(CommandRejectReason::Expired);
                }
                t
            }
            (CommandPhase::PostWhistle, None) => {
                if !matches!(self.state.cursor, EngineCursor::PausedInPostWhistle { .. }) {
                    return Err(CommandRejectReason::InvalidPhase);
                }
                self.state.sim_tick
            }
            (CommandPhase::PostWhistle, Some(_)) => {
                return Err(CommandRejectReason::InvalidPhase);
            }
        };

        // 5. Event reserve check (ForfeitMatch exempted from reserve)
        let needed = self.current_tick_events.len() + cmd.payload.max_immediate_events();
        let limit = if p_cmd == PlayerCoachCommand::ForfeitMatch {
            MAX_EVENTS_PER_TICK
        } else {
            MAX_EVENTS_PER_TICK - COMMIT_EVENT_RESERVE
        };
        if needed > limit {
            return Err(CommandRejectReason::TickEventCapacityReserved);
        }

        // 6. Queue slot limits for PreTick commands (except immediate ForfeitMatch)
        if cmd.phase == CommandPhase::PreTick
            && !(p_cmd == PlayerCoachCommand::ForfeitMatch && cmd.apply_at_tick.is_none())
        {
            if !self.state.command_queue.can_push(false) {
                return Err(CommandRejectReason::QueueFull);
            }
            if self.state.command_queue.pretick_count_at(resolved_tick, cmd.side) >= 4 {
                return Err(CommandRejectReason::TickCommandSlotFull);
            }
        }

        // 7. Semantic validation per command variant
        self.validate_player_command_semantics(cmd.side, team_idx, cmd.phase, p_cmd)?;

        let anchored = AnchoredCommand {
            client_request_id: cmd.client_request_id,
            enqueued_at_tick: self.state.sim_tick,
            enqueued_phase: cmd.phase,
            requested_apply_at_tick: cmd.apply_at_tick,
            resolved_tick,
            resolved_phase: cmd.phase,
            ingress_seq: self.state.next_ingress_seq,
            side: cmd.side,
            payload: cmd.payload,
        };
        let enc = anchored.to_canonical_le_bytes();
        if enc.len() > MAX_ANCHORED_PLAYER_BYTES {
            return Err(CommandRejectReason::PayloadTooLarge);
        }
        if (self.state.replay_manifest_projected_bytes as usize) + enc.len()
            > MAX_REPLAY_MANIFEST_BYTES
        {
            return Err(CommandRejectReason::ReplayManifestCapacityExceeded);
        }
        let mut arr = [0u8; 182];
        arr[..enc.len()].copy_from_slice(&enc);
        Ok(PreparedOutcome::NewCommand(PreparedCommand {
            ingress: cmd,
            expected_enqueued_at_tick: self.state.sim_tick,
            expected_enqueued_phase: cmd.phase,
            expected_ingress_seq: self.state.next_ingress_seq,
            candidate_anchored: anchored,
            request_fingerprint: fp,
            projected_manifest_delta_bytes: enc.len() as u32,
            encoded_bytes: EncodedAnchoredBytes(arr),
            encoded_len: enc.len() as u16,
        }))
    }

    fn validate_player_command_semantics(
        &self,
        side: CommandSide,
        team_idx: usize,
        phase: CommandPhase,
        p_cmd: PlayerCoachCommand,
    ) -> Result<(), CommandRejectReason> {
        match p_cmd {
            PlayerCoachCommand::ForfeitMatch => Ok(()),
            PlayerCoachCommand::CallTimeout { queue_if_live_disallowed, .. } => {
                if self.state.quarter == 1 && !self.state.q1_game_clock_ever_started {
                    return Err(CommandRejectReason::InvalidPhase);
                }
                if self.state.game_phase == GamePhase::JumpBall {
                    if let Some(ref jb) = self.state.jump_ball_context {
                        if jb.ball_tossed && !jb.ball_tapped {
                            return Err(CommandRejectReason::JumpBallAlreadyTossed);
                        }
                    }
                }
                if self.state.game_phase == GamePhase::FreeThrows
                    && (!self.ruleset.timeouts.to_between_fts_allowed || self.state.ft_sub_stage == 1)
                {
                    return Err(CommandRejectReason::InvalidPhase);
                }
                if !self.ruleset.timeouts.live_ball_player_to_allowed
                    && matches!(self.state.game_phase, GamePhase::LiveBall | GamePhase::ShotInFlight | GamePhase::ReboundScramble)
                    && !queue_if_live_disallowed
                {
                    return Err(CommandRejectReason::InvalidPhase);
                }
                if self.state.game_phase == GamePhase::LiveBall
                    && self.ruleset.timeouts.live_ball_player_to_allowed
                {
                    let in_ctrl = self.state.possession_side == Some(side)
                        && matches!(self.state.ball.ball_state, BallState::Held | BallState::Dribble);
                    if !in_ctrl {
                        return Err(CommandRejectReason::NotInPlayerControl);
                    }
                }
                if self.state.game_phase == GamePhase::InboundSetup {
                    if let Some(ref inb) = self.state.inbound_context {
                        let perms = inbound_window_permissions(&self.state, inb, &self.ruleset);
                        let allowed = if side == CommandSide::Home {
                            perms.home_can_timeout
                        } else {
                            perms.away_can_timeout
                        };
                        if !allowed {
                            return Err(CommandRejectReason::InvalidPhase);
                        }
                    }
                }
                Ok(())
            }
            PlayerCoachCommand::Substitute {
                out_player,
                in_player,
            } => validate_substitution(&self.state, side, out_player, in_player, &self.ruleset, false),
            PlayerCoachCommand::ChallengeCall {
                target,
                expected_window_epoch,
            } => {
                if self.ruleset.challenges.mode == 0 {
                    return Err(CommandRejectReason::ChallengeDisabled);
                }
                match target {
                    ChallengeTarget::Ruling(rid) => {
                        if phase != CommandPhase::PostWhistle {
                            return Err(CommandRejectReason::InvalidPhase);
                        }
                        let ruling = self
                            .state
                            .active_pending_ruling
                            .as_ref()
                            .ok_or(CommandRejectReason::InvalidPhase)?;
                        if ruling.ruling_id != rid {
                            return Err(CommandRejectReason::RulingIdMismatch);
                        }
                        if expected_window_epoch != Some(ruling.window_epoch) {
                            return Err(CommandRejectReason::StaleWindowEpoch);
                        }
                        if !matches!(ruling.window_state, ChallengeWindowState::Open) {
                            return Err(CommandRejectReason::ChallengeAlreadyGranted);
                        }
                        if ruling.challenge_attempts_in_window
                            >= MAX_WINDOW_CHALLENGE_CYCLES_PER_RULING
                        {
                            return Err(CommandRejectReason::WindowChallengeCycleLimitReached);
                        }
                        if self.state.team_states[team_idx].challenges_remaining == 0 {
                            return Err(CommandRejectReason::NoChallengesRemaining);
                        }
                    }
                    ChallengeTarget::RecentScoringOrClockEvent { event_seq } => {
                        if self.ruleset.challenges.mode != 2 {
                            return Err(CommandRejectReason::UnchallengeableRuling);
                        }
                        if expected_window_epoch.is_some() {
                            return Err(CommandRejectReason::InvalidCodecOrPadding);
                        }
                        if self.state.team_states[team_idx].challenges_remaining == 0 {
                            return Err(CommandRejectReason::NoChallengesRemaining);
                        }
                        // Scan ring FIRST before watermark check (§3.3)!
                        if let Some(entry) =
                            self.state.recent_reviewable_events.find_by_event_seq(event_seq)
                        {
                            if entry.stoppage_occurred_since
                                || event_seq <= self.state.last_stoppage_resume_event_seq
                            {
                                return Err(CommandRejectReason::ChallengeTargetPastFirstStoppage);
                            }
                        } else if event_seq
                            <= self.state.reviewable_ring_overflow_watermark_event_seq
                        {
                            return Err(CommandRejectReason::ReviewableEventEvicted);
                        } else {
                            return Err(CommandRejectReason::ChallengeTargetPastFirstStoppage);
                        }
                    }
                }
                Ok(())
            }
            PlayerCoachCommand::WithdrawChallenge {
                target,
                expected_request_seq,
                expected_window_epoch,
            } => {
                if self.ruleset.challenges.mode != 2 {
                    return Err(CommandRejectReason::ChallengeDisabled);
                }
                match target {
                    ChallengeTarget::Ruling(rid) => {
                        let ruling = self
                            .state
                            .active_pending_ruling
                            .as_ref()
                            .ok_or(CommandRejectReason::InvalidPhase)?;
                        if ruling.ruling_id != rid {
                            return Err(CommandRejectReason::RulingIdMismatch);
                        }
                        if expected_window_epoch != Some(ruling.window_epoch) {
                            return Err(CommandRejectReason::StaleWindowEpoch);
                        }
                        match ruling.window_state {
                            ChallengeWindowState::PendingRefereeGrant { requester }
                                if requester == side => {}
                            _ => return Err(CommandRejectReason::InvalidPhase),
                        }
                        if ruling.request_seq != expected_request_seq {
                            return Err(CommandRejectReason::StaleRequestSeq);
                        }
                    }
                    ChallengeTarget::RecentScoringOrClockEvent { event_seq } => {
                        if expected_window_epoch.is_some() {
                            return Err(CommandRejectReason::StaleWindowEpoch);
                        }
                        let req = self.state.pending_fiba_hcc_requests[team_idx]
                            .ok_or(CommandRejectReason::InvalidPhase)?;
                        if req.target_event_seq != event_seq {
                            return Err(CommandRejectReason::RulingIdMismatch);
                        }
                        if req.request_seq != expected_request_seq {
                            return Err(CommandRejectReason::StaleRequestSeq);
                        }
                    }
                }
                Ok(())
            }
            PlayerCoachCommand::SetTimeoutAdvance {
                expected_window_epoch,
                ..
            } => {
                if phase == CommandPhase::PostWhistle {
                    let ruling = self
                        .state
                        .active_pending_ruling
                        .as_ref()
                        .ok_or(CommandRejectReason::InvalidPhase)?;
                    if expected_window_epoch != Some(ruling.window_epoch) {
                        return Err(CommandRejectReason::StaleWindowEpoch);
                    }
                } else if expected_window_epoch.is_some() {
                    return Err(CommandRejectReason::StaleWindowEpoch);
                }
                Ok(())
            }
            PlayerCoachCommand::SetOffensivePlay { play_id, .. } => {
                if !self.playbooks.iter().any(|p| p.play_id == play_id) {
                    return Err(CommandRejectReason::InvalidCodecOrPadding);
                }
                Ok(())
            }
            PlayerCoachCommand::SetDefensiveGameplan { .. }
            | PlayerCoachCommand::SetPaceAndPressure { .. } => Ok(()),
        }
    }

    pub fn commit_prepared_command(
        &mut self,
        prepared: PreparedCommand,
        sink: &mut impl SimSink,
    ) -> AnchoredCommand {
        debug_assert_eq!(prepared.expected_ingress_seq, self.state.next_ingress_seq);
        let anchored = prepared.candidate_anchored;
        self.state.next_ingress_seq += 1;
        self.state.replay_manifest_projected_bytes = self
            .state
            .replay_manifest_projected_bytes
            .saturating_add(prepared.projected_manifest_delta_bytes);

        if anchored.payload.is_system() {
            self.state.system_commands_accepted_count += 1;
        } else {
            self.state.player_commands_accepted_count += 1;
            if let Some(team_idx) = anchored.side.team_index() {
                if anchored.client_request_id > self.state.highest_client_request_id[team_idx] {
                    self.state.highest_client_request_id[team_idx] = anchored.client_request_id;
                }
                self.state.client_dedup_ring[team_idx].insert(DedupEntry {
                    client_request_id: anchored.client_request_id,
                    request_fingerprint: prepared.request_fingerprint,
                    anchored,
                });
            }
        }

        sink.on_anchored_command(&anchored);
        self.route_or_execute_anchored(anchored, sink);
        anchored
    }

    pub fn enqueue_command(
        &mut self,
        cmd: IngressCommand,
        sink: &mut impl SimSink,
    ) -> Result<AnchoredCommand, CommandRejectReason> {
        match self.prepare_command(cmd)? {
            PreparedOutcome::AlreadyAccepted {
                existing_anchored, ..
            } => Ok(existing_anchored),
            PreparedOutcome::NewCommand(prep) => Ok(self.commit_prepared_command(prep, sink)),
        }
    }

    pub fn replay_anchored_command(
        &mut self,
        anchored: AnchoredCommand,
        sink: &mut impl SimSink,
    ) -> Result<(), ReplayCommandError> {
        if self.state.next_ingress_seq != anchored.ingress_seq {
            return Err(ReplayCommandError::IngressSeqMismatch {
                expected: self.state.next_ingress_seq,
                got: anchored.ingress_seq,
            });
        }
        let enc_len = anchored.to_canonical_le_bytes().len() as u32;

        if anchored.payload.is_system() {
            if self.state.system_commands_accepted_count >= MAX_SYSTEM_COMMANDS_PER_GAME {
                return Err(ReplayCommandError::QuotaExceeded);
            }
            self.state.system_commands_accepted_count += 1;
        } else {
            if self.state.player_commands_accepted_count >= MAX_PLAYER_COMMANDS_PER_GAME {
                return Err(ReplayCommandError::QuotaExceeded);
            }
            self.state.player_commands_accepted_count += 1;
            if let Some(team_idx) = anchored.side.team_index() {
                let fp = compute_request_fingerprint(&(
                    anchored.side,
                    anchored.enqueued_phase,
                    anchored.requested_apply_at_tick,
                    anchored.payload,
                ));
                if anchored.client_request_id > self.state.highest_client_request_id[team_idx] {
                    self.state.highest_client_request_id[team_idx] = anchored.client_request_id;
                }
                self.state.client_dedup_ring[team_idx].insert(DedupEntry {
                    client_request_id: anchored.client_request_id,
                    request_fingerprint: fp,
                    anchored,
                });
            }
        }

        self.state.next_ingress_seq += 1;
        self.state.replay_manifest_projected_bytes = self
            .state
            .replay_manifest_projected_bytes
            .saturating_add(enc_len);
        sink.on_anchored_command(&anchored);
        self.route_or_execute_anchored(anchored, sink);
        Ok(())
    }

    fn route_or_execute_anchored(&mut self, anchored: AnchoredCommand, sink: &mut impl SimSink) {
        if anchored.resolved_phase == CommandPhase::PostWhistle {
            self.execute_post_whistle_command(anchored, sink);
            self.emit_new_events_to_sink(sink);
            return;
        }
        if anchored.payload.is_forfeit() && anchored.requested_apply_at_tick.is_none() {
            self.execute_forfeit_match(anchored.side, sink);
            return;
        }
        let _ = self.state.command_queue.push(anchored);
    }

    pub(super) fn execute_forfeit_match(&mut self, forfeiting_side: CommandSide, sink: &mut impl SimSink) {
        let cleared_ruling_id = self.state.active_pending_ruling.take().map(|r| r.ruling_id);
        self.state.ft_series_queue.clear();
        // Emit 0x18 (0x0A) for <= 2 active pending FIBA HCC requests
        for slot_idx in 0..2 {
            if let Some(req) = self.state.pending_fiba_hcc_requests[slot_idx].take() {
                self.push_event(GameRecordEventPayload::AnchoredCommandAbortedRecord {
                    ingress_seq: req.enqueued_ingress_seq,
                    side: req.requester as u8,
                    abort_reason_code: CommandAbortReason::SupercededByLaterForfeit as u8,
                    cmd_tag: 1,
                });
            }
        }
        // Bulk silent clear of future queue (0 events)
        self.state.command_queue.clear();

        let winner = forfeiting_side.opponent();
        let is_fiba = matches!(
            self.ruleset.clocks.shot_clock_policy,
            ShotClockResetPolicy::FibaArt29
        );
        let win_pts = if is_fiba { 20 } else { 2 };
        let (off_home, off_away) = match winner {
            CommandSide::Home => (win_pts, 0),
            _ => (0, win_pts),
        };
        self.state.team_states[0].score_official_override = Some(off_home);
        self.state.team_states[1].score_official_override = Some(off_away);
        let end_reason = match forfeiting_side {
            CommandSide::Home => GameEndReason::ForfeitHome,
            _ => GameEndReason::ForfeitAway,
        };
        self.state.end_reason = Some(end_reason);
        self.state.game_phase = GamePhase::GameOver;
        self.state.cursor = EngineCursor::PendingForfeitFinalize {
            forfeiting_side,
            cleared_ruling_id,
        };

        let mut court_times = [0u32; 30];
        for (i, p) in self.state.players.iter().enumerate() {
            court_times[i] = p.court_time_live_ds;
        }
        self.push_event(GameRecordEventPayload::GameEnded {
            score_home_official: off_home,
            score_away_official: off_away,
            on_court_score_home: self.state.team_states[0].score_on_court,
            on_court_score_away: self.state.team_states[1].score_on_court,
            winner_side: winner as u8,
            end_reason: end_reason as u8,
            final_quarter: self.state.quarter,
            final_court_time_ds: FinalCourtTimeDs(court_times),
        });
        self.emit_new_events_to_sink(sink);
    }

    fn execute_post_whistle_command(&mut self, anchored: AnchoredCommand, sink: &mut impl SimSink) {
        match anchored.payload {
            CommandPayload::Player(PlayerCoachCommand::ForfeitMatch) => {
                self.execute_forfeit_match(anchored.side, sink);
            }
            CommandPayload::Player(PlayerCoachCommand::ChallengeCall {
                target: ChallengeTarget::Ruling(rid),
                ..
            }) => {
                let team_idx = match anchored.side.team_index() {
                    Some(i) => i,
                    None => return,
                };
                if let Some(mut ruling) = self.state.active_pending_ruling.take() {
                    if ruling.ruling_id != rid {
                        self.state.active_pending_ruling = Some(ruling);
                        return;
                    }
                    // NBA Rule 14 §V-g: Unchallengeable ruling
                    let side_bit = 1u8 << team_idx;
                    if self.ruleset.challenges.mode == 1 && (ruling.eligible_sides.0 & side_bit) == 0
                    {
                        self.state.team_states[team_idx].timeouts_remaining = self.state.team_states
                            [team_idx]
                            .timeouts_remaining
                            .saturating_sub(1);
                        let rem_to = self.state.team_states[team_idx].timeouts_remaining;
                        let rem_ch = self.state.team_states[team_idx].challenges_remaining;
                        ruling.window_state = ChallengeWindowState::ClosedCommitted;
                        ruling.window_epoch += 1;
                        self.state.active_pending_ruling = Some(ruling);
                        self.push_event(
                            GameRecordEventPayload::UnchallengeableChallengeChargedTimeout {
                                ruling_id: rid,
                                requester: anchored.side as u8,
                                timeout_charged: 1,
                                remaining_timeouts: rem_to,
                                remaining_challenges: rem_ch,
                            },
                        );
                        return;
                    }
                    // NBA Rule 14 §V-f: 0 timeouts -> excessive timeout tech
                    if self.ruleset.challenges.mode == 1
                        && self.ruleset.challenges.requires_and_charges_timeout
                        && self.state.team_states[team_idx].timeouts_remaining == 0
                    {
                        ruling.perceived_call = RuleCallSnapshot {
                            rule_code: RuleCode::FOUL_TECH_EXCESSIVE_TIMEOUT,
                            offending_side: anchored.side,
                            interruption_spot: InboundSpot::NearestToInterruption,
                            stoppage_sideline_side: ruling.perceived_call.stoppage_sideline_side,
                            ft_count: 1,
                            ..RuleCallSnapshot::default()
                        };
                        ruling.ground_truth = ruling.perceived_call;
                        ruling.window_state = ChallengeWindowState::ClosedCommitted;
                        ruling.window_epoch += 1;
                        self.state.active_pending_ruling = Some(ruling);
                        return;
                    }

                    ruling.request_seq += 1;
                    ruling.window_epoch += 1;
                    ruling.challenge_attempts_in_window += 1;
                    let req_seq = ruling.request_seq;
                    self.push_event(GameRecordEventPayload::CoachChallengeRequested {
                        ruling_id: rid,
                        target_tag: 0,
                        requester: anchored.side as u8,
                        request_seq: req_seq,
                        target_event_seq: 0,
                    });

                    if self.ruleset.challenges.mode == 2 {
                        // FIBA FibaIrsEnabled: wait for ConfirmChallengeGrant
                        ruling.window_state = ChallengeWindowState::PendingRefereeGrant {
                            requester: anchored.side,
                        };
                        self.state.active_pending_ruling = Some(ruling);
                        self.push_event(GameRecordEventPayload::CoachChallengePendingGrant {
                            ruling_id: rid,
                            requester: anchored.side as u8,
                            request_seq: req_seq,
                        });
                    } else {
                        // NBA Rule 14: immediate Stage 6b review -> ClosedCommitted
                        self.state.active_pending_ruling = Some(ruling);
                        self.perform_stage6b_review(anchored.side, 0);
                    }
                }
            }
            CommandPayload::Player(PlayerCoachCommand::WithdrawChallenge {
                target: ChallengeTarget::Ruling(rid),
                ..
            }) => {
                if let Some(ref mut ruling) = self.state.active_pending_ruling {
                    if ruling.ruling_id == rid {
                        ruling.window_state = ChallengeWindowState::Open;
                        ruling.window_epoch += 1;
                        let req_seq = ruling.request_seq;
                        self.push_event(GameRecordEventPayload::CoachChallengeWithdrawn {
                            ruling_id: rid,
                            requester: anchored.side as u8,
                            request_seq: req_seq,
                        });
                    }
                }
            }
            CommandPayload::Player(PlayerCoachCommand::CallTimeout { advance, .. }) => {
                let team = anchored.side.team_index().unwrap();
                let open = self.state.active_pending_ruling.as_ref().is_some_and(|ruling| {
                    matches!(ruling.window_state, ChallengeWindowState::Open)
                });
                if open {
                    self.state.pending_deadball_timeouts[team] = Some(advance);
                } else {
                    self.push_event(GameRecordEventPayload::AnchoredCommandAbortedRecord {
                        ingress_seq: anchored.ingress_seq,
                        side: anchored.side as u8,
                        abort_reason_code: CommandAbortReason::MootedByPriorOverturn as u8,
                        cmd_tag: anchored.payload.cmd_tag(),
                    });
                }
            }
            CommandPayload::Player(PlayerCoachCommand::SetTimeoutAdvance { advance, .. }) => {
                self.state.pending_post_commit_timeout = Some((anchored.side, advance));
            }
            CommandPayload::System(SystemCommand::ConfirmChallengeGrant { ruling_id, .. }) => {
                if let Some(ref mut ruling) = self.state.active_pending_ruling {
                    if ruling.ruling_id == ruling_id {
                        if let ChallengeWindowState::PendingRefereeGrant { requester } =
                            ruling.window_state
                        {
                            // FIBA Appendix F.4.2: consume 1 HCC immediately upon grant!
                            if let Some(idx) = requester.team_index() {
                                self.state.team_states[idx].challenges_remaining =
                                    self.state.team_states[idx].challenges_remaining.saturating_sub(1);
                            }
                            self.perform_stage6b_review(requester, 0);
                        }
                    }
                }
            }
            CommandPayload::System(SystemCommand::CloseChallengeWindow {
                ruling_id,
                window_epoch,
            }) => {
                if let Some(ref mut ruling) = self.state.active_pending_ruling {
                    if ruling.ruling_id == ruling_id && ruling.window_epoch == window_epoch {
                        ruling.window_state = ChallengeWindowState::ClosedCommitted;
                        ruling.window_epoch += 1;
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn perform_stage6b_review(&mut self, requester: CommandSide, review_origin: u8) {
        let mut ruling = match self.state.active_pending_ruling.take() {
            Some(r) => r,
            None => return,
        };
        ruling.window_state = ChallengeWindowState::UnderReview;
        let overturned = ruling.perceived_call.rule_code != ruling.ground_truth.rule_code
            || ruling.perceived_call.offending_side != ruling.ground_truth.offending_side
            || ruling.perceived_call.offender_roster_idx != ruling.ground_truth.offender_roster_idx;

        let is_second = self.ruleset.challenges.mode == 1
            && requester
                .team_index()
                .map_or(false, |i| self.state.team_states[i].first_challenge_used_and_won);

        let to_charged_side = if self.ruleset.challenges.mode == 1 && review_origin == 0 {
            apply_nba_challenge_timeout_and_bonus(
                &mut self.state,
                requester,
                ruling.window_kind,
                overturned,
                is_second,
                &self.ruleset,
            )
        } else {
            None
        };

        if let (
            ChallengeWindowKind::TimeoutPending { reason: TimeoutReason::Mandatory { .. } },
            Some(team_side),
        ) = (ruling.window_kind, to_charged_side) {
            ruling.window_kind = ChallengeWindowKind::TimeoutPending {
                reason: TimeoutReason::Mandatory { team_side },
            };
        }

        if overturned {
            let original = ruling.perceived_call;
            let retain_basket = self.ruleset.challenges.mode == 1
                && nba_rule_14_vi_retains_made_basket(&original, &ruling.ground_truth);
            ruling.perceived_call = ruling.ground_truth;
            if original.basket_made_on_play && ruling.ground_truth.rule_code == RuleCode::NONE {
                let ring = &self.state.recent_reviewable_events;
                let shot = ring.entries[..usize::from(ring.len)].iter()
                    .filter(|entry| entry.tick == ruling.window_open_tick && entry.points_awarded > 0
                        && original.victim_roster_idx.is_none_or(|victim| entry.shooter_roster_idx == Some(victim)))
                    .max_by_key(|entry| entry.event_seq).copied();
                if retain_basket {
                    let scoring_side = shot.and_then(|entry| entry.scorer_side)
                        .or(self.state.possession_side)
                        .unwrap_or_else(|| original.offending_side.opponent());
                    ruling.perceived_call.basket_made_on_play = true;
                    ruling.perceived_call.interruption_spot = InboundSpot::BackcourtEndline;
                    ruling.perceived_call.offending_side = scoring_side;
                    ruling.perceived_call.restart_shot_clock_ds = compute_shot_clock_reset(
                        ShotClockScenario::TurnoverOpponentToOwnBackcourt,
                        self.state.shot_clock_ds, self.state.game_clock_ds, &self.ruleset,
                    );
                } else {
                    ruling.perceived_call.basket_made_on_play = false;
                    if let Some(shot) = shot {
                        self.amend_scoring_event(shot, 0, 3);
                    }
                }
            }
        }
        ruling.window_state = ChallengeWindowState::ClosedCommitted;
        ruling.window_epoch += 1;

        let reason_code = if overturned { 1 } else { 0 };
        let final_rc = ruling.perceived_call.rule_code as u16;
        let sc_wire = match ruling.perceived_call.restart_shot_clock_ds {
            Some(s) => s,
            None => 0xFFFE,
        };
        self.push_event(GameRecordEventPayload::CoachChallengeReviewed {
            ruling_id: ruling.ruling_id,
            requester: if review_origin == 1 {
                0xFF
            } else {
                requester as u8
            },
            review_origin,
            overturned: if overturned { 1 } else { 0 },
            reason_code,
            timeout_charged_side: to_charged_side.map_or(0xFF, |s| s as u8),
            final_rule_code: final_rc,
            adjusted_game_clock_ds: self.state.game_clock_ds,
            new_shot_clock_ds: sc_wire,
        });
        self.state.active_pending_ruling = Some(ruling);
    }

    fn amend_scoring_event(&mut self, shot: ReviewableEventEntry, new_points: u8, amend_kind: u8) {
        let Some(side) = shot.scorer_side else { return };
        let Some(team_idx) = side.team_index() else { return };
        let pts_delta = new_points as i8 - shot.points_awarded as i8;
        let waved_off = i8::from(new_points == 0);
        let old_three = i8::from(shot.original_is_three_pointer);
        let new_three = i8::from(new_points == 3);
        let team = &mut self.state.team_states[team_idx];
        team.score_on_court = team.score_on_court.saturating_add_signed(i16::from(pts_delta));
        self.state.scoring_run_pts[team_idx] = self.state.scoring_run_pts[team_idx].saturating_add_signed(pts_delta);
        if let Some(entry) = self.state.recent_reviewable_events.find_by_event_seq_mut(shot.event_seq) {
            entry.points_awarded = new_points;
            entry.original_is_three_pointer = new_points == 3;
            entry.original_fga_counted &= new_points > 0;
        }
        self.push_event(GameRecordEventPayload::ScoringEventAmended {
            original_shot_event_seq: shot.event_seq,
            shooter_roster_idx: shot.shooter_roster_idx.unwrap_or(ROSTER_IDX_NONE),
            assist_roster_idx: shot.assist_roster_idx.unwrap_or(ROSTER_IDX_NONE),
            team_side: side as u8,
            amend_kind,
            old_points: shot.points_awarded,
            new_points,
            pts_delta,
            fga_delta: -waved_off * i8::from(shot.original_fga_counted),
            fgm_delta: -waved_off,
            tpa_delta: new_three - old_three,
            tpm_delta: new_three - old_three,
            ast_delta: -waved_off * i8::from(shot.assist_roster_idx.is_some()),
            blocker_roster_idx: shot.blocker_roster_idx.unwrap_or(ROSTER_IDX_NONE),
            blk_delta: -waved_off * i8::from(shot.blocker_roster_idx.is_some()),
        });
    }

    pub(super) fn consume_fiba_scoring_reviews(&mut self) {
        if self.ruleset.challenges.mode != 2 {
            return;
        }
        let mut requests = self.state.pending_fiba_hcc_requests;
        requests.sort_by_key(|request| request.map_or((u64::MAX, u64::MAX), |r| {
            (r.target_event_seq, r.enqueued_ingress_seq)
        }));
        for request in requests.into_iter().flatten() {
            let team = request.requester.team_index().unwrap();
            self.state.pending_fiba_hcc_requests[team] = None;
            let entry = self.state.recent_reviewable_events.find_by_event_seq(request.target_event_seq).copied();
            let Some(shot) = entry.filter(|entry| !entry.stoppage_occurred_since) else {
                self.push_event(GameRecordEventPayload::AnchoredCommandAbortedRecord {
                    ingress_seq: request.enqueued_ingress_seq,
                    side: request.requester as u8,
                    abort_reason_code: CommandAbortReason::TargetOutOfStoppageScope as u8,
                    cmd_tag: PlayerCoachCommand::ChallengeCall {
                        target: ChallengeTarget::RecentScoringOrClockEvent { event_seq: request.target_event_seq },
                        expected_window_epoch: None,
                    }.cmd_tag(),
                });
                continue;
            };
            let remaining = &mut self.state.team_states[team].challenges_remaining;
            if *remaining == 0 {
                continue;
            }
            *remaining -= 1;
            let new_points = if shot.points_awarded == 0 || shot.shot_clock_expired_before_release {
                0
            } else if shot.ground_truth_is_three_pointer {
                3
            } else {
                2
            };
            let overturned = new_points != shot.points_awarded;
            if overturned {
                let kind = match new_points { 0 => 2, 2 => 0, _ => 1 };
                self.amend_scoring_event(shot, new_points, kind);
            }
            self.push_event(GameRecordEventPayload::CoachChallengeReviewed {
                ruling_id: 0,
                requester: request.requester as u8,
                review_origin: 0,
                overturned: u8::from(overturned),
                reason_code: u8::from(overturned),
                timeout_charged_side: 0xFF,
                final_rule_code: RuleCode::NONE as u16,
                adjusted_game_clock_ds: self.state.game_clock_ds,
                new_shot_clock_ds: encode_shot_clock_opt(self.state.shot_clock_ds),
            });
        }
    }

    pub(super) fn execute_pretick_command(&mut self, cmd: AnchoredCommand) {
        let p_cmd = match cmd.payload {
            CommandPayload::Player(p) => p,
            CommandPayload::System(_) => return,
        };
        let team_idx = match cmd.side.team_index() {
            Some(i) => i,
            None => return,
        };
        match p_cmd {
            PlayerCoachCommand::Substitute {
                out_player,
                in_player,
            } => {
                if validate_substitution(&self.state, cmd.side, out_player, in_player, &self.ruleset, false).is_err() {
                    let code = if self.ruleset.roster.fiba_sub_clock_run_reentry_rule {
                        CommandAbortReason::SubIneligibleUntilClockRunAtExecute
                    } else {
                        CommandAbortReason::ActorFouledOut
                    };
                    self.push_event(GameRecordEventPayload::AnchoredCommandAbortedRecord {
                        ingress_seq: cmd.ingress_seq,
                        side: cmd.side as u8,
                        abort_reason_code: code as u8,
                        cmd_tag: p_cmd.cmd_tag(),
                    });
                    return;
                }
                self.apply_substitution(cmd.side, out_player, in_player, 1);
            }
            PlayerCoachCommand::CallTimeout {
                queue_if_live_disallowed,
                advance,
            } => {
                if !self.ruleset.timeouts.live_ball_player_to_allowed
                    && matches!(self.state.game_phase, GamePhase::LiveBall | GamePhase::ShotInFlight | GamePhase::ReboundScramble)
                {
                    if queue_if_live_disallowed {
                        self.state.pending_deadball_timeouts[team_idx] = Some(advance);
                    }
                    return;
                }
                if let Some((reason_tag, dur_ds, charged)) =
                    charge_team_timeout_with_mandatory_slots(&mut self.state, cmd.side, &self.ruleset)
                {
                    if let Some(inb) = self.state.inbound_context.as_mut() {
                        inb.allow_both_sides_advance |= self.ruleset.challenges.mode == 2;
                    }
                    self.state.pending_post_commit_timeout = Some((cmd.side, advance));
                    self.enter_timeout_state(reason_tag, dur_ds, charged);
                } else {
                    self.push_event(GameRecordEventPayload::AnchoredCommandAbortedRecord {
                        ingress_seq: cmd.ingress_seq,
                        side: cmd.side as u8,
                        abort_reason_code: CommandAbortReason::PrerequisiteTimeoutUnavailable as u8,
                        cmd_tag: p_cmd.cmd_tag(),
                    });
                }
            }
            PlayerCoachCommand::SetOffensivePlay {
                play_id,
                force_immediate,
            } => {
                if force_immediate {
                    self.state.active_play_id[team_idx] = play_id;
                    self.state.play_phase_idx[team_idx] = 0;
                    self.state.play_phase_elapsed_ds[team_idx] = 0;
                } else {
                    self.state.pending_next_play_id[team_idx] = Some(play_id);
                }
            }
            PlayerCoachCommand::SetDefensiveGameplan {
                gameplan_id,
                protect_targets,
                pnr_rule_override,
                matchup_override,
            } => {
                self.apply_defensive_gameplan_patch(
                    team_idx,
                    gameplan_id,
                    protect_targets,
                    pnr_rule_override,
                    matchup_override,
                );
            }
            PlayerCoachCommand::SetPaceAndPressure {
                pace,
                full_court_press,
            } => {
                if let PatchField::Set(p) = pace {
                    self.state.active_pace[team_idx] = p;
                }
                if let PatchField::Set(f) = full_court_press {
                    self.state.full_court_press[team_idx] = f;
                }
            }
            PlayerCoachCommand::SetTimeoutAdvance { advance, .. } => {
                if let Some(mut inb) = self.state.inbound_context.take() {
                    self.apply_timeout_advance(&mut inb, cmd.side, advance);
                    self.state.inbound_context = Some(inb);
                } else {
                    self.state.pending_post_commit_timeout = Some((cmd.side, advance));
                }
            }
            PlayerCoachCommand::ChallengeCall {
                target: ChallengeTarget::RecentScoringOrClockEvent { event_seq },
                ..
            } => {
                self.state.fiba_hcc_generation[team_idx] += 1;
                let req_seq = self.state.fiba_hcc_generation[team_idx];
                self.state.pending_fiba_hcc_requests[team_idx] = Some(PendingFibaHccRequest {
                    requester: cmd.side,
                    target_event_seq: event_seq,
                    request_seq: req_seq,
                    enqueued_ingress_seq: cmd.ingress_seq,
                });
                self.push_event(GameRecordEventPayload::CoachChallengeRequested {
                    ruling_id: 0,
                    target_tag: 1,
                    requester: cmd.side as u8,
                    request_seq: req_seq,
                    target_event_seq: event_seq,
                });
            }
            PlayerCoachCommand::WithdrawChallenge {
                target: ChallengeTarget::RecentScoringOrClockEvent { .. },
                expected_request_seq,
                ..
            } => {
                if let Some(req) = self.state.pending_fiba_hcc_requests[team_idx] {
                    if req.request_seq == expected_request_seq {
                        self.state.pending_fiba_hcc_requests[team_idx] = None;
                        self.push_event(GameRecordEventPayload::CoachChallengeWithdrawn {
                            ruling_id: 0,
                            requester: cmd.side as u8,
                            request_seq: expected_request_seq,
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

