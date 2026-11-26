use std::collections::{BTreeMap, BTreeSet};

use hoops_core::*;

use crate::error::ConfigError;
use crate::functional::{derive_functional_profile, natural_tier};
use crate::loader::{
    RawDefenseFile, RawFunctionalProfileOverride, RawMetricKey, RawPlaybookFile, RawPredicateExpr,
    RawTeamFile,
};

/// Resolve an authoritative `SlotMetricKey` / `PredicateExpr.metric_key` string name to its `u16` ID.
pub fn metric_key_from_name(name: &str) -> Option<u16> {
    if let Some(attr) = AttrIdx::from_name(name) {
        return Some(attr.metric_key());
    }
    match name {
        // 0..=6 PhysicalProfile
        "height_cm" => Some(0),
        "wingspan_cm" => Some(1),
        "standing_reach_cm" => Some(2),
        "weight_kg" => Some(3),
        "age" => Some(4),
        "dominant_hand" => Some(5),
        "display_position" => Some(6),

        // 45..=53 PlayerFunctionalProfile scores
        "primary_creator" => Some(45),
        "secondary_handler" => Some(46),
        "spacer" => Some(47),
        "movement_shooter" => Some(48),
        "roll_finisher" => Some(49),
        "post_hub" => Some(50),
        "point_of_attack" => Some(51),
        "rim_protection" => Some(52),
        "switchability" => Some(53),

        // 54..=59 Defensive context metrics
        "matchup_height_diff_cm" => Some(54),
        "shot_clock_ds" => Some(55),
        "game_clock_ds" => Some(56),
        "score_diff" => Some(57),
        "handler_shot_range_m" | "shooter_range_m" => Some(58),
        "defender_fouls" => Some(59),

        // 60..=69 PlayerTendencies
        "zone_rim" | "zone_weight_rim" => Some(60),
        "zone_short_mid" | "zone_weight_short_mid" => Some(61),
        "zone_long_mid" | "zone_weight_long_mid" => Some(62),
        "zone_corner_3" | "zone_corner3" | "zone_weight_corner3" => Some(63),
        "zone_above_break_3" | "zone_above_break3" | "zone_weight_above_break3" => Some(64),
        "drive_vs_pullup" => Some(65),
        "pass_vs_shoot" => Some(66),
        "pnr_roll_vs_pop" => Some(67),
        "crash_orb" => Some(68),
        "gamble_steal" => Some(69),

        _ => None,
    }
}

/// Resolve a `RawMetricKey` (integer ID or named string) into a validated `u16` metric key (`0..=69`).
pub fn resolve_metric_key(raw: &RawMetricKey) -> Result<u16, ConfigError> {
    match raw {
        RawMetricKey::Id(id) => {
            if *id <= 69 {
                Ok(*id)
            } else {
                Err(ConfigError::Validation(format!(
                    "metric_key ID {} out of valid range 0..=69",
                    id
                )))
            }
        }
        RawMetricKey::Name(name) => metric_key_from_name(name).ok_or_else(|| ConfigError::UnknownRef {
            kind: "metric_key",
            id: name.clone(),
        }),
    }
}

fn is_player_slot_metric_key(id: u16) -> bool {
    matches!(id, 0..=53 | 60..=69)
}

fn is_context_metric_key(id: u16) -> bool {
    matches!(id, 54..=59)
}

pub fn validate_ruleset(r: &RuleSet) -> Result<(), ConfigError> {
    validate_id_string(&r.rule_id)
        .map_err(|e| ConfigError::Validation(format!("invalid rule_id: {e}")))?;
    let c = &r.court;
    if !(28.0..=50.0).contains(&c.length_m) || !(15.0..=30.0).contains(&c.width_m) {
        return Err(ConfigError::Validation(
            "court length_m must be in [28.0, 50.0] and width_m in [15.0, 30.0]".to_owned(),
        ));
    }
    if !(2.0..=6.0).contains(&c.oob_runoff_m) {
        return Err(ConfigError::Validation(format!(
            "oob_runoff_m {} outside required [2.0, 6.0]",
            c.oob_runoff_m
        )));
    }
    if r.roster.max_team_players < 5 || r.roster.max_team_players > 15 {
        return Err(ConfigError::Validation(
            "max_team_players must be in 5..=15".to_owned(),
        ));
    }
    Ok(())
}

