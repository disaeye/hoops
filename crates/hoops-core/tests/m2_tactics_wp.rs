mod common;

use common::{make_badges, make_defense, make_playbook, make_player};
use hoops_core::physics::CourtGeometry;
use hoops_core::resolve::ShotOutcomeProbs;
use hoops_core::tactics::*;
use hoops_core::*;

#[test]
fn test_m2_functional_profile_role_assignment_and_bayesian_adaptation() {
    let p_guard = make_player("guard", 0, false, 88, 90);
    let p_wing = make_player("wing", 1, false, 85, 86);
    let p_fwd = make_player("fwd", 2, false, 78, 80);
    let p_pf = make_player("pf", 3, true, 72, 76);
    let p_c = make_player("center", 4, true, 60, 70);
    assert!(p_guard.functional.guard_tiers[0]);
    assert!(!p_c.functional.guard_tiers[0]);

    // K! role assignment across K in 2..=5 with bijectivity
    let pb = make_playbook("spread_pnr");
    let pool = [(0, &p_guard), (1, &p_wing), (2, &p_fwd), (3, &p_pf), (4, &p_c)];
    for k in 2..=5 {
        let binding = assign_playbook_roles(&pb, &pool[..k]);
        let mut seen = [false; 5];
        let mut assigned = 0usize;
        for &slot in &binding {
            if slot != ROSTER_IDX_NONE {
                assert!(!seen[slot as usize], "duplicate slot assignment for k={k}");
                seen[slot as usize] = true;
                assigned += 1;
            }
        }
        assert_eq!(assigned, k);
    }

    // Bayesian defensive coverage escalation -> cooldown -> revert / deescalate
    let gp = make_defense("drop_base");
    let mut adapt = AdaptiveCoverageState::default();
    assert_eq!(adapt.active_coverage, PnrCoverageKind::Drop);
    // Allow 18 pts over 6 possessions -> shrunk PPP = (18 + 10*1.0)/(6 + 10) = 28/16 = 1.75 >= 1.15 -> escalate!
    for _ in 0..6 {
        update_adaptive_coverage(&mut adapt, &gp, 3, true);
    }
    assert!(adapt.escalated);
    assert_eq!(adapt.active_coverage, PnrCoverageKind::Blitz);
    // Threat absent reverts escalation after cooldown expires (cooldown=2)
    for _ in 0..3 {
        update_adaptive_coverage(&mut adapt, &gp, 0, false);
    }
    assert!(!adapt.escalated);
    assert_eq!(adapt.active_coverage, PnrCoverageKind::Drop);
}

#[test]
fn test_m2_footwork_action_mask_and_2ply_epv() {
    let tunables = EngineTunables::default();
    let teammates = [(1u8, 0x01u8), (2u8, 0x02u8)];

    // Step-1 jump stop locks both feet -> no Pivot in legal actions
    let fw_locked = FootworkRecord {
        dribble_status: DribbleStatus::Gathered,
        pivot_foot: PivotFoot::LockedBothFeetAfterStep1JumpStop,
        landed_simultaneously_after_step1: true,
        steps_since_gather: 1,
        ..FootworkRecord::default()
    };
    let acts = legal_actions_for_holder(&fw_locked, &teammates, true, Some(140), &tunables, false, 0, Some(1_400_000));
    assert!(!acts.iter().any(|a| matches!(a, CandidateAction::Pivot { .. })));
    assert!(!acts.iter().any(|a| matches!(a, CandidateAction::Drive { .. })));
    assert!(acts.contains(&CandidateAction::Shoot));

    // Order key stability for deterministic Tie-Breaking
    let mut sorted = acts.clone();
    sorted.sort_by_key(|a| a.order_key());
    assert!(sorted.windows(2).all(|w| w[0].order_key() <= w[1].order_key()));

    // Bounded 2-Ply EPV (`d == 2` exercises extra pass; `d == 0` never invokes EPV_pass)
    let rules = RuleSet::nba_default();
    let geom = CourtGeometry::from_rules(&rules.court);
    let badges = make_badges();
    let p0 = make_player("p0", 0, false, 84, 88);
    let p1 = make_player("p1", 1, false, 92, 92);
    let p2 = make_player("p2", 2, true, 65, 72);
    let team = [(0u8, &p0, 6.0, 0.0), (1u8, &p1, 12.8, 7.1), (2u8, &p2, 11.8, -1.5)];
    let eval = EpvEvaluator {
        geom: &geom,
        tunables: &tunables,
        badges: &badges,
        shot_clock_ds: Some(180),
        attack_dir: 1.0,
    };
    let d0_drive = eval.epv_drive(0, 0, 0, &team, &|_x, _y| 0.4);
    let d2_pass = eval.epv_pass(0, 1, 2, &team, &|_x, y| if y > 5.0 { 0.05 } else { 0.7 });
    assert!(d0_drive.is_finite());
    assert!(d2_pass > 0.5, "open corner shooter pass should produce high EPV: {d2_pass}");
}

