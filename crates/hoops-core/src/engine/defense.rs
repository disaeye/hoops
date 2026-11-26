// 防守模块：球队防守计划运行态、单拍防守协调与每名防守人的运动学目标。
// `TeamDefensePlan` 的字段只在本模块可见，进攻侧代码在类型层面无法读取对方私有防守计划；
// 对外只开放场上可观测的对位、只读审计快照与教练指令入口。

use serde::{Deserialize, Serialize};

use crate::canonical_le_bytes::{CanonicalLeBytes, CanonicalReader, DecodeError};
use crate::tactics::{
    assign_initial_defensive_matchups, compute_help_triangle, is_protected_defender,
    resolve_pnr_coverage, update_adaptive_coverage, PnrEvalContext,
};
use crate::wire_schema_v1::*;

use super::SimulationEngine;

/// 球队防守计划运行态：按声明顺序进入 `GameState` 的规范字节序列化
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamDefensePlan {
    gameplan_id: GameplanId,
    protect_targets: InlinePlayerList<5>,
    pnr_rule_override: Option<PnrRuleId>,
    manual_matchup_lock: [u8; 2],
    matchups: [u8; 5],
    adaptive_coverage: AdaptiveCoverageState,
}

/// 防守计划只读审计快照，供归因投影器与测试核对
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DefensePlanInspection {
    pub gameplan_id: GameplanId,
    pub protect_targets_len: u8,
    pub protect_targets: [u8; 5],
    pub pnr_rule_override: Option<PnrRuleId>,
    pub manual_matchup_lock: [u8; 2],
    pub matchups: [u8; 5],
    pub active_coverage: PnrCoverageKind,
    pub escalated: bool,
}

impl TeamDefensePlan {
    pub fn new(gameplan_id: GameplanId) -> Self {
        Self {
            gameplan_id,
            protect_targets: InlinePlayerList::default(),
            pnr_rule_override: None,
            manual_matchup_lock: [ROSTER_IDX_NONE; 2],
            matchups: [0, 1, 2, 3, 4],
            adaptive_coverage: AdaptiveCoverageState::default(),
        }
    }

    /// 场上可观测的对位：下标为防守位置，值为被防守的进攻位置
    pub fn matchups(&self) -> [u8; 5] {
        self.matchups
    }

    pub fn inspect(&self) -> DefensePlanInspection {
        let protect_targets = self.protect_targets.as_slice();
        let mut items = [ROSTER_IDX_NONE; 5];
        items[..protect_targets.len()].copy_from_slice(protect_targets);
        DefensePlanInspection {
            gameplan_id: self.gameplan_id,
            protect_targets_len: protect_targets.len() as u8,
            protect_targets: items,
            pnr_rule_override: self.pnr_rule_override,
            manual_matchup_lock: self.manual_matchup_lock,
            matchups: self.matchups,
            active_coverage: self.adaptive_coverage.active_coverage,
            escalated: self.adaptive_coverage.escalated,
        }
    }

    /// 被锁定对位的任一球员下场即解除手动对位锁
    pub(crate) fn release_matchup_lock_for_player(&mut self, out_r: u8) {
        let [locked_defender, locked_attacker] = self.manual_matchup_lock;
        if locked_defender == out_r || locked_attacker == out_r {
            self.manual_matchup_lock = [ROSTER_IDX_NONE; 2];
        }
    }
}

impl CanonicalLeBytes for TeamDefensePlan {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.gameplan_id.write_canonical_le(out);
        self.protect_targets.write_canonical_le(out);
        self.pnr_rule_override.write_canonical_le(out);
        self.manual_matchup_lock.write_canonical_le(out);
        self.matchups.write_canonical_le(out);
        self.adaptive_coverage.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            gameplan_id: GameplanId::read_canonical_le(reader)?,
            protect_targets: InlinePlayerList::<5>::read_canonical_le(reader)?,
            pnr_rule_override: <Option<PnrRuleId>>::read_canonical_le(reader)?,
            manual_matchup_lock: <[u8; 2]>::read_canonical_le(reader)?,
            matchups: <[u8; 5]>::read_canonical_le(reader)?,
            adaptive_coverage: AdaptiveCoverageState::read_canonical_le(reader)?,
        })
    }
}

/// 本场全部可选防守计划：只按 id 查找，活跃计划的 id 封装在 `TeamDefensePlan` 内
#[derive(Debug, Clone)]
pub struct DefensePlaybooks(Vec<CompiledDefenseGameplan>);

impl DefensePlaybooks {
    pub fn new(plans: Vec<CompiledDefenseGameplan>) -> Self {
        Self(plans)
    }

    fn index_of(&self, gameplan_id: &GameplanId) -> Option<usize> {
        self.0.iter().position(|plan| plan.gameplan_id == *gameplan_id)
    }
}

/// 单拍防守协调结果：字段只在防守模块内使用，对外只暴露场上可观测量
pub(super) struct DefenseCoordination {
    plan_index: Option<usize>,
    base_scheme: u8,
    active_pnr_cov: PnrCoverageKind,
    active_screener_pos: Option<usize>,
    active_screen_xy: Option<(f64, f64)>,
    low_man_def_pos: Option<usize>,
    on_ball_def_xy: (f64, f64),
}

impl DefenseCoordination {
    /// 防守方是否按盯人落位（联防可由站位直接观测）
    pub(super) fn is_man_to_man(&self) -> bool {
        self.base_scheme != 3
    }
}

impl SimulationEngine {
    fn assign_matchup_pair(matchups: &mut [u8; 5], defender: usize, attacker: usize) {
        if let Some(previous) = matchups.iter().position(|&slot| usize::from(slot) == attacker) {
            matchups.swap(previous, defender);
        }
    }

