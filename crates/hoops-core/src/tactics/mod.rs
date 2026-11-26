use crate::model::SUB_DS_UNITS;
use crate::physics::CourtGeometry;
use crate::resolve::{compute_pass_intercept_prob, compute_shot_probabilities, ShotEvalInputs, ShotOutcomeProbs};
use crate::wire_schema_v1::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum CandidateAction {
    Hold = 0,
    Pass { target_slot: u8, receiver_cid: u8 } = 1,
    Shoot = 2,
    Drive { corridor: u8 } = 3,
    Pivot { dir_idx: u8 } = 4,
    FollowPlay = 5,
}

impl CandidateAction {
    pub fn order_key(self) -> (u8, u8) {
        match self {
            Self::Hold => (0, 0),
            Self::Pass { receiver_cid, .. } => (1, receiver_cid),
            Self::Shoot => (2, 0),
            Self::Drive { corridor } => (3, corridor),
            Self::Pivot { dir_idx } => (4, dir_idx),
            Self::FollowPlay => (5, 0),
        }
    }
}

/// Legal action mask from `FootworkRecord` (§6.1.1).
pub fn legal_actions_for_holder(
    fw: &FootworkRecord,
    active_teammates: &[(u8, u8)], // (slot, canonical_actor_id)
    has_active_play: bool,
    shot_clock_ds: Option<u16>,
    tunables: &EngineTunables,
    endgame_wp_mode: bool,
    score_diff: i32,
    shot_clock_rem_10us: Option<u32>,
) -> Vec<CandidateAction> {
    let mut out = Vec::with_capacity(16);
    let can_pivot = fw.pivot_foot != PivotFoot::LockedBothFeetAfterStep1JumpStop
        && !fw.landed_simultaneously_after_step1;

    let hold_allowed_by_sc = match shot_clock_rem_10us {
        Some(sc_u) if endgame_wp_mode => {
            sc_u > (tunables.hold_min_shot_clock_ds as u32) * SUB_DS_UNITS
        }
        _ => true,
    };

    match fw.dribble_status {
        DribbleStatus::None => {}
        DribbleStatus::LiveTripleThreat => {
            out.push(CandidateAction::Shoot);
            for &(slot, cid) in active_teammates {
                out.push(CandidateAction::Pass {
                    target_slot: slot,
                    receiver_cid: cid,
                });
            }
            for corridor in 0..4 {
                out.push(CandidateAction::Drive { corridor });
            }
            if can_pivot {
                out.push(CandidateAction::Pivot { dir_idx: 0 });
                out.push(CandidateAction::Pivot { dir_idx: 1 });
            }
            if has_active_play
                && shot_clock_ds.map_or(true, |sc| sc > tunables.follow_play_min_shot_clock_ds)
                && !(endgame_wp_mode && score_diff > 0 && shot_clock_ds.is_none())
            {
                out.push(CandidateAction::FollowPlay);
            }
            if hold_allowed_by_sc {
                out.push(CandidateAction::Hold);
            }
        }
        DribbleStatus::Dribbling => {
            out.push(CandidateAction::Shoot);
            for &(slot, cid) in active_teammates {
                out.push(CandidateAction::Pass {
                    target_slot: slot,
                    receiver_cid: cid,
                });
            }
            for corridor in 0..4 {
                out.push(CandidateAction::Drive { corridor });
            }
            if has_active_play
                && shot_clock_ds.map_or(true, |sc| sc > tunables.follow_play_min_shot_clock_ds)
                && !(endgame_wp_mode && score_diff > 0 && shot_clock_ds.is_none())
            {
                out.push(CandidateAction::FollowPlay);
            }
            if hold_allowed_by_sc {
                out.push(CandidateAction::Hold);
            }
        }
        DribbleStatus::Gathered => {
            out.push(CandidateAction::Shoot);
            for &(slot, cid) in active_teammates {
                out.push(CandidateAction::Pass {
                    target_slot: slot,
                    receiver_cid: cid,
                });
            }
            if can_pivot {
                out.push(CandidateAction::Pivot { dir_idx: 0 });
                out.push(CandidateAction::Pivot { dir_idx: 1 });
            }
            if hold_allowed_by_sc {
                out.push(CandidateAction::Hold);
            }
        }
        DribbleStatus::AirborneWithBall => {
            out.push(CandidateAction::Shoot);
            for &(slot, cid) in active_teammates {
                out.push(CandidateAction::Pass {
                    target_slot: slot,
                    receiver_cid: cid,
                });
            }
        }
    }
    out
}

/// 读取球员指标。防守条件中的身体数据保留原单位。
pub fn eval_slot_metric(player: &CompiledPlayerSnapshot, key: u16) -> f64 {
    if let Some(attr) = AttrIdx::from_metric_key(key) {
        return f64::from(player.attributes[attr]);
    }
    match key {
        0 => player.physical.height_cm,
        1 => player.physical.wingspan_cm,
        2 => player.physical.standing_reach_cm,
        3 => player.physical.weight_kg,
        4 => player.physical.age as f64,
        5 => player.physical.dominant_hand as f64,
        6 => player.physical.display_position as f64,
        45..=53 => player.functional.scores[(key - 45) as usize],
        60..=64 => player.tendencies.zone_weights[(key - 60) as usize] * 100.0,
        65 => player.tendencies.drive_vs_pullup * 100.0,
        66 => player.tendencies.pass_vs_shoot * 100.0,
        67 => player.tendencies.pnr_roll_vs_pop * 100.0,
        68 => player.tendencies.crash_orb * 100.0,
        69 => player.tendencies.gamble_steal * 100.0,
        _ => 0.0,
    }
}

/// 将身体指标缩放到 `0..=100`，用于和球员能力加权求和。
pub fn eval_role_weight_metric(player: &CompiledPlayerSnapshot, key: u16) -> f64 {
    let (min, max) = match key {
        0 => (180.0, 218.0),
        1 => (175.0, 235.0),
        2 => (235.0, 290.0),
        3 => (78.0, 128.0),
        4 => (18.0, 42.0),
        5 => (0.0, 1.0),
        6 => (0.0, 4.0),
        _ => return eval_slot_metric(player, key),
    };
    100.0 * ((eval_slot_metric(player, key) - min) / (max - min)).clamp(0.0, 1.0)
}

/// Deterministic K! bijective slot assignment for K in 2..=5 active offensive players (§5.2).
/// Returns `[roster_slot_or_0xFF; 5]` indexed by playbook role index `0..5`.
pub fn assign_playbook_roles(
    playbook: &CompiledPlaybook,
    active_players: &[(u8, &CompiledPlayerSnapshot)], // (team_slot 0..5, snap)
) -> [u8; 5] {
    let k = active_players.len().min(5);
    if k == 0 {
        return [ROSTER_IDX_NONE; 5];
    }
    // Priority-ordered roles: first K roles in `playbook.role_priority`
    let mut scores = [[0.0_f64; 5]; 5]; // scores[role_slot][player_idx]
    for r_pos in 0..k {
        let role_id = playbook.role_priority[r_pos] as usize;
        let weights = &playbook.slot_weights[role_id.min(4)];
        for (p_idx, &(_, snap)) in active_players.iter().take(k).enumerate() {
            let mut s = 0.0;
            for &(m_key, w) in weights {
                s += w * eval_role_weight_metric(snap, m_key);
            }
            scores[r_pos][p_idx] = s;
        }
    }

    let mut perm: [usize; 5] = [0, 1, 2, 3, 4];
    let mut best_perm = perm;
    let mut best_score = f64::NEG_INFINITY;

    permute_eval(k, 0, &mut perm, &scores, &mut best_score, &mut best_perm);

    let mut binding = [ROSTER_IDX_NONE; 5];
    for r_pos in 0..k {
        let role_id = (playbook.role_priority[r_pos] as usize).min(4);
        let p_slot = active_players[best_perm[r_pos]].0;
        binding[role_id] = p_slot;
    }
    binding
}

