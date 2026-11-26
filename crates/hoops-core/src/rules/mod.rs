use crate::model::{rem_10us_to_ds, SUB_DS_UNITS};
use crate::wire_schema_v1::*;

/// Shot clock reset scenario categories (§3.1 12-scenario matrix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShotClockScenario {
    OpeningTipOrOtJumpControl,      // 1
    PeriodStartAlternatingArrow,    // 2
    LiveStealOrDefRebound,          // 3
    TurnoverOpponentToOwnBackcourt, // 4
    TurnoverOpponentToOwnFrontcourt,// 5
    OffReboundAfterRimOrFtMiss,     // 6
    OffReboundNoRimAirballOrBlock,  // 7
    ScrambleFoulOrOobKeepFrontcourt,// 8
    OffenseFrontcourtNonShotDefFoul,// 9
    OffenseBackcourtRetain { is_foul_or_kick: bool }, // 10
    FlagrantOrUnsportsmanlikeOrDisq,// 11
    L2mTimeoutAdvance { gained_from_opponent_foul: bool }, // 12
}

/// Apply target shot clock value under rule: if `game_clock_ds <= target_ds`, shut off (`None`).
#[inline]
pub fn close_if_below_gc(target_ds: u16, game_clock_ds: u16) -> Option<u16> {
    if game_clock_ds <= target_ds {
        None
    } else {
        Some(target_ds)
    }
}

/// Compute target `shot_clock_ds` per §3.1 12-scenario table.
pub fn compute_shot_clock_reset(
    scenario: ShotClockScenario,
    current_shot_clock_ds: Option<u16>,
    game_clock_ds: u16,
    ruleset: &RuleSet,
) -> Option<u16> {
    let full = ruleset.clocks.full_shot_clock_ds;
    let reset = ruleset.clocks.reset_shot_clock_ds;
    let is_fiba = matches!(ruleset.clocks.shot_clock_policy, ShotClockResetPolicy::FibaArt29);

    match scenario {
        ShotClockScenario::OpeningTipOrOtJumpControl
        | ShotClockScenario::PeriodStartAlternatingArrow
        | ShotClockScenario::LiveStealOrDefRebound
        | ShotClockScenario::TurnoverOpponentToOwnBackcourt => {
            close_if_below_gc(full, game_clock_ds)
        }
        ShotClockScenario::TurnoverOpponentToOwnFrontcourt => {
            let target = if is_fiba { reset } else { full };
            close_if_below_gc(target, game_clock_ds)
        }
        ShotClockScenario::OffReboundAfterRimOrFtMiss
        | ShotClockScenario::ScrambleFoulOrOobKeepFrontcourt => {
            close_if_below_gc(reset, game_clock_ds)
        }
        ShotClockScenario::OffReboundNoRimAirballOrBlock => current_shot_clock_ds,
        ShotClockScenario::OffenseFrontcourtNonShotDefFoul => match current_shot_clock_ds {
            None => None,
            Some(rem) => close_if_below_gc(rem.max(reset), game_clock_ds),
        },
        ShotClockScenario::OffenseBackcourtRetain { is_foul_or_kick } => {
            if is_foul_or_kick {
                close_if_below_gc(full, game_clock_ds)
            } else {
                current_shot_clock_ds
            }
        }
        ShotClockScenario::FlagrantOrUnsportsmanlikeOrDisq => {
            let target = if is_fiba { reset } else { full };
            close_if_below_gc(target, game_clock_ds)
        }
        ShotClockScenario::L2mTimeoutAdvance {
            gained_from_opponent_foul,
        } => {
            if !is_fiba {
                current_shot_clock_ds
            } else if gained_from_opponent_foul {
                close_if_below_gc(reset, game_clock_ds)
            } else {
                match current_shot_clock_ds {
                    None => None,
                    Some(rem) => close_if_below_gc(rem.min(reset), game_clock_ds),
                }
            }
        }
    }
}

/// Set clocks cleanly to integer deci-seconds (`rem_10us = ds * 10_000`, §3.1).
#[inline]
pub fn set_clocks_exact_ds(
    state: &mut GameState,
    new_gc_ds: u16,
    new_sc_ds: Option<u16>,
) {
    state.game_clock_rem_10us = (new_gc_ds as u32) * SUB_DS_UNITS;
    state.game_clock_ds = new_gc_ds;
    state.shot_clock_rem_10us = new_sc_ds.map(|s| (s as u32) * SUB_DS_UNITS);
    state.shot_clock_ds = new_sc_ds;
}