    pub(super) fn assign_team_matchups_for_defense(&mut self, def_idx: usize) {
        let off_idx = 1 - def_idx;
        let gp_opt = self
            .defenses
            .index_of(&self.state.defense_plans[def_idx].gameplan_id)
            .map(|i| &self.defenses.0[i]);
        let mut off_snaps: [Option<&CompiledPlayerSnapshot>; 5] = [None; 5];
        let mut def_snaps: [Option<&CompiledPlayerSnapshot>; 5] = [None; 5];
        for i in 0..5 {
            let or = self.state.team_states[off_idx].active_lineup_slots[i];
            if or != ROSTER_IDX_NONE {
                off_snaps[i] = self.teams[off_idx].players.get((or as usize).saturating_sub(off_idx * 15));
            }
            let dr = self.state.team_states[def_idx].active_lineup_slots[i];
            if dr != ROSTER_IDX_NONE {
                def_snaps[i] = self.teams[def_idx].players.get((dr as usize).saturating_sub(def_idx * 15));
            }
        }
        let mut matchups = assign_initial_defensive_matchups(gp_opt, &off_snaps, &def_snaps);
        let defenders = self.state.team_states[def_idx].active_lineup_slots;
        let attackers = self.state.team_states[off_idx].active_lineup_slots;
        for &(defender_rank, attacker_rank) in &self.teams[def_idx].tactics.matchup_overrides {
            let d = defenders.iter().position(|&r| r < 30 && r % 15 == defender_rank);
            let o = attackers.iter().position(|&r| r < 30 && r % 15 == attacker_rank);
            if let (Some(d), Some(o)) = (d, o) {
                Self::assign_matchup_pair(&mut matchups, d, o);
            }
        }
        // 教练手动对位锁优先于球队静态配置，直至防守人或被防守人下场或教练 Clear
        let [locked_defender, locked_attacker] = self.state.defense_plans[def_idx].manual_matchup_lock;
        if locked_defender != ROSTER_IDX_NONE {
            let d = defenders.iter().position(|&r| r == locked_defender);
            let o = attackers.iter().position(|&r| r == locked_attacker);
            if let (Some(d), Some(o)) = (d, o) {
                Self::assign_matchup_pair(&mut matchups, d, o);
            } else {
                self.state.defense_plans[def_idx].manual_matchup_lock = [ROSTER_IDX_NONE; 2];
            }
        }
        self.state.defense_plans[def_idx].matchups = matchups;
    }

    fn reset_def_matchups(&mut self, def_idx: usize) {
        let plan = &self.state.defense_plans[def_idx];
        let should_reset = self
            .defenses
            .index_of(&plan.gameplan_id)
            .map(|i| &self.defenses.0[i])
            .is_some_and(|gp| {
                gp.base_scheme == 1
                    || gp.hide_on_weakest_shooter
                    || plan.adaptive_coverage.active_coverage == PnrCoverageKind::Switch
                    || gp.pnr_rules.iter().any(|r| r.coverage == PnrCoverageKind::Switch)
            });
        if should_reset {
            self.assign_team_matchups_for_defense(def_idx);
        }
    }

    pub(super) fn record_defense_possession_outcome(
        &mut self,
        def_idx: usize,
        off_idx: usize,
        points_allowed: u8,
    ) {
        let off_has_three_threat = (0..5).any(|p_pos| {
            let r = self.state.team_states[off_idx].active_lineup_slots[p_pos];
            if r == ROSTER_IDX_NONE {
                return false;
            }
            let rank = (r as usize).saturating_sub(off_idx * 15);
            self.teams[off_idx]
                .players
                .get(rank)
                .is_some_and(|pl| pl.attributes[AttrIdx::ThreePoint] >= 78)
        });
        let gameplan_id = self.state.defense_plans[def_idx].gameplan_id;
        if let Some(gp) = self
            .defenses
            .index_of(&gameplan_id)
            .map(|i| &self.defenses.0[i])
        {
            update_adaptive_coverage(
                &mut self.state.defense_plans[def_idx].adaptive_coverage,
                gp,
                points_allowed,
                off_has_three_threat,
            );
        }
        self.reset_def_matchups(def_idx);
    }

    /// 执行 `SetDefensiveGameplan` 教练指令的四个补丁字段
    pub(super) fn apply_defensive_gameplan_patch(
        &mut self,
        team_idx: usize,
        gameplan_id: PatchField<GameplanId>,
        protect_targets: PatchField<InlinePlayerList<5>>,
        pnr_rule_override: PatchField<PnrRuleId>,
        matchup_override: PatchField<(u8, u8)>,
    ) {
        let plan = &mut self.state.defense_plans[team_idx];
        if let PatchField::Set(id) = gameplan_id {
            plan.gameplan_id = id;
        }
        match protect_targets {
            PatchField::Clear => plan.protect_targets = InlinePlayerList::default(),
            PatchField::Set(l) => plan.protect_targets = l,
            PatchField::Keep => {}
        }
        match pnr_rule_override {
            PatchField::Clear => plan.pnr_rule_override = None,
            PatchField::Set(id) => plan.pnr_rule_override = Some(id),
            PatchField::Keep => {}
        }
        match matchup_override {
            PatchField::Set((defender, attacker)) => {
                let defenders = self.state.team_states[team_idx].active_lineup_slots;
                let attackers = self.state.team_states[1 - team_idx].active_lineup_slots;
                let d = defenders.iter().position(|&r| r == defender && r != ROSTER_IDX_NONE);
                let o = attackers.iter().position(|&r| r == attacker && r != ROSTER_IDX_NONE);
                if let (Some(d), Some(o)) = (d, o) {
                    Self::assign_matchup_pair(&mut plan.matchups, d, o);
                    plan.manual_matchup_lock = [defender, attacker];
                }
            }
            PatchField::Clear => {
                plan.manual_matchup_lock = [ROSTER_IDX_NONE; 2];
                self.assign_team_matchups_for_defense(team_idx);
            }
            PatchField::Keep => {}
        }
    }

