// 规则提交模块：暂停进入与恢复、死球时段推进、回合与子阶段复位、违例与判罚提交点、节次结束与终场。

use crate::model::SUB_DS_UNITS;
use crate::rotation::{apply_deadtime_stamina_recovery, update_stamina_live_tick};
use crate::rules::{
    apply_l2m_inbound_advance, apply_q4_timeout_caps, charge_team_timeout_with_mandatory_slots,
    check_auto_mandatory_timeout, compute_shot_clock_reset, dead_time_duration_ds,
    eligible_challenge_sides_for_ruling, inbound_window_permissions, is_team_in_bonus,
    set_clocks_exact_ds, try_apply_comments_iil_deduction, ShotClockScenario,
};
use crate::wire_schema_v1::*;

use super::SimulationEngine;

impl SimulationEngine {
    pub(super) fn apply_timeout_advance(&self, inbound: &mut InboundContext, side: CommandSide, advance: AdvanceOption) {
        if inbound.side != side || inbound.inbound_ball_at_disposal {
            return;
        }
        let team = side.team_index().unwrap();
        let attack_dir = if team == 0 { 1.0 } else { -1.0 };
        let (origin, _) = self.inbound_spot_targets(inbound, attack_dir);
        if origin.0 * attack_dir >= 0.0 {
            return;
        }
        let retained = self.state.ball.last_team_control_side == Some(side)
            && !inbound.post_made_fg && !inbound.post_made_last_ft;
        let dribbled_or_passed = retained && (self.state.backcourt_8s_timer_ds != 0
            || self.state.ball.last_passer_roster_idx.is_some()
            || self.state.ball.owner_slot.is_some_and(|slot| {
                let player = self.state.team_states[team].active_lineup_slots[usize::from(slot) % 5];
                self.state.players.get(usize::from(player)).is_some_and(|p| p.dribbles_since_catch > 0)
            }));
        let from_foul = self.state.shot_clock_ds.is_some()
            && inbound.pending_shot_clock_ds.is_some_and(|pending| {
                pending > self.state.shot_clock_ds.unwrap()
            });
        apply_l2m_inbound_advance(
            inbound, advance, inbound.sideline_side, self.state.quarter, self.state.game_clock_ds,
            self.state.team_states[team].consecutive_timeouts_used_in_deadball,
            dribbled_or_passed, from_foul, &self.ruleset,
        );
    }

    pub(super) fn enter_timeout_state(&mut self, reason_tag: u8, duration_ds: u16, charged_side: CommandSide) {
        let suspended = match self.state.game_phase {
            GamePhase::InboundSetup => self.state.inbound_context.take().map(SuspendedGameState::Inbound),
            GamePhase::JumpBall => self.state.jump_ball_context.take().map(SuspendedGameState::JumpBall),
            GamePhase::FreeThrows => Some(SuspendedGameState::FreeThrows),
            _ => {
                // LiveBall timeout: suspend as Inbound at NearestToInterruption
                let side = self.state.possession_side.unwrap_or(charged_side);
                let [x, y, _] = self.state.ball.position_m;
                self.state.oob_spot_xy = [x, y];
                self.state.oob_is_endline = false;
                Some(SuspendedGameState::Inbound(InboundContext {
                    side,
                    spot: InboundSpot::NearestToInterruption,
                    sideline_side: CourtSideLine::from_y_m(y),
                    allow_endline_run: false,
                    stop_clock_until_touch: true,
                    inbound_ball_at_disposal: false,
                    inbound_touched_court_player: false,
                    inbound_5s_timer_ds: 0,
                    original_spot: InboundSpot::NearestToInterruption,
                    pending_shot_clock_ds: self.state.shot_clock_ds,
                    is_period_start_arrow_inbound: false,
                    allow_both_sides_advance: false,
                    post_made_fg: false,
                    post_made_last_ft: false,
                }))
            }
        };
        if let Some(s) = suspended {
            let _ = self.state.suspended_states.push(s);
        }
        if let Some(idx) = charged_side.team_index() {
            self.state.team_states[idx].consecutive_timeouts_used_in_deadball =
                self.state.team_states[idx]
                    .consecutive_timeouts_used_in_deadball
                    .saturating_add(1);
        }
        self.state.game_clock_running = false;
        self.state.shot_clock_running = false;
        self.state.game_phase = GamePhase::Timeout;
        self.state.ball.pass_ctx = None;
        self.state.ball.shot_ctx = None;
        self.state.ball.owner_slot = None;
        self.state.ball.ball_state = BallState::Dead;
        self.state.dead_time_pending = Some(DeadTimeRecord {
            duration_ds,
            reason_tag,
        });
        self.push_event(GameRecordEventPayload::TimeoutGranted {
            reason_tag,
            charged_side: charged_side as u8,
            home_to_rem: self.state.team_states[0].timeouts_remaining,
            away_to_rem: self.state.team_states[1].timeouts_remaining,
        });
    }