/// Advance active clocks over `delta_tau ∈ [0.0, 1.0]` and return `(actual_gc_delta_u, gc_expired, sc_expired)`.
pub fn advance_clocks_sub_ds(
    state: &mut GameState,
    delta_tau: f64,
) -> (u32, bool, bool) {
    let delta_u_req = ((delta_tau.clamp(0.0, 1.0)) * (SUB_DS_UNITS as f64)).round() as u32;
    let mut actual_gc_delta = 0u32;
    let mut gc_expired = false;
    let mut sc_expired = false;

    if state.game_clock_running && delta_u_req > 0 {
        let prev_ds = state.game_clock_ds;
        let prev_rem = state.game_clock_rem_10us;
        let step_u = delta_u_req.min(prev_rem);
        state.game_clock_rem_10us = prev_rem.saturating_sub(step_u);
        state.game_clock_ds = rem_10us_to_ds(state.game_clock_rem_10us);
        state.game_clock_ticked_this_step |= prev_ds > state.game_clock_ds;
        actual_gc_delta = prev_rem.saturating_sub(state.game_clock_rem_10us);
        if actual_gc_delta > 0 && state.quarter == 1 {
            state.q1_game_clock_ever_started = true;
        }
        if state.game_clock_rem_10us == 0 {
            gc_expired = true;
        }
    }

    if state.shot_clock_running && delta_u_req > 0 {
        if let Some(sc_rem) = state.shot_clock_rem_10us {
            let step_sc = delta_u_req.min(sc_rem);
            let new_sc_rem = sc_rem.saturating_sub(step_sc);
            state.shot_clock_rem_10us = Some(new_sc_rem);
            state.shot_clock_ds = Some(rem_10us_to_ds(new_sc_rem));
            if new_sc_rem == 0 {
                sc_expired = true;
            }
        }
    }

    // FIBA Art. 19.2.4: Any live clock run `Δu > 0` unlocks all players' substitution re-entry locks
    if actual_gc_delta > 0 {
        for p in state.players.iter_mut() {
            p.fiba_sub_reentry_lock = FibaSubReentryLock::None;
        }
    }

    (actual_gc_delta, gc_expired, sc_expired)
}

/// Apply NBA Comments II-L minimum `0.3s` (`30,000` 10us units) deduction if triggered.
/// Returns `Some((delta_ds_signed, new_gc_ds, new_sc_ds))` if adjustment occurred.
pub fn try_apply_comments_iil_deduction(
    state: &mut GameState,
    ruleset: &RuleSet,
) -> Option<(i16, u16, u16)> {
    if matches!(ruleset.clocks.shot_clock_policy, ShotClockResetPolicy::FibaArt29) {
        return None;
    }
    let orig = state.comments_iil_origin_clocks?;
    let elapsed = orig.start_gc_rem_10us.saturating_sub(state.game_clock_rem_10us);
    if elapsed >= 30_000 {
        state.comments_iil_origin_clocks = None;
        return None;
    }
    let prev_ds = state.game_clock_ds;
    let new_gc_rem = orig.start_gc_rem_10us.saturating_sub(30_000);
    state.game_clock_rem_10us = new_gc_rem;
    state.game_clock_ds = rem_10us_to_ds(new_gc_rem);
    if let Some(orig_sc) = orig.start_sc_rem_10us {
        let new_sc_rem = orig_sc.saturating_sub(30_000);
        state.shot_clock_rem_10us = Some(new_sc_rem);
        state.shot_clock_ds = Some(rem_10us_to_ds(new_sc_rem));
    }
    state.comments_iil_origin_clocks = None;
    let delta_ds = (state.game_clock_ds as i16) - (prev_ds as i16);
    let sc_wire = encode_shot_clock_opt(state.shot_clock_ds);
    Some((delta_ds, state.game_clock_ds, sc_wire))
}

/// Compute dead-time duration from `TimeoutRules` fields (§2.4 / §3.1).
pub fn dead_time_duration_ds(
    ruleset: &RuleSet,
    reason_tag: u8,
    quarter: u8,
) -> u16 {
    let t = &ruleset.timeouts;
    match reason_tag {
        0 => t.team_to_ds,
        1 => t.mandatory_to_ds,
        2 => t.review_dead_time_ds,
        3 => {
            if quarter >= 4 {
                t.ot_break_ds
            } else {
                t.quarter_break_ds
            }
        }
        4 => t.halftime_ds,
        _ => t.team_to_ds,
    }
}

/// First mandatory timeout owner per `MandatoryFirstChargePolicy`.
pub fn first_mandatory_charge_side(
    ruleset: &RuleSet,
    canonical_team_ids: [u8; 2],
) -> CommandSide {
    match ruleset.timeouts.mandatory_first_charge_policy {
        MandatoryFirstChargePolicy::HomeFirst => CommandSide::Home,
        MandatoryFirstChargePolicy::CanonicalAlphaFirst => {
            if canonical_team_ids[0] == 0 {
                CommandSide::Home
            } else {
                CommandSide::Away
            }
        }
    }
}