fn permute_eval(
    k: usize,
    pos: usize,
    perm: &mut [usize; 5],
    scores: &[[f64; 5]; 5],
    best_score: &mut f64,
    best_perm: &mut [usize; 5],
) {
    if pos == k {
        let mut tot = 0.0;
        for r in 0..k {
            tot += scores[r][perm[r]];
        }
        if tot > *best_score {
            *best_score = tot;
            *best_perm = *perm;
        }
        return;
    }
    for i in pos..k {
        perm.swap(pos, i);
        permute_eval(k, pos + 1, perm, scores, best_score, best_perm);
        perm.swap(pos, i);
    }
}

/// Resolve semantic anchor coordinate `(x_m, y_m)` on court (§5.2).
pub fn resolve_anchor_coord(
    geom: &CourtGeometry,
    anchor_kind: u8,
    side_tag: u8,
    dx_m: f64,
    dy_m: f64,
    attack_dir: f64,
    mirror: bool,
) -> (f64, f64) {
    let (rim_x, _) = geom.rim_xy(attack_dir);
    let side_sign = match side_tag {
        1 => 1.0,  // Strong
        2 => -1.0, // Weak
        _ => 1.0,
    };
    let mirror_sign = if mirror { -1.0 } else { 1.0 };
    let y_sign = side_sign * mirror_sign;

    let (base_x, base_y) = match anchor_kind {
        0 => (rim_x, 0.0), // Rim
        1 => (
            attack_dir * (geom.half_length_m - 1.4),
            y_sign * (geom.corner_three_dist_m + 0.25),
        ), // Corner3
        2 => (
            rim_x - attack_dir * 4.8,
            y_sign * (geom.three_arc_radius_m * 0.78),
        ), // Wing3
        3 => (
            rim_x - attack_dir * (geom.three_arc_radius_m + 0.35),
            y_sign * 3.2,
        ), // Slot
        4 => (rim_x - attack_dir * (geom.three_arc_radius_m + 0.65), 0.0), // TopKey
        5 => (
            attack_dir * (geom.half_length_m - geom.paint_length_m),
            y_sign * (geom.paint_width_m * 0.5),
        ), // Elbow
        6 => (rim_x - attack_dir * 1.4, y_sign * (geom.paint_width_m * 0.55)), // LowPost
        _ => (rim_x - attack_dir * 0.45, y_sign * 1.85), // DunkerSpot
    };

    let x = base_x + attack_dir * dx_m;
    let y = base_y + mirror_sign * dy_m;
    let bounds = geom.inbounds_nav_bounds(0.55);
    bounds.clamp(x, y)
}

