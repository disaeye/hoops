use crate::wire_schema_v1::*;

use super::SimulationEngine;

// 帧投影：把 GameState 与瞬态规划缓冲投影为 V1 Public / Debug 帧，不读取任何战术私有计划
impl SimulationEngine {
    pub fn build_frames(&self, tick: u32) -> (PublicFrameV1, DebugOverlayFrameV1) {
        let mut pub_players = [PlayerFrameSlotV1::inactive(); 10];
        let mut dbg_players = [PlayerDebugSlotV1::inactive(); 10];
        let offense = self.state.possession_side.and_then(|side| side.team_index());
        for team_idx in 0..2 {
            let line = &self.state.team_states[team_idx].active_lineup_slots;
            for p_pos in 0..5 {
                let slot_idx = team_idx * 5 + p_pos;
                let r_idx = line[p_pos];
                if r_idx == ROSTER_IDX_NONE {
                    continue;
                }
                let p = &self.state.players[r_idx as usize];
                let (x_mm, _) = crate::wire_schema_v1::frames::quantize_mm(p.x_m).unwrap_or((0, true));
                let (y_mm, _) = crate::wire_schema_v1::frames::quantize_mm(p.y_m).unwrap_or((0, true));
                let vx_dm_s = crate::wire_schema_v1::frames::quantize_vel_dm_s(p.vx_m_s).unwrap_or(0);
                let vy_dm_s = crate::wire_schema_v1::frames::quantize_vel_dm_s(p.vy_m_s).unwrap_or(0);
                let facing_bin = crate::wire_schema_v1::frames::quantize_facing_bin(p.facing_rad).unwrap_or(0);
                let mut p_flags = 0u8;
                if self.state.ball.owner_slot == Some(slot_idx as u8) {
                    p_flags |= 0x01;
                }
                if p.action_state == PlayerActionState::SetScreen {
                    p_flags |= 0x02;
                }
                if p.z_m > 0.05
                    || p.footwork.dribble_status == DribbleStatus::AirborneWithBall
                    || (p.action_timer_ds >= 2
                        && matches!(
                            p.action_state,
                            PlayerActionState::ShootJumper
                                | PlayerActionState::Layup
                                | PlayerActionState::Dunk
                                | PlayerActionState::ContestJump
                                | PlayerActionState::ReboundJump
                        ))
                {
                    p_flags |= 0x04;
                }
                if p.paint_timer_ds >= 25 {
                    p_flags |= 0x08;
                }
                if p.personal_fouls >= 4 {
                    p_flags |= 0x10;
                }
                if p.footwork.dribble_status == DribbleStatus::Gathered {
                    p_flags |= 0x20;
                }
                if self.state.slot_contact_timer_ds[slot_idx] > 0 {
                    p_flags |= 0x40;
                }
                pub_players[slot_idx] = PlayerFrameSlotV1 {
                    roster_idx: r_idx,
                    x_mm,
                    y_mm,
                    vx_dm_s,
                    vy_dm_s,
                    facing_bin,
                    visible_action: p.action_state as u8,
                    public_flags: p_flags,
                };
                let (ttx, tty) = self.transient_target_xy[slot_idx];
                let (target_x_m, target_y_m) = if ttx.is_finite() && tty.is_finite() {
                    (ttx, tty)
                } else {
                    (p.x_m, p.y_m)
                };
                let other = 1 - team_idx;
                let matchup_pos = match offense {
                    Some(o) if o == team_idx => self.state.defense_plans[other]
                        .matchups().iter().position(|&pos| usize::from(pos) == p_pos),
                    Some(_) => Some(usize::from(self.state.defense_plans[team_idx].matchups()[p_pos])),
                    None => None,
                };
                let opp_slot = matchup_pos.filter(|&pos| {
                    self.state.team_states[other].active_lineup_slots.get(pos)
                        .is_some_and(|&roster| roster != ROSTER_IDX_NONE)
                }).map_or(0xFF, |pos| (other * 5 + pos) as u8);
                dbg_players[slot_idx] = PlayerDebugSlotV1 {
                    target_x_cm: crate::wire_schema_v1::frames::quantize_cm(target_x_m).unwrap_or(0),
                    target_y_cm: crate::wire_schema_v1::frames::quantize_cm(target_y_m).unwrap_or(0),
                    defender_slot: opp_slot,
                    burst_energy: p.burst_energy.round().clamp(0.0, 100.0) as u8,
                    cum_stamina: p.cumulative_stamina.round().clamp(0.0, 100.0) as u8,
                    rhythm_quant: (p.rhythm_score * 100.0).round().clamp(-100.0, 100.0) as i8,
                    epv_centi: self.transient_epv_centi[slot_idx],
                    debug_flags: self.transient_debug_flags[slot_idx],
                };
            }
        }

        let (bx, _) = crate::wire_schema_v1::frames::quantize_mm(self.state.ball.position_m[0]).unwrap_or((0, true));
        let (by, _) = crate::wire_schema_v1::frames::quantize_mm(self.state.ball.position_m[1]).unwrap_or((0, true));
        let bz = crate::wire_schema_v1::frames::quantize_z_mm(self.state.ball.position_m[2]).unwrap_or(0);

        let mut b_flags = 0u8;
        if self.state.ball.rim_touched_this_shot || (self.state.ball_fx_flags & 0x01) != 0 {
            b_flags |= 1 << 0;
        }
        if self.state.ball.backboard_contact_this_shot != BackboardContact::None
            || (self.state.ball_fx_flags & 0x02) != 0
        {
            b_flags |= 1 << 1;
        }
        if (self.state.ball_fx_flags & 0x04) != 0 {
            b_flags |= 1 << 2;
        }
        if (self.state.ball_fx_flags & 0x08) != 0 {
            b_flags |= 1 << 3;
        }
        if (self.state.ball_fx_flags & 0x10) != 0 {
            b_flags |= 1 << 4;
        }
        if self.state.game_clock_ticked_this_step {
            b_flags |= 1 << 5;
        }
        if self.state.shot_clock_running {
            b_flags |= 1 << 6;
        }

        let pub_frame = PublicFrameV1 {
            tick,
            quarter: self.state.quarter,
            game_clock_ds: self.state.game_clock_ds,
            shot_clock_ds: encode_shot_clock_opt(self.state.shot_clock_ds),
            game_phase: self.state.game_phase as u8,
            possession_side: self.state.possession_side.map_or(2, |s| s as u8),
            score_home: self.state.team_states[0].score_on_court,
            score_away: self.state.team_states[1].score_on_court,
            ball_x_mm: bx,
            ball_y_mm: by,
            ball_z_mm: bz,
            ball_state: self.state.ball.ball_state as u8,
            ball_owner_slot: self.state.ball.owner_slot.map_or(-1, |s| s as i8),
            ball_flags: b_flags,
            players: pub_players,
        };

        let off_idx = offense.unwrap_or(0);
        let active_play_idx = self
            .playbooks
            .iter()
            .position(|p| p.play_id == self.state.active_play_id[off_idx])
            .map_or(0xFFFF, |i| i as u16);
        let active_def_rule_idx = self.transient_def_rule_idx;

        let dbg_frame = DebugOverlayFrameV1 {
            tick,
            active_play_idx,
            active_def_rule_idx,
            player_debug: dbg_players,
        };
        (pub_frame, dbg_frame)
    }
}
