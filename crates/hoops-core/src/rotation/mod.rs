use crate::wire_schema_v1::*;

/// Dual-layer stamina step update on live-clock ticks (`game_clock_running` advancing) (§4.2).
pub fn update_stamina_live_tick(
    player: &mut PlayerRuntimeState,
    snap: &CompiledPlayerSnapshot,
    is_on_court: bool,
    speed_m_s: f64,
    high_contact_load: bool,
    tunables: &EngineTunables,
) {
    let max_spd = tunables.max_offball_speed_min_max_m_s[0]
        + (f64::from(snap.attributes[AttrIdx::TopSpeed]) / 99.0)
            * (tunables.max_offball_speed_min_max_m_s[1]
                - tunables.max_offball_speed_min_max_m_s[0]);
    let stamina_rating = f64::from(snap.attributes[AttrIdx::StaminaRating]);
    let stamina_factor =
        tunables.stamina_factor_base - tunables.stamina_factor_slope * (stamina_rating / 99.0);

    if is_on_court {
        let u = (speed_m_s / max_spd.max(1.0)).clamp(0.0, 1.2);
        let c_load = if high_contact_load { 1.0 } else { 0.0 };
        if u > tunables.burst_drain_speed_trigger_u || high_contact_load {
            let drain = (tunables.burst_drain_speed_coeff
                * (u - tunables.burst_drain_speed_offset_u).max(0.0)
                + tunables.burst_drain_contact_coeff * c_load)
                * stamina_factor;
            player.burst_energy = (player.burst_energy - drain).clamp(0.0, 100.0);
        } else {
            let rec = tunables.burst_recovery_rate
                * (1.0 - u.min(1.0))
                * (player.cumulative_stamina / 100.0);
            player.burst_energy =
                (player.burst_energy + rec).min(player.cumulative_stamina).clamp(0.0, 100.0);
        }

        let ratio = ((100.0 - player.burst_energy) / 100.0).clamp(0.0, 1.0);
        let cum_drain = (tunables.cum_stamina_base_drain_per_live_tick
            + tunables.cum_stamina_burst_load_coeff * ratio)
            * stamina_factor;
        player.cumulative_stamina = (player.cumulative_stamina - cum_drain).clamp(0.0, 100.0);
        if player.burst_energy > player.cumulative_stamina {
            player.burst_energy = player.cumulative_stamina;
        }
        player.stint_lock_remaining_live_ticks =
            player.stint_lock_remaining_live_ticks.saturating_sub(1);
    } else {
        player.cumulative_stamina = (player.cumulative_stamina
            + tunables.bench_stamina_recovery_per_live_tick)
            .clamp(0.0, 100.0);
        player.burst_energy = player.cumulative_stamina;
    }
}

/// Discrete 1-Tick `DeadTime` stamina recovery (§4.2).
pub fn apply_deadtime_stamina_recovery(
    players: &mut [PlayerRuntimeState; 30],
    reason_tag: u8,
    tunables: &EngineTunables,
) {
    let delta = match reason_tag {
        0 | 1 | 2 => tunables.timeout_stamina_recovery,
        3 => tunables.period_break_stamina_recovery,
        4 => tunables.halftime_stamina_recovery,
        _ => 0.0,
    };
    if delta <= 0.0 {
        return;
    }
    for p in players.iter_mut() {
        if p.is_present {
            p.cumulative_stamina = (p.cumulative_stamina + delta).clamp(0.0, 100.0);
            p.burst_energy = p.cumulative_stamina;
        }
    }
}

#[inline]
pub fn compute_fatigue_penalty(cum_stamina: f64, tunables: &EngineTunables) -> f64 {
    let thr = tunables.fatigue_penalty_threshold.max(1.0);
    if cum_stamina >= thr {
        0.0
    } else {
        libm::pow(((thr - cum_stamina) / thr).clamp(0.0, 1.0), tunables.fatigue_penalty_exponent)
    }
}

#[inline]
pub fn canonical_actor_id(canonical_team_id: u8, team_roster_rank: usize) -> u8 {
    (canonical_team_id << 4) | ((team_roster_rank as u8) & 0x0F)
}

#[inline]
pub fn is_player_eligible_for_sub_in(
    p: &PlayerRuntimeState,
    ruleset: &RuleSet,
    on_court_count: usize,
    correctable_ft_exception: bool,
) -> bool {
    if !p.is_present
        || p.on_court_slot.is_some()
        || p.ejected
        || p.locked_out_for_game
        || p.foul_out_seq.is_some()
    {
        return false;
    }
    match p.injury_state {
        PlayerInjuryState::Healthy => {}
        _ => return false,
    }
    if ruleset.roster.fiba_sub_clock_run_reentry_rule
        && p.fiba_sub_reentry_lock == FibaSubReentryLock::SubbedOutCannotReenter
        && on_court_count >= 5
        && !correctable_ft_exception
    {
        return false;
    }
    true
}