/// Safe self-loop downgrade for playbook actions when target role is UnassignedRole (§5.2).
pub fn resolve_safe_playbook_action(
    actor_role: u8,
    action_kind: u8,
    target_role: u8,
    primary_role: u8,
    slot_bindings: &[u8; 5],
    has_ball: bool,
) -> (u8, u8) {
    if action_kind != 2 && action_kind != 5 {
        return (action_kind, target_role);
    }
    let resolved_target = if (target_role as usize) < 5
        && slot_bindings[target_role as usize] != ROSTER_IDX_NONE
    {
        target_role
    } else {
        primary_role
    };
    if actor_role == resolved_target {
        // Real self-loop: downgrade to DribbleHold(1) if ball-handler or SpaceAnchor(0)
        (if has_ball { 1 } else { 0 }, resolved_target)
    } else {
        (action_kind, resolved_target)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PnrEvalContext<'a> {
    pub handler: &'a CompiledPlayerSnapshot,
    pub screener: &'a CompiledPlayerSnapshot,
    pub screener_def: &'a CompiledPlayerSnapshot,
    pub matchup_height_diff_cm: f64,
    pub shot_clock_ds: f64,
    pub game_clock_ds: f64,
    pub score_diff: f64,
    pub defender_fouls: f64,
}

pub fn eval_predicate(expr: &PredicateExpr, ctx: &PnrEvalContext<'_>, target: &CompiledPlayerSnapshot) -> bool {
    match expr {
        PredicateExpr::All(list) => list.iter().all(|c| eval_predicate(c, ctx, target)),
        PredicateExpr::Any(list) => list.iter().any(|c| eval_predicate(c, ctx, target)),
        PredicateExpr::Not(inner) => !eval_predicate(inner, ctx, target),
        PredicateExpr::Cmp {
            subject,
            metric_key,
            op,
            val,
        } => {
            let lhs = match subject {
                0 => eval_slot_metric(target, *metric_key),
                1 => eval_slot_metric(ctx.handler, *metric_key),
                2 => eval_slot_metric(ctx.screener, *metric_key),
                3 => eval_slot_metric(ctx.screener_def, *metric_key),
                _ => match *metric_key {
                    54 => ctx.matchup_height_diff_cm,
                    55 => ctx.shot_clock_ds,
                    56 => ctx.game_clock_ds,
                    57 => ctx.score_diff,
                    58 => 6.5 + (ctx.handler.attributes[AttrIdx::ShotRange] as f64 / 99.0) * 3.5,
                    59 => ctx.defender_fouls,
                    _ => 0.0,
                },
            };
            match op {
                0 => lhs < *val,
                1 => lhs <= *val,
                2 => lhs > *val,
                _ => lhs >= *val,
            }
        }
    }
}

/// Deduce coverage from the visible response around a screen.
/// `None` indicates that the positions do not distinguish a coverage.
pub fn observe_pnr_coverage(
    handler: (f64, f64),
    screener: (f64, f64),
    defenders: &[Option<(f64, f64)>; 5],
    attack_dir: f64,
) -> Option<PnrCoverageKind> {
    let screen_gap = libm::hypot(screener.0 - handler.0, screener.1 - handler.1);
    if !(0.7..=4.0).contains(&screen_gap) {
        return None;
    }
    let mut near_handler = 0;
    let mut near_screen = 0;
    let mut deep = false;
    let mut high = false;
    for &(x, y) in defenders.iter().flatten() {
        let depth = (x - screener.0) * attack_dir;
        let d_handler = libm::hypot(x - handler.0, y - handler.1);
        let d_screen = libm::hypot(x - screener.0, y - screener.1);
        near_handler += usize::from(d_handler <= 1.75);
        near_screen += usize::from(d_screen <= 1.4 && d_handler > 1.75);
        deep |= (1.4..=3.8).contains(&depth) && (y - screener.1).abs() <= 2.0;
        high |= depth <= 0.15 && d_screen <= 1.8 && (1.75..=2.7).contains(&d_handler);
    }
    if near_handler >= 2 {
        Some(PnrCoverageKind::Blitz)
    } else if near_handler == 1 && high {
        Some(PnrCoverageKind::HighShowAndRecover)
    } else if near_handler == 1 && near_screen >= 1 && !deep && screen_gap >= 2.4 {
        Some(PnrCoverageKind::Switch)
    } else if near_handler == 1 && deep {
        Some(PnrCoverageKind::Drop)
    } else {
        None
    }
}

/// 3-tier PnR coverage decision chain (§5.3).
pub fn resolve_pnr_coverage(
    gameplan: &CompiledDefenseGameplan,
    override_rule: Option<PnrRuleId>,
    adaptive: &AdaptiveCoverageState,
    ctx: &PnrEvalContext<'_>,
) -> (PnrCoverageKind, u16) {
    if let Some(ov_id) = override_rule {
        for (idx, rule) in gameplan.pnr_rules.iter().enumerate() {
            if rule.rule_id == ov_id {
                return (rule.coverage, idx as u16);
            }
        }
    }
    if adaptive.escalated {
        return (adaptive.active_coverage, 0xFFFF);
    }
    for (idx, rule) in gameplan.pnr_rules.iter().enumerate() {
        if rule
            .predicates
            .iter()
            .all(|p| eval_predicate(p, ctx, ctx.handler))
        {
            return (rule.coverage, idx as u16);
        }
    }
    (PnrCoverageKind::Drop, 0xFFFF)
}

/// Bayesian shrinkage defensive coverage adaptation update (§5.3).
pub fn update_adaptive_coverage(
    state: &mut AdaptiveCoverageState,
    gameplan: &CompiledDefenseGameplan,
    points_allowed: u8,
    off_has_three_threat: bool,
) {
    state.n_possessions = state.n_possessions.saturating_add(1);
    state.observed_points = state.observed_points.saturating_add(points_allowed as u32);
    if state.cooldown_remaining > 0 {
        state.cooldown_remaining -= 1;
        return;
    }
    for trig in &gameplan.adaptation_triggers {
        let shrunk_ppp = ((state.observed_points as f64) + trig.prior_weight * trig.prior_ppp)
            / ((state.n_possessions as f64) + trig.prior_weight);
        if !state.escalated && state.active_coverage == trig.watch_coverage {
            if state.n_possessions >= trig.min_possessions
                && shrunk_ppp >= trig.escalate_ppp_threshold
            {
                state.active_coverage = trig.escalate_to_coverage;
                state.escalated = true;
                state.n_possessions = 0;
                state.observed_points = 0;
                state.cooldown_remaining = trig.cooldown_possessions;
                return;
            }
        } else if state.escalated && state.active_coverage == trig.escalate_to_coverage {
            let fail_revert = state.n_possessions >= trig.revert_min_possessions
                && shrunk_ppp >= trig.revert_if_escalated_ppp_exceeds;
            let threat_gone = !off_has_three_threat;
            let cool_revert = state.n_possessions >= trig.deescalate_min_possessions
                && shrunk_ppp <= trig.deescalate_ppp_threshold;
            if fail_revert || threat_gone || cool_revert {
                state.active_coverage = trig.watch_coverage;
                state.escalated = false;
                state.n_possessions = 0;
                state.observed_points = 0;
                state.cooldown_remaining = trig.cooldown_possessions;
                return;
            }
        }
    }
}

/// 2D Ball-Man-Basket help triangle calculation (§6.3).
pub fn compute_help_triangle(
    ball_xy: (f64, f64),
    on_def_xy: (f64, f64),
    rim_xy: (f64, f64),
    weak_man_xy: (f64, f64),
    ball_vel_xy: (f64, f64),
    help_shrink_level: u8,
    weak_shooter_three: u8,
    weak_shooter_catch: u8,
    geom: &CourtGeometry,
    tunables: &EngineTunables,
) -> (f64, f64, (f64, f64)) {
    let rx = rim_xy.0 - ball_xy.0;
    let ry = rim_xy.1 - ball_xy.1;
    let r_ball = libm::hypot(rx, ry);
    let inv_r = 1.0 / r_ball.max(1.0e-6);
    let ux = rx * inv_r;
    let uy = ry * inv_r;

    let r_on_def = libm::hypot(rim_xy.0 - on_def_xy.0, rim_xy.1 - on_def_xy.1);
    let dx_def = on_def_xy.0 - ball_xy.0;
    let dy_def = on_def_xy.1 - ball_xy.1;
    let proj = dx_def * ux + dy_def * uy;
    let s_lat = libm::hypot(dx_def - proj * ux, dy_def - proj * uy);

    let r_outer = geom.corner_three_dist_m;
    let r_inner = geom.restricted_area_radius_m;
    let d_dist = ((r_outer - r_ball) / (r_outer - r_inner).max(0.1)).clamp(0.0, 1.0);

    let v_toward = (ball_vel_xy.0 * ux + ball_vel_xy.1 * uy).max(0.0);
    let d_vel = (v_toward / tunables.d_pen_norm_speed_m_s.max(0.5)).clamp(0.0, 1.0);

    let [b0, b1, b2, b3] = tunables.b_beat_params;
    let b_beat = (((r_on_def - r_ball) + b0) / b1.max(0.05))
        .max((s_lat - b2) / b3.max(0.05))
        .clamp(0.0, 1.0);

    let w = tunables.d_pen_weights;
    let d_pen = (w[0] * d_dist
        + w[1] * b_beat * (w[2] + w[3] * d_vel)
        + w[4] * d_dist * (w[5] * d_vel + w[6] * b_beat))
        .clamp(0.0, 1.0);

    let h_shrink = (help_shrink_level as f64) / 100.0;
    let t_3pt = (0.5 * (weak_shooter_three as f64) + 0.5 * (weak_shooter_catch as f64)) / 99.0;
    let a = tunables.lambda_help_params;
    let lambda_help = (a[0] + a[1] * h_shrink * (a[2] + a[3] * d_pen) * (a[4] - a[5] * t_3pt))
        .clamp(a[6], a[7]);

    // Target point interpolates from weak_man toward rim paint protect spot
    let protect_x = rim_xy.0 - (rim_xy.0.signum()) * 1.4;
    let protect_y = 0.0;
    let target_x = weak_man_xy.0 * (1.0 - lambda_help) + protect_x * lambda_help;
    let target_y = weak_man_xy.1 * (1.0 - lambda_help) + protect_y * lambda_help;

    (d_pen, lambda_help, (target_x, target_y))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WpOwner {
    SelfTeam,
    None,
    OppTeam,
}

/// Closed 5-parameter Endgame Win Probability evaluator V_WP (§6.1.3).
pub fn evaluate_v_wp(
    delta_s: f64,
    t_rem_ds: f64,
    owner: WpOwner,
    sc_off: bool,
    is_deadball_restart: bool,
    ruleset: &RuleSet,
    tunables: &EngineTunables,
) -> f64 {
    if t_rem_ds <= 0.0 {
        return if delta_s > 0.0 {
            1.0
        } else if delta_s < 0.0 {
            0.0
        } else {
            0.5
        };
    }

    let s_owner = match owner {
        WpOwner::SelfTeam => 1.0,
        WpOwner::None => 0.0,
        WpOwner::OppTeam => -1.0,
    };

    let hold_clock_kill = sc_off
        && ((owner == WpOwner::SelfTeam && delta_s > 0.0)
            || (owner == WpOwner::OppTeam && delta_s < 0.0));

    let is_03s_restricted = ruleset.clocks.min_catch_and_shoot_ds > 0
        && t_rem_ds < (ruleset.clocks.min_catch_and_shoot_ds as f64)
        && (ruleset.clocks.live_rebound_03s_enabled || is_deadball_restart);

    let alpha_first = if owner == WpOwner::None || hold_clock_kill {
        0.0
    } else if is_03s_restricted {
        let abs_diff = libm::fabs(delta_s);
        if abs_diff < (tunables.wp_alpha_first_03s_max_abs_diff as f64) {
            tunables.wp_alpha_first_03s_scale
        } else {
            0.0
        }
    } else {
        let [ramp, floor] = tunables.wp_alpha_first_ramp_ds_and_floor;
        (t_rem_ds / ramp.max(1.0)).clamp(floor, 1.0)
    };

    let mu_first = alpha_first * tunables.wp_first_poss_ev;
    let sigma2_first = if hold_clock_kill {
        let [kramp, kfloor] = tunables.wp_kill_var_ramp_ds_and_floor;
        tunables.wp_floor_var * (t_rem_ds / kramp.max(1.0)).clamp(kfloor, 1.0)
    } else {
        alpha_first * tunables.wp_first_poss_var
    };

    let n_pairs = if hold_clock_kill {
        0.0
    } else {
        ((t_rem_ds - t_rem_ds.min(tunables.wp_avg_poss_ds))
            / (2.0 * tunables.wp_avg_poss_ds.max(1.0)))
        .max(0.0)
    };

    let mu_net = delta_s + s_owner * mu_first;
    let sigma_tot = libm::sqrt(
        (n_pairs * tunables.wp_pair_variance + sigma2_first).max(tunables.wp_floor_var),
    );
    let z = -mu_net / (core::f64::consts::SQRT_2 * sigma_tot);
    (0.5 * libm::erfc(z)).clamp(0.0, 1.0)
}

#[derive(Debug, Clone, Copy)]
pub struct EndgameWpContext<'a> {
    pub delta_s: f64,
    pub gc_rem_10us: u32,
    pub sc_rem_10us: Option<u32>,
    pub is_deadball_restart: bool,
    pub opp_in_bonus_on_next_foul: bool,
    pub intentional_foul_policy: IntentionalFoulPolicy,
    pub lambda_steal_per_ds: f64,
    pub lambda_foul_per_ds: f64,
    pub p_ft: f64,
    pub p_orb: f64,
    pub p_orb_ft: f64,
    pub ruleset: &'a RuleSet,
    pub tunables: &'a EngineTunables,
}

#[inline]
fn ceil_10us_to_ds(u: u32) -> f64 {
    ((u.saturating_add(9_999)) / SUB_DS_UNITS) as f64
}

#[inline]
fn competing_risks(lambda_steal: f64, lambda_foul: f64, delta_t_ds: f64) -> (f64, f64, f64) {
    let l_tot = lambda_steal + lambda_foul;
    if l_tot <= 0.0 || delta_t_ds <= 0.0 {
        return (1.0, 0.0, 0.0);
    }
    let p_survive = libm::exp(-l_tot * delta_t_ds);
    let p_ev = 1.0 - p_survive;
    let p_steal = p_ev * (lambda_steal / l_tot);
    let p_int_foul = p_ev - p_steal;
    (p_survive, p_steal, p_int_foul)
}

fn expected_wp_after_foul(ctx: &EndgameWpContext<'_>, t_ev_ds: f64) -> f64 {
    if !ctx.opp_in_bonus_on_next_foul {
        return evaluate_v_wp(
            ctx.delta_s,
            t_ev_ds,
            WpOwner::SelfTeam,
            t_ev_ds <= 240.0,
            true,
            ctx.ruleset,
            ctx.tunables,
        );
    }
    let p = ctx.p_ft;
    let q = 1.0 - p;
    let p_o = ctx.p_orb_ft;
    let sc_off = t_ev_ds <= 240.0;

    let v_11 = evaluate_v_wp(ctx.delta_s + 2.0, t_ev_ds, WpOwner::OppTeam, sc_off, true, ctx.ruleset, ctx.tunables);
    let v_01 = evaluate_v_wp(ctx.delta_s + 1.0, t_ev_ds, WpOwner::OppTeam, sc_off, true, ctx.ruleset, ctx.tunables);
    let v_10_reb = p_o * evaluate_v_wp(ctx.delta_s + 1.0, t_ev_ds, WpOwner::SelfTeam, t_ev_ds <= 140.0, false, ctx.ruleset, ctx.tunables)
        + (1.0 - p_o) * evaluate_v_wp(ctx.delta_s + 1.0, t_ev_ds, WpOwner::OppTeam, sc_off, false, ctx.ruleset, ctx.tunables);
    let v_00_reb = p_o * evaluate_v_wp(ctx.delta_s, t_ev_ds, WpOwner::SelfTeam, t_ev_ds <= 140.0, false, ctx.ruleset, ctx.tunables)
        + (1.0 - p_o) * evaluate_v_wp(ctx.delta_s, t_ev_ds, WpOwner::OppTeam, sc_off, false, ctx.ruleset, ctx.tunables);

    p * p * v_11 + q * p * v_01 + p * q * v_10_reb + q * q * v_00_reb
}

fn expected_wp_n_ft(ctx: &EndgameWpContext<'_>, n_ft: u8, base_delta_s: f64, t_foul_ds: f64) -> f64 {
    let p = ctx.p_ft;
    let q = 1.0 - p;
    let p_o = ctx.p_orb_ft;
    let sc_off_opp = t_foul_ds <= 240.0;
    let sc_off_orb = t_foul_ds <= 140.0;
    if n_ft == 1 {
        let made = evaluate_v_wp(base_delta_s + 1.0, t_foul_ds, WpOwner::OppTeam, sc_off_opp, true, ctx.ruleset, ctx.tunables);
        let miss = p_o * evaluate_v_wp(base_delta_s, t_foul_ds, WpOwner::SelfTeam, sc_off_orb, false, ctx.ruleset, ctx.tunables)
            + (1.0 - p_o) * evaluate_v_wp(base_delta_s, t_foul_ds, WpOwner::OppTeam, sc_off_opp, false, ctx.ruleset, ctx.tunables);
        p * made + q * miss
    } else if n_ft == 2 {
        let mut temp_ctx = *ctx;
        temp_ctx.delta_s = base_delta_s;
        temp_ctx.opp_in_bonus_on_next_foul = true;
        expected_wp_after_foul(&temp_ctx, t_foul_ds)
    } else {
        // 3 FTs: binomial for first 2 shots + last shot rebound split
        let mut total = 0.0;
        for made_first_two in 0..=2u8 {
            let prob_first_two = match made_first_two {
                0 => q * q,
                1 => 2.0 * p * q,
                _ => p * p,
            };
            let ds_after_two = base_delta_s + (made_first_two as f64);
            let made_last = evaluate_v_wp(ds_after_two + 1.0, t_foul_ds, WpOwner::OppTeam, sc_off_opp, true, ctx.ruleset, ctx.tunables);
            let miss_last = p_o * evaluate_v_wp(ds_after_two, t_foul_ds, WpOwner::SelfTeam, sc_off_orb, false, ctx.ruleset, ctx.tunables)
                + (1.0 - p_o) * evaluate_v_wp(ds_after_two, t_foul_ds, WpOwner::OppTeam, sc_off_opp, false, ctx.ruleset, ctx.tunables);
            total += prob_first_two * (p * made_last + q * miss_last);
        }
        total
    }
}

/// Evaluate `WP(Shoot)` with exact `10 µs` release gate and per-branch timestamps (§6.1.3).
pub fn evaluate_wp_shoot(
    ctx: &EndgameWpContext<'_>,
    probs: ShotOutcomeProbs,
    shot_val: u8,
    rel_ds: u16,
    flight_ds: u16,
) -> f64 {
    let delta_u_rel = (rel_ds as u32) * SUB_DS_UNITS;
    let delta_u_flight = (flight_ds as u32) * SUB_DS_UNITS;
    let lambda_foul = match ctx.intentional_foul_policy {
        IntentionalFoulPolicy::Disabled => 0.0,
        IntentionalFoulPolicy::Auto | IntentionalFoulPolicy::ForceEnabled => ctx.lambda_foul_per_ds,
    };

    // Branch II: shot clock expires at or before release and strictly before game clock
    if let Some(sc_u) = ctx.sc_rem_10us {
        if sc_u <= delta_u_rel && sc_u < ctx.gc_rem_10us {
            let t_vio = ceil_10us_to_ds(ctx.gc_rem_10us - sc_u);
            return evaluate_v_wp(
                ctx.delta_s,
                t_vio,
                WpOwner::OppTeam,
                t_vio <= 240.0,
                true,
                ctx.ruleset,
                ctx.tunables,
            );
        }
    }

    // Branch I: game clock expires at or before release
    if ctx.gc_rem_10us <= delta_u_rel {
        if ctx.delta_s < 0.0 {
            return 0.0;
        }
        let delta_u_eff = ctx.gc_rem_10us;
        let delta_t_eff_ds = (delta_u_eff as f64) / (SUB_DS_UNITS as f64);
        let t_ev = ceil_10us_to_ds(ctx.gc_rem_10us - (delta_u_eff / 2));
        let (p_surv, p_steal, p_foul) =
            competing_risks(ctx.lambda_steal_per_ds, lambda_foul, delta_t_eff_ds);
        let wp_surv = evaluate_v_wp(ctx.delta_s, 0.0, WpOwner::None, true, false, ctx.ruleset, ctx.tunables);
        let wp_foul = expected_wp_after_foul(ctx, t_ev);
        let wp_steal = evaluate_v_wp(ctx.delta_s, t_ev, WpOwner::OppTeam, t_ev <= 240.0, false, ctx.ruleset, ctx.tunables);
        return (p_surv * wp_surv + p_foul * wp_foul + p_steal * wp_steal).clamp(0.0, 1.0);
    }

    // Branch III: legal release strictly before both horns
    let t_rel = ceil_10us_to_ds(ctx.gc_rem_10us - delta_u_rel).max(1.0);
    let t_rim = ceil_10us_to_ds(ctx.gc_rem_10us.saturating_sub(delta_u_flight));

    let v_pts = shot_val as f64;
    let wp_blk = evaluate_v_wp(ctx.delta_s, t_rel, WpOwner::OppTeam, t_rel <= 240.0, false, ctx.ruleset, ctx.tunables);
    let wp_and1 = expected_wp_n_ft(ctx, 1, ctx.delta_s + v_pts, t_rel);
    let wp_foul_miss = expected_wp_n_ft(ctx, shot_val, ctx.delta_s, t_rel);
    let wp_make = evaluate_v_wp(ctx.delta_s + v_pts, t_rim, WpOwner::OppTeam, t_rim <= 240.0, true, ctx.ruleset, ctx.tunables);
    let wp_miss = ctx.p_orb * evaluate_v_wp(ctx.delta_s, t_rim, WpOwner::SelfTeam, t_rim <= 140.0, false, ctx.ruleset, ctx.tunables)
        + (1.0 - ctx.p_orb) * evaluate_v_wp(ctx.delta_s, t_rim, WpOwner::OppTeam, t_rim <= 240.0, false, ctx.ruleset, ctx.tunables);

    (probs.p_blk_tov * wp_blk
        + probs.p_and1 * wp_and1
        + probs.p_foul_miss * wp_foul_miss
        + probs.p_clean_make * wp_make
        + probs.p_clean_miss * wp_miss)
        .clamp(0.0, 1.0)
}

/// Evaluate `WP(Hold)` using exact `10 µs` window and competing risks (§6.1.3).
pub fn evaluate_wp_hold(ctx: &EndgameWpContext<'_>) -> Option<f64> {
    let hold_min_sc_10us = (ctx.tunables.hold_min_shot_clock_ds as u32) * SUB_DS_UNITS;
    let hold_horizon_10us = (ctx.tunables.hold_eval_horizon_ds as u32) * SUB_DS_UNITS;

    let delta_u_hold = match ctx.sc_rem_10us {
        None => hold_horizon_10us.min(ctx.gc_rem_10us),
        Some(sc_u) => {
            if sc_u <= hold_min_sc_10us {
                return None;
            }
            hold_horizon_10us
                .min(ctx.gc_rem_10us)
                .min(sc_u - hold_min_sc_10us)
        }
    };
    let delta_t_hold_ds = (delta_u_hold as f64) / (SUB_DS_UNITS as f64);
    let t_after_hold = ceil_10us_to_ds(ctx.gc_rem_10us - delta_u_hold);
    let t_ev = ceil_10us_to_ds(ctx.gc_rem_10us - (delta_u_hold / 2));

    let lambda_foul = match ctx.intentional_foul_policy {
        IntentionalFoulPolicy::Disabled => 0.0,
        IntentionalFoulPolicy::Auto | IntentionalFoulPolicy::ForceEnabled => ctx.lambda_foul_per_ds,
    };

    let (p_surv, p_steal, p_foul) =
        competing_risks(ctx.lambda_steal_per_ds, lambda_foul, delta_t_hold_ds);

    let sc_off_after = ctx.sc_rem_10us.is_none() || t_after_hold <= 240.0;
    let wp_surv = evaluate_v_wp(
        ctx.delta_s,
        t_after_hold,
        WpOwner::SelfTeam,
        sc_off_after,
        false,
        ctx.ruleset,
        ctx.tunables,
    );
    let wp_foul = expected_wp_after_foul(ctx, t_ev);
    let wp_steal = evaluate_v_wp(
        ctx.delta_s,
        t_ev,
        WpOwner::OppTeam,
        t_ev <= 240.0,
        false,
        ctx.ruleset,
        ctx.tunables,
    );
    Some((p_surv * wp_surv + p_foul * wp_foul + p_steal * wp_steal).clamp(0.0, 1.0))
}

/// Bounded 2-Ply EPV evaluator (`d ∈ {0, 1, 2}`, never double-decrements `d` inside `EPV_drive`, §6.1.2).
pub struct EpvEvaluator<'a> {
    pub geom: &'a CourtGeometry,
    pub tunables: &'a EngineTunables,
    pub badges: &'a [BadgeDef],
    pub shot_clock_ds: Option<u16>,
    pub attack_dir: f64,
}

