// 物理模块：球员竖直运动学与动作计时器。

use crate::model::GRAVITY_M_S2;
use crate::wire_schema_v1::*;

use super::SimulationEngine;

impl SimulationEngine {
    pub(super) fn step_player_vertical_kinematics(&mut self) {
        for team_idx in 0..2 {
            for p_pos in 0..5 {
                let r = self.state.team_states[team_idx].active_lineup_slots[p_pos];
                if r == ROSTER_IDX_NONE {
                    continue;
                }
                let p = &mut self.state.players[r as usize];
                if p.z_m > 0.0 || p.vz_m_s > 0.0 {
                    let prev_z = p.z_m;
                    p.z_m += p.vz_m_s * 0.1;
                    p.vz_m_s -= GRAVITY_M_S2 * 0.1;
                    if p.z_m <= 0.0 {
                        p.z_m = 0.0;
                        p.vz_m_s = 0.0;
                        if prev_z > 0.0
                            && p.footwork.dribble_status == DribbleStatus::AirborneWithBall
                            && p.footwork.release_tick.is_none()
                        {
                            p.footwork.foot_touched_down_while_lifted = true;
                        }
                    }
                }
            }
        }
    }

    pub(super) fn tick_player_action_timers(&mut self) {
        for team_idx in 0..2 {
            for p_pos in 0..5 {
                let r = self.state.team_states[team_idx].active_lineup_slots[p_pos];
                if r != ROSTER_IDX_NONE {
                    let p = &mut self.state.players[r as usize];
                    if p.action_timer_ds > 0 {
                        p.action_timer_ds -= 1;
                        if p.action_timer_ds == 0
                            && matches!(
                                p.action_state,
                                PlayerActionState::ContestJump
                                    | PlayerActionState::ReboundJump
                                    | PlayerActionState::ShootJumper
                                    | PlayerActionState::Layup
                                    | PlayerActionState::Dunk
                            )
                        {
                            p.action_state = PlayerActionState::Idle;
                        }
                    }
                }
            }
        }
    }
}
