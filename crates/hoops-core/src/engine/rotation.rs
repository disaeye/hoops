// 轮换模块：换人执行、犯满或驱逐后的强制补位与 Stage 2 教练轮换。

use crate::rotation::{
    is_mandatory_out, is_player_eligible_for_sub_in, plan_ai_substitutions,
    update_clutch_coaching_flags,
};
use crate::rules::{inbound_window_permissions, is_team_in_bonus};
use crate::wire_schema_v1::*;

use super::SimulationEngine;

impl SimulationEngine {
    pub fn apply_substitution(
        &mut self,
        side: CommandSide,
        out_r: u8,
        in_r: u8,
        tier: u8,
    ) {
        let team_idx = match side.team_index() {
            Some(i) => i,
            None => return,
        };
        let slot_pos = match self.state.players[out_r as usize].on_court_slot {
            Some(s) => s,
            None => return,
        };
        let lineup_idx = (slot_pos as usize) % 5;
        let out_time = self.state.players[out_r as usize].court_time_live_ds;
        let out_x = self.state.players[out_r as usize].x_m;
        let out_y = self.state.players[out_r as usize].y_m;

        let p1_forced = self.state.players[out_r as usize].ejected
            || self.state.players[out_r as usize].locked_out_for_game
            || self.state.players[out_r as usize].foul_out_seq.is_some()
            || !matches!(
                self.state.players[out_r as usize].injury_state,
                PlayerInjuryState::Healthy
            );

        self.state.players[out_r as usize].on_court_slot = None;
        self.state.players[out_r as usize].stint_lock_remaining_live_ticks = 0;

        self.state.players[in_r as usize].on_court_slot = Some(slot_pos);
        self.state.players[in_r as usize].x_m = out_x;
        self.state.players[in_r as usize].y_m = out_y;
        self.state.players[in_r as usize].vx_m_s = 0.0;
        self.state.players[in_r as usize].vy_m_s = 0.0;
        self.state.players[in_r as usize].stint_lock_remaining_live_ticks =
            self.tunables.rotation_p0_stint_lock_live_ticks;

        if self.ruleset.roster.fiba_sub_clock_run_reentry_rule {
            if !p1_forced {
                self.state.players[out_r as usize].fiba_sub_reentry_lock =
                    FibaSubReentryLock::SubbedOutCannotReenter;
            }
            self.state.players[in_r as usize].fiba_sub_reentry_lock =
                FibaSubReentryLock::SubbedInCannotLeave;
        }

        self.state.team_states[team_idx].active_lineup_slots[lineup_idx] = in_r;
        // 被锁定对位的任一球员下场即解除对应球队的手动对位锁
        for plan in &mut self.state.defense_plans {
            plan.release_matchup_lock_for_player(out_r);
        }

        if self.ruleset.challenges.mode == 2 {
            if let Some(ref mut inb) = self.state.inbound_context {
                if inb.post_made_fg && !inb.inbound_ball_at_disposal && side == inb.side {
                    // FIBA OBRI 18/19-4(b): non-scorer L2M sub unlocks BOTH teams for sub & timeout!
                    inb.allow_both_sides_advance = true;
                }
            }
        }

        self.push_event(GameRecordEventPayload::SubstitutionApplied {
            team_side: team_idx as u8,
            out_roster_idx: out_r,
            in_roster_idx: in_r,
            sub_priority_tier: tier,
            out_player_cum_court_time_ds: out_time,
        });
    }