impl<'a> EpvEvaluator<'a> {
    pub fn epv_ft(&self, p_ft: f64, p_orb_ft: f64, n_ft: u8) -> f64 {
        (n_ft as f64) * p_ft + (1.0 - p_ft) * p_orb_ft * self.tunables.epv_putback
    }

    pub fn epv_shoot_at(
        &self,
        shooter: &CompiledPlayerSnapshot,
        x_m: f64,
        y_m: f64,
        c_geom: f64,
        def_interior: f64,
        def_block: f64,
        fatigue_pen: f64,
        rhythm: f64,
        p_orb: f64,
        p_orb_ft: f64,
    ) -> f64 {
        let zone = self.geom.shot_zone(x_m, y_m, self.attack_dir);
        let is_three = self.geom.is_three_pointer(x_m, y_m, self.attack_dir);
        let dist = self.geom.dist_to_rim(x_m, y_m, self.attack_dir);
        let probs = compute_shot_probabilities(
            shooter,
            self.tunables,
            self.badges,
            ShotEvalInputs {
                zone,
                is_three,
                dist_to_rim_m: dist,
                is_catch_and_shoot: true,
                c_geom,
                backboard_angle_factor: 0.2,
                fatigue_penalty: fatigue_pen,
                rhythm_score: rhythm,
                release_foul: false,
            },
            def_interior,
            def_block,
        );
        let v_pts = if is_three { 3.0 } else { 2.0 };
        let p_ft = (0.50 + 0.42 * (shooter.attributes[AttrIdx::FreeThrow] as f64 / 99.0)).clamp(0.45, 0.95);
        let e_ft_1 = self.epv_ft(p_ft, p_orb_ft, 1);
        let e_ft_v = self.epv_ft(p_ft, p_orb_ft, if is_three { 3 } else { 2 });

        let base_epv = probs.p_clean_make * v_pts
            + probs.p_and1 * (v_pts + e_ft_1)
            + probs.p_foul_miss * e_ft_v
            + probs.p_clean_miss * p_orb * self.tunables.epv_putback
            - probs.p_blk_tov * self.tunables.epv_opp_transition;

        // Suppress unrealistic backcourt heaves unless end-of-clock desperation (<= 2.5s)
        if (x_m * self.attack_dir) < -0.5 && self.shot_clock_ds.map_or(true, |sc| sc > 25) {
            base_epv * 0.12
        } else {
            base_epv
        }
    }