#[inline]
pub fn is_mandatory_out(p: &PlayerRuntimeState) -> bool {
    p.ejected
        || p.locked_out_for_game
        || p.foul_out_seq.is_some()
        || !matches!(p.injury_state, PlayerInjuryState::Healthy)
}

/// 6-tier strict total order AI rotation (`P1 ≻ P0 ≻ P2 ≻ P3 ≻ P4 ≻ P5`, §9).
/// Returns list of `(out_roster_idx, in_roster_idx, sub_priority_tier)` substitutions.
pub fn plan_ai_substitutions(
    team_side: CommandSide,
    team_offset: usize, // 0 for Home (0..15), 15 for Away (15..30)
    team_snap: &CompiledTeamSnapshot,
    team_state: &TeamRuntimeState,
    opp_score: u16,
    players: &[PlayerRuntimeState; 30],
    quarter: u8,
    game_clock_ds: u16,
    ruleset: &RuleSet,
    tunables: &EngineTunables,
    canonical_team_id: u8,
) -> Vec<(u8, u8, u8)> {
    let mut subs = Vec::new();
    let roster_len = team_snap.players.len().min(15);
    let score_diff = (team_state.score_on_court as i32) - (opp_score as i32);
    let is_fiba = ruleset.roster.fiba_sub_clock_run_reentry_rule;

    let mut on_court: Vec<usize> = team_state
        .active_lineup_slots
        .iter()
        .filter(|&&r| r != ROSTER_IDX_NONE && (r as usize) < team_offset + roster_len)
        .map(|&r| r as usize)
        .collect();
    let on_court_count = on_court.len();

    let mut used_in = [false; 30];

    let pick_best_bench = |role_pref: usize,
                           allow_locked_reentry: bool,
                           used: &[bool; 30]|
     -> Option<usize> {
        let mut best: Option<(usize, f64, f64, u8)> = None;
        for rank in 0..roster_len {
            let r = team_offset + rank;
            if used[r] {
                continue;
            }
            let p = &players[r];
            if !p.is_present
                || p.on_court_slot.is_some()
                || p.ejected
                || p.locked_out_for_game
                || p.foul_out_seq.is_some()
                || !matches!(p.injury_state, PlayerInjuryState::Healthy)
            {
                continue;
            }
            if is_fiba
                && p.fiba_sub_reentry_lock == FibaSubReentryLock::SubbedOutCannotReenter
                && !allow_locked_reentry
            {
                continue;
            }
            let snap = &team_snap.players[rank];
            let role_score = snap.functional.scores[role_pref.min(8)];
            let cid = canonical_actor_id(canonical_team_id, rank);
            let cand = (r, p.cumulative_stamina, role_score, cid);
            let better = match best {
                None => true,
                Some(b) => {
                    cand.1 > b.1
                        || (cand.1 == b.1 && cand.2 > b.2)
                        || (cand.1 == b.1 && cand.2 == b.2 && cand.3 < b.3)
                }
            };
            if better {
                best = Some(cand);
            }
        }
        best.map(|b| b.0)
    };

    // P1: Mandatory out (bypasses P0 lock & allows locked reentry only if no unlocked bench left)
    let mut still_on = Vec::new();
    for &r in &on_court {
        let p = &players[r];
        if is_mandatory_out(p) && !p.fouled_out_staying_on_court {
            let bench = pick_best_bench(0, false, &used_in)
                .or_else(|| pick_best_bench(0, true, &used_in));
            if let Some(in_r) = bench {
                used_in[in_r] = true;
                subs.push((r as u8, in_r as u8, 0)); // P1 = tier 0
            }
        } else {
            still_on.push(r);
        }
    }
    on_court = still_on;

    // For P2..P5, respect P0 stint lock (`stint_lock_remaining_live_ticks > 0`) & FIBA SubbedInCannotLeave
    let can_sub_out_voluntary = |r: usize| -> bool {
        let p = &players[r];
        if p.stint_lock_remaining_live_ticks > 0 {
            return false;
        }
        if is_fiba && p.fiba_sub_reentry_lock == FibaSubReentryLock::SubbedInCannotLeave {
            return false;
        }
        true
    };

    // P2: Foul trouble protection
    let foul_thresholds = if is_fiba {
        tunables.rotation_foul_trouble_fiba_q1_h1_q3
    } else {
        tunables.rotation_foul_trouble_nba_q1_h1_q3
    };
    let foul_unlock = quarter >= 4 && game_clock_ds <= tunables.rotation_foul_unlock_q4_clock_ds;
    if !foul_unlock {
        let limit = match quarter {
            1 => foul_thresholds[0],
            2 => foul_thresholds[1],
            3 => foul_thresholds[2],
            _ => 255,
        };
        let mut next_on = Vec::new();
        for &r in &on_court {
            let p = &players[r];
            if p.personal_fouls >= limit && can_sub_out_voluntary(r) {
                if let Some(in_r) = pick_best_bench(6, on_court_count < 5, &used_in) {
                    if players[in_r].personal_fouls < limit {
                        used_in[in_r] = true;
                        subs.push((r as u8, in_r as u8, 2)); // P2 = tier 2
                        continue;
                    }
                }
            }
            next_on.push(r);
        }
        on_court = next_on;
    }

    // P3: Stamina fatigue or garbage time
    let is_garbage = quarter >= 4
        && score_diff.unsigned_abs() >= (tunables.garbage_time_abs_diff_and_clock_ds[0] as u32)
        && game_clock_ds <= tunables.garbage_time_abs_diff_and_clock_ds[1];
    let mut next_on = Vec::new();
    for &r in &on_court {
        let p = &players[r];
        let fatigued = p.cumulative_stamina < tunables.rotation_sub_out_stamina;
        if (fatigued || is_garbage) && can_sub_out_voluntary(r) {
            if let Some(in_r) = pick_best_bench(2, on_court_count < 5, &used_in) {
                let threshold = if is_garbage {
                    50.0
                } else {
                    tunables.rotation_sub_in_stamina
                };
                if players[in_r].cumulative_stamina >= threshold
                    && players[in_r].cumulative_stamina > p.cumulative_stamina + 8.0
                {
                    used_in[in_r] = true;
                    subs.push((r as u8, in_r as u8, 3)); // P3 = tier 3
                    continue;
                }
            }
        }
        next_on.push(r);
    }
    on_court = next_on;

    // P4: Rotation script window matching
    let q_bit: u8 = if quarter <= 4 {
        1u8 << (quarter - 1)
    } else {
        1u8 << 4
    };
    if let Some(stint) = team_snap.tactics.rotation_script.stints.iter().find(|s| {
        (s.quarter_mask & q_bit) != 0
            && game_clock_ds <= s.start_game_clock_ds
            && game_clock_ds >= s.end_game_clock_ds
    }) {
        for &target_rank in &stint.target_lineup_ranks {
            if (target_rank as usize) >= roster_len {
                continue;
            }
            let target_r = team_offset + (target_rank as usize);
            if players[target_r].on_court_slot.is_some()
                || used_in[target_r]
                || !is_player_eligible_for_sub_in(&players[target_r], ruleset, on_court_count, false)
                || players[target_r].cumulative_stamina < tunables.rotation_sub_in_stamina
            {
                continue;
            }
            // Find a non-target voluntary player on court to sub out
            if let Some(out_pos) = on_court.iter().position(|&r| {
                let rank = r - team_offset;
                !stint.target_lineup_ranks.contains(&(rank as u8)) && can_sub_out_voluntary(r)
            }) {
                let out_r = on_court.remove(out_pos);
                used_in[target_r] = true;
                subs.push((out_r as u8, target_r as u8, 4)); // P4 = tier 4
            }
        }
    }

    let _ = team_side;
    subs
}

