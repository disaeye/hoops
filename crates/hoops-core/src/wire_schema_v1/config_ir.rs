use serde::{Deserialize, Serialize};

use crate::canonical_le_bytes::{validate_id_string, CanonicalLeBytes, CanonicalReader, DecodeError};
use crate::wire_schema_v1::commands::{GameplanId, PlayId, PnrRuleId};
use crate::wire_schema_v1::enums::*;
use crate::wire_schema_v1::tunables::EngineTunables;
use crate::wire_schema_v1::commands::InlinePlayerList;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CourtRules {
    pub length_m: f64,
    pub width_m: f64,
    pub rim_offset_m: f64,
    pub rim_height_m: f64,
    pub three_arc_radius_m: f64,
    pub corner_three_dist_m: f64,
    pub paint_width_m: f64,
    pub paint_length_m: f64,
    pub restricted_area_radius_m: f64,
    pub oob_runoff_m: f64,
}

impl CanonicalLeBytes for CourtRules {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.length_m.write_canonical_le(out);
        self.width_m.write_canonical_le(out);
        self.rim_offset_m.write_canonical_le(out);
        self.rim_height_m.write_canonical_le(out);
        self.three_arc_radius_m.write_canonical_le(out);
        self.corner_three_dist_m.write_canonical_le(out);
        self.paint_width_m.write_canonical_le(out);
        self.paint_length_m.write_canonical_le(out);
        self.restricted_area_radius_m.write_canonical_le(out);
        self.oob_runoff_m.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            length_m: f64::read_canonical_le(reader)?,
            width_m: f64::read_canonical_le(reader)?,
            rim_offset_m: f64::read_canonical_le(reader)?,
            rim_height_m: f64::read_canonical_le(reader)?,
            three_arc_radius_m: f64::read_canonical_le(reader)?,
            corner_three_dist_m: f64::read_canonical_le(reader)?,
            paint_width_m: f64::read_canonical_le(reader)?,
            paint_length_m: f64::read_canonical_le(reader)?,
            restricted_area_radius_m: f64::read_canonical_le(reader)?,
            oob_runoff_m: f64::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RosterRules {
    pub max_team_players: u8,
    pub fiba_sub_clock_run_reentry_rule: bool,
}

impl CanonicalLeBytes for RosterRules {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.max_team_players.write_canonical_le(out);
        self.fiba_sub_clock_run_reentry_rule.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            max_team_players: u8::read_canonical_le(reader)?,
            fiba_sub_clock_run_reentry_rule: bool::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClockRules {
    pub quarter_sec: u16,
    pub ot_sec: u16,
    pub stop_on_made_reg_last_sec: u16,
    pub stop_on_made_final_ot_sec: u16,
    pub min_catch_and_shoot_ds: u16,
    pub live_rebound_03s_enabled: bool,
    pub free_throw_timer_ds: u16,
    pub shot_clock_airball_defense_control_continues: bool,
    pub shot_clock_policy: ShotClockResetPolicy,
    pub full_shot_clock_ds: u16,
    pub reset_shot_clock_ds: u16,
    pub defensive_3sec_enabled: bool,
    pub back_to_basket_zone: u8,
    pub closely_guarded_5sec_enabled: bool,
}

impl CanonicalLeBytes for ClockRules {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.quarter_sec.write_canonical_le(out);
        self.ot_sec.write_canonical_le(out);
        self.stop_on_made_reg_last_sec.write_canonical_le(out);
        self.stop_on_made_final_ot_sec.write_canonical_le(out);
        self.min_catch_and_shoot_ds.write_canonical_le(out);
        self.live_rebound_03s_enabled.write_canonical_le(out);
        self.free_throw_timer_ds.write_canonical_le(out);
        self.shot_clock_airball_defense_control_continues
            .write_canonical_le(out);
        self.shot_clock_policy.write_canonical_le(out);
        self.full_shot_clock_ds.write_canonical_le(out);
        self.reset_shot_clock_ds.write_canonical_le(out);
        self.defensive_3sec_enabled.write_canonical_le(out);
        self.back_to_basket_zone.write_canonical_le(out);
        self.closely_guarded_5sec_enabled.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            quarter_sec: u16::read_canonical_le(reader)?,
            ot_sec: u16::read_canonical_le(reader)?,
            stop_on_made_reg_last_sec: u16::read_canonical_le(reader)?,
            stop_on_made_final_ot_sec: u16::read_canonical_le(reader)?,
            min_catch_and_shoot_ds: u16::read_canonical_le(reader)?,
            live_rebound_03s_enabled: bool::read_canonical_le(reader)?,
            free_throw_timer_ds: u16::read_canonical_le(reader)?,
            shot_clock_airball_defense_control_continues: bool::read_canonical_le(reader)?,
            shot_clock_policy: ShotClockResetPolicy::read_canonical_le(reader)?,
            full_shot_clock_ds: u16::read_canonical_le(reader)?,
            reset_shot_clock_ds: u16::read_canonical_le(reader)?,
            defensive_3sec_enabled: bool::read_canonical_le(reader)?,
            back_to_basket_zone: u8::read_canonical_le(reader)?,
            closely_guarded_5sec_enabled: bool::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FoulRules {
    pub foul_out_limit: u8,
    pub offensive_foul_counts_toward_team_fouls: bool,
    pub regulation_bonus_threshold: u8,
    pub regulation_l2m_bonus_threshold: Option<u8>,
    pub ot_team_foul_reset_each: bool,
    pub ot_bonus_threshold: u8,
    pub ot_l2m_bonus_threshold: Option<u8>,
    pub goaltend_mode: u8,
    pub possession_start_mode: u8,
    pub short_handed_rule: u8,
}

impl CanonicalLeBytes for FoulRules {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.foul_out_limit.write_canonical_le(out);
        self.offensive_foul_counts_toward_team_fouls
            .write_canonical_le(out);
        self.regulation_bonus_threshold.write_canonical_le(out);
        self.regulation_l2m_bonus_threshold.write_canonical_le(out);
        self.ot_team_foul_reset_each.write_canonical_le(out);
        self.ot_bonus_threshold.write_canonical_le(out);
        self.ot_l2m_bonus_threshold.write_canonical_le(out);
        self.goaltend_mode.write_canonical_le(out);
        self.possession_start_mode.write_canonical_le(out);
        self.short_handed_rule.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            foul_out_limit: u8::read_canonical_le(reader)?,
            offensive_foul_counts_toward_team_fouls: bool::read_canonical_le(reader)?,
            regulation_bonus_threshold: u8::read_canonical_le(reader)?,
            regulation_l2m_bonus_threshold: Option::<u8>::read_canonical_le(reader)?,
            ot_team_foul_reset_each: bool::read_canonical_le(reader)?,
            ot_bonus_threshold: u8::read_canonical_le(reader)?,
            ot_l2m_bonus_threshold: Option::<u8>::read_canonical_le(reader)?,
            goaltend_mode: u8::read_canonical_le(reader)?,
            possession_start_mode: u8::read_canonical_le(reader)?,
            short_handed_rule: u8::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimeoutRules {
    pub team_to_ds: u16,
    pub mandatory_to_ds: u16,
    pub quarter_break_ds: u16,
    pub ot_break_ds: u16,
    pub halftime_ds: u16,
    pub review_dead_time_ds: u16,
    pub q_total_to: u8,
    pub q4_enter_cap: u8,
    pub q4_l3m_cap: u8,
    pub h1_to: u8,
    pub h2_to: u8,
    pub h2_l2m_cap: u8,
    pub ot_to: u8,
    pub mandatory_first_charge_policy: MandatoryFirstChargePolicy,
    pub live_ball_player_to_allowed: bool,
    pub to_between_fts_allowed: bool,
}

impl CanonicalLeBytes for TimeoutRules {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.team_to_ds.write_canonical_le(out);
        self.mandatory_to_ds.write_canonical_le(out);
        self.quarter_break_ds.write_canonical_le(out);
        self.ot_break_ds.write_canonical_le(out);
        self.halftime_ds.write_canonical_le(out);
        self.review_dead_time_ds.write_canonical_le(out);
        self.q_total_to.write_canonical_le(out);
        self.q4_enter_cap.write_canonical_le(out);
        self.q4_l3m_cap.write_canonical_le(out);
        self.h1_to.write_canonical_le(out);
        self.h2_to.write_canonical_le(out);
        self.h2_l2m_cap.write_canonical_le(out);
        self.ot_to.write_canonical_le(out);
        self.mandatory_first_charge_policy.write_canonical_le(out);
        self.live_ball_player_to_allowed.write_canonical_le(out);
        self.to_between_fts_allowed.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            team_to_ds: u16::read_canonical_le(reader)?,
            mandatory_to_ds: u16::read_canonical_le(reader)?,
            quarter_break_ds: u16::read_canonical_le(reader)?,
            ot_break_ds: u16::read_canonical_le(reader)?,
            halftime_ds: u16::read_canonical_le(reader)?,
            review_dead_time_ds: u16::read_canonical_le(reader)?,
            q_total_to: u8::read_canonical_le(reader)?,
            q4_enter_cap: u8::read_canonical_le(reader)?,
            q4_l3m_cap: u8::read_canonical_le(reader)?,
            h1_to: u8::read_canonical_le(reader)?,
            h2_to: u8::read_canonical_le(reader)?,
            h2_l2m_cap: u8::read_canonical_le(reader)?,
            ot_to: u8::read_canonical_le(reader)?,
            mandatory_first_charge_policy: MandatoryFirstChargePolicy::read_canonical_le(reader)?,
            live_ball_player_to_allowed: bool::read_canonical_le(reader)?,
            to_between_fts_allowed: bool::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChallengeRules {
    pub mode: u8,
    pub max_base: u8,
    pub bonus_on_first_success: bool,
    pub requires_and_charges_timeout: bool,
}

impl CanonicalLeBytes for ChallengeRules {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.mode.write_canonical_le(out);
        self.max_base.write_canonical_le(out);
        self.bonus_on_first_success.write_canonical_le(out);
        self.requires_and_charges_timeout.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            mode: u8::read_canonical_le(reader)?,
            max_base: u8::read_canonical_le(reader)?,
            bonus_on_first_success: bool::read_canonical_le(reader)?,
            requires_and_charges_timeout: bool::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuleSet {
    pub rule_id: String,
    pub court: CourtRules,
    pub roster: RosterRules,
    pub clocks: ClockRules,
    pub fouls: FoulRules,
    pub timeouts: TimeoutRules,
    pub challenges: ChallengeRules,
}

impl RuleSet {
    pub fn nba_default() -> Self {
        Self {
            rule_id: "nba_2023_24".to_owned(),
            court: CourtRules {
                length_m: 28.6512,
                width_m: 15.24,
                rim_offset_m: 1.6002,
                rim_height_m: 3.048,
                three_arc_radius_m: 7.239,
                corner_three_dist_m: 6.7056,
                paint_width_m: 4.8768,
                paint_length_m: 5.7912,
                restricted_area_radius_m: 1.2192,
                oob_runoff_m: 3.0,
            },
            roster: RosterRules {
                max_team_players: 15,
                fiba_sub_clock_run_reentry_rule: false,
            },
            clocks: ClockRules {
                quarter_sec: 720,
                ot_sec: 300,
                stop_on_made_reg_last_sec: 60,
                stop_on_made_final_ot_sec: 120,
                min_catch_and_shoot_ds: 3,
                live_rebound_03s_enabled: true,
                free_throw_timer_ds: 100,
                shot_clock_airball_defense_control_continues: false,
                shot_clock_policy: ShotClockResetPolicy::NbaRule7,
                full_shot_clock_ds: 240,
                reset_shot_clock_ds: 140,
                defensive_3sec_enabled: true,
                back_to_basket_zone: 1,
                closely_guarded_5sec_enabled: false,
            },
            fouls: FoulRules {
                foul_out_limit: 6,
                offensive_foul_counts_toward_team_fouls: false,
                regulation_bonus_threshold: 5,
                regulation_l2m_bonus_threshold: Some(2),
                ot_team_foul_reset_each: true,
                ot_bonus_threshold: 4,
                ot_l2m_bonus_threshold: Some(2),
                goaltend_mode: 0,
                possession_start_mode: 0,
                short_handed_rule: 0,
            },
            timeouts: TimeoutRules {
                team_to_ds: 750,
                mandatory_to_ds: 1650,
                quarter_break_ds: 1500,
                ot_break_ds: 1500,
                halftime_ds: 9000,
                review_dead_time_ds: 300,
                q_total_to: 7,
                q4_enter_cap: 4,
                q4_l3m_cap: 2,
                h1_to: 4,
                h2_to: 4,
                h2_l2m_cap: 2,
                ot_to: 2,
                mandatory_first_charge_policy: MandatoryFirstChargePolicy::HomeFirst,
                live_ball_player_to_allowed: true,
                to_between_fts_allowed: true,
            },
            challenges: ChallengeRules {
                mode: 1,
                max_base: 1,
                bonus_on_first_success: true,
                requires_and_charges_timeout: true,
            },
        }
    }