#[test]
fn test_m2_endgame_wp_3_scenarios_exact_10us_boundary() {
    let rules = RuleSet::nba_default();
    let tunables = EngineTunables::default();

    // Scenario A: Lead +1, 1.8s left, zero-steal intentional foul disabled -> clock kill WP(Hold) ~ 0.9873
    let ctx_lead = EndgameWpContext {
        delta_s: 1.0,
        gc_rem_10us: 18 * SUB_DS_UNITS, // 1.8s = 18 ds
        sc_rem_10us: None,
        is_deadball_restart: false,
        opp_in_bonus_on_next_foul: false,
        intentional_foul_policy: IntentionalFoulPolicy::Disabled,
        lambda_steal_per_ds: 0.0,
        lambda_foul_per_ds: 0.0,
        p_ft: 0.85,
        p_orb: 0.25,
        p_orb_ft: 0.14,
        ruleset: &rules,
        tunables: &tunables,
    };
    let wp_hold = evaluate_wp_hold(&ctx_lead).unwrap();
    assert!((wp_hold - 0.9873).abs() < 0.02, "expected ~0.9873 clock-kill WP, got {wp_hold}");
    let open_2pt = ShotOutcomeProbs {
        p_blk_tov: 0.02,
        p_and1: 0.03,
        p_foul_miss: 0.05,
        p_clean_make: 0.50,
        p_clean_miss: 0.40,
        p_make_clean: 0.55,
        p_swish_given_nonbank_make: 0.6,
        p_airball_given_miss: 0.05,
    };
    let wp_early_shoot = evaluate_wp_shoot(&ctx_lead, open_2pt, 2, 2, 5);
    assert!(wp_hold > wp_early_shoot, "Hold ({wp_hold}) must beat early 2PT shoot ({wp_early_shoot})");

    // Scenario B: Down -3 with 2.0s left -> 3PT dominates 2PT
    let ctx_down3 = EndgameWpContext {
        delta_s: -3.0,
        gc_rem_10us: 20 * SUB_DS_UNITS,
        sc_rem_10us: Some(20 * SUB_DS_UNITS),
        is_deadball_restart: false,
        opp_in_bonus_on_next_foul: true,
        intentional_foul_policy: IntentionalFoulPolicy::Auto,
        lambda_steal_per_ds: 0.001,
        lambda_foul_per_ds: 0.002,
        p_ft: 0.85,
        p_orb: 0.25,
        p_orb_ft: 0.14,
        ruleset: &rules,
        tunables: &tunables,
    };
    let probs_3 = ShotOutcomeProbs {
        p_blk_tov: 0.02,
        p_and1: 0.01,
        p_foul_miss: 0.02,
        p_clean_make: 0.35,
        p_clean_miss: 0.60,
        p_make_clean: 0.37,
        p_swish_given_nonbank_make: 0.7,
        p_airball_given_miss: 0.08,
    };
    let wp_2pt = evaluate_wp_shoot(&ctx_down3, open_2pt, 2, 2, 5);
    let wp_3pt = evaluate_wp_shoot(&ctx_down3, probs_3, 3, 2, 7);
    assert!(wp_3pt > wp_2pt, "Down 3 with 2s: 3PT ({wp_3pt}) must beat 2PT ({wp_2pt})");

    // Scenario C: Exact 10 us release gate (`30_001` vs `30_000`) & Hold min shot clock gate (`150_001` vs `150_000`)
    let mut ctx_rel_gate = ctx_lead;
    ctx_rel_gate.gc_rem_10us = 100_000;
    ctx_rel_gate.sc_rem_10us = Some(30_000);
    let wp_sc_30000 = evaluate_wp_shoot(&ctx_rel_gate, open_2pt, 2, 3, 5);
    ctx_rel_gate.sc_rem_10us = Some(30_001);
    let wp_sc_30001 = evaluate_wp_shoot(&ctx_rel_gate, open_2pt, 2, 3, 5);
    assert!(wp_sc_30001 > wp_sc_30000, "30_001 beats clock-expired 30_000 violation");

    let mut ctx_edge = ctx_lead;
    ctx_edge.delta_s = 1.0;
    ctx_edge.gc_rem_10us = 1_000_000;
    ctx_edge.sc_rem_10us = Some(150_000);
    assert!(evaluate_wp_hold(&ctx_edge).is_none());
    ctx_edge.sc_rem_10us = Some(150_001);
    assert!(evaluate_wp_hold(&ctx_edge).is_some());

    // Exact shared 10us competing-risks boundary check (`gc == delta_u_rel == 30_000`, Hold window == 30_000 -> delta_t == 3.0)
    let mut ctx_shared = ctx_lead;
    ctx_shared.gc_rem_10us = 30_000;
    ctx_shared.sc_rem_10us = None;
    let wp_shoot_late = evaluate_wp_shoot(&ctx_shared, open_2pt, 2, 3, 5);
    let wp_hold_3ds = evaluate_wp_hold(&ctx_shared).unwrap();
    assert!((wp_shoot_late - wp_hold_3ds).abs() < 1.0e-12);
}