/// Update clutch intentional foul & foul-up-3 coaching flags (§9).
pub fn update_clutch_coaching_flags(
    team_state: &mut TeamRuntimeState,
    opp_score: u16,
    opp_in_bonus_on_next_foul: bool,
    quarter: u8,
    game_clock_ds: u16,
    shot_clock_ds: Option<u16>,
    policy: IntentionalFoulPolicy,
    foul_up_three_policy: FoulUpThreePolicy,
    tunables: &EngineTunables,
) -> bool {
    let diff = (team_state.score_on_court as i32) - (opp_score as i32);
    let [min_def, max_def] = tunables.intentional_foul_deficit_min_max;
    let margin_ok = shot_clock_ds.map_or(true, |sc| {
        game_clock_ds <= sc.saturating_add(tunables.intentional_foul_clock_margin_ds)
    });
    team_state.intentional_foul_mode = match policy {
        IntentionalFoulPolicy::Disabled => false,
        IntentionalFoulPolicy::ForceEnabled => true,
        IntentionalFoulPolicy::Auto => {
            quarter >= 4
                && diff < 0
                && diff.unsigned_abs() >= (min_def as u32)
                && diff.unsigned_abs() <= (max_def as u32)
                && margin_ok
        }
    };

    let foul_up_three_active = matches!(foul_up_three_policy, FoulUpThreePolicy::Under6sIfBonus)
        && quarter >= 4
        && diff == 3
        && game_clock_ds <= tunables.foul_up_three_max_clock_ds
        && opp_in_bonus_on_next_foul;

    team_state.intentional_foul_mode || foul_up_three_active
}