/// Charge team timeout and check mandatory slot consumption (NBA Rule 5 §VI-c).
/// Returns `(reason_tag_for_deadtime, duration_ds, charged_side)`.
pub fn charge_team_timeout_with_mandatory_slots(
    state: &mut GameState,
    caller: CommandSide,
    ruleset: &RuleSet,
) -> Option<(u8, u16, CommandSide)> {
    let team_idx = caller.team_index()?;
    if state.team_states[team_idx].timeouts_remaining == 0 {
        return None;
    }
    state.team_states[team_idx].timeouts_remaining -= 1;

    let is_nba_reg = state.quarter <= 4 && ruleset.timeouts.mandatory_to_ds > 0;
    if is_nba_reg {
        if !state.mandatory_timeout_state.slot_under_7m_taken {
            state.mandatory_timeout_state.slot_under_7m_taken = true;
            state.mandatory_timeout_state.first_mandatory_charged_to = Some(caller);
            return Some((1, ruleset.timeouts.mandatory_to_ds, caller));
        } else if !state.mandatory_timeout_state.slot_under_3m_taken {
            state.mandatory_timeout_state.slot_under_3m_taken = true;
            if state.game_clock_ds <= 1800 {
                state.mandatory_timeout_state.slot_under_3m_completed = true;
            }
            return Some((1, ruleset.timeouts.mandatory_to_ds, caller));
        }
    }
    Some((0, ruleset.timeouts.team_to_ds, caller))
}

/// Check Stage 6a automatic mandatory timeout trigger (NBA Rule 5 §VI-c).
pub fn check_auto_mandatory_timeout(
    state: &mut GameState,
    ruleset: &RuleSet,
    canonical_team_ids: [u8; 2],
    just_finished_slot1_this_tick: bool,
) -> Option<CommandSide> {
    if state.quarter > 4
        || ruleset.timeouts.mandatory_to_ds == 0
        || state.game_clock_ds == 0
        || !state.ft_series_queue.is_empty()
    {
        return None;
    }

    if !state.mandatory_timeout_state.slot_under_7m_taken && state.game_clock_ds < 4200 {
        let preferred = first_mandatory_charge_side(ruleset, canonical_team_ids);
        let pref_idx = preferred.team_index().unwrap_or(0);
        let opp_idx = 1 - pref_idx;
        let charge_idx = if state.team_states[pref_idx].timeouts_remaining > 0 {
            pref_idx
        } else if state.team_states[opp_idx].timeouts_remaining > 0 {
            opp_idx
        } else {
            return None;
        };
        let side = if charge_idx == 0 {
            CommandSide::Home
        } else {
            CommandSide::Away
        };
        state.team_states[charge_idx].timeouts_remaining -= 1;
        state.mandatory_timeout_state.slot_under_7m_taken = true;
        state.mandatory_timeout_state.first_mandatory_charged_to = Some(side);
        return Some(side);
    }

    if state.mandatory_timeout_state.slot_under_7m_taken
        && !state.mandatory_timeout_state.slot_under_3m_taken
        && state.game_clock_ds < 1800
        && !just_finished_slot1_this_tick
    {
        let first = state
            .mandatory_timeout_state
            .first_mandatory_charged_to
            .unwrap_or(CommandSide::Home);
        let preferred = first.opponent();
        let pref_idx = preferred.team_index().unwrap_or(1);
        let opp_idx = 1 - pref_idx;
        let charge_idx = if state.team_states[pref_idx].timeouts_remaining > 0 {
            pref_idx
        } else if state.team_states[opp_idx].timeouts_remaining > 0 {
            opp_idx
        } else {
            return None;
        };
        let side = if charge_idx == 0 {
            CommandSide::Home
        } else {
            CommandSide::Away
        };
        state.team_states[charge_idx].timeouts_remaining -= 1;
        state.mandatory_timeout_state.slot_under_3m_taken = true;
        state.mandatory_timeout_state.slot_under_3m_completed = false;
        return Some(side);
    }

    None
}

/// 执行末节暂停配额：FIBA 最后两分钟，NBA 最后三分钟的强制暂停结束后。
pub fn apply_q4_timeout_caps(state: &mut GameState, ruleset: &RuleSet) {
    if state.quarter != 4 {
        return;
    }
    if ruleset.clocks.shot_clock_policy == ShotClockResetPolicy::FibaArt29 {
        if state.game_clock_ds <= 1200 {
            for team in &mut state.team_states {
                team.timeouts_remaining = team.timeouts_remaining.min(ruleset.timeouts.h2_l2m_cap);
            }
        }
        return;
    }
    if state.game_clock_ds <= 1800 && state.mandatory_timeout_state.slot_under_3m_taken {
        if !matches!(state.game_phase, GamePhase::Timeout) {
            state.mandatory_timeout_state.slot_under_3m_completed = true;
        }
        if state.mandatory_timeout_state.slot_under_3m_completed {
            let cap = ruleset.timeouts.q4_l3m_cap;
            for ts in state.team_states.iter_mut() {
                if ts.timeouts_remaining > cap {
                    ts.timeouts_remaining = cap;
                }
            }
        }
    }
}