#[test]
fn test_m2_lambda_help_monotonicity() {
    let rules = RuleSet::nba_default();
    let geom = CourtGeometry::from_rules(&rules.court);
    let tunables = EngineTunables::default();
    let rim_xy = geom.rim_xy(1.0);

    // Three scenarios with strictly increasing help need: A (on-ball in front, perimeter) < B (beaten moderately) < C (deep rim drive beaten)
    let (_, l_a, _) = compute_help_triangle((7.0, 2.0), (8.2, 2.0), rim_xy, (12.0, -7.0), (0.0, 0.0), 75, 92, 90, &geom, &tunables);
    let (_, l_b, _) = compute_help_triangle((9.5, 1.5), (9.0, 2.8), rim_xy, (12.0, -7.0), (4.0, -0.5), 75, 78, 78, &geom, &tunables);
    let (_, l_c, _) = compute_help_triangle((11.5, 0.5), (10.0, 2.5), rim_xy, (12.0, -7.0), (6.0, 0.0), 75, 62, 62, &geom, &tunables);
    assert!(l_a < l_b && l_b < l_c, "expected monotonicity A ({l_a}) < B ({l_b}) < C ({l_c})");
}

#[test]
fn test_m2_spacing_potential_field_separates_crowded_teammates() {
    let rules = RuleSet::nba_default();
    let geom = CourtGeometry::from_rules(&rules.court);
    let tunables = EngineTunables::default();
    let shooter = make_player("wing", 1, false, 88, 86);

    // Teammate standing directly on base anchor (10.5, 5.5) within < 1.0m causes field to separate cleanly
    let base_anchor = (10.5, 5.5);
    let off_xy = [
        (6.8, 0.0),
        base_anchor,
        (10.7, 5.2),
        (13.0, -7.0),
        (11.5, -1.8),
    ];
    let def_xy = [(7.8, 0.0), (11.2, 4.0), (11.2, -4.0), (12.2, -6.0), (12.0, -1.2)];
    let active = [true; 5];

    let chosen = evaluate_spacing_potential_field(
        1,
        &shooter,
        base_anchor,
        &off_xy,
        &active,
        &def_xy,
        &active,
        off_xy[0],
        false,
        1.0,
        &geom,
        &tunables,
    );
    let sep_from_crowded = libm::hypot(chosen.0 - off_xy[2].0, chosen.1 - off_xy[2].1);
    assert!(
        sep_from_crowded >= 2.1,
        "spacing field should separate from crowded teammate (< 1m -> got {sep_from_crowded:.2}m at {chosen:?})"
    );
}