    pub(super) fn step_deadtime_timeout(&mut self) {
        self.state.game_clock_running = false;
        self.state.game_clock_ticked_this_step = false;
        self.state.shot_clock_running = false;
        let dt = self.state.dead_time_pending.take().unwrap_or(DeadTimeRecord {
            duration_ds: self.ruleset.timeouts.team_to_ds,
            reason_tag: 0,
        });
        self.push_event(GameRecordEventPayload::DeadTime {
            duration_ds: dt.duration_ds,
            reason_tag: dt.reason_tag,
        });
        apply_deadtime_stamina_recovery(&mut self.state.players, dt.reason_tag, &self.tunables);

        // Decay team momentum on timeout
        let decay = 1.0 - self.tunables.timeout_momentum_decay_ratio;
        for ts in self.state.team_states.iter_mut() {
            ts.team_momentum *= decay;
        }
        if dt.reason_tag == 1 && self.state.mandatory_timeout_state.slot_under_3m_taken {
            self.state.mandatory_timeout_state.slot_under_3m_completed = true;
        }
        let advance_request = self.state.pending_post_commit_timeout.take();
        apply_q4_timeout_caps(&mut self.state, &self.ruleset);

        // AI substitutions during timeout
        self.execute_stage2_coach_and_rotation();

        // Restore suspended state
        match self.state.suspended_states.pop() {
            Some(SuspendedGameState::Inbound(mut inb)) => {
                inb.inbound_ball_at_disposal = false;
                if let Some((adv_side, adv)) = advance_request {
                    self.apply_timeout_advance(&mut inb, adv_side, adv);
                }
                self.state.inbound_context = Some(inb);
                self.state.game_phase = GamePhase::InboundSetup;
            }
            Some(SuspendedGameState::JumpBall(jb)) => {
                self.state.jump_ball_context = Some(jb);
                self.state.game_phase = GamePhase::JumpBall;
            }
            Some(SuspendedGameState::FreeThrows) => {
                self.state.game_phase = GamePhase::FreeThrows;
            }
            None => {
                self.state.game_phase = GamePhase::InboundSetup;
            }
        }
        self.reset_possession_epoch();
        self.reset_substage_counters();
    }

    pub(super) fn reset_substage_counters(&mut self) {
        self.state.shot_sub_stage = 0;
        self.state.shot_stage_ticks = 0;
        self.state.shot_gc_expired_pending = false;
        self.state.scramble_ticks = 0;
        self.state.inbound_sub_stage = 0;
        self.state.inbound_stage_ticks = 0;
        self.state.ft_sub_stage = 0;
        self.state.ft_stage_ticks = 0;
        self.state.ft_pending_made = false;
    }

    pub(super) fn reset_active_player_transient_states(&mut self) {
        for p in self.state.players.iter_mut() {
            p.z_m = 0.0;
            p.vz_m_s = 0.0;
            p.oob_reentry_pending = false;
            p.dribbles_since_catch = 0;
            p.lgp_established_tick = None;
            p.paint_timer_ds = 0;
            p.exiting_paint_grace_ticks = 0;
            p.offball_cut_kind = 0;
            p.offball_cut_timer_ds = 0;
            p.offball_cut_side = 1;
            p.help_reaction_timer_ds = 0;
            p.help_target_valid = false;
            p.help_committed_xy = [0.0, 0.0];
        }
    }