    pub fn epv_drive_with_cached_best_pass(
        &self,
        handler_idx: usize,
        corridor: u8,
        d: u8,
        off_team: &[(u8, &CompiledPlayerSnapshot, f64, f64)],
        def_c_geom_fn: &dyn Fn(f64, f64) -> f64,
        cached_best_pass: Option<f64>,
    ) -> f64 {
        let &(_, h_snap, hx, hy) = &off_team[handler_idx];
        let (rim_x, rim_y) = self.geom.rim_xy(self.attack_dir);
        let (end_x, end_y) = match corridor {
            0 => (rim_x - self.attack_dir * 1.4, rim_y + 1.1),
            1 => (rim_x - self.attack_dir * 1.4, rim_y - 1.1),
            2 => (rim_x - self.attack_dir * 4.2, hy * 0.5),
            _ => (
                rim_x - self.attack_dir * (self.geom.three_arc_radius_m + 0.35),
                hy.signum() * 3.8,
            ),
        };
        let dist_to_end = libm::hypot(end_x - hx, end_y - hy);
        let excess_dist = (dist_to_end - 3.2).max(0.0);
        let mut dist_factor = 1.0 / (1.0 + 0.085 * excess_dist);
        if (hx * self.attack_dir) <= 0.0 {
            dist_factor *= 0.78;
        }
        let c_start = def_c_geom_fn(hx, hy);

        let ball_handle = h_snap.attributes[AttrIdx::BallHandle] as f64 / 99.0;
        let p_steal_drive = ((0.04 + 0.06 * (1.0 - ball_handle)) * (1.0 + 0.35 * c_start)).clamp(0.01, 0.18);
        let p_charge_cond = (0.015 + 0.025 * (1.0 - ball_handle)).clamp(0.005, 0.08);
        let p_charge = (1.0 - p_steal_drive) * p_charge_cond;
        let p_survive = (1.0 - p_steal_drive) * (1.0 - p_charge_cond);

        let c_end = def_c_geom_fn(end_x, end_y);
        let shoot_val = self.epv_shoot_at(h_snap, end_x, end_y, c_end, 72.0, 68.0, 0.0, 0.0, 0.25, 0.14)
            * dist_factor;

        if d == 0 {
            return p_survive * shoot_val
                - p_steal_drive * self.tunables.epv_opp_transition
                - p_charge * self.tunables.epv_opp_halfcourt;
        }

        let w_fin = if corridor <= 1 { 0.68 } else { 0.80 };
        let mut best_pass = shoot_val;
        if let Some(cp) = cached_best_pass {
            if cp > best_pass {
                best_pass = cp;
            }
        } else {
            for j in 0..off_team.len() {
                if j == handler_idx {
                    continue;
                }
                let ep = self.epv_pass(handler_idx, j, d, off_team, def_c_geom_fn);
                if ep > best_pass {
                    best_pass = ep;
                }
            }
        }
        p_survive * (w_fin * shoot_val + (1.0 - w_fin) * best_pass)
            - p_steal_drive * self.tunables.epv_opp_transition
            - p_charge * self.tunables.epv_opp_halfcourt
    }