/// `additional` 表示尚未计入的下一次球队犯规，用于教练和出手评估。
pub fn is_team_in_bonus(
    team: &TeamRuntimeState,
    quarter: u8,
    game_clock_ds: u16,
    fouls: &FoulRules,
    additional: u8,
) -> bool {
    let (period_limit, l2m_limit) = if quarter <= 4 {
        (fouls.regulation_bonus_threshold, fouls.regulation_l2m_bonus_threshold)
    } else {
        (fouls.ot_bonus_threshold, fouls.ot_l2m_bonus_threshold)
    };
    team.period_team_fouls.saturating_add(additional) >= period_limit
        || (game_clock_ds <= 1200 && l2m_limit.is_some_and(|limit| {
            team.l2m_period_fouls.saturating_add(additional) >= limit
        }))
}

/// Resolve L2M advancement on InboundContext per NBA Rule 5 §VI-g / FIBA Art. 17.2.4 & OBRI 17-16 (§3.1 / §3.3).
pub fn apply_l2m_inbound_advance(
    inbound: &mut InboundContext,
    advance: AdvanceOption,
    stoppage_sideline_side: CourtSideLine,
    quarter: u8,
    game_clock_ds: u16,
    consecutive_tos: u8,
    had_dribble_or_pass_since_control: bool,
    gained_from_opponent_foul: bool,
    ruleset: &RuleSet,
) {
    if quarter < 4 || game_clock_ds > 1200 {
        return;
    }
    if matches!(advance, AdvanceOption::StayBackcourt) {
        return;
    }
    let is_fiba = matches!(ruleset.clocks.shot_clock_policy, ShotClockResetPolicy::FibaArt29);
    if !is_fiba {
        // NBA Rule 5 §VI-g: allowed if no dribble/pass yet OR consecutive timeout >= 2
        if !had_dribble_or_pass_since_control || consecutive_tos >= 2 {
            inbound.spot = InboundSpot::FrontcourtSideline28ft;
            inbound.sideline_side = match advance {
                AdvanceOption::AdvanceOppositeSide => CourtSideLine::OppositeTableSide,
                _ => CourtSideLine::TableSide,
            };
        }
    } else {
        // FIBA 2024 Art. 17.2.4 & OBRI 17-16
        inbound.spot = InboundSpot::FibaFrontcourtThrowInLine;
        if inbound.post_made_fg || inbound.post_made_last_ft {
            // Free choice of either sideline
            inbound.sideline_side = match advance {
                AdvanceOption::AdvanceOppositeSide => CourtSideLine::OppositeTableSide,
                _ => CourtSideLine::TableSide,
            };
        } else {
            // Stoppage (foul/violation/OOB): strictly clamped to original stoppage sideline side!
            inbound.sideline_side = stoppage_sideline_side;
        }
        inbound.pending_shot_clock_ds = compute_shot_clock_reset(
            ShotClockScenario::L2mTimeoutAdvance {
                gained_from_opponent_foul,
            },
            inbound.pending_shot_clock_ds,
            game_clock_ds,
            ruleset,
        );
    }
}

/// Check if team `side` can call timeout or substitute during `InboundSetup` (§3.2 / §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboundWindowPermissions {
    pub home_can_timeout: bool,
    pub away_can_timeout: bool,
    pub home_can_sub: bool,
    pub away_can_sub: bool,
}