    pub fn fiba_default() -> Self {
        Self {
            rule_id: "fiba_2024".to_owned(),
            court: CourtRules {
                length_m: 28.0,
                width_m: 15.0,
                rim_offset_m: 1.575,
                rim_height_m: 3.05,
                three_arc_radius_m: 6.75,
                corner_three_dist_m: 6.60,
                paint_width_m: 4.90,
                paint_length_m: 5.80,
                restricted_area_radius_m: 1.25,
                oob_runoff_m: 3.0,
            },
            roster: RosterRules {
                max_team_players: 12,
                fiba_sub_clock_run_reentry_rule: true,
            },
            clocks: ClockRules {
                quarter_sec: 600,
                ot_sec: 300,
                stop_on_made_reg_last_sec: 0,
                stop_on_made_final_ot_sec: 120,
                min_catch_and_shoot_ds: 3,
                live_rebound_03s_enabled: false,
                free_throw_timer_ds: 50,
                shot_clock_airball_defense_control_continues: true,
                shot_clock_policy: ShotClockResetPolicy::FibaArt29,
                full_shot_clock_ds: 240,
                reset_shot_clock_ds: 140,
                defensive_3sec_enabled: false,
                back_to_basket_zone: 0,
                closely_guarded_5sec_enabled: true,
            },
            fouls: FoulRules {
                foul_out_limit: 5,
                offensive_foul_counts_toward_team_fouls: true,
                regulation_bonus_threshold: 5,
                regulation_l2m_bonus_threshold: None,
                ot_team_foul_reset_each: false,
                ot_bonus_threshold: 5,
                ot_l2m_bonus_threshold: None,
                goaltend_mode: 1,
                possession_start_mode: 1,
                short_handed_rule: 1,
            },
            timeouts: TimeoutRules {
                team_to_ds: 600,
                mandatory_to_ds: 0,
                quarter_break_ds: 1200,
                ot_break_ds: 1200,
                halftime_ds: 9000,
                review_dead_time_ds: 300,
                q_total_to: 5,
                q4_enter_cap: 3,
                q4_l3m_cap: 2,
                h1_to: 2,
                h2_to: 3,
                h2_l2m_cap: 2,
                ot_to: 1,
                mandatory_first_charge_policy: MandatoryFirstChargePolicy::HomeFirst,
                live_ball_player_to_allowed: false,
                to_between_fts_allowed: false,
            },
            challenges: ChallengeRules {
                mode: 2,
                max_base: 1,
                bonus_on_first_success: false,
                requires_and_charges_timeout: false,
            },
        }
    }
}

impl CanonicalLeBytes for RuleSet {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.rule_id.write_canonical_le(out);
        self.court.write_canonical_le(out);
        self.roster.write_canonical_le(out);
        self.clocks.write_canonical_le(out);
        self.fouls.write_canonical_le(out);
        self.timeouts.write_canonical_le(out);
        self.challenges.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let rule_id = String::read_canonical_le(reader)?;
        validate_id_string(&rule_id)?;
        Ok(Self {
            rule_id,
            court: CourtRules::read_canonical_le(reader)?,
            roster: RosterRules::read_canonical_le(reader)?,
            clocks: ClockRules::read_canonical_le(reader)?,
            fouls: FoulRules::read_canonical_le(reader)?,
            timeouts: TimeoutRules::read_canonical_le(reader)?,
            challenges: ChallengeRules::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompiledPlayPhase {
    pub base_epv: f64,
    pub max_duration_ds: u16,
    pub exit_trigger_kind: u8,
    pub exit_target_role: u8,
    pub exit_fallback_ds: u16,
    pub actions: [(u8, u8, u8, u8, f64, f64, f64); 5],
}

impl CanonicalLeBytes for CompiledPlayPhase {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.base_epv.write_canonical_le(out);
        self.max_duration_ds.write_canonical_le(out);
        self.exit_trigger_kind.write_canonical_le(out);
        self.exit_target_role.write_canonical_le(out);
        self.exit_fallback_ds.write_canonical_le(out);
        self.actions.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            base_epv: f64::read_canonical_le(reader)?,
            max_duration_ds: u16::read_canonical_le(reader)?,
            exit_trigger_kind: u8::read_canonical_le(reader)?,
            exit_target_role: u8::read_canonical_le(reader)?,
            exit_fallback_ds: u16::read_canonical_le(reader)?,
            actions: <[(u8, u8, u8, u8, f64, f64, f64); 5]>::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompiledPlaybook {
    pub play_id: PlayId,
    pub min_shot_clock_ds: u16,
    pub allow_mirror: bool,
    pub role_priority: [u8; 5],
    pub slot_weights: [Vec<(u16, f64)>; 5],
    pub initial_positions: [(u8, u8, f64, f64); 5],
    pub phases: Vec<CompiledPlayPhase>,
}

impl CanonicalLeBytes for CompiledPlaybook {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.play_id.write_canonical_le(out);
        self.min_shot_clock_ds.write_canonical_le(out);
        self.allow_mirror.write_canonical_le(out);
        self.role_priority.write_canonical_le(out);
        self.slot_weights.write_canonical_le(out);
        self.initial_positions.write_canonical_le(out);
        self.phases.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            play_id: PlayId::read_canonical_le(reader)?,
            min_shot_clock_ds: u16::read_canonical_le(reader)?,
            allow_mirror: bool::read_canonical_le(reader)?,
            role_priority: <[u8; 5]>::read_canonical_le(reader)?,
            slot_weights: <[Vec<(u16, f64)>; 5]>::read_canonical_le(reader)?,
            initial_positions: <[(u8, u8, f64, f64); 5]>::read_canonical_le(reader)?,
            phases: Vec::<CompiledPlayPhase>::read_canonical_le(reader)?,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PredicateExpr {
    All(Vec<PredicateExpr>) = 0,
    Any(Vec<PredicateExpr>) = 1,
    Not(Box<PredicateExpr>) = 2,
    Cmp {
        subject: u8,
        metric_key: u16,
        op: u8,
        val: f64,
    } = 3,
}

pub type RoleDefExpr = PredicateExpr;

impl PredicateExpr {
    pub fn depth(&self) -> usize {
        match self {
            Self::All(v) | Self::Any(v) => {
                1 + v.iter().map(|c| c.depth()).max().unwrap_or(0)
            }
            Self::Not(inner) => 1 + inner.depth(),
            Self::Cmp { .. } => 1,
        }
    }
}

impl CanonicalLeBytes for PredicateExpr {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        match self {
            Self::All(v) => {
                out.push(0);
                v.write_canonical_le(out);
            }
            Self::Any(v) => {
                out.push(1);
                v.write_canonical_le(out);
            }
            Self::Not(inner) => {
                out.push(2);
                inner.write_canonical_le(out);
            }
            Self::Cmp {
                subject,
                metric_key,
                op,
                val,
            } => {
                out.push(3);
                subject.write_canonical_le(out);
                metric_key.write_canonical_le(out);
                op.write_canonical_le(out);
                val.write_canonical_le(out);
            }
        }
    }

    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::All(Vec::<PredicateExpr>::read_canonical_le(reader)?)),
            1 => Ok(Self::Any(Vec::<PredicateExpr>::read_canonical_le(reader)?)),
            2 => Ok(Self::Not(Box::new(PredicateExpr::read_canonical_le(reader)?))),
            3 => Ok(Self::Cmp {
                subject: u8::read_canonical_le(reader)?,
                metric_key: u16::read_canonical_le(reader)?,
                op: u8::read_canonical_le(reader)?,
                val: f64::read_canonical_le(reader)?,
            }),
            v => Err(DecodeError::InvalidEnumValue {
                field: "PredicateExpr",
                value: v as u32,
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompiledPnrRule {
    pub rule_id: PnrRuleId,
    pub predicates: Vec<PredicateExpr>,
    pub coverage: PnrCoverageKind,
    pub on_ball_action: u8,
    pub help_rule: u8,
}

impl CanonicalLeBytes for CompiledPnrRule {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.rule_id.write_canonical_le(out);
        self.predicates.write_canonical_le(out);
        self.coverage.write_canonical_le(out);
        self.on_ball_action.write_canonical_le(out);
        self.help_rule.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            rule_id: PnrRuleId::read_canonical_le(reader)?,
            predicates: Vec::<PredicateExpr>::read_canonical_le(reader)?,
            coverage: PnrCoverageKind::read_canonical_le(reader)?,
            on_ball_action: u8::read_canonical_le(reader)?,
            help_rule: u8::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompiledAdaptationTrigger {
    pub watch_coverage: PnrCoverageKind,
    pub min_possessions: u16,
    pub prior_weight: f64,
    pub prior_ppp: f64,
    pub escalate_ppp_threshold: f64,
    pub deescalate_ppp_threshold: f64,
    pub revert_if_escalated_ppp_exceeds: f64,
    pub revert_min_possessions: u16,
    pub deescalate_min_possessions: u16,
    pub threat_absent_three_point_lt: u8,
    pub cooldown_possessions: u8,
    pub escalate_to_coverage: PnrCoverageKind,
}

impl CanonicalLeBytes for CompiledAdaptationTrigger {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.watch_coverage.write_canonical_le(out);
        self.min_possessions.write_canonical_le(out);
        self.prior_weight.write_canonical_le(out);
        self.prior_ppp.write_canonical_le(out);
        self.escalate_ppp_threshold.write_canonical_le(out);
        self.deescalate_ppp_threshold.write_canonical_le(out);
        self.revert_if_escalated_ppp_exceeds.write_canonical_le(out);
        self.revert_min_possessions.write_canonical_le(out);
        self.deescalate_min_possessions.write_canonical_le(out);
        self.threat_absent_three_point_lt.write_canonical_le(out);
        self.cooldown_possessions.write_canonical_le(out);
        self.escalate_to_coverage.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            watch_coverage: PnrCoverageKind::read_canonical_le(reader)?,
            min_possessions: u16::read_canonical_le(reader)?,
            prior_weight: f64::read_canonical_le(reader)?,
            prior_ppp: f64::read_canonical_le(reader)?,
            escalate_ppp_threshold: f64::read_canonical_le(reader)?,
            deescalate_ppp_threshold: f64::read_canonical_le(reader)?,
            revert_if_escalated_ppp_exceeds: f64::read_canonical_le(reader)?,
            revert_min_possessions: u16::read_canonical_le(reader)?,
            deescalate_min_possessions: u16::read_canonical_le(reader)?,
            threat_absent_three_point_lt: u8::read_canonical_le(reader)?,
            cooldown_possessions: u8::read_canonical_le(reader)?,
            escalate_to_coverage: PnrCoverageKind::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompiledDefenseGameplan {
    pub gameplan_id: GameplanId,
    pub base_scheme: u8,
    pub switch_max_height_diff_cm: f64,
    pub help_shrink_level: u8,
    pub role_defs: Vec<(String, RoleDefExpr)>,
    pub protect_roles: Vec<String>,
    pub hide_on_weakest_shooter: bool,
    pub pre_switch_enabled: bool,
    pub pre_switch_trigger_radius_m: f64,
    pub scram_switch_enabled: bool,
    pub peel_switch_enabled: bool,
    pub pnr_rules: Vec<CompiledPnrRule>,
    pub adaptation_triggers: Vec<CompiledAdaptationTrigger>,
}

/// 返回 HPS1 防守规则字典中某个比赛计划的首项编号。
pub fn pnr_rule_dict_offset(plans: &[CompiledDefenseGameplan], plan_index: usize) -> usize {
    plans[..plan_index].iter().map(|plan| plan.pnr_rules.len()).sum()
}

impl CanonicalLeBytes for CompiledDefenseGameplan {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.gameplan_id.write_canonical_le(out);
        self.base_scheme.write_canonical_le(out);
        self.switch_max_height_diff_cm.write_canonical_le(out);
        self.help_shrink_level.write_canonical_le(out);
        self.role_defs.write_canonical_le(out);
        self.protect_roles.write_canonical_le(out);
        self.hide_on_weakest_shooter.write_canonical_le(out);
        self.pre_switch_enabled.write_canonical_le(out);
        self.pre_switch_trigger_radius_m.write_canonical_le(out);
        self.scram_switch_enabled.write_canonical_le(out);
        self.peel_switch_enabled.write_canonical_le(out);
        self.pnr_rules.write_canonical_le(out);
        self.adaptation_triggers.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            gameplan_id: GameplanId::read_canonical_le(reader)?,
            base_scheme: u8::read_canonical_le(reader)?,
            switch_max_height_diff_cm: f64::read_canonical_le(reader)?,
            help_shrink_level: u8::read_canonical_le(reader)?,
            role_defs: Vec::<(String, RoleDefExpr)>::read_canonical_le(reader)?,
            protect_roles: Vec::<String>::read_canonical_le(reader)?,
            hide_on_weakest_shooter: bool::read_canonical_le(reader)?,
            pre_switch_enabled: bool::read_canonical_le(reader)?,
            pre_switch_trigger_radius_m: f64::read_canonical_le(reader)?,
            scram_switch_enabled: bool::read_canonical_le(reader)?,
            peel_switch_enabled: bool::read_canonical_le(reader)?,
            pnr_rules: Vec::<CompiledPnrRule>::read_canonical_le(reader)?,
            adaptation_triggers: Vec::<CompiledAdaptationTrigger>::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BadgeDef {
    pub badge_id: String,
    pub hook: u8,
    pub op: u8,
    pub value: f64,
}

impl CanonicalLeBytes for BadgeDef {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.badge_id.write_canonical_le(out);
        self.hook.write_canonical_le(out);
        self.op.write_canonical_le(out);
        self.value.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let badge_id = String::read_canonical_le(reader)?;
        validate_id_string(&badge_id)?;
        Ok(Self {
            badge_id,
            hook: u8::read_canonical_le(reader)?,
            op: u8::read_canonical_le(reader)?,
            value: f64::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PhysicalProfile {
    pub height_cm: f64,
    pub wingspan_cm: f64,
    pub standing_reach_cm: f64,
    pub weight_kg: f64,
    pub age: u8,
    pub dominant_hand: u8,
    pub display_position: u8,
}

impl PhysicalProfile {
    pub fn collision_radius_m(&self) -> f64 {
        (0.24 + (self.weight_kg - 75.0) * 0.0014).clamp(0.22, 0.42)
    }

    pub fn reach_radius_m(&self) -> f64 {
        (self.wingspan_cm * 0.5 / 100.0).clamp(0.85, 1.35)
    }

    pub fn max_reach_z_m(&self) -> f64 {
        (self.standing_reach_cm / 100.0 + 0.72).clamp(2.6, 3.85)
    }
}

impl CanonicalLeBytes for PhysicalProfile {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.height_cm.write_canonical_le(out);
        self.wingspan_cm.write_canonical_le(out);
        self.standing_reach_cm.write_canonical_le(out);
        self.weight_kg.write_canonical_le(out);
        self.age.write_canonical_le(out);
        self.dominant_hand.write_canonical_le(out);
        self.display_position.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            height_cm: f64::read_canonical_le(reader)?,
            wingspan_cm: f64::read_canonical_le(reader)?,
            standing_reach_cm: f64::read_canonical_le(reader)?,
            weight_kg: f64::read_canonical_le(reader)?,
            age: u8::read_canonical_le(reader)?,
            dominant_hand: u8::read_canonical_le(reader)?,
            display_position: u8::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlayerTendencies {
    pub zone_weights: [f64; 5],
    pub drive_vs_pullup: f64,
    pub pass_vs_shoot: f64,
    pub pnr_roll_vs_pop: f64,
    pub crash_orb: f64,
    pub gamble_steal: f64,
}

impl CanonicalLeBytes for PlayerTendencies {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.zone_weights.write_canonical_le(out);
        self.drive_vs_pullup.write_canonical_le(out);
        self.pass_vs_shoot.write_canonical_le(out);
        self.pnr_roll_vs_pop.write_canonical_le(out);
        self.crash_orb.write_canonical_le(out);
        self.gamble_steal.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            zone_weights: <[f64; 5]>::read_canonical_le(reader)?,
            drive_vs_pullup: f64::read_canonical_le(reader)?,
            pass_vs_shoot: f64::read_canonical_le(reader)?,
            pnr_roll_vs_pop: f64::read_canonical_le(reader)?,
            crash_orb: f64::read_canonical_le(reader)?,
            gamble_steal: f64::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlayerFunctionalProfile {
    pub scores: [f64; 9],
    pub guard_tiers: [bool; 5],
}

impl CanonicalLeBytes for PlayerFunctionalProfile {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.scores.write_canonical_le(out);
        self.guard_tiers.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            scores: <[f64; 9]>::read_canonical_le(reader)?,
            guard_tiers: <[bool; 5]>::read_canonical_le(reader)?,
        })
    }
}

macro_rules! define_attributes {
    ($($variant:ident = $index:literal => $name:literal),+ $(,)?) => {
        /// 球员属性在 V1 字节数组中的固定顺序。
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        pub enum AttrIdx {
            $($variant = $index),+
        }

        impl AttrIdx {
            pub const ALL: [Self; 38] = [$(Self::$variant),+];
            pub const FIRST_METRIC_KEY: u16 = 7;
            pub const LAST_METRIC_KEY: u16 = Self::FIRST_METRIC_KEY + Self::ALL.len() as u16 - 1;

            pub const fn name(self) -> &'static str {
                match self { $(Self::$variant => $name),+ }
            }

            pub fn from_name(name: &str) -> Option<Self> {
                match name { $($name => Some(Self::$variant),)+ _ => None }
            }

            pub const fn metric_key(self) -> u16 {
                Self::FIRST_METRIC_KEY + self as u16
            }

            pub fn from_metric_key(key: u16) -> Option<Self> {
                let index = key.checked_sub(Self::FIRST_METRIC_KEY)?;
                Self::ALL.get(index as usize).copied()
            }
        }
    };
}

define_attributes! {
    Layup = 0 => "layup",
    Dunk = 1 => "dunk",
    CloseTouch = 2 => "close_touch",
    PostControl = 3 => "post_control",
    DrawFoul = 4 => "draw_foul",
    HandsCatch = 5 => "hands_catch",
    MidRange = 6 => "mid_range",
    ThreePoint = 7 => "three_point",
    FreeThrow = 8 => "free_throw",
    CatchAndShoot = 9 => "catch_and_shoot",
    OffDribbleShot = 10 => "off_dribble_shot",
    ContestedShot = 11 => "contested_shot",
    ShotRange = 12 => "shot_range",
    ShotSelectionIq = 13 => "shot_selection_iq",
    BallHandle = 14 => "ball_handle",
    PassAccuracy = 15 => "pass_accuracy",
    PassVision = 16 => "pass_vision",
    OffBallMovement = 17 => "off_ball_movement",
    ScreenSetting = 18 => "screen_setting",
    PerimeterDefense = 19 => "perimeter_defense",
    InteriorDefense = 20 => "interior_defense",
    Block = 21 => "block",
    Steal = 22 => "steal",
    HelpDefenseIq = 23 => "help_defense_iq",
    DefCloseout = 24 => "def_closeout",
    ScreenNavigation = 25 => "screen_navigation",
    OffensiveRebound = 26 => "offensive_rebound",
    DefensiveRebound = 27 => "defensive_rebound",
    BoxOut = 28 => "box_out",
    TopSpeed = 29 => "top_speed",
    Acceleration = 30 => "acceleration",
    LateralAgility = 31 => "lateral_agility",
    Strength = 32 => "strength",
    Vertical = 33 => "vertical",
    StaminaRating = 34 => "stamina_rating",
    Durability = 35 => "durability",
    Hustle = 36 => "hustle",
    ComposureClutch = 37 => "composure_clutch",
}

/// 38 attributes fixed order wrapper (`[u8; 38]` exceeds standard Serde 32-element derive).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PlayerAttributes38(pub [u8; 38]);

impl core::ops::Index<AttrIdx> for PlayerAttributes38 {
    type Output = u8;

    fn index(&self, attr: AttrIdx) -> &Self::Output {
        &self.0[attr as usize]
    }
}

impl core::ops::IndexMut<AttrIdx> for PlayerAttributes38 {
    fn index_mut(&mut self, attr: AttrIdx) -> &mut Self::Output {
        &mut self.0[attr as usize]
    }
}

impl Default for PlayerAttributes38 {
    fn default() -> Self {
        Self([70; 38])
    }
}

impl serde::Serialize for PlayerAttributes38 {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple;
        let mut tup = serializer.serialize_tuple(38)?;
        for &v in &self.0 {
            tup.serialize_element(&v)?;
        }
        tup.end()
    }
}

impl<'de> serde::Deserialize<'de> for PlayerAttributes38 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ArrVisitor;
        impl<'de> serde::de::Visitor<'de> for ArrVisitor {
            type Value = PlayerAttributes38;
            fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                write!(f, "38-byte player attribute array")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut arr = [0u8; 38];
                for (i, slot) in arr.iter_mut().enumerate() {
                    *slot = seq
                        .next_element::<u8>()?
                        .ok_or_else(|| serde::de::Error::invalid_length(i, &self))?;
                }
                Ok(PlayerAttributes38(arr))
            }
        }
        deserializer.deserialize_tuple(38, ArrVisitor)
    }
}

impl CanonicalLeBytes for PlayerAttributes38 {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.0.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self(<[u8; 38]>::read_canonical_le(reader)?))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompiledPlayerSnapshot {
    pub player_id: String,
    pub display_name: String,
    pub jersey_number_code: u8,
    pub physical: PhysicalProfile,
    pub attributes: PlayerAttributes38,
    pub tendencies: PlayerTendencies,
    pub functional: PlayerFunctionalProfile,
    pub badge_indices: Vec<u16>,
}

impl CanonicalLeBytes for CompiledPlayerSnapshot {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.player_id.write_canonical_le(out);
        self.display_name.write_canonical_le(out);
        self.jersey_number_code.write_canonical_le(out);
        self.physical.write_canonical_le(out);
        self.attributes.write_canonical_le(out);
        self.tendencies.write_canonical_le(out);
        self.functional.write_canonical_le(out);
        self.badge_indices.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let player_id = String::read_canonical_le(reader)?;
        validate_id_string(&player_id)?;
        let display_name = String::read_canonical_le(reader)?;
        validate_id_string(&display_name)?;
        let jersey_number_code = u8::read_canonical_le(reader)?;
        if jersey_number_code > 100 {
            return Err(DecodeError::InvalidEnumValue {
                field: "jersey_number_code",
                value: jersey_number_code as u32,
            });
        }
        Ok(Self {
            player_id,
            display_name,
            jersey_number_code,
            physical: PhysicalProfile::read_canonical_le(reader)?,
            attributes: PlayerAttributes38::read_canonical_le(reader)?,
            tendencies: PlayerTendencies::read_canonical_le(reader)?,
            functional: PlayerFunctionalProfile::read_canonical_le(reader)?,
            badge_indices: Vec::<u16>::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompiledRotationStint {
    pub quarter_mask: u8,
    pub start_game_clock_ds: u16,
    pub end_game_clock_ds: u16,
    pub target_lineup_ranks: [u8; 5],
}

impl CanonicalLeBytes for CompiledRotationStint {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.quarter_mask.write_canonical_le(out);
        self.start_game_clock_ds.write_canonical_le(out);
        self.end_game_clock_ds.write_canonical_le(out);
        self.target_lineup_ranks.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            quarter_mask: u8::read_canonical_le(reader)?,
            start_game_clock_ds: u16::read_canonical_le(reader)?,
            end_game_clock_ds: u16::read_canonical_le(reader)?,
            target_lineup_ranks: <[u8; 5]>::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CompiledRotationScript {
    pub stints: Vec<CompiledRotationStint>,
}

impl CanonicalLeBytes for CompiledRotationScript {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.stints.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            stints: Vec::<CompiledRotationStint>::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamTacticsConfig {
    pub pace: u8,
    pub transition_push: u8,
    pub default_spacing: u8,
    pub primary_handlers: InlinePlayerList<5>,
    pub play_weights: Vec<(String, f64)>,
    pub orb_crash_count: u8,
    pub defensive_gameplan_id: GameplanId,
    pub sag_off_3pt_threshold: u8,
    pub foul_up_three_policy: FoulUpThreePolicy,
    pub matchup_overrides: Vec<(u8, u8)>,
    pub full_court_press_clutch: bool,
    pub rotation_script: CompiledRotationScript,
}

impl CanonicalLeBytes for TeamTacticsConfig {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.pace.write_canonical_le(out);
        self.transition_push.write_canonical_le(out);
        self.default_spacing.write_canonical_le(out);
        self.primary_handlers.write_canonical_le(out);
        self.play_weights.write_canonical_le(out);
        self.orb_crash_count.write_canonical_le(out);
        self.defensive_gameplan_id.write_canonical_le(out);
        self.sag_off_3pt_threshold.write_canonical_le(out);
        self.foul_up_three_policy.write_canonical_le(out);
        self.matchup_overrides.write_canonical_le(out);
        self.full_court_press_clutch.write_canonical_le(out);
        self.rotation_script.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            pace: u8::read_canonical_le(reader)?,
            transition_push: u8::read_canonical_le(reader)?,
            default_spacing: u8::read_canonical_le(reader)?,
            primary_handlers: InlinePlayerList::<5>::read_canonical_le(reader)?,
            play_weights: Vec::<(String, f64)>::read_canonical_le(reader)?,
            orb_crash_count: u8::read_canonical_le(reader)?,
            defensive_gameplan_id: GameplanId::read_canonical_le(reader)?,
            sag_off_3pt_threshold: u8::read_canonical_le(reader)?,
            foul_up_three_policy: FoulUpThreePolicy::read_canonical_le(reader)?,
            matchup_overrides: Vec::<(u8, u8)>::read_canonical_le(reader)?,
            full_court_press_clutch: bool::read_canonical_le(reader)?,
            rotation_script: CompiledRotationScript::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompiledTeamSnapshot {
    pub team_id: String,
    pub tactics: TeamTacticsConfig,
    pub players: Vec<CompiledPlayerSnapshot>,
}

impl CanonicalLeBytes for CompiledTeamSnapshot {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.team_id.write_canonical_le(out);
        self.tactics.write_canonical_le(out);
        self.players.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let team_id = String::read_canonical_le(reader)?;
        validate_id_string(&team_id)?;
        Ok(Self {
            team_id,
            tactics: TeamTacticsConfig::read_canonical_le(reader)?,
            players: Vec::<CompiledPlayerSnapshot>::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ConfigHashes {
    pub ruleset_sha256: [u8; 32],
    pub tunables_sha256: [u8; 32],
    pub playbooks_sha256: [u8; 32],
    pub defenses_sha256: [u8; 32],
    pub badges_sha256: [u8; 32],
    pub rosters_sha256: [u8; 32],
}

impl CanonicalLeBytes for ConfigHashes {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.ruleset_sha256.write_canonical_le(out);
        self.tunables_sha256.write_canonical_le(out);
        self.playbooks_sha256.write_canonical_le(out);
        self.defenses_sha256.write_canonical_le(out);
        self.badges_sha256.write_canonical_le(out);
        self.rosters_sha256.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            ruleset_sha256: <[u8; 32]>::read_canonical_le(reader)?,
            tunables_sha256: <[u8; 32]>::read_canonical_le(reader)?,
            playbooks_sha256: <[u8; 32]>::read_canonical_le(reader)?,
            defenses_sha256: <[u8; 32]>::read_canonical_le(reader)?,
            badges_sha256: <[u8; 32]>::read_canonical_le(reader)?,
            rosters_sha256: <[u8; 32]>::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MatchInitSpecV1 {
    pub schema_version: u16,
    pub engine_semver: String,
    pub engine_git_sha: String,
    pub rustc_version_sha256: [u8; 32],
    pub engine_wasm_sha256: [u8; 32],
    pub cargo_lock_sha256: [u8; 32],
    pub seed: u64,
    pub canonical_team_id_override: Option<[u8; 2]>,
    pub intentional_foul_policy: IntentionalFoulPolicy,
    pub config_hashes: ConfigHashes,
    pub ruleset: RuleSet,
    pub tunables: EngineTunables,
    pub playbooks: Vec<CompiledPlaybook>,
    pub defenses: Vec<CompiledDefenseGameplan>,
    pub badges: Vec<BadgeDef>,
    pub home_team: CompiledTeamSnapshot,
    pub away_team: CompiledTeamSnapshot,
    pub state_hash_mode: StateHashMode,
}

impl CanonicalLeBytes for MatchInitSpecV1 {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.schema_version.write_canonical_le(out);
        self.engine_semver.write_canonical_le(out);
        self.engine_git_sha.write_canonical_le(out);
        self.rustc_version_sha256.write_canonical_le(out);
        self.engine_wasm_sha256.write_canonical_le(out);
        self.cargo_lock_sha256.write_canonical_le(out);
        self.seed.write_canonical_le(out);
        self.canonical_team_id_override.write_canonical_le(out);
        self.intentional_foul_policy.write_canonical_le(out);
        self.config_hashes.write_canonical_le(out);
        self.ruleset.write_canonical_le(out);
        self.tunables.write_canonical_le(out);
        self.playbooks.write_canonical_le(out);
        self.defenses.write_canonical_le(out);
        self.badges.write_canonical_le(out);
        self.home_team.write_canonical_le(out);
        self.away_team.write_canonical_le(out);
        self.state_hash_mode.write_canonical_le(out);
    }

    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let schema_version = u16::read_canonical_le(reader)?;
        if schema_version != 1 {
            return Err(DecodeError::UnsupportedVersion {
                version: schema_version,
            });
        }
        Ok(Self {
            schema_version,
            engine_semver: String::read_canonical_le(reader)?,
            engine_git_sha: String::read_canonical_le(reader)?,
            rustc_version_sha256: <[u8; 32]>::read_canonical_le(reader)?,
            engine_wasm_sha256: <[u8; 32]>::read_canonical_le(reader)?,
            cargo_lock_sha256: <[u8; 32]>::read_canonical_le(reader)?,
            seed: u64::read_canonical_le(reader)?,
            canonical_team_id_override: Option::<[u8; 2]>::read_canonical_le(reader)?,
            intentional_foul_policy: IntentionalFoulPolicy::read_canonical_le(reader)?,
            config_hashes: ConfigHashes::read_canonical_le(reader)?,
            ruleset: RuleSet::read_canonical_le(reader)?,
            tunables: EngineTunables::read_canonical_le(reader)?,
            playbooks: Vec::<CompiledPlaybook>::read_canonical_le(reader)?,
            defenses: Vec::<CompiledDefenseGameplan>::read_canonical_le(reader)?,
            badges: Vec::<BadgeDef>::read_canonical_le(reader)?,
            home_team: CompiledTeamSnapshot::read_canonical_le(reader)?,
            away_team: CompiledTeamSnapshot::read_canonical_le(reader)?,
            state_hash_mode: StateHashMode::read_canonical_le(reader)?,
        })
    }
}