    pub fn epv_drive(
        &self,
        handler_idx: usize,
        corridor: u8,
        d: u8,
        off_team: &[(u8, &CompiledPlayerSnapshot, f64, f64)], // (slot, snap, x, y)
        def_c_geom_fn: &dyn Fn(f64, f64) -> f64,
    ) -> f64 {
        self.epv_drive_with_cached_best_pass(handler_idx, corridor, d, off_team, def_c_geom_fn, None)
    }

    pub fn epv_pass(
        &self,
        passer_idx: usize,
        target_idx: usize,
        d: u8,
        off_team: &[(u8, &CompiledPlayerSnapshot, f64, f64)],
        def_c_geom_fn: &dyn Fn(f64, f64) -> f64,
    ) -> f64 {
        assert!(d >= 1, "EPV_pass must never be called with d == 0");
        let &(_, p_snap, px, py) = &off_team[passer_idx];
        let &(_, t_snap, tx, ty) = &off_team[target_idx];
        let pass_dist = libm::hypot(tx - px, ty - py).max(0.5);
        let mid_c = def_c_geom_fn((px + tx) * 0.5, (py + ty) * 0.5);
        let def_dist_est = (2.6 * (1.0 - mid_c)).max(0.35);
        let reach_eff = 1.65 + (pass_dist / 12.0) * 0.35;
        let (p_int, p_bob) = compute_pass_intercept_prob(p_snap, t_snap, def_dist_est, reach_eff, self.tunables);
        let d_rem = d - 1;
        let c_t = def_c_geom_fn(tx, ty);
        let shoot_t = self.epv_shoot_at(t_snap, tx, ty, c_t, 70.0, 65.0, 0.0, 0.0, 0.24, 0.14);
        let mut best_drive_t = f64::NEG_INFINITY;
        for corr in 0..4 {
            let dv = self.epv_drive(target_idx, corr, d_rem, off_team, def_c_geom_fn);
            if dv > best_drive_t {
                best_drive_t = dv;
            }
        }
        let mut catch_val = shoot_t.max(best_drive_t);
        if d == 2 {
            let pass_ds = (pass_dist / 1.4).round() as u16;
            let rem_ok = self.shot_clock_ds.map_or(true, |sc| {
                sc.saturating_sub(pass_ds) >= self.tunables.epv_extra_pass_min_rem_shot_ds
            });
            if rem_ok {
                for k in 0..off_team.len() {
                    if k == passer_idx || k == target_idx {
                        continue;
                    }
                    let &(_, k_snap, kx, ky) = &off_team[k];
                    let c_k = (def_c_geom_fn(kx, ky) * 0.8).clamp(0.0, 1.0);
                    let shoot_k = self.epv_shoot_at(k_snap, kx, ky, c_k, 68.0, 62.0, 0.0, 0.0, 0.24, 0.14);
                    let drive_k = self.epv_drive(k, 0, 0, off_team, def_c_geom_fn);
                    let extra = self.tunables.epv_gamma_extra
                        * ((1.0 - p_int * 0.5) * (1.0 - p_bob * 0.5) * shoot_k.max(drive_k)
                            - p_int * 0.5 * self.tunables.epv_opp_transition);
                    if extra > catch_val {
                        catch_val = extra;
                    }
                }
            }
        }
        let pass_friction = 0.05 + 0.008 * pass_dist;
        ((1.0 - p_int) * (1.0 - p_bob) * catch_val - p_int * self.tunables.epv_opp_transition - pass_friction)
            .max(0.0)
    }

    pub fn epv_hold(&self, p_steal_per_ds: f64) -> f64 {
        let horizon_ds = self.tunables.epv_hold_horizon_ds as f64;
        let p_steal = 1.0 - libm::exp(-p_steal_per_ds * horizon_ds);
        let [c0, c1, c2] = self.tunables.epv_hold_clock_params;
        let gamma_clk = match self.shot_clock_ds {
            None => 1.0,
            Some(sc) => (((sc as f64) - c0) / c1.max(1.0)).clamp(c2, 1.0),
        };
        (1.0 - p_steal) * gamma_clk * self.tunables.epv_halfcourt_base
            - p_steal * self.tunables.epv_opp_transition
    }
}

#[inline]
fn dist_point_to_segment(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let vx = bx - ax;
    let vy = by - ay;
    let len_sq = vx * vx + vy * vy;
    if len_sq < 1.0e-8 {
        return libm::hypot(px - ax, py - ay);
    }
    let t = (((px - ax) * vx + (py - ay) * vy) / len_sq).clamp(0.0, 1.0);
    libm::hypot(px - (ax + t * vx), py - (ay + t * vy))
}

/// Evaluate a static `PredicateExpr` against a single player snapshot (used for `[role_defs]`).
pub fn eval_player_role_expr(expr: &PredicateExpr, player: &CompiledPlayerSnapshot) -> bool {
    match expr {
        PredicateExpr::All(list) => list.iter().all(|c| eval_player_role_expr(c, player)),
        PredicateExpr::Any(list) => list.iter().any(|c| eval_player_role_expr(c, player)),
        PredicateExpr::Not(inner) => !eval_player_role_expr(inner, player),
        PredicateExpr::Cmp {
            metric_key,
            op,
            val,
            ..
        } => {
            let lhs = eval_slot_metric(player, *metric_key);
            match op {
                0 => lhs < *val,
                1 => lhs <= *val,
                2 => lhs > *val,
                _ => lhs >= *val,
            }
        }
    }
}