pub fn inbound_window_permissions(
    state: &GameState,
    inbound: &InboundContext,
    ruleset: &RuleSet,
) -> InboundWindowPermissions {
    // Q1 before game clock ever started (FIBA OBRI 18/19-2 & NBA Rule 6 §II): strictly forbidden!
    if state.quarter == 1 && !state.q1_game_clock_ever_started {
        return InboundWindowPermissions {
            home_can_timeout: false,
            away_can_timeout: false,
            home_can_sub: false,
            away_can_sub: false,
        };
    }

    let is_fiba = matches!(ruleset.clocks.shot_clock_policy, ShotClockResetPolicy::FibaArt29);
    if !is_fiba {
        // NBA: deadball inbound allows timeouts/subs unless ball at disposal & live clock run, never on plain made FG without TO
        let stop_on_made = inbound.stop_clock_until_touch;
        let can = !inbound.inbound_ball_at_disposal && stop_on_made;
        return InboundWindowPermissions {
            home_can_timeout: !inbound.post_made_fg || inbound.side == CommandSide::Home,
            away_can_timeout: !inbound.post_made_fg || inbound.side == CommandSide::Away,
            home_can_sub: can && !inbound.post_made_fg,
            away_can_sub: can && !inbound.post_made_fg,
        };
    }

    // FIBA Art. 18.2.3, 18.2.4, 19.2.2, 19.2.3 & OBRI 18/19-4(b)
    if inbound.inbound_ball_at_disposal {
        return InboundWindowPermissions {
            home_can_timeout: false,
            away_can_timeout: false,
            home_can_sub: false,
            away_can_sub: false,
        };
    }

    if inbound.post_made_last_ft {
        // Conventional last/only FT made: BOTH teams open for timeout AND sub before ball at disposal
        return InboundWindowPermissions {
            home_can_timeout: true,
            away_can_timeout: true,
            home_can_sub: true,
            away_can_sub: true,
        };
    }

    if inbound.post_made_fg {
        let non_scorer = inbound.side; // inbounder is non-scoring team
        let is_l2m = state.quarter >= 4 && state.game_clock_ds <= 1200;
        if inbound.allow_both_sides_advance {
            // Unlocked (via non-scorer timeout or OBRI 18/19-4(b) non-scorer L2M sub)
            return InboundWindowPermissions {
                home_can_timeout: true,
                away_can_timeout: true,
                home_can_sub: true,
                away_can_sub: true,
            };
        }
        return InboundWindowPermissions {
            home_can_timeout: non_scorer == CommandSide::Home,
            away_can_timeout: non_scorer == CommandSide::Away,
            home_can_sub: is_l2m && non_scorer == CommandSide::Home,
            away_can_sub: is_l2m && non_scorer == CommandSide::Away,
        };
    }

    // Normal stoppage inbound (foul/violation/OOB): both teams open before ball at disposal
    InboundWindowPermissions {
        home_can_timeout: true,
        away_can_timeout: true,
        home_can_sub: true,
        away_can_sub: true,
    }
}

/// Validate substitution eligibility including FIBA 2024 Art. 19.2.4 clock-run re-entry lock (§9).
pub fn validate_substitution(
    state: &GameState,
    side: CommandSide,
    out_player: RosterIdx,
    in_player: RosterIdx,
    ruleset: &RuleSet,
    correctable_ft_exception: bool,
) -> Result<(), CommandRejectReason> {
    if state.quarter == 1 && !state.q1_game_clock_ever_started {
        return Err(CommandRejectReason::InvalidPhase);
    }
    let team_idx = side.team_index().ok_or(CommandRejectReason::InvalidPhase)?;
    let offset = team_idx * 15;
    if (out_player as usize) < offset
        || (out_player as usize) >= offset + 15
        || (in_player as usize) < offset
        || (in_player as usize) >= offset + 15
        || out_player == in_player
    {
        return Err(CommandRejectReason::InvalidPhase);
    }

    let p_out = &state.players[out_player as usize];
    let p_in = &state.players[in_player as usize];
    if !p_out.is_present
        || p_out.on_court_slot.is_none()
        || !p_in.is_present
        || p_in.on_court_slot.is_some()
        || p_in.ejected
        || p_in.locked_out_for_game
        || p_in.foul_out_seq.is_some()
        || !matches!(p_in.injury_state, PlayerInjuryState::Healthy)
    {
        return Err(CommandRejectReason::InvalidPhase);
    }

    // Phase check
    let phase_ok = match state.game_phase {
        GamePhase::Timeout | GamePhase::PeriodBreak | GamePhase::WhistlePendingCommit => true,
        GamePhase::InboundSetup => {
            if let Some(ref inb) = state.inbound_context {
                let perms = inbound_window_permissions(state, inb, ruleset);
                if side == CommandSide::Home {
                    perms.home_can_sub
                } else {
                    perms.away_can_sub
                }
            } else {
                false
            }
        }
        GamePhase::FreeThrows => true,
        _ => false,
    };
    if !phase_ok {
        return Err(CommandRejectReason::InvalidPhase);
    }

    // FIBA Art. 19.2.4 re-entry / same-stoppage leave lock
    if ruleset.roster.fiba_sub_clock_run_reentry_rule && !correctable_ft_exception {
        let on_court_count = state.team_states[team_idx]
            .active_lineup_slots
            .iter()
            .filter(|&&r| r != ROSTER_IDX_NONE)
            .count();
        if on_court_count >= 5 {
            let p1_forced_out = p_out.ejected
                || p_out.locked_out_for_game
                || p_out.foul_out_seq.is_some()
                || !matches!(p_out.injury_state, PlayerInjuryState::Healthy);

            if p_out.fiba_sub_reentry_lock == FibaSubReentryLock::SubbedInCannotLeave
                && !p1_forced_out
            {
                return Err(CommandRejectReason::SubIneligibleUntilClockRun);
            }
            if p_in.fiba_sub_reentry_lock == FibaSubReentryLock::SubbedOutCannotReenter {
                // Allowed only if p1_forced_out AND no unlocked eligible bench player exists
                let has_unlocked_bench = (offset..offset + 15).any(|idx| {
                    let b = &state.players[idx];
                    b.is_present
                        && b.on_court_slot.is_none()
                        && !b.ejected
                        && !b.locked_out_for_game
                        && b.foul_out_seq.is_none()
                        && matches!(b.injury_state, PlayerInjuryState::Healthy)
                        && b.fiba_sub_reentry_lock != FibaSubReentryLock::SubbedOutCannotReenter
                });
                if !(p1_forced_out && !has_unlocked_bench) {
                    return Err(CommandRejectReason::SubIneligibleUntilClockRun);
                }
            }
        }
    }
    Ok(())
}

