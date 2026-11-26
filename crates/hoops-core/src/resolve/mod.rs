use crate::rng::{sample_u53_checked, RngBudgetExceeded, RngPurpose};
use crate::wire_schema_v1::*;

/// Deterministic badge modifier evaluation: sorted by `badge_id` ascending,
/// accumulating all additions first, then multiplying all multipliers `(base + ΣΔ) * Πf` (§4.1).
pub fn apply_badges_for_hook(
    base: f64,
    hook: u8,
    badge_indices: &[u16],
    all_badges: &[BadgeDef],
) -> f64 {
    if badge_indices.is_empty() || all_badges.is_empty() {
        return base;
    }
    let mut matched: [Option<&BadgeDef>; 16] = [None; 16];
    let mut n = 0usize;
    for &idx in badge_indices {
        if let Some(b) = all_badges.get(idx as usize) {
            if b.hook == hook && n < 16 {
                matched[n] = Some(b);
                n += 1;
            }
        }
    }
    if n == 0 {
        return base;
    }
    if n > 1 {
        matched[..n].sort_by(|a, b| a.unwrap().badge_id.cmp(&b.unwrap().badge_id));
    }

    let mut sum_add = 0.0_f64;
    for b in matched[..n].iter().flatten() {
        if b.op == 0 {
            sum_add += b.value;
        }
    }
    let mut val = base + sum_add;
    for b in matched[..n].iter().flatten() {
        if b.op == 1 {
            val *= b.value;
        }
    }
    val
}

#[inline]
pub fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + libm::exp(-x))
}