/// Return true if defender matches any protected role in `gp.protect_roles` (with physical fallback).
pub fn is_protected_defender(
    gp: &CompiledDefenseGameplan,
    def_snap: &CompiledPlayerSnapshot,
) -> bool {
    for role_name in &gp.protect_roles {
        if let Some((_, expr)) = gp.role_defs.iter().find(|(n, _)| n == role_name) {
            if eval_player_role_expr(expr, def_snap) {
                return true;
            }
        }
    }
    def_snap.attributes[AttrIdx::LateralAgility] <= 66
        || (def_snap.physical.height_cm <= 190.0 && def_snap.attributes[AttrIdx::Strength] <= 66)
}

/// Initial defensive matchup assignment supporting `hide_on_weakest_shooter` (§5.3).
/// Returns `[off_pos; 5]` indexed by `def_pos`.
pub fn assign_initial_defensive_matchups(
    gp_opt: Option<&CompiledDefenseGameplan>,
    off_snaps: &[Option<&CompiledPlayerSnapshot>; 5],
    def_snaps: &[Option<&CompiledPlayerSnapshot>; 5],
) -> [u8; 5] {
    let default_m = [0u8, 1, 2, 3, 4];
    let Some(gp) = gp_opt else {
        return default_m;
    };
    if !gp.hide_on_weakest_shooter || gp.base_scheme == 3 {
        return default_m;
    }
    let active_def_cnt = def_snaps.iter().filter(|s| s.is_some()).count();
    let active_off_cnt = off_snaps.iter().filter(|s| s.is_some()).count();
    if active_def_cnt < 5 || active_off_cnt < 5 {
        return default_m;
    }

    let mut off_threat = [0.0_f64; 5];
    let mut off_creator = [0.0_f64; 5];
    for i in 0..5 {
        if let Some(o) = off_snaps[i] {
            off_threat[i] = 0.45 * f64::from(o.attributes[AttrIdx::ThreePoint])
                + 0.35 * f64::from(o.attributes[AttrIdx::BallHandle])
                + 0.20 * f64::from(o.attributes[AttrIdx::CatchAndShoot]);
            off_creator[i] = 0.55 * f64::from(o.attributes[AttrIdx::BallHandle])
                + 0.45 * f64::from(o.attributes[AttrIdx::PassVision]);
        }
    }

    let mut is_prot = [false; 5];
    let mut perim_stop = [0.0_f64; 5];
    for i in 0..5 {
        if let Some(d) = def_snaps[i] {
            is_prot[i] = is_protected_defender(gp, d);
            perim_stop[i] = 0.55 * f64::from(d.attributes[AttrIdx::PerimeterDefense])
                + 0.45 * f64::from(d.attributes[AttrIdx::LateralAgility]);
        }
    }

    let mut perm = [0usize, 1, 2, 3, 4];
    let mut best_perm = perm;
    let mut best_score = f64::NEG_INFINITY;
    permute_matchup_score(
        0,
        &mut perm,
        off_snaps,
        def_snaps,
        &off_threat,
        &off_creator,
        &is_prot,
        &perim_stop,
        &mut best_score,
        &mut best_perm,
    );

    [
        best_perm[0] as u8,
        best_perm[1] as u8,
        best_perm[2] as u8,
        best_perm[3] as u8,
        best_perm[4] as u8,
    ]
}

#[allow(clippy::too_many_arguments)]
fn permute_matchup_score(
    pos: usize,
    perm: &mut [usize; 5],
    off_snaps: &[Option<&CompiledPlayerSnapshot>; 5],
    def_snaps: &[Option<&CompiledPlayerSnapshot>; 5],
    off_threat: &[f64; 5],
    off_creator: &[f64; 5],
    is_prot: &[bool; 5],
    perim_stop: &[f64; 5],
    best_score: &mut f64,
    best_perm: &mut [usize; 5],
) {
    if pos == 5 {
        let mut sc = 0.0;
        for d_pos in 0..5 {
            let o_pos = perm[d_pos];
            let (Some(ds), Some(os)) = (def_snaps[d_pos], off_snaps[o_pos]) else {
                continue;
            };
            let h_diff = (ds.physical.height_cm - os.physical.height_cm).abs();
            // Natural positional proximity baseline
            sc -= h_diff * 0.25;
            if is_prot[d_pos] {
                // Strongly reward hiding protected defender onto lowest off_threat player
                sc -= off_threat[o_pos] * 0.85;
                if os.attributes[AttrIdx::PostControl] >= 78 && h_diff > 15.0 {
                    sc -= 18.0;
                }
            } else {
                // Match strong perimeter defenders onto high-creation / high-threat offensive guards
                sc += (perim_stop[d_pos] / 99.0) * (off_creator[o_pos] * 0.22);
            }
        }
        if sc > *best_score {
            *best_score = sc;
            *best_perm = *perm;
        }
        return;
    }
    for i in pos..5 {
        perm.swap(pos, i);
        permute_matchup_score(
            pos + 1,
            perm,
            off_snaps,
            def_snaps,
            off_threat,
            off_creator,
            is_prot,
            perim_stop,
            best_score,
            best_perm,
        );
        perm.swap(pos, i);
    }
}

/// Deterministic 13-point continuous off-ball spacing potential field (§6.2).
#[allow(clippy::too_many_arguments)]
pub fn evaluate_spacing_potential_field(
    my_pos_idx: usize,
    actor_snap: &CompiledPlayerSnapshot,
    base_anchor: (f64, f64),
    off_xy: &[(f64, f64); 5],
    off_active: &[bool; 5],
    def_xy: &[(f64, f64); 5],
    def_active: &[bool; 5],
    handler_xy: (f64, f64),
    handler_is_driving: bool,
    attack_dir: f64,
    geom: &CourtGeometry,
    tunables: &EngineTunables,
) -> (f64, f64) {
    let [w_rep, d_safe, w_epv, w_def, w_lane] = tunables.spacing_field_params;
    let bounds = geom.inbounds_nav_bounds(0.85);
    let rim_xy = geom.rim_xy(attack_dir);
    let t3 = (0.5 * f64::from(actor_snap.attributes[AttrIdx::ThreePoint])
        + 0.5 * f64::from(actor_snap.attributes[AttrIdx::CatchAndShoot])) / 99.0;
    let mid_r = f64::from(actor_snap.attributes[AttrIdx::MidRange]) / 99.0;

    const COS6: [f64; 6] = [1.0, 0.5, -0.5, -1.0, -0.5, 0.5];
    const SIN6: [f64; 6] = [0.0, 0.8660254, 0.8660254, 0.0, -0.8660254, -0.8660254];

    let mut best_xy = bounds.clamp(base_anchor.0, base_anchor.1);
    best_xy.0 = if attack_dir > 0.0 {
        best_xy.0.max(1.35)
    } else {
        best_xy.0.min(-1.35)
    };
    let mut best_val = f64::NEG_INFINITY;

    for idx in 0..13 {
        let (dx, dy) = if idx == 0 {
            (0.0, 0.0)
        } else if idx <= 6 {
            let k = idx - 1;
            (2.1 * COS6[k], 2.1 * SIN6[k])
        } else {
            let k = idx - 7;
            (3.6 * COS6[k], 3.6 * SIN6[k])
        };

        let (mut cx, cy) = bounds.clamp(base_anchor.0 + dx, base_anchor.1 + dy);
        if attack_dir > 0.0 {
            cx = cx.max(1.35);
        } else {
            cx = cx.min(-1.35);
        }

        let mut rep_sum = 0.0;
        let mut crowd_pen = 0.0;
        for k in 0..5 {
            if k == my_pos_idx || !off_active[k] {
                continue;
            }
            let dm = libm::hypot(cx - off_xy[k].0, cy - off_xy[k].1);
            rep_sum += dm.min(d_safe);
            if dm < 2.85 {
                crowd_pen += 3.8 * (2.85 - dm);
            }
            if dm < 1.85 {
                crowd_pen += 5.5 * (1.85 - dm);
            }
        }

        let d_rim = geom.dist_to_rim(cx, cy, attack_dir);
        let is_three = geom.is_three_pointer(cx, cy, attack_dir);
        let epv_catch = if is_three {
            0.95 + 0.45 * t3
        } else if d_rim < 2.4 {
            0.90 - if t3 > 0.72 { 0.18 } else { 0.0 }
        } else {
            0.55 + 0.14 * mid_r - if t3 > 0.75 { 0.16 } else { 0.0 }
        };

        let mut def_dens = 0.0;
        let mut min_lane_d = 2.4;
        for d in 0..5 {
            if !def_active[d] {
                continue;
            }
            let dd = libm::hypot(cx - def_xy[d].0, cy - def_xy[d].1);
            def_dens += libm::exp(-(dd * dd) / 2.8);
            let dl = dist_point_to_segment(
                def_xy[d].0,
                def_xy[d].1,
                handler_xy.0,
                handler_xy.1,
                cx,
                cy,
            );
            if dl < min_lane_d {
                min_lane_d = dl;
            }
        }
        let lane_clarity = (min_lane_d / 2.4).clamp(0.0, 1.0);

        let mut drive_corr_pen = 0.0;
        if handler_is_driving {
            let d_corr = dist_point_to_segment(cx, cy, handler_xy.0, handler_xy.1, rim_xy.0, rim_xy.1);
            if d_corr < 1.65 {
                drive_corr_pen = 1.85 * (1.65 - d_corr);
            }
        }

        // Mild anchor tether prevents wandering too far from offensive shape
        let anchor_dist = libm::hypot(cx - base_anchor.0, cy - base_anchor.1);
        let val = w_rep * rep_sum + w_epv * epv_catch - w_def * def_dens + w_lane * lane_clarity
            - crowd_pen
            - drive_corr_pen
            - 0.09 * anchor_dist
            + if idx == 0 { 0.04 } else { 0.0 };

        if val > best_val {
            best_val = val;
            best_xy = (cx, cy);
        }
    }
    best_xy
}