/// Determine whether a ruling is challengeable in NBA Rule 14 / FIBA Appendix F.3 (§8.3).
pub fn eligible_challenge_sides_for_ruling(
    call: &RuleCallSnapshot,
    quarter: u8,
    game_clock_ds: u16,
    ruleset: &RuleSet,
) -> EligibleSidesMask {
    match ruleset.challenges.mode {
        0 => EligibleSidesMask(0),
        1 => {
            // NbaRule14
            let is_l2m = quarter >= 4 && game_clock_ds <= 1200;
            if call.rule_code == RuleCode::VIO_OOB {
                // Rule 14 §I(c): OOB is coach challengeable even in L2M by penalized side
                return match call.offending_side {
                    CommandSide::Home => EligibleSidesMask(0b01),
                    CommandSide::Away => EligibleSidesMask(0b10),
                    _ => EligibleSidesMask(0),
                };
            }
            if matches!(
                call.rule_code,
                RuleCode::VIO_GOALTEND | RuleCode::VIO_BASKET_INTERFERENCE
            ) {
                if is_l2m {
                    // Rule 13 §I(a)(12): L2M goaltending/interference is exclusive OfficialAutoReview
                    return EligibleSidesMask(0);
                }
                return match call.offending_side.opponent() {
                    CommandSide::Home => EligibleSidesMask(0b01),
                    CommandSide::Away => EligibleSidesMask(0b10),
                    _ => EligibleSidesMask(0),
                };
            }
            if call.rule_code.is_personal_foul() {
                return match call.offending_side {
                    CommandSide::Home => EligibleSidesMask(0b01),
                    CommandSide::Away => EligibleSidesMask(0b10),
                    _ => EligibleSidesMask(0),
                };
            }
            EligibleSidesMask(0)
        }
        2 => {
            // FibaIrsEnabled: both sides eligible when challenges remain
            EligibleSidesMask(0b11)
        }
        _ => EligibleSidesMask(0),
    }
}

/// NBA Rule 14 §V(a)-(e) 4-row timeout deduction truth table (§8.3).
pub fn apply_nba_challenge_timeout_and_bonus(
    state: &mut GameState,
    challenger: CommandSide,
    window_kind: ChallengeWindowKind,
    success: bool,
    is_second_challenge: bool,
    ruleset: &RuleSet,
) -> Option<CommandSide> {
    let ch_idx = challenger.team_index()?;
    let opp_idx = 1 - ch_idx;
    let opp = challenger.opponent();

    if !is_second_challenge && success {
        if ruleset.challenges.bonus_on_first_success && !state.team_states[ch_idx].first_challenge_used_and_won {
            state.team_states[ch_idx].first_challenge_used_and_won = true;
            state.team_states[ch_idx].challenges_remaining = 1;
        } else {
            state.team_states[ch_idx].challenges_remaining = 0;
        }
    } else {
        state.team_states[ch_idx].challenges_remaining = 0;
    }

    if !ruleset.challenges.requires_and_charges_timeout {
        return None;
    }

    match window_kind {
        ChallengeWindowKind::LiveDeadball => {
            if success && !is_second_challenge {
                None
            } else {
                state.team_states[ch_idx].timeouts_remaining =
                    state.team_states[ch_idx].timeouts_remaining.saturating_sub(1);
                Some(challenger)
            }
        }
        ChallengeWindowKind::TimeoutPending {
            reason: TimeoutReason::TeamCalled { side: caller },
        } => {
            if caller == challenger {
                // Own team-called timeout
                if success && !is_second_challenge {
                    // Refund challenger's own timeout
                    state.team_states[ch_idx].timeouts_remaining =
                        state.team_states[ch_idx].timeouts_remaining.saturating_add(1);
                    None
                } else {
                    // Already charged when entering TimeoutPending; keep charged to challenger
                    Some(challenger)
                }
            } else {
                // Opponent's team-called timeout (Rule 14 §V-e): NEVER double charge!
                if success && !is_second_challenge {
                    // Opponent keeps its timeout deduction, challenger pays 0
                    Some(opp)
                } else {
                    // Refund opponent, charge challenger 1
                    state.team_states[opp_idx].timeouts_remaining =
                        state.team_states[opp_idx].timeouts_remaining.saturating_add(1);
                    state.team_states[ch_idx].timeouts_remaining =
                        state.team_states[ch_idx].timeouts_remaining.saturating_sub(1);
                    Some(challenger)
                }
            }
        }
        ChallengeWindowKind::TimeoutPending {
            reason: TimeoutReason::Mandatory { team_side: holder },
        } => {
            if holder == challenger {
                // Mandatory charged to challenger must always be charged once
                Some(challenger)
            } else {
                // Opponent's mandatory timeout (Rule 14 §V-d)
                if success && !is_second_challenge {
                    Some(opp)
                } else {
                    // Transfer mandatory to challenger, refund opponent
                    state.team_states[opp_idx].timeouts_remaining =
                        state.team_states[opp_idx].timeouts_remaining.saturating_add(1);
                    state.team_states[ch_idx].timeouts_remaining =
                        state.team_states[ch_idx].timeouts_remaining.saturating_sub(1);
                    if !state.mandatory_timeout_state.slot_under_3m_taken
                        && state.mandatory_timeout_state.first_mandatory_charged_to == Some(opp)
                    {
                        state.mandatory_timeout_state.first_mandatory_charged_to = Some(challenger);
                    }
                    Some(challenger)
                }
            }
        }
    }
}