#[derive(Debug, Clone, Copy)]
pub struct ShotEvalInputs {
    pub zone: u8,
    pub is_three: bool,
    pub dist_to_rim_m: f64,
    pub is_catch_and_shoot: bool,
    pub c_geom: f64,
    pub backboard_angle_factor: f64,
    pub fatigue_penalty: f64,
    pub rhythm_score: f64,
    pub release_foul: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ShotOutcomeProbs {
    pub p_blk_tov: f64,
    pub p_and1: f64,
    pub p_foul_miss: f64,
    pub p_clean_make: f64,
    pub p_clean_miss: f64,
    pub p_make_clean: f64,
    pub p_swish_given_nonbank_make: f64,
    pub p_airball_given_miss: f64,
}

pub fn compute_shot_probabilities(
    shooter: &CompiledPlayerSnapshot,
    tunables: &EngineTunables,
    all_badges: &[BadgeDef],
    inp: ShotEvalInputs,
    def_interior_attr: f64,
    def_block_attr: f64,
) -> ShotOutcomeProbs {
    let attrs = &shooter.attributes;
    let a_zone = match inp.zone {
        0 => f64::from(attrs[AttrIdx::Layup].max(attrs[AttrIdx::Dunk]).max(attrs[AttrIdx::CloseTouch])),
        1 | 2 => f64::from(attrs[AttrIdx::MidRange]),
        _ => f64::from(attrs[AttrIdx::ThreePoint]),
    };
    let a_style = f64::from(attrs[if inp.is_catch_and_shoot {
        AttrIdx::CatchAndShoot
    } else {
        AttrIdx::OffDribbleShot
    }]);
    let a_contested = f64::from(attrs[AttrIdx::ContestedShot]);
    let r_range = 6.5 + (f64::from(attrs[AttrIdx::ShotRange]) / 99.0) * 3.5;

    let bias_idx = match inp.zone {
        0 => 0,
        1 | 2 => 1,
        _ => 2,
    };
    let mut eta_clean = tunables.shot_zone_bias_3[bias_idx]
        + tunables.shot_alpha_zone * (a_zone - 75.0) / 25.0
        + tunables.shot_alpha_style * (a_style - 75.0) / 25.0
        - tunables.shot_alpha_range * (inp.dist_to_rim_m - r_range).max(0.0)
        - tunables.shot_alpha_contest
            * inp.c_geom
            * (1.0 - tunables.shot_contest_attr_relief * (a_contested / 99.0))
        - tunables.shot_alpha_fatigue * inp.fatigue_penalty
        + tunables.shot_alpha_rhythm * inp.rhythm_score.clamp(-tunables.rhythm_logit_cap, tunables.rhythm_logit_cap);

    // Hook 0: OnShotEval
    eta_clean = apply_badges_for_hook(eta_clean, 0, &shooter.badge_indices, all_badges);

    let p_make_clean = sigmoid(eta_clean).clamp(0.03, 0.94);

    // Immediate block/strip turnover probability
    let p_blk_tov = (0.008 + 0.075 * inp.c_geom * (def_block_attr / 99.0)).clamp(0.0, 0.22);

    // Foul given not blocked
    let draw_foul = f64::from(attrs[AttrIdx::DrawFoul]) / 99.0;
    let p_foul_given_noblk = if inp.release_foul {
        1.0
    } else {
        (tunables.shooting_foul_contact_base * (0.4 + 0.8 * inp.c_geom) * (0.65 + 0.7 * draw_foul)
            * (1.0 - 0.15 * (def_interior_attr / 99.0)))
            .clamp(0.0, 0.45)
    };

    let p_make_foul = sigmoid(eta_clean - 0.85).clamp(0.02, 0.72);
    let rem = 1.0 - p_blk_tov;
    let p_foul = rem * p_foul_given_noblk;
    let p_and1 = p_foul * p_make_foul;
    let p_foul_miss = p_foul * (1.0 - p_make_foul);
    let p_clean = rem * (1.0 - p_foul_given_noblk);
    let p_clean_make = p_clean * p_make_clean;
    let p_clean_miss = p_clean * (1.0 - p_make_clean);

    let p_swish = (tunables.swish_base
        - tunables.swish_contest_slope * inp.c_geom
        + tunables.swish_zone_attr_coeff * (a_zone - 75.0) / 25.0)
        .clamp(tunables.swish_prob_min_max[0], tunables.swish_prob_min_max[1]);

    let p_airball = (tunables.airball_base
        + tunables.airball_contest_coeff * inp.c_geom * inp.c_geom
        + tunables.airball_range_coeff * (inp.dist_to_rim_m - r_range).max(0.0)
        - tunables.airball_attr_relief_coeff * (a_zone / 99.0))
        .clamp(tunables.airball_prob_min_max[0], tunables.airball_prob_min_max[1]);

    ShotOutcomeProbs {
        p_blk_tov,
        p_and1,
        p_foul_miss,
        p_clean_make,
        p_clean_miss,
        p_make_clean,
        p_swish_given_nonbank_make: p_swish,
        p_airball_given_miss: p_airball,
    }
}

pub fn sample_intended_shot_outcome(
    philox_key: [u32; 2],
    possession_id: u32,
    possession_tick: u16,
    counters: &mut [[u16; 9]; 33],
    canonical_actor_id: u8,
    probs: ShotOutcomeProbs,
    tunables: &EngineTunables,
    inp: ShotEvalInputs,
) -> Result<(IntendedOutcomeClass, bool), RngBudgetExceeded> {
    let u1 = sample_u53_checked(
        philox_key,
        possession_id,
        possession_tick,
        counters,
        canonical_actor_id,
        RngPurpose::ShotOutcome,
    )?;
    if !inp.release_foul && u1 < probs.p_blk_tov {
        return Ok((IntendedOutcomeClass::Blocked, false));
    }

    let u2 = sample_u53_checked(
        philox_key,
        possession_id,
        possession_tick,
        counters,
        canonical_actor_id,
        RngPurpose::ShotOutcome,
    )?;

    let made = if inp.release_foul {
        u1 < probs.p_and1 / (probs.p_and1 + probs.p_foul_miss).max(1.0e-9)
    } else {
        u1 < probs.p_blk_tov + probs.p_and1 + probs.p_clean_make
    };

    if made {
        let q_bank = (inp.backboard_angle_factor * tunables.bank_make_max_prob)
            .clamp(0.0, tunables.bank_make_max_prob);
        if u2 < q_bank {
            Ok((IntendedOutcomeClass::BankMake, true))
        } else {
            let u_rem = (u2 - q_bank) / (1.0 - q_bank).max(1.0e-9);
            if u_rem < probs.p_swish_given_nonbank_make {
                Ok((IntendedOutcomeClass::Swish, true))
            } else {
                Ok((IntendedOutcomeClass::RimIn, true))
            }
        }
    } else {
        let p_ab = probs.p_airball_given_miss;
        let q_bb_only = (inp.backboard_angle_factor * tunables.backboard_only_miss_max_prob)
            .clamp(0.0, tunables.backboard_only_miss_max_prob);
        if u2 < p_ab {
            Ok((IntendedOutcomeClass::Airball, false))
        } else if u2 < p_ab + q_bb_only {
            Ok((IntendedOutcomeClass::BackboardOnlyMiss, false))
        } else {
            let rem = (1.0 - p_ab - q_bb_only).max(1.0e-9);
            let u_rem = (u2 - p_ab - q_bb_only) / rem;
            let q_bank_miss = (inp.backboard_angle_factor * tunables.bank_miss_max_prob)
                .clamp(0.0, tunables.bank_miss_max_prob);
            if u_rem < q_bank_miss {
                Ok((IntendedOutcomeClass::BankMiss, false))
            } else {
                Ok((IntendedOutcomeClass::RimOut, false))
            }
        }
    }
}

/// Rebound scramble candidate weighting with zero-attribute safety floor (`w0 > 0`) & underflow fallback (§7.3).
pub fn compute_rebound_weight(
    dist_to_land_m: f64,
    reb_attr: u8,
    vertical_attr: u8,
    hustle_attr: u8,
    boxout_disadvantaged: bool,
    fatigue_penalty: f64,
    tunables: &EngineTunables,
) -> f64 {
    let w = tunables.rebound_attr_weights;
    let spatial = libm::exp(-dist_to_land_m / tunables.rebound_sigma_m.max(0.25));
    let attr_term = w[0].max(1.0e-4)
        + w[1] * (reb_attr as f64 / 99.0)
        + w[2] * (vertical_attr as f64 / 99.0)
        + w[3] * (hustle_attr as f64 / 99.0);
    let box_mult = if boxout_disadvantaged {
        tunables.boxout_disadvantage_factor
    } else {
        1.0
    };
    let fat_mult = (1.0 - tunables.rebound_fatigue_penalty_coeff * fatigue_penalty).max(0.1);
    (spatial * attr_term * box_mult * fat_mult).max(0.0)
}

pub fn select_rebound_winner(
    candidates: &[(u8, u8, f64, f64)], // (slot, canonical_actor_id, weight, dist_3d)
    philox_key: [u32; 2],
    possession_id: u32,
    possession_tick: u16,
    counters: &mut [[u16; 9]; 33],
) -> Result<Option<(u8, u8)>, RngBudgetExceeded> {
    if candidates.is_empty() {
        return Ok(None);
    }
    let total_w: f64 = candidates.iter().map(|c| c.2).sum();
    if total_w < 1.0e-12 || !total_w.is_finite() {
        // Deterministic closest 3D distance, tie-break smallest canonical_actor_id
        let mut best = candidates[0];
        for &c in candidates.iter().skip(1) {
            if c.3 < best.3 || (c.3 == best.3 && c.1 < best.1) {
                best = c;
            }
        }
        return Ok(Some((best.0, best.1)));
    }
    let u = sample_u53_checked(
        philox_key,
        possession_id,
        possession_tick,
        counters,
        0xFF,
        RngPurpose::ReboundBounce,
    )?;
    let target = u * total_w;
    let mut acc = 0.0;
    for &(slot, cid, w, _) in candidates {
        acc += w;
        if target <= acc {
            return Ok(Some((slot, cid)));
        }
    }
    let last = candidates[candidates.len() - 1];
    Ok(Some((last.0, last.1)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassTickOutcome {
    InFlight,
    CleanSteal { stealer_slot: u8 },
    DeflectedLoose { stealer_slot: u8 },
    ArrivedClean,
    ArrivedBobbledLoose,
}

pub fn compute_pass_intercept_prob(
    passer: &CompiledPlayerSnapshot,
    defender: &CompiledPlayerSnapshot,
    dist_to_lane_m: f64,
    reach_m: f64,
    tunables: &EngineTunables,
) -> (f64, f64) {
    if dist_to_lane_m > reach_m {
        return (0.0, 0.0);
    }
    let prox = (1.0 - dist_to_lane_m / reach_m.max(0.2)).clamp(0.0, 1.0);
    let def_steal = f64::from(defender.attributes[AttrIdx::Steal]) / 99.0;
    let def_lat = f64::from(defender.attributes[AttrIdx::LateralAgility]) / 99.0;
    let pass_acc = f64::from(passer.attributes[AttrIdx::PassAccuracy]) / 99.0;
    let base = tunables.steal_hazard_base * prox * (0.6 + 0.8 * def_steal + 0.4 * def_lat - 0.5 * pass_acc);
    let p_clean = (base * 0.45).clamp(0.0, 0.18);
    let p_deflect = (base * 0.55).clamp(0.0, 0.22);
    (p_clean, p_deflect)
}