/// Drive-and-Reaction off-ball 5-player floor balancing (§6.2).
#[allow(clippy::too_many_arguments)]
pub fn compute_drive_and_react_relocation(
    px: f64,
    py: f64,
    handler_xy: (f64, f64),
    drive_target_xy: (f64, f64),
    attack_dir: f64,
    three_pt_attr: u8,
    my_def_dist_to_rim: f64,
    geom: &CourtGeometry,
) -> (f64, f64, f64) {
    let rim_xy = geom.rim_xy(attack_dir);
    let bounds = geom.inbounds_nav_bounds(0.85);
    let drive_y = if libm::fabs(drive_target_xy.1) > 0.45 {
        drive_target_xy.1
    } else {
        handler_xy.1
    };
    let drive_side = if drive_y >= 0.0 { 1.0 } else { -1.0 };
    let d_rim = geom.dist_to_rim(px, py, attack_dir);
    let depth_from_rim = (rim_xy.0 - px) * attack_dir;

    // 1. Paint / non-shooter clears out to Opposite Dunker Spot
    if d_rim < 3.8 || three_pt_attr < 74 {
        let (cx, cy) = bounds.clamp(rim_xy.0 - attack_dir * 1.45, -drive_side * 2.05);
        return (cx, cy, 0.74);
    }

    // 2. Strong-side corner Lift / wing Drift or Slot Lift
    if py * drive_side > 0.5 {
        if depth_from_rim < 3.2 {
            // Deep corner lifts to wing
            let (cx, cy) = bounds.clamp(
                rim_xy.0 - attack_dir * 5.2,
                drive_side * (geom.corner_three_dist_m - 0.55),
            );
            return (cx, cy, 0.72);
        } else if libm::fabs(drive_target_xy.1) < libm::fabs(handler_xy.1) - 0.3 {
            // Middle penetration -> strong wing drifts to corner
            let (cx, cy) = bounds.clamp(
                rim_xy.0 - attack_dir * 1.15,
                drive_side * (geom.corner_three_dist_m + 0.10),
            );
            return (cx, cy, 0.72);
        } else {
            // Baseline drive -> lift to slot safety outlet
            let (cx, cy) = bounds.clamp(rim_xy.0 - attack_dir * 7.5, drive_side * 2.6);
            return (cx, cy, 0.72);
        }
    }

    // 3. Weak-side 45 Cut (when defender helps too low), keep weak corner spaced, or shake/lift weak slot
    if my_def_dist_to_rim < 3.4 && three_pt_attr < 80 && depth_from_rim >= 3.6 {
        let (cx, cy) = bounds.clamp(rim_xy.0 - attack_dir * 2.4, -drive_side * 1.9);
        (cx, cy, 0.84)
    } else if depth_from_rim < 4.2 {
        let (cx, cy) = bounds.clamp(
            rim_xy.0 - attack_dir * 1.35,
            -drive_side * (geom.corner_three_dist_m - 0.10),
        );
        (cx, cy, 0.64)
    } else {
        let (cx, cy) = bounds.clamp(rim_xy.0 - attack_dir * 7.4, -drive_side * 2.9);
        (cx, cy, 0.68)
    }
}

/// Off-ball screen Read & React (`Backdoor = 2`, `ScreenCurl = 4`, `ScreenFlare = 5`) (§6.2).
pub fn compute_offball_screen_read(
    cutter_xy: (f64, f64),
    screener_xy: (f64, f64),
    def_xy: (f64, f64),
    ball_xy: (f64, f64),
    attack_dir: f64,
    cutter_three_pt: u8,
    geom: &CourtGeometry,
) -> (u8, (f64, f64), f64) {
    let rim_xy = geom.rim_xy(attack_dir);
    let bounds = geom.inbounds_nav_bounds(0.85);

    let d_cut_scr = libm::hypot(screener_xy.0 - cutter_xy.0, screener_xy.1 - cutter_xy.1);
    let d_def_scr = libm::hypot(screener_xy.0 - def_xy.0, screener_xy.1 - def_xy.1);
    let d_cut_ball = libm::hypot(ball_xy.0 - cutter_xy.0, ball_xy.1 - cutter_xy.1);
    let d_def_ball = libm::hypot(ball_xy.0 - def_xy.0, ball_xy.1 - def_xy.1);
    let d_def_rim = libm::hypot(rim_xy.0 - def_xy.0, rim_xy.1 - def_xy.1);
    let d_scr_rim = libm::hypot(rim_xy.0 - screener_xy.0, rim_xy.1 - screener_xy.1);

    // Top-lock -> Backdoor cut
    if d_def_scr + 0.15 < d_cut_scr && d_def_ball < d_cut_ball {
        let side = if cutter_xy.1 >= 0.0 { 1.0 } else { -1.0 };
        return (
            2,
            bounds.clamp(rim_xy.0 - attack_dir * 1.65, side * 1.35),
            0.88,
        );
    }

    // Defender goes Under / sags inside screen -> Flare out for open 3PT
    if d_def_rim + 0.65 < d_scr_rim && cutter_three_pt >= 76 {
        let flare_y = if screener_xy.1 >= cutter_xy.1 {
            cutter_xy.1 - 2.6
        } else {
            cutter_xy.1 + 2.6
        };
        return (
            5,
            bounds.clamp(rim_xy.0 - attack_dir * 6.8, flare_y),
            0.78,
        );
    }

    // Defender trails behind -> tight shoulder Curl around screener toward elbow/rim
    let s_side = if screener_xy.1 >= 0.0 { 1.0 } else { -1.0 };
    let target = if d_cut_scr > 1.35 {
        bounds.clamp(screener_xy.0 - attack_dir * 0.45, screener_xy.1 + s_side * 0.90)
    } else {
        bounds.clamp(rim_xy.0 - attack_dir * 3.8, screener_xy.1 * 0.45)
    };
    (4, target, 0.84)
}