/// NBA Rule 14 §VI exact 2-case made basket retention on overturned challenge (§8.3).
#[inline]
pub fn nba_rule_14_vi_retains_made_basket(
    original_perceived: &RuleCallSnapshot,
    overturned_to: &RuleCallSnapshot,
) -> bool {
    if !original_perceived.basket_made_on_play || overturned_to.rule_code != RuleCode::NONE {
        return false;
    }
    // Case 1: Defensive foul overturned, occurred after shooting motion started
    let def_foul_overturned = original_perceived.rule_code.is_personal_foul()
        && !original_perceived.rule_code.is_offensive_foul()
        && original_perceived.foul_occurred_after_shooting_motion_started;
    // Case 2: Simultaneous OOB overturned, whistle after shooting motion started
    let oob_overturned = original_perceived.rule_code == RuleCode::VIO_OOB
        && original_perceived.oob_simultaneous_with_made_fg
        && original_perceived.whistle_after_shooting_motion_started;

    def_foul_overturned || oob_overturned
}

/// NBA vs FIBA injury replacement free-throw shooter selection (§9).
/// Returns `(replacement_roster_idx, lock_injured_out_for_game)`.
pub fn select_injury_ft_replacement(
    state: &GameState,
    injured_roster_idx: RosterIdx,
    injured_side: CommandSide,
    foul_rule: RuleCode,
    teams: &[CompiledTeamSnapshot; 2],
    ruleset: &RuleSet,
) -> Option<(RosterIdx, bool)> {
    let team_idx = injured_side.team_index()?;
    let offset = team_idx * 15;
    let is_fiba = matches!(ruleset.clocks.shot_clock_policy, ShotClockResetPolicy::FibaArt29);
    let p_inj = &state.players[injured_roster_idx as usize];
    let is_concussion = matches!(p_inj.injury_state, PlayerInjuryState::ConcussionEvaluation);
    let free_throw = |rank: usize| teams[team_idx].players[rank].attributes[AttrIdx::FreeThrow];

    if is_fiba {
        // FIBA Art. 19.3.3 & 43.2.1: replacement sub if available, else remaining on-court highest FT
        let bench_sub = (0..teams[team_idx].players.len())
            .filter_map(|rank| {
                let r = offset + rank;
                let p = &state.players[r];
                if r != injured_roster_idx as usize
                    && p.is_present
                    && p.on_court_slot.is_none()
                    && !p.ejected
                    && !p.locked_out_for_game
                    && p.foul_out_seq.is_none()
                    && matches!(p.injury_state, PlayerInjuryState::Healthy)
                {
                    Some((r as u8, free_throw(rank)))
                } else {
                    None
                }
            })
            .max_by_key(|&(_, ft)| ft);
        if let Some((r, _)) = bench_sub {
            return Some((r, false));
        }
        return state.team_states[team_idx]
            .active_lineup_slots
            .iter()
            .filter(|&&r| r != ROSTER_IDX_NONE && r != injured_roster_idx)
            .map(|&r| (r, free_throw(r as usize - offset)))
            .max_by_key(|&(_, ft)| ft)
            .map(|(r, _)| (r, false));
    }

    // NBA Rule 3 §V-e & Rule 9 §II(a)(2) & Rule 12B §IV-a/§X/§XI
    if is_concussion && !matches!(foul_rule, RuleCode::FOUL_FLAGRANT_2) {
        // Concussion evaluation (common/shooting/F1): OWN coach chooses from remaining 4 on-court; may return later
        let pick = state.team_states[team_idx]
            .active_lineup_slots
            .iter()
            .filter(|&&r| r != ROSTER_IDX_NONE && r != injured_roster_idx)
            .map(|&r| (r, free_throw(r as usize - offset)))
            .max_by_key(|&(_, ft)| ft)
            .map(|(r, _)| r);
        return pick.map(|r| (r, false));
    }

    match foul_rule {
        RuleCode::FOUL_FLAGRANT_1 => {
            // Own coach picks from remaining 4 on-court
            let pick = state.team_states[team_idx]
                .active_lineup_slots
                .iter()
                .filter(|&&r| r != ROSTER_IDX_NONE && r != injured_roster_idx)
                .map(|&r| (r, free_throw(r as usize - offset)))
                .max_by_key(|&(_, ft)| ft)
                .map(|(r, _)| r);
            // Locked out for game unless concussion/away-from-play/transition-take
            pick.map(|r| (r, !is_concussion))
        }
        RuleCode::FOUL_AWAY_FROM_PLAY | RuleCode::FOUL_TRANSITION_TAKE => {
            // Rule 12B §X & §XI statutory exceptions: may return
            let pick = state.team_states[team_idx]
                .active_lineup_slots
                .iter()
                .filter(|&&r| r != ROSTER_IDX_NONE && r != injured_roster_idx)
                .map(|&r| (r, free_throw(r as usize - offset)))
                .max_by_key(|&(_, ft)| ft)
                .map(|(r, _)| r);
            pick.map(|r| (r, false))
        }
        RuleCode::FOUL_FLAGRANT_2 => {
            // Own coach picks any eligible player; may return
            let pick = (0..teams[team_idx].players.len())
                .filter_map(|rank| {
                    let r = offset + rank;
                    let p = &state.players[r];
                    if r != injured_roster_idx as usize
                        && p.is_present
                        && !p.ejected
                        && !p.locked_out_for_game
                        && p.foul_out_seq.is_none()
                    {
                        Some((r as u8, free_throw(rank)))
                    } else {
                        None
                    }
                })
                .max_by_key(|&(_, ft)| ft)
                .map(|(r, _)| r);
            pick.map(|r| (r, false))
        }
        _ => {
            // Common personal/shooting foul: OPPOSING coach picks from eligible bench (lowest FT%), locked out for game
            let pick = (0..teams[team_idx].players.len())
                .filter_map(|rank| {
                    let r = offset + rank;
                    let p = &state.players[r];
                    if r != injured_roster_idx as usize
                        && p.is_present
                        && p.on_court_slot.is_none()
                        && !p.ejected
                        && !p.locked_out_for_game
                        && p.foul_out_seq.is_none()
                        && matches!(p.injury_state, PlayerInjuryState::Healthy)
                    {
                        Some((r as u8, free_throw(rank)))
                    } else {
                        None
                    }
                })
                .min_by_key(|&(_, ft)| ft)
                .map(|(r, _)| r);
            pick.map(|r| (r, true))
        }
    }
}