pub fn validate_tunables(t: &EngineTunables) -> Result<(), ConfigError> {
    if t.tunables_schema_version != 1 {
        return Err(ConfigError::Validation(format!(
            "unsupported tunables_schema_version {}",
            t.tunables_schema_version
        )));
    }
    Ok(())
}

pub fn validate_badges(badges: &[BadgeDef]) -> Result<BTreeMap<String, u16>, ConfigError> {
    let mut map = BTreeMap::new();
    for (i, b) in badges.iter().enumerate() {
        validate_id_string(&b.badge_id)
            .map_err(|e| ConfigError::Validation(format!("invalid badge_id: {e}")))?;
        if !b.value.is_finite() {
            return Err(ConfigError::Validation(format!(
                "non-finite badge value in {}",
                b.badge_id
            )));
        }
        if map.insert(b.badge_id.clone(), i as u16).is_some() {
            return Err(ConfigError::Validation(format!(
                "duplicate badge_id {}",
                b.badge_id
            )));
        }
    }
    Ok(map)
}

pub fn compile_playbook(raw: RawPlaybookFile) -> Result<CompiledPlaybook, ConfigError> {
    let play_id = PlayId::new(&raw.play_id)
        .map_err(|e| ConfigError::Validation(format!("invalid play_id: {e}")))?;
    if raw.role_priority.len() != 5 {
        return Err(ConfigError::Validation(format!(
            "playbook {} role_priority len must be 5",
            raw.play_id
        )));
    }
    let mut seen_prio = [false; 5];
    let mut role_priority = [0u8; 5];
    for (i, &v) in raw.role_priority.iter().enumerate() {
        if (v as usize) >= 5 || seen_prio[v as usize] {
            return Err(ConfigError::Validation(format!(
                "playbook {} role_priority must be a permutation of 0..5",
                raw.play_id
            )));
        }
        seen_prio[v as usize] = true;
        role_priority[i] = v;
    }

    let mut slot_weights: [Vec<(u16, f64)>; 5] = Default::default();
    for sw in raw.slot_weights {
        if (sw.role_slot as usize) >= 5 {
            return Err(ConfigError::Validation(format!(
                "playbook {} invalid role_slot {}",
                raw.play_id, sw.role_slot
            )));
        }
        let mut terms: Vec<(u16, f64)> = Vec::with_capacity(sw.terms.len());
        for (raw_k, w) in sw.terms {
            let k = resolve_metric_key(&raw_k)?;
            if !is_player_slot_metric_key(k) {
                return Err(ConfigError::Validation(format!(
                    "playbook {} slot_weights metric_key {} is not a player slot metric",
                    raw.play_id, k
                )));
            }
            if !w.is_finite() {
                return Err(ConfigError::Validation("non-finite slot weight".to_owned()));
            }
            terms.push((k, w));
        }
        // Canonical deterministic SlotMetricKey ascending sort + merge duplicate keys
        terms.sort_by_key(|&(k, _)| k);
        let mut merged: Vec<(u16, f64)> = Vec::with_capacity(terms.len());
        for (k, w) in terms {
            if let Some(last) = merged.last_mut() {
                if last.0 == k {
                    last.1 += w;
                    continue;
                }
            }
            merged.push((k, w));
        }
        slot_weights[sw.role_slot as usize] = merged;
    }

    if raw.initial_positions.len() != 5 {
        return Err(ConfigError::Validation(format!(
            "playbook {} must define exactly 5 initial_positions",
            raw.play_id
        )));
    }
    let mut initial_positions = [(0u8, 0u8, 0.0f64, 0.0f64); 5];
    for (i, pos) in raw.initial_positions.into_iter().enumerate() {
        if !pos.dx_m.is_finite() || !pos.dy_m.is_finite() {
            return Err(ConfigError::Validation("non-finite initial position".to_owned()));
        }
        initial_positions[i] = (pos.anchor_kind, pos.side_tag, pos.dx_m, pos.dy_m);
    }

    if raw.phases.is_empty() || raw.phases.len() > 12 {
        return Err(ConfigError::Validation(format!(
            "playbook {} must contain 1..=12 phases, got {}",
            raw.play_id,
            raw.phases.len()
        )));
    }
    let mut phases = Vec::with_capacity(raw.phases.len());
    for ph in raw.phases {
        if ph.actions.len() != 5 {
            return Err(ConfigError::Validation(format!(
                "playbook {} phase must contain exactly 5 actions",
                raw.play_id
            )));
        }
        let mut actions = [(0u8, 0u8, 0u8, 0u8, 0.0f64, 0.0f64, 0.0f64); 5];
        for (i, row) in ph.actions.into_iter().enumerate() {
            if !row.4.is_finite() || !row.5.is_finite() || !row.6.is_finite() {
                return Err(ConfigError::Validation("non-finite phase action float".to_owned()));
            }
            actions[i] = row;
        }
        phases.push(CompiledPlayPhase {
            base_epv: ph.base_epv,
            max_duration_ds: ph.max_duration_ds,
            exit_trigger_kind: ph.exit_trigger_kind,
            exit_target_role: ph.exit_target_role,
            exit_fallback_ds: ph.exit_fallback_ds,
            actions,
        });
    }

    Ok(CompiledPlaybook {
        play_id,
        min_shot_clock_ds: raw.min_shot_clock_ds,
        allow_mirror: raw.allow_mirror,
        role_priority,
        slot_weights,
        initial_positions,
        phases,
    })
}