    /// 审计用：当前生效的 PnR 掩护防守覆盖
    pub(super) fn audit_defensive_coverage(&self, defense: usize) -> PnrCoverageKind {
        let plan = &self.state.defense_plans[defense];
        let adaptive = &plan.adaptive_coverage;
        if adaptive.escalated {
            return adaptive.active_coverage;
        }
        let gameplan = self
            .defenses
            .index_of(&plan.gameplan_id)
            .map(|i| &self.defenses.0[i]);
        if let (Some(gameplan), Some(rule_id)) = (gameplan, plan.pnr_rule_override) {
            if let Some(rule) = gameplan.pnr_rules.iter().find(|rule| rule.rule_id == rule_id) {
                return rule.coverage;
            }
        }
        match gameplan.map(|gameplan| gameplan.base_scheme) {
            Some(1) => PnrCoverageKind::Switch,
            Some(2) => PnrCoverageKind::Blitz,
            _ => PnrCoverageKind::Drop,
        }
    }

    /// 单拍防守协调：解析当前 PnR 覆盖规则，执行一次性换防、Pre-Switch、Scram Switch 与 Peel Switch，
    /// 并确定弱侧协防人与持球人防守者的位置
    #[allow(clippy::too_many_arguments)]
    pub(super) fn coordinate_defense(
        &mut self,
        def_idx: usize,
        off_idx: usize,
        handler_pos_idx: usize,
        handler_r: u8,
        handler_xy: (f64, f64),
        handler_is_driving: bool,
        in_frontcourt: bool,
        active_ball_screen: Option<(usize, usize)>,
        active_screener_pos: Option<usize>,
        active_screen_target_pos: Option<usize>,
        active_screen_xy: Option<(f64, f64)>,
        off_xy: &[(f64, f64); 5],
        def_xy: &[(f64, f64); 5],
        def_active: &[bool; 5],
        attack_dir: f64,
    ) -> DefenseCoordination {
        let gp_idx_opt = self
            .defenses
            .index_of(&self.state.defense_plans[def_idx].gameplan_id);

        let gp_opt = gp_idx_opt.map(|i| &self.defenses.0[i]);
        let gp_base_scheme = gp_opt.map_or(0, |gp| gp.base_scheme);
        let (active_pnr_cov, local_rule_idx) = if gp_base_scheme == 3 {
            (PnrCoverageKind::Drop, 0xFFFF)
        } else if let Some(gp) = gp_opt {
            let h_def_pos = (0..5)
                .find(|&dp| self.state.defense_plans[def_idx].matchups[dp] as usize == handler_pos_idx)
                .unwrap_or(0);
            let scr_pos = active_screener_pos.unwrap_or(4);
            let s_def_pos = (0..5)
                .find(|&dp| self.state.defense_plans[def_idx].matchups[dp] as usize == scr_pos)
                .unwrap_or(4);
            let h_rank = if handler_r != ROSTER_IDX_NONE {
                (handler_r as usize).saturating_sub(off_idx * 15)
            } else {
                0
            };
            let sr = self.state.team_states[off_idx].active_lineup_slots[scr_pos];
            let s_rank = if sr != ROSTER_IDX_NONE {
                (sr as usize).saturating_sub(off_idx * 15)
            } else {
                h_rank
            };
            let hdr = self.state.team_states[def_idx].active_lineup_slots[h_def_pos];
            let sdr = self.state.team_states[def_idx].active_lineup_slots[s_def_pos];
            let hd_rank = if hdr != ROSTER_IDX_NONE {
                (hdr as usize).saturating_sub(def_idx * 15)
            } else {
                0
            };
            let sd_rank = if sdr != ROSTER_IDX_NONE {
                (sdr as usize).saturating_sub(def_idx * 15)
            } else {
                0
            };
            if let (Some(h_snap), Some(s_snap), Some(hd_snap), Some(sd_snap)) = (
                self.teams[off_idx].players.get(h_rank),
                self.teams[off_idx].players.get(s_rank),
                self.teams[def_idx].players.get(hd_rank),
                self.teams[def_idx].players.get(sd_rank),
            ) {
                let score_diff = (self.state.team_states[off_idx].score_on_court as i32)
                    - (self.state.team_states[def_idx].score_on_court as i32);
                let pctx = PnrEvalContext {
                    handler: h_snap,
                    screener: s_snap,
                    screener_def: sd_snap,
                    matchup_height_diff_cm: (s_snap.physical.height_cm as f64
                        - hd_snap.physical.height_cm as f64)
                        .abs(),
                    shot_clock_ds: self.state.shot_clock_ds.unwrap_or(240) as f64,
                    game_clock_ds: self.state.game_clock_ds as f64,
                    score_diff: score_diff as f64,
                    defender_fouls: if hdr != ROSTER_IDX_NONE {
                        self.state.players[hdr as usize].personal_fouls as f64
                    } else {
                        0.0
                    },
                };
                resolve_pnr_coverage(
                    gp,
                    self.state.defense_plans[def_idx].pnr_rule_override,
                    &self.state.defense_plans[def_idx].adaptive_coverage,
                    &pctx,
                )
            } else {
                (PnrCoverageKind::Drop, 0xFFFF)
            }
        } else {
            (PnrCoverageKind::Drop, 0xFFFF)
        };
        if let Some(plan_index) = gp_idx_opt.filter(|_| local_rule_idx != 0xFFFF) {
            let rule_index = pnr_rule_dict_offset(&self.defenses.0, plan_index) + usize::from(local_rule_idx);
            self.transient_def_rule_idx = u16::try_from(rule_index).unwrap_or(0xFFFF);
        }

        let protected_targets = self.state.defense_plans[def_idx].protect_targets;
        let is_protected = |roster_idx, snapshot: &CompiledPlayerSnapshot| {
            protected_targets.as_slice().contains(&roster_idx)
                || gp_opt.is_some_and(|plan| is_protected_defender(plan, snapshot))
        };
        // 教练手动对位锁：涉及被锁定防守人或被锁定进攻人的任何换防一律跳过
        let [locked_defender, locked_attacker] = self.state.defense_plans[def_idx].manual_matchup_lock;
        let locked_def_pos = (locked_defender != ROSTER_IDX_NONE)
            .then(|| self.state.team_states[def_idx].active_lineup_slots.iter().position(|&r| r == locked_defender))
            .flatten();
        let locked_off_pos = (locked_attacker != ROSTER_IDX_NONE)
            .then(|| self.state.team_states[off_idx].active_lineup_slots.iter().position(|&r| r == locked_attacker))
            .flatten();
        let matchup_swap_allowed = |matchups: [u8; 5], a: usize, b: usize| {
            locked_def_pos != Some(a)
                && locked_def_pos != Some(b)
                && locked_off_pos != Some(usize::from(matchups[a]))
                && locked_off_pos != Some(usize::from(matchups[b]))
        };

        // One-shot PnR / off-ball screen defensive switch (never oscillates tick-to-tick)
        if in_frontcourt
            && gp_base_scheme != 3
            && (active_pnr_cov == PnrCoverageKind::Switch || gp_base_scheme == 1)
            && self.state.play_screen_contact_ds[off_idx] == 2
        {
            if let (Some(scr_pos), Some(tgt_pos)) = (active_screener_pos, active_screen_target_pos)
            {
                if scr_pos != tgt_pos {
                    let dh_opt = (0..5)
                        .find(|&dp| self.state.defense_plans[def_idx].matchups[dp] as usize == tgt_pos);
                    let ds_opt = (0..5)
                        .find(|&dp| self.state.defense_plans[def_idx].matchups[dp] as usize == scr_pos);
                    if let (Some(dh), Some(ds)) = (dh_opt, ds_opt) {
                        let dhr = self.state.team_states[def_idx].active_lineup_slots[dh];
                        let dsr = self.state.team_states[def_idx].active_lineup_slots[ds];
                        if dhr != ROSTER_IDX_NONE && dsr != ROSTER_IDX_NONE && dh != ds {
                            let tr = self.state.team_states[off_idx].active_lineup_slots[tgt_pos];
                            let sr = self.state.team_states[off_idx].active_lineup_slots[scr_pos];
                            let h_diff_ok = if gp_base_scheme == 1 {
                                true
                            } else if let (Some(gp), true, true) =
                                (gp_opt, tr != ROSTER_IDX_NONE, sr != ROSTER_IDX_NONE)
                            {
                                let t_snap = &self.teams[off_idx].players
                                    [(tr as usize).saturating_sub(off_idx * 15)];
                                let dh_snap = &self.teams[def_idx].players
                                    [(dhr as usize).saturating_sub(def_idx * 15)];
                                let ds_snap = &self.teams[def_idx].players
                                    [(dsr as usize).saturating_sub(def_idx * 15)];
                                ((t_snap.physical.height_cm as f64)
                                    - (ds_snap.physical.height_cm as f64))
                                    .abs()
                                    <= gp.switch_max_height_diff_cm
                                    && ((dh_snap.physical.height_cm as f64)
                                        - (ds_snap.physical.height_cm as f64))
                                        .abs()
                                        <= gp.switch_max_height_diff_cm + 6.0
                            } else {
                                false
                            };
                            if h_diff_ok
                                && !protected_targets.as_slice().contains(&dhr)
                                && !protected_targets.as_slice().contains(&dsr)
                                && matchup_swap_allowed(self.state.defense_plans[def_idx].matchups, dh, ds)
                            {
                                self.state.defense_plans[def_idx].matchups.swap(dh, ds);
                            }
                        }
                    }
                }
            }
        }

        // Precompute low-man help defender slot without repeated inner scans
        let mut low_man_def_pos = None;
        let mut best_rim_d = 99.0_f64;
        for dp in 0..5 {
            let opp = (self.state.defense_plans[def_idx].matchups[dp] as usize).min(4);
            if opp == handler_pos_idx || active_screener_pos == Some(opp) {
                continue;
            }
            let dr = self.state.team_states[def_idx].active_lineup_slots[dp];
            if dr != ROSTER_IDX_NONE {
                let d_rim = self.geom.dist_to_rim(
                    self.state.players[dr as usize].x_m,
                    self.state.players[dr as usize].y_m,
                    attack_dir,
                );
                if d_rim < best_rim_d {
                    best_rim_d = d_rim;
                    low_man_def_pos = Some(dp);
                }
            }
        }

        // Dynamic Pre-Switch, Scram-Switch, and Peel-Switch (§5.3)
        if in_frontcourt && gp_base_scheme != 3 {
            if let Some(gp) = gp_opt {
                if gp.pre_switch_enabled
                    && self.state.screen_ready_ticks[off_idx] == 0
                    && self.state.play_screen_contact_ds[off_idx] == 0
                {
                    if let Some((scr_p, tgt_p)) = active_ball_screen {
                        let d_scr_h = libm::hypot(off_xy[scr_p].0 - off_xy[tgt_p].0, off_xy[scr_p].1 - off_xy[tgt_p].1);
                        if d_scr_h <= gp.pre_switch_trigger_radius_m && d_scr_h > 1.25 {
                            let dh_opt = (0..5).find(|&dp| self.state.defense_plans[def_idx].matchups[dp] as usize == tgt_p);
                            let ds_opt = (0..5).find(|&dp| self.state.defense_plans[def_idx].matchups[dp] as usize == scr_p);
                            if let (Some(dh), Some(ds)) = (dh_opt, ds_opt) {
                                let dhr = self.state.team_states[def_idx].active_lineup_slots[dh];
                                let dsr = self.state.team_states[def_idx].active_lineup_slots[ds];
                                if dhr != ROSTER_IDX_NONE && dsr != ROSTER_IDX_NONE {
                                    let dh_snap = &self.teams[def_idx].players[(dhr as usize).saturating_sub(def_idx * 15)];
                                    let ds_snap = &self.teams[def_idx].players[(dsr as usize).saturating_sub(def_idx * 15)];
                                    if is_protected(dhr, dh_snap) || is_protected(dsr, ds_snap) {
                                        let mut alt_dp = None;
                                        let mut alt_d = 99.0_f64;
                                        for cand_dp in 0..5 {
                                            if cand_dp == dh || cand_dp == ds {
                                                continue;
                                            }
                                            let cr = self.state.team_states[def_idx].active_lineup_slots[cand_dp];
                                            if cr != ROSTER_IDX_NONE
                                                && def_active[cand_dp]
                                                && matchup_swap_allowed(self.state.defense_plans[def_idx].matchups, dh, cand_dp)
                                                && matchup_swap_allowed(self.state.defense_plans[def_idx].matchups, ds, cand_dp)
                                            {
                                                let cs = &self.teams[def_idx].players[(cr as usize).saturating_sub(def_idx * 15)];
                                                if !is_protected(cr, cs) {
                                                    let d_to_s = libm::hypot(def_xy[cand_dp].0 - off_xy[scr_p].0, def_xy[cand_dp].1 - off_xy[scr_p].1);
                                                    if d_to_s < alt_d {
                                                        alt_d = d_to_s;
                                                        alt_dp = Some(cand_dp);
                                                    }
                                                }
                                            }
                                        }
                                        if let Some(adp) = alt_dp {
                                            if is_protected(dsr, ds_snap) {
                                                self.state.defense_plans[def_idx].matchups.swap(ds, adp);
                                                self.transient_debug_flags[def_idx * 5 + ds] |= 1;
                                            } else {
                                                self.state.defense_plans[def_idx].matchups.swap(dh, adp);
                                                self.transient_debug_flags[def_idx * 5 + dh] |= 1;
                                            }
                                            self.transient_debug_flags[def_idx * 5 + adp] |= 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if gp.scram_switch_enabled {
                    let mut mismatch_dp = None;
                    for dp in 0..5 {
                        let opp_p = (self.state.defense_plans[def_idx].matchups[dp] as usize).min(4);
                        let dr = self.state.team_states[def_idx].active_lineup_slots[dp];
                        let or = self.state.team_states[off_idx].active_lineup_slots[opp_p];
                        if dr != ROSTER_IDX_NONE && or != ROSTER_IDX_NONE {
                            let d_snap = &self.teams[def_idx].players[(dr as usize).saturating_sub(def_idx * 15)];
                            let o_snap = &self.teams[off_idx].players[(or as usize).saturating_sub(off_idx * 15)];
                            if self.geom.dist_to_rim(off_xy[opp_p].0, off_xy[opp_p].1, attack_dir) < 3.8
                                && o_snap.physical.height_cm - d_snap.physical.height_cm >= 14.0
                            {
                                mismatch_dp = Some(dp);
                                break;
                            }
                        }
                    }
                    if let Some(mdp) = mismatch_dp {
                        for bdp in 0..5 {
                            if bdp == mdp {
                                continue;
                            }
                            let opp_b = (self.state.defense_plans[def_idx].matchups[bdp] as usize).min(4);
                            let br = self.state.team_states[def_idx].active_lineup_slots[bdp];
                            if br != ROSTER_IDX_NONE
                                && opp_b != handler_pos_idx
                                && self.geom.dist_to_rim(off_xy[opp_b].0, off_xy[opp_b].1, attack_dir) > 4.2
                            {
                                let b_snap = &self.teams[def_idx].players[(br as usize).saturating_sub(def_idx * 15)];
                                if b_snap.physical.height_cm >= 200.0
                                    && matchup_swap_allowed(self.state.defense_plans[def_idx].matchups, mdp, bdp)
                                {
                                    self.state.defense_plans[def_idx].matchups.swap(mdp, bdp);
                                    self.transient_debug_flags[def_idx * 5 + mdp] |= 1 << 1;
                                    self.transient_debug_flags[def_idx * 5 + bdp] |= 1 << 1;
                                    break;
                                }
                            }
                        }
                    }
                }
                if gp.peel_switch_enabled && handler_is_driving {
                    if let (Some(h_def_pos), Some(lm)) = (
                        (0..5).find(|&dp| self.state.defense_plans[def_idx].matchups[dp] as usize == handler_pos_idx),
                        low_man_def_pos,
                    ) {
                        let d_h_rim = self.geom.dist_to_rim(handler_xy.0, handler_xy.1, attack_dir);
                        let d_hd_rim = self.geom.dist_to_rim(def_xy[h_def_pos].0, def_xy[h_def_pos].1, attack_dir);
                        if d_h_rim < 3.6
                            && d_hd_rim > d_h_rim + 0.35
                            && libm::hypot(def_xy[lm].0 - handler_xy.0, def_xy[lm].1 - handler_xy.1) < 2.25
                            && matchup_swap_allowed(self.state.defense_plans[def_idx].matchups, h_def_pos, lm)
                        {
                            self.state.defense_plans[def_idx].matchups.swap(h_def_pos, lm);
                            self.transient_debug_flags[def_idx * 5 + h_def_pos] |= 1 << 2;
                            self.transient_debug_flags[def_idx * 5 + lm] |= 1 << 2;
                        }
                    }
                }
            }
        }

        let on_ball_def_xy = (0..5)
            .find(|&dp| self.state.defense_plans[def_idx].matchups[dp] as usize == handler_pos_idx && def_active[dp])
            .map_or((handler_xy.0 + attack_dir * 1.0, handler_xy.1), |dp| def_xy[dp]);
        DefenseCoordination {
            plan_index: gp_idx_opt,
            base_scheme: gp_base_scheme,
            active_pnr_cov,
            active_screener_pos,
            active_screen_xy,
            low_man_def_pos,
            on_ball_def_xy,
        }
    }

    /// 单名防守人的运动学目标：退防、2-3 联防、持球防守、换防后的掩护人防守、弱侧协防三角与绕掩护
    #[allow(clippy::too_many_arguments)]
    pub(super) fn compute_defense_kinematic_target(
        &self,
        off_idx: usize,
        def_idx: usize,
        p_pos: usize,
        r: u8,
        in_frontcourt: bool,
        full_press: bool,
        handler_pos_idx: usize,
        handler_is_driving: bool,
        ball_xy: (f64, f64),
        ball_vel: (f64, f64),
        active_offball_screen: Option<(usize, usize, (f64, f64))>,
        attack_dir: f64,
        rim_xy: (f64, f64),
        coordination: &DefenseCoordination,
        debug_flags: &mut u8,
    ) -> (f64, f64, Option<PlayerActionState>, f64, bool) {
        let on_ball_def_xy = coordination.on_ball_def_xy;
        let gp_opt = coordination.plan_index.map(|i| &self.defenses.0[i]);
        let active_pnr_cov = coordination.active_pnr_cov;
        let active_screener_pos = coordination.active_screener_pos;
        let active_screen_xy = coordination.active_screen_xy;
        let low_man_def_pos = coordination.low_man_def_pos;
        let px = self.state.players[r as usize].x_m;
        let py = self.state.players[r as usize].y_m;
        let bounds = self.geom.inbounds_nav_bounds(0.55);
        let opp_pos = (self.state.defense_plans[def_idx].matchups[p_pos] as usize).min(4);
        let opp_r = self.state.team_states[off_idx].active_lineup_slots[opp_pos];
        let is_on_ball_def = opp_pos == handler_pos_idx;
        let base_scheme = gp_opt.map_or(0, |g| g.base_scheme);

        // Retreat cleanly in transition unless full-court press or tight backcourt ball pickup
        if !in_frontcourt && !full_press {
            if is_on_ball_def {
                if (ball_xy.0 * attack_dir) < -1.0 {
                    return (
                        attack_dir * 1.8,
                        ball_xy.1.clamp(-3.2, 3.2),
                        None,
                        0.96,
                        true,
                    );
                }
            } else if opp_r != ROSTER_IDX_NONE {
                let mx = self.state.players[opp_r as usize].x_m;
                let my = self.state.players[opp_r as usize].y_m;
                let tx_half = (mx * 0.45 + (rim_xy.0 - attack_dir * 3.8) * 0.55)
                    .clamp(-self.geom.half_length_m + 1.5, self.geom.half_length_m - 1.5);
                let final_tx = if attack_dir > 0.0 {
                    tx_half.max(0.6)
                } else {
                    tx_half.min(-0.6)
                };
                return (final_tx, my * 0.72, None, 0.88, false);
            }
        }

        // Scheme 3: true 2-3 Zone in frontcourt (guards up top, wings bottom corners, center anchors paint)
        if base_scheme == 3 && in_frontcourt {
            let ball_shift_x =
                (ball_xy.0 - (rim_xy.0 - attack_dir * 6.0)).clamp(-1.4, 1.4) * 0.45;
            let ball_shift_y = (ball_xy.1 * 0.35).clamp(-1.5, 1.5);
            let zone_anchor = match p_pos {
                0 => (rim_xy.0 - attack_dir * 5.7 + ball_shift_x, 2.35 + ball_shift_y),
                1 => (rim_xy.0 - attack_dir * 5.7 + ball_shift_x, -2.35 + ball_shift_y),
                2 => (
                    rim_xy.0 - attack_dir * 2.8 + ball_shift_x * 0.4,
                    3.25 + (ball_xy.1 * 0.38).clamp(-1.0, 1.8),
                ),
                3 => (
                    rim_xy.0 - attack_dir * 2.8 + ball_shift_x * 0.4,
                    -3.25 + (ball_xy.1 * 0.38).clamp(-1.8, 1.0),
                ),
                _ => (
                    rim_xy.0 - attack_dir * 1.95,
                    (ball_xy.1 * 0.28).clamp(-1.35, 1.35),
                ),
            };
            let radius = self.teams[def_idx].players[usize::from(r) % 15].physical.collision_radius_m();
            if self.ruleset.clocks.defensive_3sec_enabled
                && self.state.players[r as usize].paint_timer_ds >= 10
                && self.geom.footprint_intersects_paint(px, py, attack_dir, 1.2192, radius)
            {
                let ft_line = self.geom.half_length_m - self.geom.paint_length_m;
                let lane_edge = self.geom.paint_width_m * 0.5;
                let margin = radius + 0.35;
                let exit_x = (attack_dir * (ft_line - margin), py);
                let exit_side = match p_pos {
                    0 | 2 => 1.0,
                    1 | 3 => -1.0,
                    _ if py < 0.0 || (py == 0.0 && ball_xy.1 < 0.0) => -1.0,
                    _ => 1.0,
                };
                let exit_y = (px, exit_side * (lane_edge + margin));
                let exit = if p_pos < 2 || (p_pos == 4 && (px - exit_x.0).abs() <= (py - exit_y.1).abs()) {
                    exit_x
                } else {
                    exit_y
                };
                let (x, y) = bounds.clamp(exit.0, exit.1);
                return (x, y, None, 0.94, false);
            }
            let (zx, zy) = bounds.clamp(zone_anchor.0, zone_anchor.1);
            let dist_ball_zone = libm::hypot(ball_xy.0 - zx, ball_xy.1 - zy);
            if p_pos < 4 && dist_ball_zone < 3.55 {
                let vx = rim_xy.0 - ball_xy.0;
                let vy = rim_xy.1 - ball_xy.1;
                let vn = libm::hypot(vx, vy).max(0.4);
                let cushion = 1.28_f64.min(vn * 0.7);
                let (cx, cy) =
                    bounds.clamp(ball_xy.0 + (vx / vn) * cushion, ball_xy.1 + (vy / vn) * cushion);
                return (cx, cy, None, 0.92, true);
            }
            return (zx, zy, None, 0.78, false);
        }

        let is_screener_def = active_screener_pos.is_some_and(|sp| opp_pos == sp);
        let screen_active = in_frontcourt && active_screener_pos.is_some();
        let blitz_engaged = screen_active
            && (active_pnr_cov == PnrCoverageKind::Blitz || base_scheme == 2)
            && is_screener_def;

        let def_slot_u8 = (def_idx * 5 + p_pos) as u8;
        let active_xout = in_frontcourt
            && self.state.xout_timer_ds[def_idx] > 0
            && self.state.xout_rotator_slot[def_idx] == def_slot_u8;

        // Compute base man-to-man / PnR / X-Out coverage assignment target
        let (mut tx, mut ty, mut spd_scale, on_ball_flag) = if active_xout {
            *debug_flags |= 1 << 4;
            let b_side = if ball_xy.1 >= 0.0 { 1.0 } else { -1.0 };
            (
                rim_xy.0 - attack_dir * 1.35,
                b_side * (self.geom.corner_three_dist_m - 0.3),
                0.96,
                false,
            )
        } else if is_on_ball_def {
            let vx = rim_xy.0 - ball_xy.0;
            let vy = rim_xy.1 - ball_xy.1;
            let vn = libm::hypot(vx, vy).max(0.4);
            let cushion = 1.08_f64.min(vn * 0.65);
            let bx = ball_xy.0 + (vx / vn) * cushion;
            let mut by = ball_xy.1 + (vy / vn) * cushion;
            if screen_active && active_pnr_cov == PnrCoverageKind::Ice {
                // Push handler away from middle screen toward sideline
                by += ball_xy.1.signum() * 0.55;
            }
            (bx, by, 1.0, true)
        } else if blitz_engaged {
            *debug_flags |= 1 << 6;
            // Trap ball-handler on blitz angle opposite on-ball defender
            let trap_y = ball_xy.1 - ball_xy.1.signum() * 0.85;
            (ball_xy.0 + attack_dir * 0.55, trap_y, 0.98, true)
        } else if screen_active && is_screener_def {
            let my = if opp_r != ROSTER_IDX_NONE {
                self.state.players[opp_r as usize].y_m
            } else {
                ball_xy.1
            };
            match active_pnr_cov {
                PnrCoverageKind::HighShowAndRecover
                    if self.state.play_screen_contact_ds[off_idx] <= 12 =>
                {
                    (
                        ball_xy.0 + attack_dir * 0.35,
                        ball_xy.1 + ball_xy.1.signum() * 0.80,
                        0.95,
                        false,
                    )
                }
                PnrCoverageKind::Ice => (
                    (ball_xy.0 + attack_dir * 1.65).clamp(-self.geom.half_length_m + 1.5, self.geom.half_length_m - 1.5),
                    (ball_xy.1 * 0.55).clamp(-2.5, 2.5),
                    0.86,
                    false,
                ),
                PnrCoverageKind::Drop => (
                    rim_xy.0 - attack_dir * 3.8,
                    (ball_xy.1 * 0.45).clamp(-1.85, 1.85),
                    0.84,
                    false,
                ),
                _ => (
                    (rim_xy.0 - attack_dir * 3.1).clamp(-self.geom.half_length_m + 1.5, self.geom.half_length_m - 1.5),
                    my * 0.65,
                    0.84,
                    false,
                ),
            }
        } else if opp_r != ROSTER_IDX_NONE {
            let mx = self.state.players[opp_r as usize].x_m;
            let my = self.state.players[opp_r as usize].y_m;
            let opp_rank = (opp_r as usize).saturating_sub(off_idx * 15);
            if let Some(opp_snap) = self.teams[off_idx].players.get(opp_rank) {
                // Off-ball screen navigation (Top-lock / Under / Trail)
                if let Some((_, cut_p, s_xy)) = active_offball_screen {
                    if in_frontcourt && opp_pos == cut_p {
                        if opp_snap.attributes[AttrIdx::ThreePoint] >= 84
                            && opp_snap.attributes[AttrIdx::OffBallMovement] >= 78
                        {
                            // Top-lock elite off-ball mover above ball-side shoulder
                            let vb_x = ball_xy.0 - mx;
                            let vb_y = ball_xy.1 - my;
                            let vn = libm::hypot(vb_x, vb_y).max(0.2);
                            let (cx, cy) = bounds.clamp(mx + (vb_x / vn) * 0.85, my + (vb_y / vn) * 0.85);
                            return (cx, cy, None, 0.90, false);
                        } else if opp_snap.attributes[AttrIdx::ThreePoint] < 75 {
                            // Go Under weak shooter toward inside of screen
                            let (cx, cy) = bounds.clamp(s_xy.0 + attack_dir * 0.55, s_xy.1 * 0.65);
                            return (cx, cy, None, 0.80, false);
                        }
                    }
                }

                // Low-Man Tag roller & paint protection during active drive / Blitz
                if in_frontcourt
                    && low_man_def_pos == Some(p_pos)
                    && (handler_is_driving
                        || (screen_active
                            && matches!(
                                active_pnr_cov,
                                PnrCoverageKind::Blitz | PnrCoverageKind::HighShowAndRecover
                            )))
                {
                    let roller_xy = active_screener_pos
                        .and_then(|sp| {
                            let sr = self.state.team_states[off_idx].active_lineup_slots[sp];
                            (sr != ROSTER_IDX_NONE).then(|| {
                                (
                                    self.state.players[sr as usize].x_m,
                                    self.state.players[sr as usize].y_m,
                                )
                            })
                        })
                        .unwrap_or((rim_xy.0 - attack_dir * 2.1, -ball_xy.1.signum() * 1.1));
                    let (tx_lm, ty_lm) = bounds.clamp(
                        roller_xy.0 * 0.60 + (rim_xy.0 - attack_dir * 1.45) * 0.40,
                        roller_xy.1 * 0.55,
                    );
                    *debug_flags |= 1 << 3;
                    return (tx_lm, ty_lm, None, 0.90, false);
                }

                let ball_dist_rim = libm::hypot(rim_xy.0 - ball_xy.0, rim_xy.1 - ball_xy.1);
                let pen_t = ((7.0 - ball_dist_rim) / 4.2).clamp(0.0, 1.0);
                let gp_shrink = gp_opt.map_or(55, |g| g.help_shrink_level) as f64;
                let mut help_shrink = (gp_shrink * 0.48 + pen_t * (gp_shrink * 0.68)).clamp(14.0, 92.0);
                // Sag off poor 3PT shooters below team threshold & collapse hard to tag on Blitz
                if opp_snap.attributes[AttrIdx::ThreePoint] < self.teams[def_idx].tactics.sag_off_3pt_threshold {
                    *debug_flags |= 1 << 5;
                    help_shrink = help_shrink.max(76.0);
                }
                if screen_active
                    && (active_pnr_cov == PnrCoverageKind::Blitz || base_scheme == 2)
                    && low_man_def_pos == Some(p_pos)
                {
                    help_shrink = help_shrink.max(82.0);
                }
                let (_, _, help_xy) = compute_help_triangle(
                    ball_xy,
                    on_ball_def_xy,
                    rim_xy,
                    (mx, my),
                    ball_vel,
                    help_shrink.round() as u8,
                    opp_snap.attributes[AttrIdx::ThreePoint],
                    opp_snap.attributes[AttrIdx::CatchAndShoot],
                    &self.geom,
                    &self.tunables,
                );
                (help_xy.0, help_xy.1, 0.78, false)
            } else {
                (mx, my, 0.78, false)
            }
        } else {
            (rim_xy.0 - attack_dir * 2.2, 0.0, 0.72, false)
        };

        // Screen navigation drag & physical avoidance (`NavScreen`) when not switched onto screener
        let mut desired_action = None;
        if let Some((sx, sy)) = active_screen_xy {
            if !is_screener_def
                && !blitz_engaged
                && active_pnr_cov != PnrCoverageKind::Switch
                && libm::hypot(sx - px, sy - py) < 2.1
                && Self::point_to_segment_dist(sx, sy, px, py, tx, ty) < 1.08
            {
                let dx = tx - px;
                let dy = ty - py;
                let dn = libm::hypot(dx, dy).max(0.1);
                let rx = -dy / dn;
                let ry = dx / dn;
                let alt_a = (px + rx * 1.1, py + ry * 1.1);
                let alt_b = (px - rx * 1.1, py - ry * 1.1);
                let pick = if libm::hypot(alt_a.0 - tx, alt_a.1 - ty)
                    <= libm::hypot(alt_b.0 - tx, alt_b.1 - ty)
                {
                    alt_a
                } else {
                    alt_b
                };
                tx = pick.0;
                ty = pick.1;
                spd_scale *= 0.62;
                desired_action = Some(PlayerActionState::NavScreen);
            }
        }

        if active_xout {
            desired_action = Some(PlayerActionState::Closeout);
        }

        // Proactively tap out of the paint in NBA before defensive 3-second violation
        if self.ruleset.clocks.defensive_3sec_enabled
            && in_frontcourt
            && self.state.players[r as usize].paint_timer_ds >= 18
        {
            ty = if py >= 0.0 {
                self.geom.paint_width_m * 0.5 + 0.75
            } else {
                -(self.geom.paint_width_m * 0.5 + 0.75)
            };
        }

        let (cx, cy) = bounds.clamp(tx, ty);
        (cx, cy, desired_action, spd_scale, on_ball_flag)
    }
}