#[test]
fn test_m2_drive_and_react_and_offball_screen_reads() {
    let rules = RuleSet::nba_default();
    let geom = CourtGeometry::from_rules(&rules.court);

    // Paint big relocates to opposite dunker spot when handler drives strong-side (+y)
    let (dx, dy, _) = compute_drive_and_react_relocation(
        11.2,
        1.5,
        (8.0, 2.5),
        (12.2, 1.4),
        1.0,
        62,
        4.0,
        &geom,
    );
    assert!(dy < -0.5 && dx > 10.5, "big should move to opposite dunker spot: ({dx}, {dy})");

    // Strong corner shooter lifts when ball drives baseline strong side
    let (lx, ly, _) = compute_drive_and_react_relocation(
        13.0,
        6.8,
        (9.5, 4.2),
        (12.0, 4.5),
        1.0,
        88,
        2.0,
        &geom,
    );
    assert!(lx < 11.0 && ly > 4.0, "strong corner should lift to wing: ({lx}, {ly})");

    // Off-ball screen reads: Top-lock -> Backdoor (2), Under -> Flare (5), Trail -> Curl (4)
    let (bk, _, _) = compute_offball_screen_read((8.0, 5.2), (10.5, 3.0), (8.9, 4.1), (6.5, 0.0), 1.0, 86, &geom);
    assert_eq!(bk, 2, "top-lock defender should trigger backdoor read");

    let (fl, _, _) = compute_offball_screen_read((12.5, 6.2), (11.0, 4.5), (12.6, 2.2), (7.5, -2.0), 1.0, 88, &geom);
    assert_eq!(fl, 5, "sagging/under defender should trigger flare read");

    let (cl, _, _) = compute_offball_screen_read((12.5, 6.2), (10.5, 3.5), (13.1, 6.9), (7.5, -2.0), 1.0, 85, &geom);
    assert_eq!(cl, 4, "trailing defender should trigger curl read");
}

#[test]
fn test_m2_hide_on_weakest_shooter_matchups() {
    let mut gp = make_defense("drop_base"); // drop_base enables hide_on_weakest_shooter + weak_handler < 74 3pt
    gp.hide_on_weakest_shooter = true;
    gp.protect_roles = vec!["weak_handler".to_owned()];
    gp.role_defs = vec![(
        "weak_handler".to_owned(),
        PredicateExpr::Cmp {
            subject: 0,
            metric_key: 7,
            op: 0,
            val: 74.0,
        },
    )];
    let o0 = make_player("star_pg", 0, false, 90, 90);
    let mut o1 = make_player("non_shooter_lock", 1, false, 58, 72);
    o1.attributes.0[14] = 55;
    o1.attributes.0[9] = 56;
    let o2 = make_player("wing_3d", 2, false, 84, 82);
    let o3 = make_player("pf", 3, true, 78, 76);
    let o4 = make_player("c", 4, true, 52, 68);

    let mut d0 = make_player("weak_def", 0, false, 68, 75);
    d0.attributes.0[19] = 58;
    d0.attributes.0[31] = 62; // Protected low-lateral / low-3pt guard
    let mut d1 = make_player("lock_wing", 1, false, 80, 80);
    d1.attributes.0[19] = 90;
    d1.attributes.0[31] = 88;
    let d2 = make_player("wing_d", 2, false, 78, 78);
    let d3 = make_player("pf_d", 3, true, 75, 75);
    let d4 = make_player("c_d", 4, true, 60, 70);

    let off_arr = [Some(&o0), Some(&o1), Some(&o2), Some(&o3), Some(&o4)];
    let def_arr = [Some(&d0), Some(&d1), Some(&d2), Some(&d3), Some(&d4)];
    let m = assign_initial_defensive_matchups(Some(&gp), &off_arr, &def_arr);
    assert_ne!(m[0], 0, "weak/protected defender should not guard star creator o0: {m:?}");
    assert!(m[0] == 1 || m[0] == 4, "protected defender should hide on weakest shooter (o1 or o4), got {}", m[0]);
}