fn compile_predicate(raw: RawPredicateExpr) -> Result<PredicateExpr, ConfigError> {
    let expr = match raw {
        RawPredicateExpr::All { items } => PredicateExpr::All(
            items
                .into_iter()
                .map(compile_predicate)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        RawPredicateExpr::Any { items } => PredicateExpr::Any(
            items
                .into_iter()
                .map(compile_predicate)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        RawPredicateExpr::Not { inner } => {
            PredicateExpr::Not(Box::new(compile_predicate(*inner)?))
        }
        RawPredicateExpr::Cmp {
            subject,
            metric_key,
            op,
            val,
        } => {
            if subject > 4 {
                return Err(ConfigError::Validation(format!(
                    "predicate subject {} out of valid range 0..=4",
                    subject
                )));
            }
            if op > 3 {
                return Err(ConfigError::Validation(format!(
                    "predicate op {} out of valid range 0..=3",
                    op
                )));
            }
            let key = resolve_metric_key(&metric_key)?;
            if subject == 4 && !is_context_metric_key(key) {
                return Err(ConfigError::Validation(format!(
                    "context predicate (subject=4) requires context metric_key 54..=59, got {}",
                    key
                )));
            }
            if subject < 4 && !is_player_slot_metric_key(key) {
                return Err(ConfigError::Validation(format!(
                    "player predicate (subject={}) cannot use context metric_key {}",
                    subject, key
                )));
            }
            if !val.is_finite() {
                return Err(ConfigError::Validation("non-finite predicate value".to_owned()));
            }
            PredicateExpr::Cmp {
                subject,
                metric_key: key,
                op,
                val,
            }
        }
    };
    if expr.depth() > 4 {
        return Err(ConfigError::Validation(format!(
            "predicate nesting depth {} exceeds maximum 4",
            expr.depth()
        )));
    }
    Ok(expr)
}

pub fn compile_defense(raw: RawDefenseFile) -> Result<CompiledDefenseGameplan, ConfigError> {
    let gameplan_id = GameplanId::new(&raw.gameplan_id)
        .map_err(|e| ConfigError::Validation(format!("invalid gameplan_id: {e}")))?;
    if raw.base_scheme > 3 {
        return Err(ConfigError::Validation(format!(
            "defense {} base_scheme {} out of range 0..=3",
            raw.gameplan_id, raw.base_scheme
        )));
    }

    let mut role_defs = Vec::with_capacity(raw.role_defs.len());
    let mut defined_roles = BTreeSet::new();
    for rd in raw.role_defs {
        validate_id_string(&rd.name)
            .map_err(|e| ConfigError::Validation(format!("invalid role_def name: {e}")))?;
        let compiled_expr = compile_predicate(rd.expr)?;
        defined_roles.insert(rd.name.clone());
        role_defs.push((rd.name, compiled_expr));
    }

    for pr in &raw.protect_roles {
        if !defined_roles.contains(pr) {
            return Err(ConfigError::UnknownRef {
                kind: "role_def",
                id: pr.clone(),
            });
        }
    }

    if raw.pnr_rules.len() > 24 {
        return Err(ConfigError::Validation(format!(
            "defense {} has {} pnr_rules (> 24 max)",
            raw.gameplan_id,
            raw.pnr_rules.len()
        )));
    }
    if raw.adaptation_triggers.len() > 8 {
        return Err(ConfigError::Validation(format!(
            "defense {} has {} adaptation_triggers (> 8 max)",
            raw.gameplan_id,
            raw.adaptation_triggers.len()
        )));
    }
    let mut seen_rules = BTreeSet::new();
    let mut pnr_rules = Vec::with_capacity(raw.pnr_rules.len());
    for rr in raw.pnr_rules {
        let rid = PnrRuleId::new(&rr.rule_id)
            .map_err(|e| ConfigError::Validation(format!("invalid rule_id: {e}")))?;
        if !seen_rules.insert(rr.rule_id.clone()) {
            return Err(ConfigError::Validation(format!(
                "duplicate PnrRuleId {} in defense {}",
                rr.rule_id, raw.gameplan_id
            )));
        }
        let predicates = rr
            .predicates
            .into_iter()
            .map(compile_predicate)
            .collect::<Result<Vec<_>, _>>()?;
        pnr_rules.push(CompiledPnrRule {
            rule_id: rid,
            predicates,
            coverage: rr.coverage,
            on_ball_action: rr.on_ball_action,
            help_rule: rr.help_rule,
        });
    }

    let mut adaptation_triggers = Vec::with_capacity(raw.adaptation_triggers.len());
    for at in raw.adaptation_triggers {
        adaptation_triggers.push(CompiledAdaptationTrigger {
            watch_coverage: at.watch_coverage,
            min_possessions: at.min_possessions,
            prior_weight: at.prior_weight,
            prior_ppp: at.prior_ppp,
            escalate_ppp_threshold: at.escalate_ppp_threshold,
            deescalate_ppp_threshold: at.deescalate_ppp_threshold,
            revert_if_escalated_ppp_exceeds: at.revert_if_escalated_ppp_exceeds,
            revert_min_possessions: at.revert_min_possessions,
            deescalate_min_possessions: at.deescalate_min_possessions,
            threat_absent_three_point_lt: at.threat_absent_three_point_lt,
            cooldown_possessions: at.cooldown_possessions,
            escalate_to_coverage: at.escalate_to_coverage,
        });
    }

    Ok(CompiledDefenseGameplan {
        gameplan_id,
        base_scheme: raw.base_scheme,
        switch_max_height_diff_cm: raw.switch_max_height_diff_cm,
        help_shrink_level: raw.help_shrink_level,
        role_defs,
        protect_roles: raw.protect_roles,
        hide_on_weakest_shooter: raw.hide_on_weakest_shooter,
        pre_switch_enabled: raw.pre_switch_enabled,
        pre_switch_trigger_radius_m: raw.pre_switch_trigger_radius_m,
        scram_switch_enabled: raw.scram_switch_enabled,
        peel_switch_enabled: raw.peel_switch_enabled,
        pnr_rules,
        adaptation_triggers,
    })
}

fn apply_functional_override(
    player_id: &str,
    height_cm: f64,
    profile: &mut PlayerFunctionalProfile,
    ov: RawFunctionalProfileOverride,
) -> Result<(), ConfigError> {
    let validate_score = |field: &str, v: f64| -> Result<f64, ConfigError> {
        if !v.is_finite() || !(0.0..=100.0).contains(&v) {
            return Err(ConfigError::Validation(format!(
                "player {} functional_profile_override.{}={} out of [0.0, 100.0]",
                player_id, field, v
            )));
        }
        Ok(v)
    };

    if let Some(scores) = ov.scores {
        for (i, v) in scores.into_iter().enumerate() {
            profile.scores[i] = validate_score("scores", v)?;
        }
    }

    let named = [
        ("primary_creator", ov.primary_creator, 0usize),
        ("secondary_handler", ov.secondary_handler, 1),
        ("spacer", ov.spacer, 2),
        ("movement_shooter", ov.movement_shooter, 3),
        ("roll_finisher", ov.roll_finisher, 4),
        ("post_hub", ov.post_hub, 5),
        ("point_of_attack", ov.point_of_attack, 6),
        ("rim_protection", ov.rim_protection, 7),
        ("switchability", ov.switchability, 8),
    ];
    for (name, opt, idx) in named {
        if let Some(v) = opt {
            profile.scores[idx] = validate_score(name, v)?;
        }
    }

    if let Some(mut tiers) = ov.guard_tiers {
        if !tiers.iter().any(|&v| v) {
            tiers[natural_tier(height_cm)] = true;
        }
        profile.guard_tiers = tiers;
    }

    Ok(())
}

pub fn compile_team(
    raw: RawTeamFile,
    ruleset: &RuleSet,
    badge_map: &BTreeMap<String, u16>,
    playbooks: &[CompiledPlaybook],
    defenses: &[CompiledDefenseGameplan],
) -> Result<CompiledTeamSnapshot, ConfigError> {
    validate_id_string(&raw.team_id)
        .map_err(|e| ConfigError::Validation(format!("invalid team_id: {e}")))?;

    let max_p = ruleset.roster.max_team_players as usize;
    if raw.players.len() < 5 || raw.players.len() > max_p {
        return Err(ConfigError::Validation(format!(
            "team {} roster count {} not in 5..={max_p}",
            raw.team_id,
            raw.players.len()
        )));
    }

    if !defenses
        .iter()
        .any(|d| d.gameplan_id.as_str() == raw.tactics.defensive_gameplan_id)
    {
        return Err(ConfigError::UnknownRef {
            kind: "defensive_gameplan",
            id: raw.tactics.defensive_gameplan_id,
        });
    }
    for (pid, w) in &raw.tactics.play_weights {
        if !w.is_finite() || *w < 0.0 {
            return Err(ConfigError::Validation("invalid play weight".to_owned()));
        }
        if !playbooks.iter().any(|p| p.play_id.as_str() == pid) {
            return Err(ConfigError::UnknownRef {
                kind: "playbook",
                id: pid.clone(),
            });
        }
    }

    let primary_handlers = InlinePlayerList::from_slice(&raw.tactics.primary_handlers)
        .map_err(|e| ConfigError::Validation(format!("invalid primary_handlers: {e}")))?;
    let def_id = GameplanId::new(&raw.tactics.defensive_gameplan_id)
        .map_err(|e| ConfigError::Validation(format!("invalid defensive_gameplan_id: {e}")))?;

    if raw.tactics.rotation_script.stints.len() > 32 {
        return Err(ConfigError::Validation(format!(
            "team {} rotation_script stints {} exceeds maximum 32",
            raw.team_id,
            raw.tactics.rotation_script.stints.len()
        )));
    }
    let mut stints = Vec::with_capacity(raw.tactics.rotation_script.stints.len());
    for st in raw.tactics.rotation_script.stints {
        if st.target_lineup_ranks.len() != 5 {
            return Err(ConfigError::Validation(
                "rotation stint target_lineup_ranks len must be 5".to_owned(),
            ));
        }
        if st.start_game_clock_ds < st.end_game_clock_ds {
            return Err(ConfigError::Validation(format!(
                "rotation stint start_game_clock_ds ({}) < end_game_clock_ds ({})",
                st.start_game_clock_ds, st.end_game_clock_ds
            )));
        }
        let mut ranks = [0u8; 5];
        for (i, &r) in st.target_lineup_ranks.iter().enumerate() {
            if (r as usize) >= raw.players.len() {
                return Err(ConfigError::Validation(format!(
                    "rotation stint rank {r} out of roster bounds"
                )));
            }
            ranks[i] = r;
        }
        stints.push(CompiledRotationStint {
            quarter_mask: st.quarter_mask,
            start_game_clock_ds: st.start_game_clock_ds,
            end_game_clock_ds: st.end_game_clock_ds,
            target_lineup_ranks: ranks,
        });
    }

    let mut players = Vec::with_capacity(raw.players.len());
    let mut seen_pids = BTreeSet::new();
    for rp in raw.players {
        validate_id_string(&rp.player_id)
            .map_err(|e| ConfigError::Validation(format!("invalid player_id: {e}")))?;
        validate_id_string(&rp.display_name)
            .map_err(|e| ConfigError::Validation(format!("invalid display_name: {e}")))?;
        if !seen_pids.insert(rp.player_id.clone()) {
            return Err(ConfigError::Validation(format!(
                "duplicate player_id {}",
                rp.player_id
            )));
        }
        if rp.jersey_number_code > 100 {
            return Err(ConfigError::Validation(format!(
                "jersey_number_code {} > 100",
                rp.jersey_number_code
            )));
        }
        if rp.attributes.values.len() != 38 {
            return Err(ConfigError::Validation(format!(
                "player {} attributes len must be 38, got {}",
                rp.player_id,
                rp.attributes.values.len()
            )));
        }
        let mut arr = [0u8; 38];
        for (i, &v) in rp.attributes.values.iter().enumerate() {
            if v > 99 {
                return Err(ConfigError::Validation(format!(
                    "player {} attr[{i}]={} > 99",
                    rp.player_id, v
                )));
            }
            arr[i] = v;
        }
        let attrs = PlayerAttributes38(arr);
        let mut functional = derive_functional_profile(&rp.physical, &attrs, &rp.tendencies);
        if let Some(ov) = rp.functional_profile_override {
            apply_functional_override(&rp.player_id, rp.physical.height_cm, &mut functional, ov)?;
        }
        let mut badge_indices = Vec::with_capacity(rp.badges.len());
        for bname in rp.badges {
            let idx = badge_map
                .get(&bname)
                .ok_or_else(|| ConfigError::UnknownRef {
                    kind: "badge",
                    id: bname,
                })?;
            badge_indices.push(*idx);
        }

        players.push(CompiledPlayerSnapshot {
            player_id: rp.player_id,
            display_name: rp.display_name,
            jersey_number_code: rp.jersey_number_code,
            physical: rp.physical,
            attributes: attrs,
            tendencies: rp.tendencies,
            functional,
            badge_indices,
        });
    }

    Ok(CompiledTeamSnapshot {
        team_id: raw.team_id,
        tactics: TeamTacticsConfig {
            pace: raw.tactics.pace,
            transition_push: raw.tactics.transition_push,
            default_spacing: raw.tactics.default_spacing,
            primary_handlers,
            play_weights: raw.tactics.play_weights,
            orb_crash_count: raw.tactics.orb_crash_count,
            defensive_gameplan_id: def_id,
            sag_off_3pt_threshold: raw.tactics.sag_off_3pt_threshold,
            foul_up_three_policy: raw.tactics.foul_up_three_policy,
            matchup_overrides: raw.tactics.matchup_overrides,
            full_court_press_clutch: raw.tactics.full_court_press_clutch,
            rotation_script: CompiledRotationScript { stints },
        },
        players,
    })
}
