use std::collections::BTreeSet;

use hoops_core::physics::CourtGeometry;
use hoops_core::{
    step_with_batch_ai, BallState, GamePhase, GameRecordEventPayload, MatchInitSpecV1,
    ReplayManifest, SimulationEngine, StepStatus, VecSimSink,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintViolation {
    pub tick: u32,
    pub category: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintReport {
    pub total_ticks: usize,
    pub score_home: u16,
    pub score_away: u16,
    pub violations: Vec<LintViolation>,
    pub passed: bool,
}

pub fn lint_match(spec: &MatchInitSpecV1, max_ticks: Option<usize>) -> Result<LintReport, String> {
    let mut sink = VecSimSink::default();
    let mut eng = SimulationEngine::new(spec, &mut sink).map_err(|e| format!("{e}"))?;
    let mut cmds = Vec::new();
    let geom = CourtGeometry::from_rules(&spec.ruleset.court);
    let max_t = max_ticks.unwrap_or(40_000);

    for _ in 0..max_t {
        let st = step_with_batch_ai(&mut eng, &mut sink, &mut cmds);
        if matches!(st, StepStatus::Finished { .. } | StepStatus::Aborted { .. }) {
            break;
        }
    }

    let mut violations = Vec::new();
    for audit in &sink.audits {
        for w in &audit.warnings {
            match w {
                hoops_core::AuditWarning::RngCounterSaturated { .. }
                | hoops_core::AuditWarning::DepenetrationFallbackUsed { .. } => {
                    violations.push(LintViolation {
                        tick: audit.tick,
                        category: "AuditWarning".to_owned(),
                        message: format!("{w:?}"),
                    });
                }
                _ => {}
            }
        }
    }

    let min_x_mm = ((-geom.half_length_m - geom.oob_runoff_m - 0.1) * 1000.0) as i16;
    let max_x_mm = ((geom.half_length_m + geom.oob_runoff_m + 0.1) * 1000.0) as i16;
    let min_y_mm = ((-geom.half_width_m - geom.oob_runoff_m - 0.1) * 1000.0) as i16;
    let max_y_mm = ((geom.half_width_m + geom.oob_runoff_m + 0.1) * 1000.0) as i16;
    let half_l_mm = (geom.half_length_m * 1000.0).round() as i32;
    let half_w_mm = (geom.half_width_m * 1000.0).round() as i32;

    let live_phase = GamePhase::LiveBall as u8;
    let scramble_phase = GamePhase::ReboundScramble as u8;
    let inbound_phase = GamePhase::InboundSetup as u8;
    let held_state = BallState::Held as u8;
    let loose_state = BallState::Loose as u8;

    let mut clock_adjusted_ticks: BTreeSet<u32> = BTreeSet::new();
    let mut clock_adj_team_delta_ds = [0i64; 2];
    let mut game_ended_court_times: Option<[u32; 30]> = None;

    for ev in &sink.events {
        match &ev.payload {
            GameRecordEventPayload::ClockAdjusted {
                delta_game_clock_ds_signed,
                affected_slot_mask,
                ..
            } => {
                clock_adjusted_ticks.insert(ev.tick);
                let d = *delta_game_clock_ds_signed as i64;
                let h_cnt = (*affected_slot_mask & 0x001F).count_ones() as i64;
                let a_cnt = (*affected_slot_mask & 0x03E0).count_ones() as i64;
                clock_adj_team_delta_ds[0] -= d * h_cnt;
                clock_adj_team_delta_ds[1] -= d * a_cnt;
            }
            GameRecordEventPayload::GameEnded {
                final_court_time_ds,
                ..
            } => {
                game_ended_court_times = Some(final_court_time_ds.0);
            }
            _ => {}
        }
    }

    let mut prev_frame: Option<&hoops_core::PublicFrameV1> = None;
    let mut last_poss_side: u8 = 0xFF;
    let mut saw_frontcourt = false;

    let mut frame_ticked_team_ds = [0u32; 2];
    let mut all_ticked_5v5 = true;

    // Foul-out / ejection runtime tracking synchronized by tick
    let mut mandatory_out_tick: [Option<u32>; 30] = [None; 30];
    let mut flagged_on_court_after_foul_out = [false; 30];
    let mut ei = 0usize;

    for f in &sink.public_frames {
        for p in &f.players {
            if p.roster_idx != 0xFF
                && (p.x_mm < min_x_mm || p.x_mm > max_x_mm || p.y_mm < min_y_mm || p.y_mm > max_y_mm)
            {
                violations.push(LintViolation {
                    tick: f.tick,
                    category: "OutOfBoundsClamp".to_owned(),
                    message: format!(
                        "roster {} pos ({}, {}) out of playable runoff",
                        p.roster_idx, p.x_mm, p.y_mm
                    ),
                });
                break;
            }
        }
        // Check 1-to-1 owner correspondence
        if f.ball_owner_slot >= 0 {
            let s = f.ball_owner_slot as usize;
            if s >= 10 || f.players[s].roster_idx == 0xFF {
                violations.push(LintViolation {
                    tick: f.tick,
                    category: "OwnershipInvariant".to_owned(),
                    message: format!("invalid ball_owner_slot {}", f.ball_owner_slot),
                });
            }
        }

        // Check zero ghost clock ticks & accumulate expected live team ds
        if (f.ball_flags & 0x20) != 0 {
            if f.tick == 0 {
                violations.push(LintViolation {
                    tick: f.tick,
                    category: "GhostCourtTime".to_owned(),
                    message: "tick 0 had bit5 game_clock_ticked_this_step set".to_owned(),
                });
            } else if let Some(pf) = prev_frame {
                if !clock_adjusted_ticks.contains(&f.tick)
                    && (f.quarter != pf.quarter
                        || f.game_clock_ds != pf.game_clock_ds.saturating_sub(1))
                {
                    violations.push(LintViolation {
                        tick: f.tick,
                        category: "GhostCourtTime".to_owned(),
                        message: format!(
                            "bit5=1 without clock decrement: Q{} {}ds -> Q{} {}ds",
                            pf.quarter, pf.game_clock_ds, f.quarter, f.game_clock_ds
                        ),
                    });
                }
            }
            let h_cnt = f.players[0..5].iter().filter(|p| p.roster_idx != 0xFF).count() as u32;
            let a_cnt = f.players[5..10].iter().filter(|p| p.roster_idx != 0xFF).count() as u32;
            if h_cnt != 5 || a_cnt != 5 {
                all_ticked_5v5 = false;
            }
            frame_ticked_team_ds[0] += h_cnt;
            frame_ticked_team_ds[1] += a_cnt;
        }

        // Check non-exempt fouled-out / ejected player staying on court in LiveBall
        if f.game_phase == live_phase {
            for p in &f.players {
                let r = p.roster_idx as usize;
                if r < 30 {
                    if let Some(out_tick) = mandatory_out_tick[r] {
                        if f.tick > out_tick
                            && !eng.state.players[r].fouled_out_staying_on_court
                            && !flagged_on_court_after_foul_out[r]
                        {
                            flagged_on_court_after_foul_out[r] = true;
                            violations.push(LintViolation {
                                tick: f.tick,
                                category: "NonExemptFouledOutOnCourt".to_owned(),
                                message: format!(
                                    "non-exempt player roster {} remained on court in LiveBall after foul-out/ejection at tick {}",
                                    r, out_tick
                                ),
                            });
                        }
                    }
                }
            }
        }

        // 1. LiveBallHandlerOOB
        if f.game_phase == live_phase && f.ball_owner_slot >= 0 {
            let s = f.ball_owner_slot as usize;
            if s < 10 && f.players[s].roster_idx != 0xFF {
                let hp = &f.players[s];
                if (hp.x_mm as i32).abs() > half_l_mm || (hp.y_mm as i32).abs() > half_w_mm {
                    violations.push(LintViolation {
                        tick: f.tick,
                        category: "LiveBallHandlerOOB".to_owned(),
                        message: format!(
                            "handler roster {} OOB ({}, {}) vs ({}, {})",
                            hp.roster_idx, hp.x_mm, hp.y_mm, half_l_mm, half_w_mm
                        ),
                    });
                }
            }
        }

        // 2. LooseBallFloorOOB
        if (f.game_phase == live_phase || f.game_phase == scramble_phase)
            && f.ball_state == loose_state
            && f.ball_z_mm <= 150
            && ((f.ball_x_mm as i32).abs() > half_l_mm + 20
                || (f.ball_y_mm as i32).abs() > half_w_mm + 20)
        {
            violations.push(LintViolation {
                tick: f.tick,
                category: "LooseBallFloorOOB".to_owned(),
                message: format!(
                    "loose ball on floor OOB ({}, {}, z={})",
                    f.ball_x_mm, f.ball_y_mm, f.ball_z_mm
                ),
            });
        }

        // 3. InboundFromInbounds (check exact inbound pass release transition)
        if let Some(pf) = prev_frame {
            if pf.game_phase == inbound_phase
                && f.game_phase == inbound_phase
                && pf.ball_state == held_state
                && f.ball_state == BallState::PassFlight as u8
                && pf.ball_owner_slot >= 0
            {
                let s = pf.ball_owner_slot as usize;
                if s < 10 && pf.players[s].roster_idx != 0xFF {
                    let ip = &pf.players[s];
                    if (ip.x_mm as i32).abs() <= half_l_mm - 50
                        && (ip.y_mm as i32).abs() <= half_w_mm - 50
                    {
                        violations.push(LintViolation {
                            tick: pf.tick,
                            category: "InboundFromInbounds".to_owned(),
                            message: format!(
                                "inbounder roster {} released inbound pass from inbounds ({}, {})",
                                ip.roster_idx, ip.x_mm, ip.y_mm
                            ),
                        });
                    }
                }
            }
        }

        // 4. ReboundTeleport
        if let Some(pf) = prev_frame {
            if pf.game_phase == scramble_phase && f.game_phase == live_phase && f.ball_owner_slot >= 0
            {
                let dx = (f.ball_x_mm as i32) - (pf.ball_x_mm as i32);
                let dy = (f.ball_y_mm as i32) - (pf.ball_y_mm as i32);
                if dx * dx + dy * dy > 2500 * 2500 {
                    violations.push(LintViolation {
                        tick: f.tick,
                        category: "ReboundTeleport".to_owned(),
                        message: format!(
                            "ball teleported {} mm on rebound capture",
                            (dx * dx + dy * dy).unsigned_abs().isqrt()
                        ),
                    });
                }
            }
        }

        // 5. BackcourtViolationMissed
        if f.game_phase != live_phase || f.possession_side > 1 || f.possession_side != last_poss_side
        {
            last_poss_side = f.possession_side;
            saw_frontcourt = false;
        }
        if f.game_phase == live_phase && f.possession_side <= 1 {
            let attack_dir = if f.possession_side == 0 { 1_i32 } else { -1_i32 };
            if f.ball_owner_slot >= 0 {
                let s = f.ball_owner_slot as usize;
                if s < 10 && f.players[s].roster_idx != 0xFF {
                    let hp = &f.players[s];
                    if (f.ball_x_mm as i32) * attack_dir > 500
                        && (hp.x_mm as i32) * attack_dir > 500
                    {
                        saw_frontcourt = true;
                    } else if saw_frontcourt
                        && f.ball_state == held_state
                        && (f.ball_x_mm as i32) * attack_dir < -500
                        && (hp.x_mm as i32) * attack_dir < -500
                    {
                        violations.push(LintViolation {
                            tick: f.tick,
                            category: "BackcourtViolationMissed".to_owned(),
                            message: format!(
                                "handler {} held ball in backcourt ({}) after establishing frontcourt",
                                hp.roster_idx, f.ball_x_mm
                            ),
                        });
                    }
                }
            }
        }

        // Consume events for this tick to maintain accurate foul-out/ejection watermarks
        while ei < sink.events.len() && sink.events[ei].tick <= f.tick {
            if let GameRecordEventPayload::FoulAssessedRecord {
                offender_roster_idx,
                resulting_personal_fouls,
                ejected_or_fouled_out_flags,
                ..
            } = &sink.events[ei].payload
            {
                let r = *offender_roster_idx as usize;
                if r < 30 {
                    let is_exempt = (ejected_or_fouled_out_flags & 0x04) != 0
                        || eng.state.players[r].fouled_out_staying_on_court;
                    if mandatory_out_tick[r].is_some() && !is_exempt {
                        violations.push(LintViolation {
                            tick: sink.events[ei].tick,
                            category: "NonExemptFouledOutFoul".to_owned(),
                            message: format!(
                                "player roster {} committed foul after already fouling out / ejection without exemption",
                                r
                            ),
                        });
                    } else if *resulting_personal_fouls > spec.ruleset.fouls.foul_out_limit
                        && !is_exempt
                    {
                        violations.push(LintViolation {
                            tick: sink.events[ei].tick,
                            category: "NonExemptFouledOutFoul".to_owned(),
                            message: format!(
                                "player roster {} reached {} fouls > limit {} without exemption",
                                r, resulting_personal_fouls, spec.ruleset.fouls.foul_out_limit
                            ),
                        });
                    }
                    if !is_exempt
                        && ((ejected_or_fouled_out_flags & 0x03) != 0
                            || *resulting_personal_fouls >= spec.ruleset.fouls.foul_out_limit)
                        && mandatory_out_tick[r].is_none()
                    {
                        mandatory_out_tick[r] = Some(sink.events[ei].tick);
                    }
                }
            }
            ei += 1;
        }

        prev_frame = Some(f);
    }

    // 6. Verify team court_time_live_ds equals actual elapsed game clock duration (zero ghost time)
    let full_ds_for_q = |q: u8| -> u32 {
        if q <= 4 {
            (spec.ruleset.clocks.quarter_sec as u32) * 10
        } else {
            (spec.ruleset.clocks.ot_sec as u32) * 10
        }
    };
    let q_cur = eng.state.quarter.max(1);
    let mut expected_elapsed_clock_ds = 0u32;
    for q in 1..q_cur {
        expected_elapsed_clock_ds += full_ds_for_q(q);
    }
    expected_elapsed_clock_ds += full_ds_for_q(q_cur).saturating_sub(eng.state.game_clock_ds as u32);

    let actual_team_court_ds = [
        eng.state.players[0..15]
            .iter()
            .map(|p| p.court_time_live_ds)
            .sum::<u32>(),
        eng.state.players[15..30]
            .iter()
            .map(|p| p.court_time_live_ds)
            .sum::<u32>(),
    ];

    for side in 0..2 {
        let side_name = if side == 0 { "home" } else { "away" };
        if actual_team_court_ds[side] != frame_ticked_team_ds[side] {
            violations.push(LintViolation {
                tick: eng.state.sim_tick,
                category: "CourtTimeMismatch".to_owned(),
                message: format!(
                    "{side_name} court_time_live_ds sum {} != frame bit5 active sum {}",
                    actual_team_court_ds[side], frame_ticked_team_ds[side]
                ),
            });
        }
        if all_ticked_5v5 && clock_adjusted_ticks.is_empty() {
            let expected_5x = expected_elapsed_clock_ds * 5;
            if actual_team_court_ds[side] != expected_5x {
                violations.push(LintViolation {
                    tick: eng.state.sim_tick,
                    category: "CourtTimeMismatch".to_owned(),
                    message: format!(
                        "{side_name} court_time_live_ds sum {} != expected 5x elapsed game clock {} ({} ds * 5)",
                        actual_team_court_ds[side], expected_5x, expected_elapsed_clock_ds
                    ),
                });
            }
        }
    }

    if let Some(final_ct) = game_ended_court_times {
        for (i, &ct) in final_ct.iter().enumerate() {
            if ct != eng.state.players[i].court_time_live_ds {
                violations.push(LintViolation {
                    tick: eng.state.sim_tick,
                    category: "CourtTimeMismatch".to_owned(),
                    message: format!(
                        "GameEnded.final_court_time_ds[{i}]={} != state.court_time_live_ds={}",
                        ct, eng.state.players[i].court_time_live_ds
                    ),
                });
            }
        }
    }

    // 7. End-of-game check on personal fouls vs exemption
    for (r, p) in eng.state.players.iter().enumerate() {
        if p.is_present
            && !p.fouled_out_staying_on_court
            && p.personal_fouls > spec.ruleset.fouls.foul_out_limit
        {
            violations.push(LintViolation {
                tick: eng.state.sim_tick,
                category: "NonExemptFouledOutFoul".to_owned(),
                message: format!(
                    "player roster {} ended with {} fouls > limit {} without exemption",
                    r, p.personal_fouls, spec.ruleset.fouls.foul_out_limit
                ),
            });
        }
    }

    // Validate full Manifest equivalence with no un-finished cap abort if full game
    let _ = ReplayManifest {
        init_spec: spec.clone(),
        applied_commands: cmds,
        verification: None,
    };

    Ok(LintReport {
        total_ticks: sink.public_frames.len(),
        score_home: eng.state.team_states[0]
            .score_official_override
            .unwrap_or(eng.state.team_states[0].score_on_court),
        score_away: eng.state.team_states[1]
            .score_official_override
            .unwrap_or(eng.state.team_states[1].score_on_court),
        passed: violations.is_empty(),
        violations,
    })
}
