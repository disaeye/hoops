use std::collections::BTreeMap;

use hoops_core::{
    CommandSide, DebugOverlayFrameV1, GamePhase, GameRecordEventPayload, GameRecordEventV1,
    MatchInitSpecV1, PublicFrameV1, RuleCode, ROSTER_IDX_NONE,
};
use serde::{Deserialize, Serialize};

use crate::boxscore::LiveBoxScoreState;

const CROWDING_THRESHOLD_M: f64 = 1.8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalyticsVisibility {
    PublicLiveOwnSide(hoops_core::CommandSide),
    PostGameFull,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LineupStintRecord {
    pub stint_id: u32,
    pub side: u8,
    pub lineup_roster_indices: [u8; 5],
    pub start_tick: u32,
    pub end_tick: u32,
    pub live_ds: u32,
    pub off_poss: u16,
    pub def_poss: u16,
    pub pts_for: u16,
    pub pts_against: u16,
    pub net_rtg: f64,
    pub median_min_off_pair_m: f64,
    pub crowding_ratio: f64,
    pub def_3s_count: u16,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MatchupPairStats {
    pub def_roster_idx: u8,
    pub off_roster_idx: u8,
    pub guarded_live_ds: u32,
    pub opp_pts: u16,
    pub opp_fga: u16,
    pub opp_fgm: u16,
    pub opp_3pa: u16,
    pub opp_3pm: u16,
    pub forced_tov: u16,
    pub fouls_committed: u16,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PlaybookReportCard {
    pub side: u8,
    pub play_id: String,
    pub possessions: u16,
    pub points: u16,
    pub ppp: f64,
    pub fga: u16,
    pub fgm: u16,
    pub turnovers: u16,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DefenseCoverageCard {
    pub side: u8,
    pub gameplan_id: String,
    pub possessions_guarded: u16,
    pub points_allowed: u16,
    pub ppp_allowed: f64,
    pub forced_turnovers: u16,
    pub def_3s_violations: u16,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TeamEfficiencySnapshot {
    pub score: u16,
    pub possessions: f64,
    pub ortg: f64,
    pub drtg: f64,
    pub efg_pct: f64,
    pub tpa_share: f64,
    pub ftr: f64,
    pub orb_pct: f64,
    pub tov_pct: f64,
    pub crowding_ratio: f64,
    pub def_3s_count: u16,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MatchAnalyticsSummary {
    pub home_efficiency: TeamEfficiencySnapshot,
    pub away_efficiency: TeamEfficiencySnapshot,
    pub stints: Vec<LineupStintRecord>,
    pub matchups: Vec<MatchupPairStats>,
    pub play_cards: Vec<PlaybookReportCard>,
    pub defense_cards: Vec<DefenseCoverageCard>,
    pub debrief_findings: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WhatIfCausalDelta {
    pub delta_margin_from_side: f64,
    pub delta_ortg: f64,
    pub delta_drtg: f64,
    pub delta_efg_pct: f64,
    pub delta_tpa_share: f64,
    pub delta_tov_pct: f64,
    pub delta_orb_pct: f64,
    pub delta_crowding_ratio: f64,
}

#[derive(Debug, Clone)]
struct StintAccum {
    side: u8,
    lineup: [u8; 5],
    start_tick: u32,
    end_tick: u32,
    live_ds: i64,
    off_poss: u16,
    def_poss: u16,
    pts_for: i32,
    pts_against: i32,
    pair_dists_m: Vec<f64>,
    crowded_frames: u32,
    def_3s_count: u16,
}

#[derive(Debug, Clone)]
struct ShotAttribution {
    scorer_team: u8,
    home_stint_idx: Option<usize>,
    away_stint_idx: Option<usize>,
    play_key: Option<(u8, String)>,
    def_key: Option<(u8, String)>,
    matchup_key: Option<(u8, u8)>,
}

#[derive(Debug, Clone, Copy)]
struct PendingShotMeta {
    is_three: bool,
    defender_slot: u8,
    release_tick: u32,
}

/// Extract sorted 5-player lineup roster indices for `side` (`0 = Home`, `1 = Away`).
fn extract_sorted_lineup(frame: &PublicFrameV1, side: usize) -> [u8; 5] {
    let base = side * 5;
    let mut lineup = [
        frame.players[base].roster_idx,
        frame.players[base + 1].roster_idx,
        frame.players[base + 2].roster_idx,
        frame.players[base + 3].roster_idx,
        frame.players[base + 4].roster_idx,
    ];
    lineup.sort_unstable();
    lineup
}

/// Compute minimum Euclidean pairwise distance (meters) among active players of `side`.
pub fn min_offensive_pair_distance_m(frame: &PublicFrameV1, side: usize) -> Option<f64> {
    let base = side * 5;
    let mut pts = [(0.0_f64, 0.0_f64); 5];
    let mut n = 0usize;
    for slot in &frame.players[base..base + 5] {
        if slot.roster_idx != ROSTER_IDX_NONE {
            pts[n] = (slot.x_mm as f64 / 1000.0, slot.y_mm as f64 / 1000.0);
            n += 1;
        }
    }
    if n < 2 {
        return None;
    }
    let mut min_d = f64::INFINITY;
    for i in 0..n {
        for j in (i + 1)..n {
            let d = libm::hypot(pts[i].0 - pts[j].0, pts[i].1 - pts[j].1);
            if d < min_d {
                min_d = d;
            }
        }
    }
    if min_d.is_finite() {
        Some(min_d)
    } else {
        None
    }
}

/// Compute exact median of a non-empty or empty slice of finite `f64` values.
pub fn median_f64(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return 0.0;
    }
    sorted.sort_by(|a, b| a.total_cmp(b));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        0.5 * (sorted[mid - 1] + sorted[mid])
    }
}

fn find_frame_at_tick(frames: &[PublicFrameV1], tick: u32) -> Option<&PublicFrameV1> {
    if frames.is_empty() {
        return None;
    }
    let t0 = frames[0].tick;
    if tick >= t0 {
        let off = (tick - t0) as usize;
        if let Some(f) = frames.get(off) {
            if f.tick == tick {
                return Some(f);
            }
        }
    }
    frames
        .binary_search_by_key(&tick, |f| f.tick)
        .ok()
        .map(|idx| &frames[idx])
}

fn find_debug_frame_at_tick(
    debug_frames: Option<&[DebugOverlayFrameV1]>,
    frame_idx: usize,
    tick: u32,
) -> Option<&DebugOverlayFrameV1> {
    let dfs = debug_frames?;
    if let Some(df) = dfs.get(frame_idx) {
        if df.tick == tick {
            return Some(df);
        }
    }
    dfs.binary_search_by_key(&tick, |d| d.tick)
        .ok()
        .map(|idx| &dfs[idx])
}

fn nearest_offender_for_def_slot(frame: &PublicFrameV1, def_slot: usize, off_side: usize) -> Option<u8> {
    let dp = &frame.players[def_slot];
    if dp.roster_idx == ROSTER_IDX_NONE {
        return None;
    }
    let base = off_side * 5;
    let mut best_r = None;
    let mut best_d2 = i64::MAX;
    for op in &frame.players[base..base + 5] {
        if op.roster_idx == ROSTER_IDX_NONE {
            continue;
        }
        let dx = (op.x_mm as i64) - (dp.x_mm as i64);
        let dy = (op.y_mm as i64) - (dp.y_mm as i64);
        let d2 = dx * dx + dy * dy;
        if d2 < best_d2 {
            best_d2 = d2;
            best_r = Some(op.roster_idx);
        }
    }
    best_r
}

fn nearest_defender_for_off_roster(
    frame: &PublicFrameV1,
    off_roster_idx: u8,
    def_side: usize,
) -> Option<u8> {
    let off_slot = frame
        .players
        .iter()
        .position(|p| p.roster_idx == off_roster_idx)?;
    let op = &frame.players[off_slot];
    let base = def_side * 5;
    let mut best_r = None;
    let mut best_d2 = i64::MAX;
    for dp in &frame.players[base..base + 5] {
        if dp.roster_idx == ROSTER_IDX_NONE {
            continue;
        }
        let dx = (dp.x_mm as i64) - (op.x_mm as i64);
        let dy = (dp.y_mm as i64) - (op.y_mm as i64);
        let d2 = dx * dx + dy * dy;
        if d2 < best_d2 {
            best_d2 = d2;
            best_r = Some(dp.roster_idx);
        }
    }
    best_r
}

fn resolve_matchup_on_frame(
    frame: &PublicFrameV1,
    dbg: Option<&DebugOverlayFrameV1>,
    def_slot: usize,
    off_side: usize,
) -> Option<u8> {
    if let Some(df) = dbg {
        let cand = df.player_debug[def_slot].defender_slot as usize;
        let base = off_side * 5;
        if cand >= base && cand < base + 5 {
            let r = frame.players[cand].roster_idx;
            if r != ROSTER_IDX_NONE {
                return Some(r);
            }
        }
    }
    nearest_offender_for_def_slot(frame, def_slot, off_side)
}

fn resolve_play_id(init_spec: &MatchInitSpecV1, idx: u16) -> Option<String> {
    if idx == 0xFFFF {
        return None;
    }
    init_spec
        .playbooks
        .get(idx as usize)
        .map(|p| p.play_id.as_str().to_owned())
}

fn resolve_gameplan_id(init_spec: &MatchInitSpecV1, idx: u16) -> Option<String> {
    if idx == 0xFFFF {
        return None;
    }
    init_spec.defenses.iter()
        .flat_map(|plan| plan.pnr_rules.iter().map(move |_| plan))
        .nth(usize::from(idx))
        .map(|plan| plan.gameplan_id.as_str().to_owned())
}

fn side_name_zh(side: u8) -> &'static str {
    if side == 0 {
        "主队"
    } else {
        "客队"
    }
}

fn build_efficiency_snapshot(
    side: usize,
    boxs: &LiveBoxScoreState,
    discrete_off_poss: [u32; 2],
    off_crowded_frames: [u32; 2],
    off_live_frames: [u32; 2],
    def_3s_count: [u16; 2],
) -> TeamEfficiencySnapshot {
    let (tm, opp) = if side == 0 {
        (&boxs.home, &boxs.away)
    } else {
        (&boxs.away, &boxs.home)
    };
    let raw_poss =
        (tm.fga as f64) + 0.44 * (tm.fta as f64) - (tm.oreb as f64) + (tm.tov as f64);
    let poss = if raw_poss > 0.0 {
        raw_poss
    } else {
        discrete_off_poss[side] as f64
    };
    let opp_raw_poss =
        (opp.fga as f64) + 0.44 * (opp.fta as f64) - (opp.oreb as f64) + (opp.tov as f64);
    let opp_poss = if opp_raw_poss > 0.0 {
        opp_raw_poss
    } else {
        discrete_off_poss[1 - side] as f64
    };
    let ortg = if poss > 0.0 {
        100.0 * (tm.score_official as f64) / poss
    } else {
        0.0
    };
    let drtg = if opp_poss > 0.0 {
        100.0 * (opp.score_official as f64) / opp_poss
    } else {
        0.0
    };
    let efg_pct = if tm.fga > 0 {
        (tm.fgm as f64 + 0.5 * (tm.tpm as f64)) / (tm.fga as f64)
    } else {
        0.0
    };
    let tpa_share = if tm.fga > 0 {
        (tm.tpa as f64) / (tm.fga as f64)
    } else {
        0.0
    };
    let ftr = if tm.fga > 0 {
        (tm.fta as f64) / (tm.fga as f64)
    } else {
        0.0
    };
    let reb_chances = tm.oreb + opp.dreb;
    let orb_pct = if reb_chances > 0 {
        (tm.oreb as f64) / (reb_chances as f64)
    } else {
        0.0
    };
    let tov_denom = (tm.fga as f64) + 0.44 * (tm.fta as f64) + (tm.tov as f64);
    let tov_pct = if tov_denom > 0.0 {
        (tm.tov as f64) / tov_denom
    } else {
        0.0
    };
    let crowding_ratio = if off_live_frames[side] > 0 {
        (off_crowded_frames[side] as f64) / (off_live_frames[side] as f64)
    } else {
        0.0
    };

    TeamEfficiencySnapshot {
        score: tm.score_official,
        possessions: poss,
        ortg,
        drtg,
        efg_pct,
        tpa_share,
        ftr,
        orb_pct,
        tov_pct,
        crowding_ratio,
        def_3s_count: def_3s_count[side],
    }
}

/// Project deterministic match analytics from `PublicFrameV1`, optional `DebugOverlayFrameV1`, and `GameRecordEventV1`
/// with strict PvP fog-of-war enforcement.
pub fn project_match_analytics(
    init_spec: &MatchInitSpecV1,
    public_frames: &[PublicFrameV1],
    debug_frames: Option<&[DebugOverlayFrameV1]>,
    events: &[GameRecordEventV1],
    visibility: AnalyticsVisibility,
) -> MatchAnalyticsSummary {
    let (full_access, own_side_filter) = match visibility {
        AnalyticsVisibility::PostGameFull => (true, None),
        AnalyticsVisibility::PublicLiveOwnSide(side) => match side {
            CommandSide::Home => (false, Some(0u8)),
            CommandSide::Away => (false, Some(1u8)),
            CommandSide::System => return MatchAnalyticsSummary::default(),
        },
    };
    let effective_debug = if full_access { debug_frames } else { None };
    let has_debug = effective_debug.is_some();

    let mut boxs = LiveBoxScoreState::from_init_spec(init_spec);

    let mut stints: Vec<StintAccum> = Vec::new();
    let mut active_stint_idx: [Option<usize>; 2] = [None, None];
    let mut discrete_off_poss = [0u32; 2];
    let mut off_crowded_frames = [0u32; 2];
    let mut off_live_frames = [0u32; 2];
    let mut def_3s_count = [0u16; 2];

    let mut poss_open: Option<u8> = None;
    let mut active_play_id: [Option<String>; 2] = [
        init_spec
            .home_team
            .tactics
            .play_weights
            .first()
            .map(|(id, _)| id.clone()),
        init_spec
            .away_team
            .tactics
            .play_weights
            .first()
            .map(|(id, _)| id.clone()),
    ];
    let mut active_def_id: [String; 2] = [
        init_spec
            .home_team
            .tactics
            .defensive_gameplan_id
            .as_str()
            .to_owned(),
        init_spec
            .away_team
            .tactics
            .defensive_gameplan_id
            .as_str()
            .to_owned(),
    ];
    let mut open_poss_play: Option<(u8, String)> = None;
    let mut open_poss_def: Option<(u8, String)> = None;

    let mut pending_shots: BTreeMap<u32, PendingShotMeta> = BTreeMap::new();
    let mut shot_attributions: BTreeMap<u64, ShotAttribution> = BTreeMap::new();
    let mut matchups: BTreeMap<(u8, u8), MatchupPairStats> = BTreeMap::new();
    let mut play_cards: BTreeMap<(u8, String), PlaybookReportCard> = BTreeMap::new();
    let mut defense_cards: BTreeMap<(u8, String), DefenseCoverageCard> = BTreeMap::new();

    // Latest defensive matchup mapping observed: def_roster_idx -> off_roster_idx, off_roster_idx -> def_roster_idx
    let mut latest_def_guards_off: BTreeMap<u8, u8> = BTreeMap::new();
    let mut latest_off_guarded_by: BTreeMap<u8, u8> = BTreeMap::new();

    let live_phase = GamePhase::LiveBall as u8;
    let mut ev_idx = 0usize;

    let mut process_event = |ev: &GameRecordEventV1,
                             frame_ctx: Option<&PublicFrameV1>,
                             dbg_ctx: Option<&DebugOverlayFrameV1>,
                             boxs: &mut LiveBoxScoreState,
                             stints: &mut Vec<StintAccum>,
                             active_stint_idx: &mut [Option<usize>; 2],
                             poss_open: &mut Option<u8>,
                             active_play_id: &mut [Option<String>; 2],
                             active_def_id: &mut [String; 2],
                             open_poss_play: &mut Option<(u8, String)>,
                             open_poss_def: &mut Option<(u8, String)>,
                             matchups: &mut BTreeMap<(u8, u8), MatchupPairStats>,
                             latest_def_guards_off: &BTreeMap<u8, u8>,
                             latest_off_guarded_by: &BTreeMap<u8, u8>| {
        boxs.apply_event(ev);
        match &ev.payload {
            GameRecordEventPayload::PeriodStarted { .. } => {
                *poss_open = None;
                *open_poss_play = None;
                *open_poss_def = None;
            }
            GameRecordEventPayload::PossessionEstablished {
                side,
                trigger_kind,
                ..
            } => {
                if *side < 2 {
                    let s = *side as usize;
                    let def_s = 1 - s;
                    // Capture active play / defense for this possession from current tick's debug frame
                    if full_access {
                        if let Some(df) = dbg_ctx {
                            if let Some(pid) = resolve_play_id(init_spec, df.active_play_idx) {
                                active_play_id[s] = Some(pid);
                            }
                            if let Some(gid) = resolve_gameplan_id(init_spec, df.active_def_rule_idx) {
                                active_def_id[def_s] = gid;
                            }
                        }
                    }
                    if *trigger_kind != 4 && *poss_open != Some(*side) {
                        *poss_open = Some(*side);
                        discrete_off_poss[s] += 1;
                        if let Some(si) = active_stint_idx[s] {
                            stints[si].off_poss = stints[si].off_poss.saturating_add(1);
                        }
                        if let Some(di) = active_stint_idx[def_s] {
                            stints[di].def_poss = stints[di].def_poss.saturating_add(1);
                        }
                        if full_access {
                            if has_debug {
                                if let Some(ref pid) = active_play_id[s] {
                                    let pk = (*side, pid.clone());
                                    let card = play_cards.entry(pk.clone()).or_insert_with(|| {
                                        PlaybookReportCard {
                                            side: *side,
                                            play_id: pid.clone(),
                                            ..PlaybookReportCard::default()
                                        }
                                    });
                                    card.possessions = card.possessions.saturating_add(1);
                                    *open_poss_play = Some(pk);
                                } else {
                                    *open_poss_play = None;
                                }
                            }
                            let dk = (def_s as u8, active_def_id[def_s].clone());
                            let dcard = defense_cards.entry(dk.clone()).or_insert_with(|| {
                                DefenseCoverageCard {
                                    side: def_s as u8,
                                    gameplan_id: active_def_id[def_s].clone(),
                                    ..DefenseCoverageCard::default()
                                }
                            });
                            dcard.possessions_guarded = dcard.possessions_guarded.saturating_add(1);
                            *open_poss_def = Some(dk);
                        }
                    }
                }
            }
            GameRecordEventPayload::ShotReleased {
                shot_id,
                shot_value,
                defender_slot,
                ..
            } => {
                pending_shots.insert(
                    *shot_id,
                    PendingShotMeta {
                        is_three: *shot_value == 3,
                        defender_slot: *defender_slot,
                        release_tick: ev.tick,
                    },
                );
            }
            GameRecordEventPayload::ShotResolved {
                shot_id,
                shooter_roster_idx,
                points_awarded,
                is_blocked,
                blocker_roster_idx,
                foul_on_shot,
                ..
            } => {
                let s_idx = *shooter_roster_idx as usize;
                let shooter_team_opt = if s_idx < 15 {
                    Some(0usize)
                } else if s_idx < 30 {
                    Some(1usize)
                } else {
                    None
                };
                let meta = pending_shots.remove(shot_id);
                let pts = *points_awarded as i32;
                let made = pts > 0;
                let count_fga = made || *foul_on_shot == 0;
                let is_three = meta.map(|m| m.is_three).unwrap_or(pts == 3);

                let mut rec_play_key = None;
                let mut rec_def_key = None;
                let mut rec_matchup_key = None;

                if let Some(st) = shooter_team_opt {
                    let def_team = 1 - st;
                    if pts != 0 {
                        if let Some(si) = active_stint_idx[st] {
                            stints[si].pts_for += pts;
                        }
                        if let Some(di) = active_stint_idx[def_team] {
                            stints[di].pts_against += pts;
                        }
                    }

                    if full_access {
                        if has_debug {
                            let pk_opt = open_poss_play
                                .as_ref()
                                .filter(|(ps, _)| *ps as usize == st)
                                .cloned()
                                .or_else(|| {
                                    active_play_id[st].as_ref().map(|p| (st as u8, p.clone()))
                                });
                            if let Some(pk) = pk_opt {
                                let card = play_cards.entry(pk.clone()).or_insert_with(|| {
                                    PlaybookReportCard {
                                        side: pk.0,
                                        play_id: pk.1.clone(),
                                        ..PlaybookReportCard::default()
                                    }
                                });
                                if count_fga {
                                    card.fga = card.fga.saturating_add(1);
                                }
                                if made {
                                    card.fgm = card.fgm.saturating_add(1);
                                }
                                card.points = card.points.saturating_add(*points_awarded as u16);
                                rec_play_key = Some(pk);
                            }
                        }

                        let dk = open_poss_def
                            .as_ref()
                            .filter(|(ds, _)| *ds as usize == def_team)
                            .cloned()
                            .unwrap_or_else(|| (def_team as u8, active_def_id[def_team].clone()));
                        let dcard = defense_cards.entry(dk.clone()).or_insert_with(|| {
                            DefenseCoverageCard {
                                side: dk.0,
                                gameplan_id: dk.1.clone(),
                                ..DefenseCoverageCard::default()
                            }
                        });
                        dcard.points_allowed =
                            dcard.points_allowed.saturating_add(*points_awarded as u16);
                        rec_def_key = Some(dk);

                        // Matchup defender attribution
                        let mut def_r_opt: Option<u8> = None;
                        if let Some(m) = meta {
                            if (m.defender_slot as usize) < 10 {
                                if let Some(rel_f) = find_frame_at_tick(public_frames, m.release_tick) {
                                    let cand = rel_f.players[m.defender_slot as usize].roster_idx;
                                    if cand != ROSTER_IDX_NONE && (cand as usize) / 15 == def_team {
                                        def_r_opt = Some(cand);
                                    }
                                }
                            }
                        }
                        if def_r_opt.is_none()
                            && *is_blocked != 0
                            && (*blocker_roster_idx as usize) < 30
                            && (*blocker_roster_idx as usize) / 15 == def_team
                        {
                            def_r_opt = Some(*blocker_roster_idx);
                        }
                        if def_r_opt.is_none() {
                            def_r_opt = latest_off_guarded_by.get(shooter_roster_idx).copied();
                        }
                        if def_r_opt.is_none() {
                            if let Some(fc) = frame_ctx {
                                def_r_opt = nearest_defender_for_off_roster(
                                    fc,
                                    *shooter_roster_idx,
                                    def_team,
                                );
                            }
                        }
                        if let Some(def_r) = def_r_opt {
                            let mk = (def_r, *shooter_roster_idx);
                            let mp = matchups.entry(mk).or_insert_with(|| MatchupPairStats {
                                def_roster_idx: def_r,
                                off_roster_idx: *shooter_roster_idx,
                                ..MatchupPairStats::default()
                            });
                            mp.opp_pts = mp.opp_pts.saturating_add(*points_awarded as u16);
                            if count_fga {
                                mp.opp_fga = mp.opp_fga.saturating_add(1);
                                if is_three {
                                    mp.opp_3pa = mp.opp_3pa.saturating_add(1);
                                }
                            }
                            if made {
                                mp.opp_fgm = mp.opp_fgm.saturating_add(1);
                                if is_three {
                                    mp.opp_3pm = mp.opp_3pm.saturating_add(1);
                                }
                            }
                            rec_matchup_key = Some(mk);
                        }
                    }

                    shot_attributions.insert(
                        ev.event_seq,
                        ShotAttribution {
                            scorer_team: st as u8,
                            home_stint_idx: active_stint_idx[0],
                            away_stint_idx: active_stint_idx[1],
                            play_key: rec_play_key,
                            def_key: rec_def_key,
                            matchup_key: rec_matchup_key,
                        },
                    );

                    if made && *foul_on_shot == 0 {
                        *poss_open = None;
                        *open_poss_play = None;
                        *open_poss_def = None;
                    }
                }
            }
            GameRecordEventPayload::ScoringEventAmended {
                original_shot_event_seq,
                pts_delta,
                fga_delta,
                fgm_delta,
                tpa_delta,
                tpm_delta,
                ..
            } => {
                if let Some(attr) = shot_attributions.get(original_shot_event_seq).cloned() {
                    let st = attr.scorer_team as usize;
                    let def_t = 1 - st;
                    let pd = *pts_delta as i32;
                    if pd != 0 {
                        if let Some(hi) = attr.home_stint_idx {
                            if st == 0 {
                                stints[hi].pts_for += pd;
                            } else {
                                stints[hi].pts_against += pd;
                            }
                        }
                        if let Some(ai) = attr.away_stint_idx {
                            if st == 1 {
                                stints[ai].pts_for += pd;
                            } else {
                                stints[ai].pts_against += pd;
                            }
                        }
                    }
                    if full_access {
                        if let Some(pk) = attr.play_key {
                            if let Some(card) = play_cards.get_mut(&pk) {
                                card.points = (card.points as i32 + pd).max(0) as u16;
                                card.fga = (card.fga as i32 + *fga_delta as i32).max(0) as u16;
                                card.fgm = (card.fgm as i32 + *fgm_delta as i32).max(0) as u16;
                            }
                        }
                        if let Some(dk) = attr.def_key {
                            if let Some(dcard) = defense_cards.get_mut(&dk) {
                                dcard.points_allowed =
                                    (dcard.points_allowed as i32 + pd).max(0) as u16;
                            }
                        }
                        if let Some(mk) = attr.matchup_key {
                            if let Some(mp) = matchups.get_mut(&mk) {
                                mp.opp_pts = (mp.opp_pts as i32 + pd).max(0) as u16;
                                mp.opp_fga =
                                    (mp.opp_fga as i32 + *fga_delta as i32).max(0) as u16;
                                mp.opp_fgm =
                                    (mp.opp_fgm as i32 + *fgm_delta as i32).max(0) as u16;
                                mp.opp_3pa =
                                    (mp.opp_3pa as i32 + *tpa_delta as i32).max(0) as u16;
                                mp.opp_3pm =
                                    (mp.opp_3pm as i32 + *tpm_delta as i32).max(0) as u16;
                            }
                        }
                    }
                    let _ = def_t;
                }
            }
            GameRecordEventPayload::FreeThrowResolved {
                team_side,
                made,
                shot_number,
                total_shots,
                is_technical_or_flagrant,
                post_ft_outcome_tag,
                ft_flags,
                ..
            } => {
                if *team_side < 2 {
                    let st = *team_side as usize;
                    let def_team = 1 - st;
                    if (*ft_flags & 0x01) != 0 && *made != 0 {
                        if let Some(si) = active_stint_idx[st] {
                            stints[si].pts_for += 1;
                        }
                        if let Some(di) = active_stint_idx[def_team] {
                            stints[di].pts_against += 1;
                        }
                        if full_access {
                            if has_debug {
                                if let Some(pk) = open_poss_play
                                    .as_ref()
                                    .filter(|(ps, _)| *ps as usize == st)
                                    .cloned()
                                    .or_else(|| {
                                        active_play_id[st].as_ref().map(|p| (*team_side, p.clone()))
                                    })
                                {
                                    let card = play_cards.entry(pk.clone()).or_insert_with(|| {
                                        PlaybookReportCard {
                                            side: pk.0,
                                            play_id: pk.1.clone(),
                                            ..PlaybookReportCard::default()
                                        }
                                    });
                                    card.points = card.points.saturating_add(1);
                                }
                            }
                            let dk = open_poss_def
                                .as_ref()
                                .filter(|(ds, _)| *ds as usize == def_team)
                                .cloned()
                                .unwrap_or_else(|| (def_team as u8, active_def_id[def_team].clone()));
                            let dcard = defense_cards.entry(dk.clone()).or_insert_with(|| {
                                DefenseCoverageCard {
                                    side: dk.0,
                                    gameplan_id: dk.1.clone(),
                                    ..DefenseCoverageCard::default()
                                }
                            });
                            dcard.points_allowed = dcard.points_allowed.saturating_add(1);
                        }
                    }
                    if *shot_number == *total_shots
                        && *is_technical_or_flagrant == 0
                        && (*made != 0 || *post_ft_outcome_tag == 2)
                    {
                        *poss_open = None;
                        *open_poss_play = None;
                        *open_poss_def = None;
                    }
                }
            }
            GameRecordEventPayload::ReboundSecured {
                is_offensive,
                rebound_team,
                ..
            } => {
                if *is_offensive == 0 && *rebound_team < 2 && *poss_open == Some(1 - *rebound_team) {
                    *poss_open = None;
                    *open_poss_play = None;
                    *open_poss_def = None;
                }
            }
            GameRecordEventPayload::TurnoverCommitted {
                offender_roster_idx,
                team_side,
                stealer_roster_idx,
                is_live_steal,
                ..
            } => {
                if *team_side < 2 {
                    let st = *team_side as usize;
                    let def_team = 1 - st;
                    if full_access {
                        if has_debug {
                            if let Some(pk) = open_poss_play
                                .as_ref()
                                .filter(|(ps, _)| *ps as usize == st)
                                .cloned()
                                .or_else(|| {
                                    active_play_id[st].as_ref().map(|p| (*team_side, p.clone()))
                                })
                            {
                                let card = play_cards.entry(pk.clone()).or_insert_with(|| {
                                    PlaybookReportCard {
                                        side: pk.0,
                                        play_id: pk.1.clone(),
                                        ..PlaybookReportCard::default()
                                    }
                                });
                                card.turnovers = card.turnovers.saturating_add(1);
                            }
                        }
                        let dk = open_poss_def
                            .as_ref()
                            .filter(|(ds, _)| *ds as usize == def_team)
                            .cloned()
                            .unwrap_or_else(|| (def_team as u8, active_def_id[def_team].clone()));
                        let dcard = defense_cards.entry(dk.clone()).or_insert_with(|| {
                            DefenseCoverageCard {
                                side: dk.0,
                                gameplan_id: dk.1.clone(),
                                ..DefenseCoverageCard::default()
                            }
                        });
                        dcard.forced_turnovers = dcard.forced_turnovers.saturating_add(1);

                        if (*offender_roster_idx as usize) < 30 {
                            let def_r_opt = if *is_live_steal != 0
                                && (*stealer_roster_idx as usize) < 30
                                && (*stealer_roster_idx as usize) / 15 == def_team
                            {
                                Some(*stealer_roster_idx)
                            } else {
                                latest_off_guarded_by
                                    .get(offender_roster_idx)
                                    .copied()
                                    .or_else(|| {
                                        frame_ctx.and_then(|fc| {
                                            nearest_defender_for_off_roster(
                                                fc,
                                                *offender_roster_idx,
                                                def_team,
                                            )
                                        })
                                    })
                            };
                            if let Some(def_r) = def_r_opt {
                                let mk = (def_r, *offender_roster_idx);
                                let mp = matchups.entry(mk).or_insert_with(|| MatchupPairStats {
                                    def_roster_idx: def_r,
                                    off_roster_idx: *offender_roster_idx,
                                    ..MatchupPairStats::default()
                                });
                                mp.forced_tov = mp.forced_tov.saturating_add(1);
                            }
                        }
                    }
                }
                *poss_open = None;
                *open_poss_play = None;
                *open_poss_def = None;
            }
            GameRecordEventPayload::ViolationTriggered {
                team_side,
                rule_code,
                ..
            } => {
                if *rule_code == RuleCode::VIO_DEF_3S as u16 && *team_side < 2 {
                    let s = *team_side as usize;
                    def_3s_count[s] = def_3s_count[s].saturating_add(1);
                    if let Some(si) = active_stint_idx[s] {
                        stints[si].def_3s_count = stints[si].def_3s_count.saturating_add(1);
                    }
                    if full_access {
                        let dk = open_poss_def
                            .as_ref()
                            .filter(|(ds, _)| *ds == *team_side)
                            .cloned()
                            .unwrap_or_else(|| (*team_side, active_def_id[s].clone()));
                        let dcard = defense_cards.entry(dk.clone()).or_insert_with(|| {
                            DefenseCoverageCard {
                                side: dk.0,
                                gameplan_id: dk.1.clone(),
                                ..DefenseCoverageCard::default()
                            }
                        });
                        dcard.def_3s_violations = dcard.def_3s_violations.saturating_add(1);
                    }
                }
            }
            GameRecordEventPayload::FoulAssessedRecord {
                offending_side,
                offender_roster_idx,
                ..
            } => {
                if full_access
                    && *offending_side < 2
                    && (*offender_roster_idx as usize) < 30
                {
                    let def_side = *offending_side as usize;
                    let off_side = 1 - def_side;
                    let off_r_opt = latest_def_guards_off
                        .get(offender_roster_idx)
                        .copied()
                        .or_else(|| {
                            frame_ctx.and_then(|fc| {
                                let def_slot = fc
                                    .players
                                    .iter()
                                    .position(|p| p.roster_idx == *offender_roster_idx)?;
                                nearest_offender_for_def_slot(fc, def_slot, off_side)
                            })
                        });
                    if let Some(off_r) = off_r_opt {
                        let mk = (*offender_roster_idx, off_r);
                        let mp = matchups.entry(mk).or_insert_with(|| MatchupPairStats {
                            def_roster_idx: *offender_roster_idx,
                            off_roster_idx: off_r,
                            ..MatchupPairStats::default()
                        });
                        mp.fouls_committed = mp.fouls_committed.saturating_add(1);
                    }
                }
            }
            GameRecordEventPayload::ClockAdjusted {
                delta_game_clock_ds_signed,
                affected_slot_mask,
                ..
            } => {
                let d = *delta_game_clock_ds_signed as i64;
                if (*affected_slot_mask & 0x001F) != 0 {
                    if let Some(si) = active_stint_idx[0] {
                        stints[si].live_ds = (stints[si].live_ds - d).max(0);
                    }
                }
                if (*affected_slot_mask & 0x03E0) != 0 {
                    if let Some(si) = active_stint_idx[1] {
                        stints[si].live_ds = (stints[si].live_ds - d).max(0);
                    }
                }
            }
            _ => {}
        }
    };

    for (fi, f) in public_frames.iter().enumerate() {
        boxs.on_frame(f);
        let dbg = find_debug_frame_at_tick(effective_debug, fi, f.tick);
        let clock_ticked = (f.ball_flags & 0x20) != 0;

        for side in 0..2 {
            let lineup = extract_sorted_lineup(f, side);
            let need_new = match active_stint_idx[side] {
                None => true,
                Some(si) => stints[si].lineup != lineup,
            };
            if need_new {
                stints.push(StintAccum {
                    side: side as u8,
                    lineup,
                    start_tick: f.tick,
                    end_tick: f.tick,
                    live_ds: 0,
                    off_poss: 0,
                    def_poss: 0,
                    pts_for: 0,
                    pts_against: 0,
                    pair_dists_m: Vec::new(),
                    crowded_frames: 0,
                    def_3s_count: 0,
                });
                active_stint_idx[side] = Some(stints.len() - 1);
            }
            let si = active_stint_idx[side].unwrap();
            stints[si].end_tick = f.tick;
            if clock_ticked {
                stints[si].live_ds += 1;
            }
        }

        if f.game_phase == live_phase && f.possession_side < 2 {
            let off_s = f.possession_side as usize;
            let def_s = 1 - off_s;
            if let Some(min_d) = min_offensive_pair_distance_m(f, off_s) {
                off_live_frames[off_s] += 1;
                let crowded = min_d < CROWDING_THRESHOLD_M;
                if crowded {
                    off_crowded_frames[off_s] += 1;
                }
                if let Some(si) = active_stint_idx[off_s] {
                    stints[si].pair_dists_m.push(min_d);
                    if crowded {
                        stints[si].crowded_frames += 1;
                    }
                }
            }
            if full_access {
                if let Some(df) = dbg {
                    if let Some(pid) = resolve_play_id(init_spec, df.active_play_idx) {
                        active_play_id[off_s] = Some(pid.clone());
                        if poss_open == Some(off_s as u8) && open_poss_play.is_none() {
                            open_poss_play = Some((off_s as u8, pid));
                        }
                    }
                    if let Some(gid) = resolve_gameplan_id(init_spec, df.active_def_rule_idx) {
                        active_def_id[def_s] = gid.clone();
                        if poss_open == Some(off_s as u8) && open_poss_def.is_none() {
                            open_poss_def = Some((def_s as u8, gid));
                        }
                    }
                }
                // Update matchup guarding mappings and accumulate guarded_live_ds
                for dp in 0..5 {
                    let def_slot = def_s * 5 + dp;
                    let def_r = f.players[def_slot].roster_idx;
                    if def_r == ROSTER_IDX_NONE {
                        continue;
                    }
                    if let Some(off_r) = resolve_matchup_on_frame(f, dbg, def_slot, off_s) {
                        latest_def_guards_off.insert(def_r, off_r);
                        latest_off_guarded_by.insert(off_r, def_r);
                        if clock_ticked {
                            let mk = (def_r, off_r);
                            let mp = matchups.entry(mk).or_insert_with(|| MatchupPairStats {
                                def_roster_idx: def_r,
                                off_roster_idx: off_r,
                                ..MatchupPairStats::default()
                            });
                            mp.guarded_live_ds = mp.guarded_live_ds.saturating_add(1);
                        }
                    }
                }
            }
        }

        while ev_idx < events.len() && events[ev_idx].tick <= f.tick {
            process_event(
                &events[ev_idx],
                Some(f),
                dbg,
                &mut boxs,
                &mut stints,
                &mut active_stint_idx,
                &mut poss_open,
                &mut active_play_id,
                &mut active_def_id,
                &mut open_poss_play,
                &mut open_poss_def,
                &mut matchups,
                &latest_def_guards_off,
                &latest_off_guarded_by,
            );
            ev_idx += 1;
        }
    }

    while ev_idx < events.len() {
        let last_f = public_frames.last();
        process_event(
            &events[ev_idx],
            last_f,
            None,
            &mut boxs,
            &mut stints,
            &mut active_stint_idx,
            &mut poss_open,
            &mut active_play_id,
            &mut active_def_id,
            &mut open_poss_play,
            &mut open_poss_def,
            &mut matchups,
            &latest_def_guards_off,
            &latest_off_guarded_by,
        );
        ev_idx += 1;
    }

    let mut home_efficiency = build_efficiency_snapshot(
        0,
        &boxs,
        discrete_off_poss,
        off_crowded_frames,
        off_live_frames,
        def_3s_count,
    );
    let mut away_efficiency = build_efficiency_snapshot(
        1,
        &boxs,
        discrete_off_poss,
        off_crowded_frames,
        off_live_frames,
        def_3s_count,
    );

    if let Some(own) = own_side_filter {
        if own == 0 {
            away_efficiency = TeamEfficiencySnapshot::default();
        } else {
            home_efficiency = TeamEfficiencySnapshot::default();
        }
    }

    let mut out_stints: Vec<LineupStintRecord> = Vec::with_capacity(stints.len());
    for st in stints {
        if let Some(own) = own_side_filter {
            if st.side != own {
                continue;
            }
        }
        let pts_for = st.pts_for.max(0) as u16;
        let pts_against = st.pts_against.max(0) as u16;
        let off_p = if st.off_poss > 0 {
            st.off_poss as f64
        } else if pts_for > 0 {
            1.0
        } else {
            0.0
        };
        let def_p = if st.def_poss > 0 {
            st.def_poss as f64
        } else if pts_against > 0 {
            1.0
        } else {
            0.0
        };
        let ortg = if off_p > 0.0 {
            100.0 * (pts_for as f64) / off_p
        } else {
            0.0
        };
        let drtg = if def_p > 0.0 {
            100.0 * (pts_against as f64) / def_p
        } else {
            0.0
        };
        let net_rtg = ortg - drtg;
        let median_min_off_pair_m = median_f64(&st.pair_dists_m);
        let crowding_ratio = if st.pair_dists_m.is_empty() {
            0.0
        } else {
            (st.crowded_frames as f64) / (st.pair_dists_m.len() as f64)
        };
        let stint_id = (out_stints.len() + 1) as u32;
        out_stints.push(LineupStintRecord {
            stint_id,
            side: st.side,
            lineup_roster_indices: st.lineup,
            start_tick: st.start_tick,
            end_tick: st.end_tick,
            live_ds: st.live_ds.max(0) as u32,
            off_poss: st.off_poss,
            def_poss: st.def_poss,
            pts_for,
            pts_against,
            net_rtg,
            median_min_off_pair_m,
            crowding_ratio,
            def_3s_count: st.def_3s_count,
        });
    }

    if !full_access {
        return MatchAnalyticsSummary {
            home_efficiency,
            away_efficiency,
            stints: out_stints,
            matchups: Vec::new(),
            play_cards: Vec::new(),
            defense_cards: Vec::new(),
            debrief_findings: Vec::new(),
        };
    }

    let mut out_matchups: Vec<MatchupPairStats> = matchups.into_values().collect();
    out_matchups.sort_by(|a, b| {
        b.guarded_live_ds
            .cmp(&a.guarded_live_ds)
            .then_with(|| a.def_roster_idx.cmp(&b.def_roster_idx))
            .then_with(|| a.off_roster_idx.cmp(&b.off_roster_idx))
    });

    let mut out_play_cards: Vec<PlaybookReportCard> = play_cards
        .into_values()
        .map(|mut c| {
            let denom = (c.possessions as f64).max(if c.points > 0 || c.fga > 0 || c.turnovers > 0 {
                1.0
            } else {
                0.0
            });
            c.ppp = if denom > 0.0 {
                (c.points as f64) / denom
            } else {
                0.0
            };
            c
        })
        .collect();
    out_play_cards.sort_by(|a, b| {
        a.side
            .cmp(&b.side)
            .then_with(|| b.possessions.cmp(&a.possessions))
            .then_with(|| a.play_id.cmp(&b.play_id))
    });

    let mut out_defense_cards: Vec<DefenseCoverageCard> = defense_cards
        .into_values()
        .map(|mut c| {
            let denom = (c.possessions_guarded as f64).max(
                if c.points_allowed > 0 || c.forced_turnovers > 0 || c.def_3s_violations > 0 {
                    1.0
                } else {
                    0.0
                },
            );
            c.ppp_allowed = if denom > 0.0 {
                (c.points_allowed as f64) / denom
            } else {
                0.0
            };
            c
        })
        .collect();
    out_defense_cards.sort_by(|a, b| {
        a.side
            .cmp(&b.side)
            .then_with(|| b.possessions_guarded.cmp(&a.possessions_guarded))
            .then_with(|| a.gameplan_id.cmp(&b.gameplan_id))
    });

    let debrief_findings = build_debrief_findings(
        &home_efficiency,
        &away_efficiency,
        &out_stints,
        &out_matchups,
        &out_play_cards,
        &out_defense_cards,
    );

    MatchAnalyticsSummary {
        home_efficiency,
        away_efficiency,
        stints: out_stints,
        matchups: out_matchups,
        play_cards: out_play_cards,
        defense_cards: out_defense_cards,
        debrief_findings,
    }
}

fn build_debrief_findings(
    home: &TeamEfficiencySnapshot,
    away: &TeamEfficiencySnapshot,
    stints: &[LineupStintRecord],
    matchups: &[MatchupPairStats],
    play_cards: &[PlaybookReportCard],
    defense_cards: &[DefenseCoverageCard],
) -> Vec<String> {
    let mut findings = Vec::new();
    findings.push(format!(
        "四要素概览：主队 {} 分 (ORtg {:.1}, eFG% {:.1}%, TOV% {:.1}%, ORB% {:.1}%) vs 客队 {} 分 (ORtg {:.1}, eFG% {:.1}%, TOV% {:.1}%, ORB% {:.1}%)",
        home.score,
        home.ortg,
        home.efg_pct * 100.0,
        home.tov_pct * 100.0,
        home.orb_pct * 100.0,
        away.score,
        away.ortg,
        away.efg_pct * 100.0,
        away.tov_pct * 100.0,
        away.orb_pct * 100.0,
    ));

    for (side, snap) in [(0u8, home), (1u8, away)] {
        let sname = side_name_zh(side);
        if snap.crowding_ratio > 0.15 {
            findings.push(format!(
                "空间拥堵诊断：{sname}进攻回合队友 <1.8m 拥堵率达 {:.1}%，建议优化落位间距或增加弱侧拉开",
                snap.crowding_ratio * 100.0
            ));
        }
        if snap.def_3s_count > 0 {
            findings.push(format!(
                "防守三秒统计：{sname}累计触发 {} 次防守3秒违例 (VIO_DEF_3S)",
                snap.def_3s_count
            ));
        }
        if snap.tov_pct > 0.16 {
            findings.push(format!(
                "失误警示：{sname}失误率偏高 ({:.1}%)，需降低高风险传球或调整进攻节奏",
                snap.tov_pct * 100.0
            ));
        }
    }

    if let Some(best_stint) = stints
        .iter()
        .filter(|s| s.off_poss >= 3)
        .max_by(|a, b| a.net_rtg.total_cmp(&b.net_rtg))
    {
        findings.push(format!(
            "最优五人组：{} {:?} 出战 {:.1} 秒 (OffPoss={}, NetRtg {:+.1}, 拥堵率 {:.1}%)",
            side_name_zh(best_stint.side),
            best_stint.lineup_roster_indices,
            best_stint.live_ds as f64 / 10.0,
            best_stint.off_poss,
            best_stint.net_rtg,
            best_stint.crowding_ratio * 100.0,
        ));
    }

    for side in [0u8, 1u8] {
        if let Some(best_play) = play_cards
            .iter()
            .filter(|c| c.side == side && c.possessions >= 3)
            .max_by(|a, b| a.ppp.total_cmp(&b.ppp))
        {
            findings.push(format!(
                "最佳进攻战术：{} `{}` ({} 回合, {} 分, PPP {:.2})",
                side_name_zh(side),
                best_play.play_id,
                best_play.possessions,
                best_play.points,
                best_play.ppp,
            ));
        }
        if let Some(best_def) = defense_cards
            .iter()
            .filter(|c| c.side == side && c.possessions_guarded >= 3)
            .min_by(|a, b| a.ppp_allowed.total_cmp(&b.ppp_allowed))
        {
            findings.push(format!(
                "最佳防守策略：{} `{}` (守 {} 回合, 失 {} 分, PPP {:.2}, 造失误 {})",
                side_name_zh(side),
                best_def.gameplan_id,
                best_def.possessions_guarded,
                best_def.points_allowed,
                best_def.ppp_allowed,
                best_def.forced_turnovers,
            ));
        }
    }

    if let Some(top_vuln) = matchups
        .iter()
        .filter(|m| m.opp_fga >= 3)
        .max_by_key(|m| (m.opp_pts, m.opp_fga))
    {
        findings.push(format!(
            "关键对位样本：防守者 #r{} 对位进攻者 #r{} ({:.1}s, 对手 {} 分 {}/{} FG, 造失误 {})",
            top_vuln.def_roster_idx,
            top_vuln.off_roster_idx,
            top_vuln.guarded_live_ds as f64 / 10.0,
            top_vuln.opp_pts,
            top_vuln.opp_fgm,
            top_vuln.opp_fga,
            top_vuln.forced_tov,
        ));
    }

    findings
}

/// Compute counterfactual causal efficiency deltas from `side`'s perspective (`counterfactual - baseline`).
pub fn compute_whatif_causal_delta(
    side: CommandSide,
    baseline: &MatchAnalyticsSummary,
    counterfactual: &MatchAnalyticsSummary,
) -> WhatIfCausalDelta {
    let (base_self, base_opp, cf_self, cf_opp) = match side {
        CommandSide::Away => (
            &baseline.away_efficiency,
            &baseline.home_efficiency,
            &counterfactual.away_efficiency,
            &counterfactual.home_efficiency,
        ),
        CommandSide::Home | CommandSide::System => (
            &baseline.home_efficiency,
            &baseline.away_efficiency,
            &counterfactual.home_efficiency,
            &counterfactual.away_efficiency,
        ),
    };
    let base_margin = (base_self.score as f64) - (base_opp.score as f64);
    let cf_margin = (cf_self.score as f64) - (cf_opp.score as f64);
    WhatIfCausalDelta {
        delta_margin_from_side: cf_margin - base_margin,
        delta_ortg: cf_self.ortg - base_self.ortg,
        delta_drtg: cf_self.drtg - base_self.drtg,
        delta_efg_pct: cf_self.efg_pct - base_self.efg_pct,
        delta_tpa_share: cf_self.tpa_share - base_self.tpa_share,
        delta_tov_pct: cf_self.tov_pct - base_self.tov_pct,
        delta_orb_pct: cf_self.orb_pct - base_self.orb_pct,
        delta_crowding_ratio: cf_self.crowding_ratio - base_self.crowding_ratio,
    }
}