    pub(super) fn replace_disqualified_player(&mut self, side: CommandSide, out_r: u8) {
        let team_idx = side.team_index().unwrap();
        let on_court = self.state.team_states[team_idx].active_lineup_slots
            .iter().filter(|&&r| r != ROSTER_IDX_NONE).count();
        let offset = team_idx * 15;
        let candidates = || (offset..offset + self.teams[team_idx].players.len())
            .filter(|&r| is_player_eligible_for_sub_in(
                &self.state.players[r], &self.ruleset, on_court, true,
            ));
        let replacement = candidates().max_by(|&a, &b| {
            let priority = |r: usize| {
                let player = &self.state.players[r];
                (
                    player.fiba_sub_reentry_lock != FibaSubReentryLock::SubbedOutCannotReenter,
                    player.cumulative_stamina,
                    self.teams[team_idx].players[r - offset].functional.scores[0],
                )
            };
            let a_key = priority(a);
            let b_key = priority(b);
            a_key.0.cmp(&b_key.0)
                .then(a_key.1.total_cmp(&b_key.1))
                .then(a_key.2.total_cmp(&b_key.2))
                .then(b.cmp(&a))
        });
        if let Some(in_r) = replacement {
            self.apply_substitution(side, out_r, in_r as u8, 0);
        }
    }

    pub(super) fn execute_stage2_coach_and_rotation(&mut self) {
        if matches!(self.state.game_phase, GamePhase::FreeThrows | GamePhase::InboundSetup | GamePhase::JumpBall)
            && self.state.q1_game_clock_ever_started
        {
            let protect_ft_shooter = self.state.ft_series_queue.top_mut().map(|ft| ft.shooter_roster_idx);
            for (side, team_idx) in [(CommandSide::Home, 0), (CommandSide::Away, 1)] {
                for r in self.state.team_states[team_idx].active_lineup_slots {
                    if r != ROSTER_IDX_NONE && Some(r) != protect_ft_shooter
                        && is_mandatory_out(&self.state.players[r as usize])
                        && !self.state.players[r as usize].fouled_out_staying_on_court
                    {
                        self.replace_disqualified_player(side, r);
                    }
                }
            }
        }
        let can_sub_phase = matches!(
            self.state.game_phase,
            GamePhase::Timeout | GamePhase::PeriodBreak | GamePhase::WhistlePendingCommit
        ) || (self.state.game_phase == GamePhase::InboundSetup
            && self
                .state
                .inbound_context
                .as_ref()
                .map_or(false, |inb| {
                    let p = inbound_window_permissions(&self.state, inb, &self.ruleset);
                    p.home_can_sub || p.away_can_sub
                }));

        if !(self.state.quarter == 1 && !self.state.q1_game_clock_ever_started) && can_sub_phase {
            for team_idx in 0..2 {
                let side = if team_idx == 0 {
                    CommandSide::Home
                } else {
                    CommandSide::Away
                };
                if self.state.game_phase == GamePhase::InboundSetup {
                    if let Some(ref inb) = self.state.inbound_context {
                        let p = inbound_window_permissions(&self.state, inb, &self.ruleset);
                        let allowed = if team_idx == 0 { p.home_can_sub } else { p.away_can_sub };
                        if !allowed {
                            continue;
                        }
                    }
                }
                let opp_score = self.state.team_states[1 - team_idx].score_on_court;
                let planned = plan_ai_substitutions(
                    side,
                    team_idx * 15,
                    &self.teams[team_idx],
                    &self.state.team_states[team_idx],
                    opp_score,
                    &self.state.players,
                    self.state.quarter,
                    self.state.game_clock_ds,
                    &self.ruleset,
                    &self.tunables,
                    self.canonical_team_ids[team_idx],
                );
                for (out_r, in_r, tier) in planned {
                    self.apply_substitution(side, out_r, in_r, tier);
                }
            }
        }

        // Update clutch coaching flags
        for team_idx in 0..2 {
            let opp_idx = 1 - team_idx;
            let opp_in_bonus_next = is_team_in_bonus(
                &self.state.team_states[opp_idx], self.state.quarter, self.state.game_clock_ds,
                &self.ruleset.fouls, 1,
            );
            let opp_score = self.state.team_states[opp_idx].score_on_court;
            let foul_up_3 = self.teams[team_idx].tactics.foul_up_three_policy;
            update_clutch_coaching_flags(
                &mut self.state.team_states[team_idx],
                opp_score,
                opp_in_bonus_next,
                self.state.quarter,
                self.state.game_clock_ds,
                self.state.shot_clock_ds,
                self.intentional_foul_policy,
                foul_up_3,
                &self.tunables,
            );
        }
    }
}