/// FIBA 2024 Art. 42.2.3-42.2.7 penalty cancellation and execution ordering (§8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenaltyKind {
    PersonalFoul { ft_count: u8 },
    CommonTechnicalFoul,
    CoachBenchDisqualifyingTech,
    UnsportsmanlikeOrDisqualifying { ft_count: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedPenalty {
    pub seq_order: u32,
    pub offended_side: CommandSide,
    pub kind: PenaltyKind,
    pub grants_possession: bool,
}

impl QueuedPenalty {
    pub fn cancel_weight(self) -> u8 {
        match self.kind {
            PenaltyKind::PersonalFoul { ft_count } => ft_count,
            PenaltyKind::CommonTechnicalFoul | PenaltyKind::CoachBenchDisqualifyingTech => 1,
            PenaltyKind::UnsportsmanlikeOrDisqualifying { ft_count } => ft_count + 2,
        }
    }
}

pub fn resolve_fiba_art42_penalties(penalties: &[QueuedPenalty]) -> Vec<QueuedPenalty> {
    let mut active = vec![true; penalties.len()];
    // Cancel equal penalties chronologically across opposing sides
    for i in 0..penalties.len() {
        if !active[i] {
            continue;
        }
        for j in (i + 1)..penalties.len() {
            if !active[j] {
                continue;
            }
            if penalties[i].offended_side != penalties[j].offended_side
                && penalties[i].cancel_weight() == penalties[j].cancel_weight()
            {
                active[i] = false;
                active[j] = false;
                break;
            }
        }
    }

    let mut remaining: Vec<QueuedPenalty> = penalties
        .iter()
        .enumerate()
        .filter(|&(i, _)| active[i])
        .map(|(_, &p)| p)
        .collect();

    // Art. 42.2.4: Remaining CommonTechnicalFoul goes FIRST (exception: CoachBenchDisqualifyingTech stays in chronological order)
    remaining.sort_by_key(|p| {
        let priority = match p.kind {
            PenaltyKind::CommonTechnicalFoul => 0u8,
            _ => 1u8,
        };
        (priority, p.seq_order)
    });
    remaining
}