    pub(super) fn step_deadtime_period_break(&mut self) {
        self.state.game_clock_running = false;
        self.state.game_clock_ticked_this_step = false;
        self.state.shot_clock_running = false;
        let reason_tag = if self.state.quarter == 2 { 4 } else { 3 };
        let dur = dead_time_duration_ds(&self.ruleset, reason_tag, self.state.quarter);
        self.push_event(GameRecordEventPayload::DeadTime {
            duration_ds: dur,
            reason_tag,
        });
        apply_deadtime_stamina_recovery(&mut self.state.players, reason_tag, &self.tunables);

        // Advance to next quarter
        self.state.quarter += 1;
        let q_sec = if self.state.quarter <= 4 {
            self.ruleset.clocks.quarter_sec
        } else {
            self.ruleset.clocks.ot_sec
        };
        set_clocks_exact_ds(
            &mut self.state,
            q_sec * 10,
            Some(self.ruleset.clocks.full_shot_clock_ds),
        );
        self.state.mandatory_timeout_state = MandatoryTimeoutState::default();

        // Team fouls & timeout caps per league
        if self.state.quarter <= 4 {
            for ts in self.state.team_states.iter_mut() {
                ts.period_team_fouls = 0;
                ts.l2m_period_fouls = 0;
            }
            if self.state.quarter == 4 {
                let cap = self.ruleset.timeouts.q4_enter_cap;
                for ts in self.state.team_states.iter_mut() {
                    ts.timeouts_remaining = ts.timeouts_remaining.min(cap);
                }
            } else if self.state.quarter == 3 && self.ruleset.timeouts.h2_to > 0 {
                // FIBA H2 reset if configured
                if matches!(
                    self.ruleset.clocks.shot_clock_policy,
                    ShotClockResetPolicy::FibaArt29
                ) {
                    for ts in self.state.team_states.iter_mut() {
                        ts.timeouts_remaining = self.ruleset.timeouts.h2_to;
                    }
                }
            }
        } else {
            // OT
            if self.ruleset.fouls.ot_team_foul_reset_each {
                for ts in self.state.team_states.iter_mut() {
                    ts.period_team_fouls = 0;
                    ts.l2m_period_fouls = 0;
                }
            }
            if self.ruleset.timeouts.ot_to > 0 {
                for ts in self.state.team_states.iter_mut() {
                    ts.timeouts_remaining = self.ruleset.timeouts.ot_to;
                }
            }
        }

        // AI subs during break
        self.execute_stage2_coach_and_rotation();

        self.reset_possession_epoch();

        // Determine period start mode
        let is_nba = self.ruleset.fouls.possession_start_mode == 0;
        let (start_mode, inbounding_side, spot) = if is_nba && self.state.quarter > 4 {
            (0u8, 0xFF, InboundSpot::BackcourtEndline)
        } else if is_nba {
            let winner = self.state.opening_tip_winner.unwrap_or(CommandSide::Home);
            let side = if self.state.quarter == 4 {
                winner
            } else {
                winner.opponent()
            };
            (1u8, side as u8, InboundSpot::BackcourtEndline)
        } else {
            (
                1u8,
                self.state.possession_arrow as u8,
                InboundSpot::CenterLineExtendedOppositeTable,
            )
        };

        if start_mode == 0 {
            self.state.game_phase = GamePhase::JumpBall;
            self.state.ball.last_team_control_side = None;
            self.state.shot_clock_ds = None;
            self.state.shot_clock_rem_10us = None;
            self.state.jump_ball_context = Some(JumpBallContext {
                spot: JumpBallSpot::CenterCircle,
                jumpers: [self.state.team_states[0].active_lineup_slots[4], self.state.team_states[1].active_lineup_slots[4]],
                toss_timer_ds: 10,
                ball_tossed: false,
                ball_tapped: false,
                is_opening_tip: false,
            });
        } else {
            let side = if inbounding_side == 0 {
                CommandSide::Home
            } else {
                CommandSide::Away
            };
            self.state.game_phase = GamePhase::InboundSetup;
            self.reset_substage_counters();
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
                pending_shot_clock_ds: Some(self.ruleset.clocks.full_shot_clock_ds),
                is_period_start_arrow_inbound: !is_nba,
                allow_both_sides_advance: false,
                post_made_fg: false,
                post_made_last_ft: false,
            });
        }
        self.push_event(GameRecordEventPayload::PeriodStarted {
            quarter: self.state.quarter,
            start_mode,
            inbounding_side,
            inbound_spot: spot as u8,
            arrow_side: if is_nba {
                0xFF
            } else {
                self.state.possession_arrow as u8
            },
        });
    }

    pub(super) fn reset_possession_epoch(&mut self) {
        self.state.possession_id = self.state.possession_id.saturating_add(1);
        self.state.possession_tick = 0;
        self.state.play_phase_idx = [0, 0];
        self.state.play_phase_elapsed_ds = [0, 0];
        self.state.active_drive_corridor = 0;
        self.state.play_screen_contact_ds = [0, 0];
        self.state.transition_window_ds = [0, 0];
        self.state.ball.last_passer_roster_idx = None;
        self.state.ball.last_pass_completed_tick = 0;
        self.state.screen_ready_ticks = [0, 0];
        self.state.active_drive_target_xy = [0.0, 0.0];
        self.state.pnr_read_branch = [0, 0];
        self.state.xout_rotator_slot = [0xFF, 0xFF];
        self.state.xout_timer_ds = [0, 0];
        for p in self.state.players.iter_mut() {
            p.offball_cut_kind = 0;
            p.offball_cut_timer_ds = 0;
            p.help_target_valid = false;
            p.help_reaction_timer_ds = 0;
        }
    }

    pub(super) fn trigger_turnover_stoppage(&mut self, rule_code: RuleCode, off_side: CommandSide, offender_r: u8) {
        let off_idx = off_side.team_index().unwrap_or(0);
        self.record_defense_possession_outcome(1 - off_idx, off_idx, 0);
        self.push_event(GameRecordEventPayload::ViolationTriggered {
            offender_roster_idx: offender_r,
            team_side: off_side as u8,
            rule_code: rule_code as u16,
            footwork: FootworkTelemetryPublic::default(),
        });
        self.push_event(GameRecordEventPayload::TurnoverCommitted {
            offender_roster_idx: offender_r,
            team_side: off_side as u8,
            turnover_rule_code: rule_code as u16,
            stealer_roster_idx: 0xFF,
            is_live_steal: 0,
        });
        let mut spot = [self.state.ball.position_m[0], self.state.ball.position_m[1]];
        if rule_code == RuleCode::VIO_OFF_3S {
            let attack_dir = if off_side == CommandSide::Home { 1.0 } else { -1.0 };
            spot = [
                attack_dir * (self.geom.half_length_m - self.geom.paint_length_m),
                self.state.players[usize::from(offender_r)].y_m,
            ];
        }
        self.state.oob_spot_xy = spot;
        self.state.oob_is_endline = false;
        let rid = self.state.next_ruling_id;
        self.state.next_ruling_id += 1;
        let call = RuleCallSnapshot {
            rule_code,
            offending_side: off_side,
            offender_roster_idx: offender_r,
            interruption_spot: InboundSpot::NearestToInterruption,
            stoppage_sideline_side: CourtSideLine::from_y_m(spot[1]),
            restart_shot_clock_ds: compute_shot_clock_reset(
                ShotClockScenario::TurnoverOpponentToOwnBackcourt,
                self.state.shot_clock_ds,
                self.state.game_clock_ds,
                &self.ruleset,
            ),
            ..RuleCallSnapshot::default()
        };
        self.open_pending_ruling(rid, self.state.sim_tick, call, false);
    }

    pub(super) fn advance_backcourt_count(&mut self, side: CommandSide, handler: u8) -> bool {
        if !self.state.game_clock_ticked_this_step {
            return false;
        }
        self.state.backcourt_8s_timer_ds = self.state.backcourt_8s_timer_ds.saturating_add(1);
        if self.state.backcourt_8s_timer_ds < 80 {
            return false;
        }
        self.trigger_turnover_stoppage(RuleCode::VIO_8S, side, handler);
        true
    }

    pub(super) fn trigger_shot_clock_violation(&mut self) {
        let off_side = self.state.possession_side.unwrap_or(CommandSide::Home);
        let t_idx = off_side.team_index().unwrap_or(0);
        let offender_r = self.state.team_states[t_idx].active_lineup_slots[0];
        self.trigger_turnover_stoppage(RuleCode::VIO_SHOT_CLOCK, off_side, offender_r);
    }

    pub(super) fn open_pending_ruling(
        &mut self,
        ruling_id: u64,
        whistle_tick: u32,
        call: RuleCallSnapshot,
        period_expired: bool,
    ) {
        self.state.game_clock_running = false;
        self.state.shot_clock_running = false;
        self.state.game_phase = GamePhase::WhistlePendingCommit;
        self.state.ball.pass_ctx = None;
        self.state.ball.shot_ctx = None;
        self.state.ball.owner_slot = None;
        self.state.ball.ball_state = BallState::Dead;
        let eligible = eligible_challenge_sides_for_ruling(
            &call,
            self.state.quarter,
            self.state.game_clock_ds,
            &self.ruleset,
        );
        // Check Stage 6a automatic mandatory timeout wrapping
        let mand = if period_expired {
            None
        } else {
            check_auto_mandatory_timeout(
                &mut self.state, &self.ruleset, self.canonical_team_ids, false,
            )
        };
        let window_kind = match mand {
            Some(side) => ChallengeWindowKind::TimeoutPending {
                reason: TimeoutReason::Mandatory { team_side: side },
            },
            None => ChallengeWindowKind::LiveDeadball,
        };
        let ruling = PendingRuling {
            ruling_id,
            whistle_tick,
            window_open_tick: self.state.sim_tick,
            request_seq: 0,
            window_epoch: 0,
            challenge_attempts_in_window: 0,
            window_state: ChallengeWindowState::Open,
            window_kind,
            perceived_call: call,
            ground_truth: call,
            eligible_sides: eligible,
            period_expired_after_ruling: period_expired,
        };
        self.push_event(GameRecordEventPayload::RefereeInitialCall {
            ruling_id,
            offending_side: call.offending_side as u8,
            offender_roster_idx: call.offender_roster_idx,
            initial_rule_code: call.rule_code as u16,
        });
        self.push_event(GameRecordEventPayload::ChallengeWindowOpenedRecord {
            ruling_id,
            whistle_tick,
            window_open_tick: self.state.sim_tick,
            window_epoch: 0,
            initial_rule_code: call.rule_code as u16,
            eligible_sides_mask: eligible.0,
            window_kind_tag: window_kind.window_kind_tag(),
        });
        self.state.active_pending_ruling = Some(ruling);
    }

    pub(super) fn execute_commit_point(&mut self) {
        let ruling = match self.state.active_pending_ruling.take() {
            Some(r) => r,
            None => return,
        };
        let call = ruling.perceived_call;
        let mut resulting_side = call.offending_side.opponent();
        let mut ft_enqueued = 0u8;

        // If foul, assess personal/team foul & check bonus
        if call.rule_code.is_foul() {
            let off_idx = call.offending_side.team_index().unwrap_or(0);
            let r = call.offender_roster_idx;
            let mut flags = 0u8;
            let mut counts_team = 0u8;
            if (r as usize) < 30 {
                let p = &mut self.state.players[r as usize];
                if call.rule_code.is_personal_foul() {
                    p.personal_fouls += 1;
                    let is_off_foul = call.rule_code.is_offensive_foul();
                    if self.ruleset.fouls.offensive_foul_counts_toward_team_fouls
                        || !is_off_foul
                        || p.fouled_out_staying_on_court
                    {
                        let team = &mut self.state.team_states[off_idx];
                        team.period_team_fouls = team.period_team_fouls.saturating_add(1);
                        if self.state.game_clock_ds <= 1200 {
                            team.l2m_period_fouls = team.l2m_period_fouls.saturating_add(1);
                        }
                        counts_team = 1;
                    }
                    if p.personal_fouls >= self.ruleset.fouls.foul_out_limit {
                        flags |= 0x01;
                        if p.foul_out_seq.is_none() {
                            p.foul_out_seq = Some(self.state.next_foul_out_seq);
                            self.state.next_foul_out_seq += 1;
                        }
                    }
                }
            }
            self.push_event(GameRecordEventPayload::FoulAssessedRecord {
                ruling_id: ruling.ruling_id,
                rule_code: call.rule_code as u16,
                offending_side: call.offending_side as u8,
                offender_roster_idx: r,
                foul_category: call.foul_category,
                counts_team_foul: counts_team,
                resulting_personal_fouls: if (r as usize) < 30 {
                    self.state.players[r as usize].personal_fouls
                } else {
                    0
                },
                ejected_or_fouled_out_flags: flags,
            });

            // 活球结束当拍维持帧上的计时阵容；换人最迟在下一拍恢复比赛前处理。
            if flags & 1 != 0 && !self.state.game_clock_ticked_this_step
                && !self.state.players[r as usize].fouled_out_staying_on_court
            {
                self.replace_disqualified_player(call.offending_side, r);
            }

            let ft_cnt = if call.ft_count > 0 {
                call.ft_count
            } else if counts_team != 0 && !call.rule_code.is_offensive_foul()
                && is_team_in_bonus(
                    &self.state.team_states[off_idx], self.state.quarter, self.state.game_clock_ds,
                    &self.ruleset.fouls, 0,
                )
            {
                2
            } else {
                0
            };
            if ft_cnt > 0 {
                ft_enqueued = ft_cnt;
                let shooter_r = call
                    .victim_roster_idx
                    .unwrap_or(self.state.team_states[1 - off_idx].active_lineup_slots[0]);
                self.state.ft_series_queue.push(FtSeriesSpec {
                    shooter_roster_idx: shooter_r,
                    total_shots: ft_cnt,
                    shots_remaining: ft_cnt,
                    is_technical_or_flagrant: matches!(
                        call.rule_code,
                        RuleCode::FOUL_FLAGRANT_1
                            | RuleCode::FOUL_FLAGRANT_2
                            | RuleCode::FOUL_UNSPORTSMANLIKE
                            | RuleCode::FOUL_DISQUALIFYING
                            | RuleCode::FOUL_TECH_CONDUCT
                            | RuleCode::FOUL_TECH_EXCESSIVE_TIMEOUT
                            | RuleCode::FOUL_TECH_FLOPPING
                    ),
                    on_complete: if call.rule_code == RuleCode::FOUL_TECH_EXCESSIVE_TIMEOUT {
                        PostFtOutcome::RetainedInbound {
                            side: resulting_side,
                            spot: InboundSpot::NearestToInterruption,
                        }
                    } else {
                        PostFtOutcome::LiveReboundOnLastMiss
                    },
                });
                self.state.game_phase = GamePhase::FreeThrows;
            }
        }

        if call.rule_code == RuleCode::VIO_DEF_3S {
            let off_idx = call.offending_side.team_index().unwrap_or(0);
            self.push_event(GameRecordEventPayload::ViolationTriggered {
                offender_roster_idx: call.offender_roster_idx,
                team_side: call.offending_side as u8,
                rule_code: call.rule_code as u16,
                footwork: FootworkTelemetryPublic::default(),
            });
            resulting_side = call.offending_side.opponent();
            let ft_shooter = call
                .victim_roster_idx
                .filter(|&r| (r as usize) < 30 && self.state.players[r as usize].on_court_slot.is_some())
                .unwrap_or(self.state.team_states[1 - off_idx].active_lineup_slots[0]);
            ft_enqueued = 1;
            self.state.ft_series_queue.push(FtSeriesSpec {
                shooter_roster_idx: ft_shooter,
                total_shots: 1,
                shots_remaining: 1,
                is_technical_or_flagrant: true,
                on_complete: PostFtOutcome::RetainedInbound {
                    side: resulting_side,
                    spot: InboundSpot::FrontcourtSideline28ft,
                },
            });
            self.state.game_phase = GamePhase::FreeThrows;
        } else if call.rule_code == RuleCode::HELD_BALL && self.ruleset.fouls.possession_start_mode == 1 {
            resulting_side = self.state.possession_arrow;
        }

        if call.rule_code == RuleCode::NONE && !call.basket_made_on_play {
            resulting_side = self.state.possession_side.unwrap_or(CommandSide::Home);
        }

        let sc = if call.rule_code == RuleCode::HELD_BALL {
            if self.ruleset.fouls.possession_start_mode == 0
                || self.state.ball.last_team_control_side == Some(resulting_side)
            {
                self.state.shot_clock_ds
            } else {
                let attack_dir = if resulting_side == CommandSide::Home { 1.0 } else { -1.0 };
                let scenario = if self.state.oob_spot_xy[0] * attack_dir > 0.0 {
                    ShotClockScenario::TurnoverOpponentToOwnFrontcourt
                } else {
                    ShotClockScenario::TurnoverOpponentToOwnBackcourt
                };
                compute_shot_clock_reset(
                    scenario, self.state.shot_clock_ds, self.state.game_clock_ds, &self.ruleset,
                )
            }
        } else if call.rule_code == RuleCode::VIO_DEF_3S {
            compute_shot_clock_reset(
                ShotClockScenario::OffenseFrontcourtNonShotDefFoul,
                self.state.shot_clock_ds,
                self.state.game_clock_ds,
                &self.ruleset,
            )
        } else {
            call.restart_shot_clock_ds.or_else(|| {
                compute_shot_clock_reset(
                    ShotClockScenario::TurnoverOpponentToOwnBackcourt,
                    self.state.shot_clock_ds,
                    self.state.game_clock_ds,
                    &self.ruleset,
                )
            })
        };
        if ft_enqueued != 0 {
            self.state.shot_clock_ds = sc;
            self.state.shot_clock_rem_10us = sc.map(|ds| u32::from(ds) * SUB_DS_UNITS);
        }
        self.push_event(GameRecordEventPayload::RulingFinalized {
            ruling_id: ruling.ruling_id,
            final_rule_code: call.rule_code as u16,
            offending_side: call.offending_side as u8,
            offender_roster_idx: call.offender_roster_idx,
            points_delta_signed: 0,
            ft_shots_enqueued: ft_enqueued,
            resulting_possession_side: resulting_side as u8,
            foul_out_or_ejected_flag: 0,
            restored_game_clock_ds: self.state.game_clock_ds,
            new_shot_clock_ds: encode_shot_clock_opt(sc),
        });

        if ft_enqueued > 0 {
            self.reset_substage_counters();
            self.state.game_phase = GamePhase::FreeThrows;
        } else if ruling.period_expired_after_ruling || self.state.game_clock_rem_10us == 0 {
            self.handle_period_expiration();
        } else if call.rule_code == RuleCode::HELD_BALL {
            if self.ruleset.fouls.possession_start_mode == 0 {
                let mut home_r = self.state.team_states[0].active_lineup_slots[4];
                let mut away_r = self.state.team_states[1].active_lineup_slots[4];
                for opt_r in [Some(call.offender_roster_idx), call.victim_roster_idx] {
                    if let Some(rr) = opt_r {
                        if (rr as usize) < 30 && self.state.players[rr as usize].on_court_slot.is_some() {
                            if (rr as usize) < 15 {
                                home_r = rr;
                            } else {
                                away_r = rr;
                            }
                        }
                    }
                }
                if home_r == ROSTER_IDX_NONE {
                    home_r = self.state.team_states[0].active_lineup_slots[0];
                }
                if away_r == ROSTER_IDX_NONE {
                    away_r = self.state.team_states[1].active_lineup_slots[0];
                }
                self.state.game_phase = GamePhase::JumpBall;
                self.reset_substage_counters();
                self.state.shot_clock_running = false;
                self.state.jump_ball_context = Some(JumpBallContext {
                    spot: JumpBallSpot::NearestCircle,
                    jumpers: [home_r, away_r],
                    toss_timer_ds: 5,
                    ball_tossed: false,
                    ball_tapped: false,
                    is_opening_tip: false,
                });
            } else {
                let arrow_side = self.state.possession_arrow;
                self.state.game_phase = GamePhase::InboundSetup;
                self.reset_substage_counters();
                self.state.inbound_context = Some(InboundContext {
                    side: arrow_side,
                    spot: InboundSpot::NearestToInterruption,
                    sideline_side: call.stoppage_sideline_side,
                    allow_endline_run: false,
                    stop_clock_until_touch: true,
                    inbound_ball_at_disposal: false,
                    inbound_touched_court_player: false,
                    inbound_5s_timer_ds: 0,
                    original_spot: InboundSpot::NearestToInterruption,
                    pending_shot_clock_ds: sc,
                    is_period_start_arrow_inbound: true,
                    allow_both_sides_advance: false,
                    post_made_fg: false,
                    post_made_last_ft: false,
                });
            }
        } else {
            self.state.game_phase = GamePhase::InboundSetup;
            self.reset_substage_counters();
            self.state.inbound_context = Some(InboundContext {
                side: resulting_side,
                spot: call.interruption_spot,
                sideline_side: call.stoppage_sideline_side,
                allow_endline_run: call.basket_made_on_play && call.rule_code == RuleCode::NONE,
                stop_clock_until_touch: true,
                inbound_ball_at_disposal: false,
                inbound_touched_court_player: false,
                inbound_5s_timer_ds: 0,
                original_spot: call.interruption_spot,
                pending_shot_clock_ds: sc,
                is_period_start_arrow_inbound: false,
                allow_both_sides_advance: false,
                post_made_fg: call.basket_made_on_play && call.rule_code == RuleCode::NONE,
                post_made_last_ft: false,
            });
        }

        if matches!(self.state.game_phase, GamePhase::GameOver | GamePhase::PeriodBreak) {
            self.state.pending_post_commit_timeout = None;
            self.state.pending_deadball_timeouts = [None; 2];
        } else if let ChallengeWindowKind::TimeoutPending {
            reason: TimeoutReason::Mandatory { team_side },
        } = ruling.window_kind {
            self.enter_timeout_state(1, self.ruleset.timeouts.mandatory_to_ds, team_side);
        } else if !self.consume_queued_timeout() {
            self.state.pending_post_commit_timeout = None;
        }
    }

    pub(super) fn consume_queued_timeout(&mut self) -> bool {
        let (allowed, first) = match self.state.game_phase {
            GamePhase::InboundSetup => {
                let Some(inbound) = &self.state.inbound_context else { return false };
                let p = inbound_window_permissions(&self.state, inbound, &self.ruleset);
                ([p.home_can_timeout, p.away_can_timeout], inbound.side)
            }
            GamePhase::JumpBall => ([true; 2], CommandSide::Home),
            GamePhase::FreeThrows if self.ruleset.timeouts.to_between_fts_allowed => {
                ([true; 2], self.state.possession_side.unwrap_or(CommandSide::Home))
            }
            _ => return false,
        };
        for side in [first, first.opponent()] {
            let team = side.team_index().unwrap();
            if !allowed[team] {
                continue;
            }
            let Some(advance) = self.state.pending_deadball_timeouts[team].take() else { continue };
            if let Some((reason, duration, charged)) =
                charge_team_timeout_with_mandatory_slots(&mut self.state, side, &self.ruleset)
            {
                if self.state.pending_post_commit_timeout.is_none() {
                    self.state.pending_post_commit_timeout = Some((side, advance));
                }
                if let Some(inbound) = self.state.inbound_context.as_mut() {
                    if self.ruleset.clocks.shot_clock_policy == ShotClockResetPolicy::FibaArt29 {
                        inbound.allow_both_sides_advance = true;
                    }
                }
                self.enter_timeout_state(reason, duration, charged);
                return true;
            }
        }
        false
    }

    pub(super) fn execute_stage7_dynamics(&mut self) {
        if !self.state.game_clock_ticked_this_step && !self.state.game_clock_running {
            return;
        }
        // Live clock tick: update stamina and court_time_live_ds
        for team_idx in 0..2 {
            let offset = team_idx * 15;
            for (rank, snap) in self.teams[team_idx].players.iter().enumerate() {
                let r = offset + rank;
                let on_court = self.state.players[r].on_court_slot.is_some();
                if on_court && self.state.game_clock_ticked_this_step {
                    self.state.players[r].court_time_live_ds += 1;
                }
                let spd = libm::hypot(self.state.players[r].vx_m_s, self.state.players[r].vy_m_s);
                update_stamina_live_tick(
                    &mut self.state.players[r],
                    snap,
                    on_court,
                    spd,
                    false,
                    &self.tunables,
                );
            }
        }
        apply_q4_timeout_caps(&mut self.state, &self.ruleset);
    }

    pub(super) fn handle_period_expiration(&mut self) {
        self.reset_substage_counters();
        self.state.ball_fx_flags = 0;
        self.state.ball_fx_timer_ds = 0;
        self.state.slot_contact_timer_ds = [0; 10];
        self.state.game_clock_running = false;
        self.state.shot_clock_running = false;
        self.state.ball.pass_ctx = None;
        self.state.ball.shot_ctx = None;
        self.state.ball.owner_slot = None;
        self.state.ball.ball_state = BallState::Dead;
        let h_pts = self.state.team_states[0].score_on_court;
        let a_pts = self.state.team_states[1].score_on_court;
        if self.state.quarter >= 4 && h_pts != a_pts {
            self.end_game_normal_or_cap(GameEndReason::Normal);
        } else {
            self.state.game_phase = GamePhase::PeriodBreak;
        }
    }

    pub(super) fn end_game_normal_or_cap(&mut self, reason: GameEndReason) {
        self.state.game_clock_running = false;
        self.state.shot_clock_running = false;
        self.state.game_phase = GamePhase::GameOver;
        self.state.end_reason = Some(reason);
        self.state.command_queue.clear();
    }

    pub(super) fn record_game_ended(&mut self, reason: GameEndReason) {
        let h = self.state.team_states[0].score_on_court;
        let a = self.state.team_states[1].score_on_court;
        let winner = if h > a {
            0
        } else if a > h {
            1
        } else {
            0xFF
        };
        let mut court_times = [0u32; 30];
        for (i, p) in self.state.players.iter().enumerate() {
            court_times[i] = p.court_time_live_ds;
        }
        self.push_event(GameRecordEventPayload::GameEnded {
            score_home_official: h,
            score_away_official: a,
            on_court_score_home: h,
            on_court_score_away: a,
            winner_side: winner,
            end_reason: reason as u8,
            final_quarter: self.state.quarter,
            final_court_time_ds: FinalCourtTimeDs(court_times),
        });
    }

    /// Expose direct Comments II-L helper for deterministic verification & rule enforcement.
    pub fn trigger_comments_iil_if_needed(&mut self) {
        if let Some((delta_ds, new_gc_ds, new_sc_wire)) =
            try_apply_comments_iil_deduction(&mut self.state, &self.ruleset)
        {
            let mut mask = 0u16;
            let mut indices = [0xFFu8; 10];
            for team_idx in 0..2 {
                for p_pos in 0..5 {
                    let s = team_idx * 5 + p_pos;
                    let r = self.state.team_states[team_idx].active_lineup_slots[p_pos];
                    if r != ROSTER_IDX_NONE {
                        mask |= 1u16 << s;
                        indices[s] = r;
                        let elapsed = &mut self.state.players[r as usize].court_time_live_ds;
                        *elapsed = elapsed.saturating_add_signed(-i32::from(delta_ds));
                    }
                }
            }
            self.push_event(GameRecordEventPayload::ClockAdjusted {
                adjust_reason: 0,
                delta_game_clock_ds_signed: delta_ds,
                new_game_clock_ds: new_gc_ds,
                new_shot_clock_ds: new_sc_wire,
                affected_slot_mask: mask,
                affected_roster_indices: indices,
            });
        }
    }
}
