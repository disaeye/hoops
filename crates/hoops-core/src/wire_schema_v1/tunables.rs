use serde::{Deserialize, Serialize};

use crate::canonical_le_bytes::{CanonicalLeBytes, CanonicalReader, DecodeError};

/// Frozen 113-field / 153-scalar `EngineTunables` (§2.4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EngineTunables {
    pub tunables_schema_version: u16,
    pub max_offball_speed_min_max_m_s: [f64; 2],
    pub burst_drain_speed_trigger_u: f64,
    pub burst_drain_speed_offset_u: f64,
    pub burst_drain_speed_coeff: f64,
    pub burst_drain_contact_coeff: f64,
    pub burst_recovery_rate: f64,
    pub stamina_factor_base: f64,
    pub stamina_factor_slope: f64,
    pub cum_stamina_base_drain_per_live_tick: f64,
    pub cum_stamina_burst_load_coeff: f64,
    pub bench_stamina_recovery_per_live_tick: f64,
    pub timeout_stamina_recovery: f64,
    pub period_break_stamina_recovery: f64,
    pub halftime_stamina_recovery: f64,
    pub fatigue_penalty_threshold: f64,
    pub fatigue_penalty_exponent: f64,
    pub rhythm_logit_cap: f64,
    pub timeout_momentum_decay_ratio: f64,
    pub help_reaction_delay_min_max_ds: [u16; 2],
    pub injury_base_prob: f64,
    pub pace_tempo_scale: f64,
    pub transition_push_scale: f64,
    pub shot_zone_bias_3: [f64; 3],
    pub shot_alpha_zone: f64,
    pub shot_alpha_style: f64,
    pub shot_alpha_range: f64,
    pub shot_alpha_contest: f64,
    pub shot_contest_attr_relief: f64,
    pub shot_alpha_fatigue: f64,
    pub shot_alpha_rhythm: f64,
    pub bank_make_max_prob: f64,
    pub swish_base: f64,
    pub swish_contest_slope: f64,
    pub swish_zone_attr_coeff: f64,
    pub swish_prob_min_max: [f64; 2],
    pub airball_base: f64,
    pub airball_contest_coeff: f64,
    pub airball_range_coeff: f64,
    pub airball_attr_relief_coeff: f64,
    pub airball_prob_min_max: [f64; 2],
    pub backboard_only_miss_max_prob: f64,
    pub bank_miss_max_prob: f64,
    pub rebound_sigma_m: f64,
    pub boxout_disadvantage_factor: f64,
    pub rebound_attr_weights: [f64; 4],
    pub rebound_fatigue_penalty_coeff: f64,
    pub rebound_tip_continue_prob: f64,
    pub shooting_foul_contact_base: f64,
    pub reach_foul_contact_base: f64,
    pub charge_block_contact_scale: f64,
    pub pass_speed_min_max: [f64; 2],
    pub pass_intercept_cone_rad: f64,
    pub steal_hazard_base: f64,
    pub gathered_steal_hazard_mult: f64,
    pub hustle_loose_ball_scale: f64,
    pub decision_softmax_temp_base: f64,
    pub shot_release_duration_ds: u16,
    pub wp_avg_poss_ds: f64,
    pub wp_first_poss_ev: f64,
    pub wp_first_poss_var: f64,
    pub wp_pair_variance: f64,
    pub wp_floor_var: f64,
    pub wp_alpha_first_03s_scale: f64,
    pub wp_alpha_first_03s_max_abs_diff: u8,
    pub wp_alpha_first_ramp_ds_and_floor: [f64; 2],
    pub wp_kill_var_ramp_ds_and_floor: [f64; 2],
    pub wp_prune_delta: f64,
    pub wp_mode_max_game_clock_ds: u16,
    pub wp_mode_max_abs_score_diff: u8,
    pub wp_argmax_clock_threshold_ds: u16,
    pub hold_min_shot_clock_ds: u16,
    pub hold_eval_horizon_ds: u16,
    pub hold_reeval_interval_ds: u16,
    pub epv_halfcourt_base: f64,
    pub epv_hold_clock_params: [f64; 3],
    pub epv_gamma_extra: f64,
    pub epv_extra_pass_min_rem_shot_ds: u16,
    pub epv_pivot_delay_cost: f64,
    pub epv_play_base_scale: f64,
    pub epv_play_decay_min: f64,
    pub epv_role_fit_beta: f64,
    pub epv_tendency_bias_cap: f64,
    pub epv_putback: f64,
    pub epv_opp_transition: f64,
    pub epv_opp_halfcourt: f64,
    pub drive_eval_horizon_ds: u16,
    pub pivot_action_duration_ds: u16,
    pub follow_play_min_shot_clock_ds: u16,
    pub epv_hold_horizon_ds: u16,
    pub epv_bad_shot_iq_penalty_scale: f64,
    pub epv_urgent_shot_clock_ds: u16,
    pub epv_urgent_shot_bonus_max: f64,
    pub spacing_field_params: [f64; 5],
    pub backdoor_cut_overplay_dist_m: f64,
    pub help_alpha_strong: f64,
    pub d_pen_norm_speed_m_s: f64,
    pub b_beat_params: [f64; 4],
    pub d_pen_weights: [f64; 7],
    pub lambda_help_params: [f64; 8],
    pub referee_base_error_rate: f64,
    pub home_whistle_bias: f64,
    pub sym_perturbation_sigma: f64,
    pub rotation_p0_stint_lock_live_ticks: u16,
    pub rotation_foul_trouble_nba_q1_h1_q3: [u8; 3],
    pub rotation_foul_trouble_fiba_q1_h1_q3: [u8; 3],
    pub rotation_foul_unlock_q4_clock_ds: u16,
    pub rotation_sub_out_stamina: f64,
    pub rotation_sub_in_stamina: f64,
    pub garbage_time_abs_diff_and_clock_ds: [u16; 2],
    pub intentional_foul_deficit_min_max: [u8; 2],
    pub intentional_foul_clock_margin_ds: u16,
    pub foul_up_three_max_clock_ds: u16,
}

impl EngineTunables {
    pub const FIELD_COUNT: usize = 113;
    pub const SCALAR_COUNT: usize = 153;
}

impl Default for EngineTunables {
    fn default() -> Self {
        Self {
            tunables_schema_version: 1,
            max_offball_speed_min_max_m_s: [6.8, 8.4],
            burst_drain_speed_trigger_u: 0.72,
            burst_drain_speed_offset_u: 0.55,
            burst_drain_speed_coeff: 1.6,
            burst_drain_contact_coeff: 0.9,
            burst_recovery_rate: 1.25,
            stamina_factor_base: 1.25,
            stamina_factor_slope: 0.45,
            cum_stamina_base_drain_per_live_tick: 0.0052,
            cum_stamina_burst_load_coeff: 0.0070,
            bench_stamina_recovery_per_live_tick: 0.0140,
            timeout_stamina_recovery: 3.5,
            period_break_stamina_recovery: 8.0,
            halftime_stamina_recovery: 22.0,
            fatigue_penalty_threshold: 75.0,
            fatigue_penalty_exponent: 1.45,
            rhythm_logit_cap: 0.18,
            timeout_momentum_decay_ratio: 0.60,
            help_reaction_delay_min_max_ds: [1, 3],
            injury_base_prob: 0.0005,
            pace_tempo_scale: 1.0,
            transition_push_scale: 1.0,
            shot_zone_bias_3: [0.55, -0.30, -0.52],
            shot_alpha_zone: 0.42,
            shot_alpha_style: 0.22,
            shot_alpha_range: 0.28,
            shot_alpha_contest: 1.15,
            shot_contest_attr_relief: 0.35,
            shot_alpha_fatigue: 0.45,
            shot_alpha_rhythm: 0.25,
            bank_make_max_prob: 0.45,
            swish_base: 0.72,
            swish_contest_slope: 0.38,
            swish_zone_attr_coeff: 0.12,
            swish_prob_min_max: [0.18, 0.85],
            airball_base: 0.002,
            airball_contest_coeff: 0.095,
            airball_range_coeff: 0.025,
            airball_attr_relief_coeff: 0.002,
            airball_prob_min_max: [0.001, 0.22],
            backboard_only_miss_max_prob: 0.06,
            bank_miss_max_prob: 0.35,
            rebound_sigma_m: 1.85,
            boxout_disadvantage_factor: 0.35,
            rebound_attr_weights: [0.05, 0.45, 0.30, 0.25],
            rebound_fatigue_penalty_coeff: 0.35,
            rebound_tip_continue_prob: 0.20,
            shooting_foul_contact_base: 0.11,
            reach_foul_contact_base: 0.035,
            charge_block_contact_scale: 0.14,
            pass_speed_min_max: [11.0, 18.5],
            pass_intercept_cone_rad: 0.26,
            steal_hazard_base: 0.012,
            gathered_steal_hazard_mult: 1.8,
            hustle_loose_ball_scale: 1.0,
            decision_softmax_temp_base: 0.14,
            shot_release_duration_ds: 3,
            wp_avg_poss_ds: 140.0,
            wp_first_poss_ev: 1.08,
            wp_first_poss_var: 1.18,
            wp_pair_variance: 2.76,
            wp_floor_var: 0.20,
            wp_alpha_first_03s_scale: 0.25,
            wp_alpha_first_03s_max_abs_diff: 4,
            wp_alpha_first_ramp_ds_and_floor: [20.0, 0.35],
            wp_kill_var_ramp_ds_and_floor: [180.0, 0.25],
            wp_prune_delta: 0.05,
            wp_mode_max_game_clock_ds: 1200,
            wp_mode_max_abs_score_diff: 8,
            wp_argmax_clock_threshold_ds: 240,
            hold_min_shot_clock_ds: 15,
            hold_eval_horizon_ds: 20,
            hold_reeval_interval_ds: 5,
            epv_halfcourt_base: 0.96,
            epv_hold_clock_params: [20.0, 160.0, 0.25],
            epv_gamma_extra: 0.92,
            epv_extra_pass_min_rem_shot_ds: 12,
            epv_pivot_delay_cost: 0.04,
            epv_play_base_scale: 1.00,
            epv_play_decay_min: 0.30,
            epv_role_fit_beta: 0.08,
            epv_tendency_bias_cap: 0.12,
            epv_putback: 1.08,
            epv_opp_transition: 1.22,
            epv_opp_halfcourt: 0.96,
            drive_eval_horizon_ds: 8,
            pivot_action_duration_ds: 3,
            follow_play_min_shot_clock_ds: 50,
            epv_hold_horizon_ds: 5,
            epv_bad_shot_iq_penalty_scale: 0.15,
            epv_urgent_shot_clock_ds: 40,
            epv_urgent_shot_bonus_max: 0.18,
            spacing_field_params: [0.35, 3.2, 1.00, 0.45, 0.40],
            backdoor_cut_overplay_dist_m: 1.8,
            help_alpha_strong: 0.35,
            d_pen_norm_speed_m_s: 5.5,
            b_beat_params: [0.20, 0.60, 0.45, 0.75],
            d_pen_weights: [0.55, 0.25, 0.30, 0.70, 0.20, 0.50, 0.50],
            lambda_help_params: [0.20, 0.75, 0.35, 0.65, 1.25, 0.65, 0.15, 0.85],
            referee_base_error_rate: 0.08,
            home_whistle_bias: 0.0,
            sym_perturbation_sigma: 0.002,
            rotation_p0_stint_lock_live_ticks: 1200,
            rotation_foul_trouble_nba_q1_h1_q3: [2, 3, 4],
            rotation_foul_trouble_fiba_q1_h1_q3: [255, 3, 4],
            rotation_foul_unlock_q4_clock_ds: 3600,
            rotation_sub_out_stamina: 62.0,
            rotation_sub_in_stamina: 84.0,
            garbage_time_abs_diff_and_clock_ds: [22, 3600],
            intentional_foul_deficit_min_max: [1, 8],
            intentional_foul_clock_margin_ds: 40,
            foul_up_three_max_clock_ds: 60,
        }
    }
}

impl CanonicalLeBytes for EngineTunables {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.tunables_schema_version.write_canonical_le(out);
        self.max_offball_speed_min_max_m_s.write_canonical_le(out);
        self.burst_drain_speed_trigger_u.write_canonical_le(out);
        self.burst_drain_speed_offset_u.write_canonical_le(out);
        self.burst_drain_speed_coeff.write_canonical_le(out);
        self.burst_drain_contact_coeff.write_canonical_le(out);
        self.burst_recovery_rate.write_canonical_le(out);
        self.stamina_factor_base.write_canonical_le(out);
        self.stamina_factor_slope.write_canonical_le(out);
        self.cum_stamina_base_drain_per_live_tick.write_canonical_le(out);
        self.cum_stamina_burst_load_coeff.write_canonical_le(out);
        self.bench_stamina_recovery_per_live_tick.write_canonical_le(out);
        self.timeout_stamina_recovery.write_canonical_le(out);
        self.period_break_stamina_recovery.write_canonical_le(out);
        self.halftime_stamina_recovery.write_canonical_le(out);
        self.fatigue_penalty_threshold.write_canonical_le(out);
        self.fatigue_penalty_exponent.write_canonical_le(out);
        self.rhythm_logit_cap.write_canonical_le(out);
        self.timeout_momentum_decay_ratio.write_canonical_le(out);
        self.help_reaction_delay_min_max_ds.write_canonical_le(out);
        self.injury_base_prob.write_canonical_le(out);
        self.pace_tempo_scale.write_canonical_le(out);
        self.transition_push_scale.write_canonical_le(out);
        self.shot_zone_bias_3.write_canonical_le(out);
        self.shot_alpha_zone.write_canonical_le(out);
        self.shot_alpha_style.write_canonical_le(out);
        self.shot_alpha_range.write_canonical_le(out);
        self.shot_alpha_contest.write_canonical_le(out);
        self.shot_contest_attr_relief.write_canonical_le(out);
        self.shot_alpha_fatigue.write_canonical_le(out);
        self.shot_alpha_rhythm.write_canonical_le(out);
        self.bank_make_max_prob.write_canonical_le(out);
        self.swish_base.write_canonical_le(out);
        self.swish_contest_slope.write_canonical_le(out);
        self.swish_zone_attr_coeff.write_canonical_le(out);
        self.swish_prob_min_max.write_canonical_le(out);
        self.airball_base.write_canonical_le(out);
        self.airball_contest_coeff.write_canonical_le(out);
        self.airball_range_coeff.write_canonical_le(out);
        self.airball_attr_relief_coeff.write_canonical_le(out);
        self.airball_prob_min_max.write_canonical_le(out);
        self.backboard_only_miss_max_prob.write_canonical_le(out);
        self.bank_miss_max_prob.write_canonical_le(out);
        self.rebound_sigma_m.write_canonical_le(out);
        self.boxout_disadvantage_factor.write_canonical_le(out);
        self.rebound_attr_weights.write_canonical_le(out);
        self.rebound_fatigue_penalty_coeff.write_canonical_le(out);
        self.rebound_tip_continue_prob.write_canonical_le(out);
        self.shooting_foul_contact_base.write_canonical_le(out);
        self.reach_foul_contact_base.write_canonical_le(out);
        self.charge_block_contact_scale.write_canonical_le(out);
        self.pass_speed_min_max.write_canonical_le(out);
        self.pass_intercept_cone_rad.write_canonical_le(out);
        self.steal_hazard_base.write_canonical_le(out);
        self.gathered_steal_hazard_mult.write_canonical_le(out);
        self.hustle_loose_ball_scale.write_canonical_le(out);
        self.decision_softmax_temp_base.write_canonical_le(out);
        self.shot_release_duration_ds.write_canonical_le(out);
        self.wp_avg_poss_ds.write_canonical_le(out);
        self.wp_first_poss_ev.write_canonical_le(out);
        self.wp_first_poss_var.write_canonical_le(out);
        self.wp_pair_variance.write_canonical_le(out);
        self.wp_floor_var.write_canonical_le(out);
        self.wp_alpha_first_03s_scale.write_canonical_le(out);
        self.wp_alpha_first_03s_max_abs_diff.write_canonical_le(out);
        self.wp_alpha_first_ramp_ds_and_floor.write_canonical_le(out);
        self.wp_kill_var_ramp_ds_and_floor.write_canonical_le(out);
        self.wp_prune_delta.write_canonical_le(out);
        self.wp_mode_max_game_clock_ds.write_canonical_le(out);
        self.wp_mode_max_abs_score_diff.write_canonical_le(out);
        self.wp_argmax_clock_threshold_ds.write_canonical_le(out);
        self.hold_min_shot_clock_ds.write_canonical_le(out);
        self.hold_eval_horizon_ds.write_canonical_le(out);
        self.hold_reeval_interval_ds.write_canonical_le(out);
        self.epv_halfcourt_base.write_canonical_le(out);
        self.epv_hold_clock_params.write_canonical_le(out);
        self.epv_gamma_extra.write_canonical_le(out);
        self.epv_extra_pass_min_rem_shot_ds.write_canonical_le(out);
        self.epv_pivot_delay_cost.write_canonical_le(out);
        self.epv_play_base_scale.write_canonical_le(out);
        self.epv_play_decay_min.write_canonical_le(out);
        self.epv_role_fit_beta.write_canonical_le(out);
        self.epv_tendency_bias_cap.write_canonical_le(out);
        self.epv_putback.write_canonical_le(out);
        self.epv_opp_transition.write_canonical_le(out);
        self.epv_opp_halfcourt.write_canonical_le(out);
        self.drive_eval_horizon_ds.write_canonical_le(out);
        self.pivot_action_duration_ds.write_canonical_le(out);
        self.follow_play_min_shot_clock_ds.write_canonical_le(out);
        self.epv_hold_horizon_ds.write_canonical_le(out);
        self.epv_bad_shot_iq_penalty_scale.write_canonical_le(out);
        self.epv_urgent_shot_clock_ds.write_canonical_le(out);
        self.epv_urgent_shot_bonus_max.write_canonical_le(out);
        self.spacing_field_params.write_canonical_le(out);
        self.backdoor_cut_overplay_dist_m.write_canonical_le(out);
        self.help_alpha_strong.write_canonical_le(out);
        self.d_pen_norm_speed_m_s.write_canonical_le(out);
        self.b_beat_params.write_canonical_le(out);
        self.d_pen_weights.write_canonical_le(out);
        self.lambda_help_params.write_canonical_le(out);
        self.referee_base_error_rate.write_canonical_le(out);
        self.home_whistle_bias.write_canonical_le(out);
        self.sym_perturbation_sigma.write_canonical_le(out);
        self.rotation_p0_stint_lock_live_ticks.write_canonical_le(out);
        self.rotation_foul_trouble_nba_q1_h1_q3.write_canonical_le(out);
        self.rotation_foul_trouble_fiba_q1_h1_q3.write_canonical_le(out);
        self.rotation_foul_unlock_q4_clock_ds.write_canonical_le(out);
        self.rotation_sub_out_stamina.write_canonical_le(out);
        self.rotation_sub_in_stamina.write_canonical_le(out);
        self.garbage_time_abs_diff_and_clock_ds.write_canonical_le(out);
        self.intentional_foul_deficit_min_max.write_canonical_le(out);
        self.intentional_foul_clock_margin_ds.write_canonical_le(out);
        self.foul_up_three_max_clock_ds.write_canonical_le(out);
    }

    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            tunables_schema_version: u16::read_canonical_le(reader)?,
            max_offball_speed_min_max_m_s: <[f64; 2]>::read_canonical_le(reader)?,
            burst_drain_speed_trigger_u: f64::read_canonical_le(reader)?,
            burst_drain_speed_offset_u: f64::read_canonical_le(reader)?,
            burst_drain_speed_coeff: f64::read_canonical_le(reader)?,
            burst_drain_contact_coeff: f64::read_canonical_le(reader)?,
            burst_recovery_rate: f64::read_canonical_le(reader)?,
            stamina_factor_base: f64::read_canonical_le(reader)?,
            stamina_factor_slope: f64::read_canonical_le(reader)?,
            cum_stamina_base_drain_per_live_tick: f64::read_canonical_le(reader)?,
            cum_stamina_burst_load_coeff: f64::read_canonical_le(reader)?,
            bench_stamina_recovery_per_live_tick: f64::read_canonical_le(reader)?,
            timeout_stamina_recovery: f64::read_canonical_le(reader)?,
            period_break_stamina_recovery: f64::read_canonical_le(reader)?,
            halftime_stamina_recovery: f64::read_canonical_le(reader)?,
            fatigue_penalty_threshold: f64::read_canonical_le(reader)?,
            fatigue_penalty_exponent: f64::read_canonical_le(reader)?,
            rhythm_logit_cap: f64::read_canonical_le(reader)?,
            timeout_momentum_decay_ratio: f64::read_canonical_le(reader)?,
            help_reaction_delay_min_max_ds: <[u16; 2]>::read_canonical_le(reader)?,
            injury_base_prob: f64::read_canonical_le(reader)?,
            pace_tempo_scale: f64::read_canonical_le(reader)?,
            transition_push_scale: f64::read_canonical_le(reader)?,
            shot_zone_bias_3: <[f64; 3]>::read_canonical_le(reader)?,
            shot_alpha_zone: f64::read_canonical_le(reader)?,
            shot_alpha_style: f64::read_canonical_le(reader)?,
            shot_alpha_range: f64::read_canonical_le(reader)?,
            shot_alpha_contest: f64::read_canonical_le(reader)?,
            shot_contest_attr_relief: f64::read_canonical_le(reader)?,
            shot_alpha_fatigue: f64::read_canonical_le(reader)?,
            shot_alpha_rhythm: f64::read_canonical_le(reader)?,
            bank_make_max_prob: f64::read_canonical_le(reader)?,
            swish_base: f64::read_canonical_le(reader)?,
            swish_contest_slope: f64::read_canonical_le(reader)?,
            swish_zone_attr_coeff: f64::read_canonical_le(reader)?,
            swish_prob_min_max: <[f64; 2]>::read_canonical_le(reader)?,
            airball_base: f64::read_canonical_le(reader)?,
            airball_contest_coeff: f64::read_canonical_le(reader)?,
            airball_range_coeff: f64::read_canonical_le(reader)?,
            airball_attr_relief_coeff: f64::read_canonical_le(reader)?,
            airball_prob_min_max: <[f64; 2]>::read_canonical_le(reader)?,
            backboard_only_miss_max_prob: f64::read_canonical_le(reader)?,
            bank_miss_max_prob: f64::read_canonical_le(reader)?,
            rebound_sigma_m: f64::read_canonical_le(reader)?,
            boxout_disadvantage_factor: f64::read_canonical_le(reader)?,
            rebound_attr_weights: <[f64; 4]>::read_canonical_le(reader)?,
            rebound_fatigue_penalty_coeff: f64::read_canonical_le(reader)?,
            rebound_tip_continue_prob: f64::read_canonical_le(reader)?,
            shooting_foul_contact_base: f64::read_canonical_le(reader)?,
            reach_foul_contact_base: f64::read_canonical_le(reader)?,
            charge_block_contact_scale: f64::read_canonical_le(reader)?,
            pass_speed_min_max: <[f64; 2]>::read_canonical_le(reader)?,
            pass_intercept_cone_rad: f64::read_canonical_le(reader)?,
            steal_hazard_base: f64::read_canonical_le(reader)?,
            gathered_steal_hazard_mult: f64::read_canonical_le(reader)?,
            hustle_loose_ball_scale: f64::read_canonical_le(reader)?,
            decision_softmax_temp_base: f64::read_canonical_le(reader)?,
            shot_release_duration_ds: u16::read_canonical_le(reader)?,
            wp_avg_poss_ds: f64::read_canonical_le(reader)?,
            wp_first_poss_ev: f64::read_canonical_le(reader)?,
            wp_first_poss_var: f64::read_canonical_le(reader)?,
            wp_pair_variance: f64::read_canonical_le(reader)?,
            wp_floor_var: f64::read_canonical_le(reader)?,
            wp_alpha_first_03s_scale: f64::read_canonical_le(reader)?,
            wp_alpha_first_03s_max_abs_diff: u8::read_canonical_le(reader)?,
            wp_alpha_first_ramp_ds_and_floor: <[f64; 2]>::read_canonical_le(reader)?,
            wp_kill_var_ramp_ds_and_floor: <[f64; 2]>::read_canonical_le(reader)?,
            wp_prune_delta: f64::read_canonical_le(reader)?,
            wp_mode_max_game_clock_ds: u16::read_canonical_le(reader)?,
            wp_mode_max_abs_score_diff: u8::read_canonical_le(reader)?,
            wp_argmax_clock_threshold_ds: u16::read_canonical_le(reader)?,
            hold_min_shot_clock_ds: u16::read_canonical_le(reader)?,
            hold_eval_horizon_ds: u16::read_canonical_le(reader)?,
            hold_reeval_interval_ds: u16::read_canonical_le(reader)?,
            epv_halfcourt_base: f64::read_canonical_le(reader)?,
            epv_hold_clock_params: <[f64; 3]>::read_canonical_le(reader)?,
            epv_gamma_extra: f64::read_canonical_le(reader)?,
            epv_extra_pass_min_rem_shot_ds: u16::read_canonical_le(reader)?,
            epv_pivot_delay_cost: f64::read_canonical_le(reader)?,
            epv_play_base_scale: f64::read_canonical_le(reader)?,
            epv_play_decay_min: f64::read_canonical_le(reader)?,
            epv_role_fit_beta: f64::read_canonical_le(reader)?,
            epv_tendency_bias_cap: f64::read_canonical_le(reader)?,
            epv_putback: f64::read_canonical_le(reader)?,
            epv_opp_transition: f64::read_canonical_le(reader)?,
            epv_opp_halfcourt: f64::read_canonical_le(reader)?,
            drive_eval_horizon_ds: u16::read_canonical_le(reader)?,
            pivot_action_duration_ds: u16::read_canonical_le(reader)?,
            follow_play_min_shot_clock_ds: u16::read_canonical_le(reader)?,
            epv_hold_horizon_ds: u16::read_canonical_le(reader)?,
            epv_bad_shot_iq_penalty_scale: f64::read_canonical_le(reader)?,
            epv_urgent_shot_clock_ds: u16::read_canonical_le(reader)?,
            epv_urgent_shot_bonus_max: f64::read_canonical_le(reader)?,
            spacing_field_params: <[f64; 5]>::read_canonical_le(reader)?,
            backdoor_cut_overplay_dist_m: f64::read_canonical_le(reader)?,
            help_alpha_strong: f64::read_canonical_le(reader)?,
            d_pen_norm_speed_m_s: f64::read_canonical_le(reader)?,
            b_beat_params: <[f64; 4]>::read_canonical_le(reader)?,
            d_pen_weights: <[f64; 7]>::read_canonical_le(reader)?,
            lambda_help_params: <[f64; 8]>::read_canonical_le(reader)?,
            referee_base_error_rate: f64::read_canonical_le(reader)?,
            home_whistle_bias: f64::read_canonical_le(reader)?,
            sym_perturbation_sigma: f64::read_canonical_le(reader)?,
            rotation_p0_stint_lock_live_ticks: u16::read_canonical_le(reader)?,
            rotation_foul_trouble_nba_q1_h1_q3: <[u8; 3]>::read_canonical_le(reader)?,
            rotation_foul_trouble_fiba_q1_h1_q3: <[u8; 3]>::read_canonical_le(reader)?,
            rotation_foul_unlock_q4_clock_ds: u16::read_canonical_le(reader)?,
            rotation_sub_out_stamina: f64::read_canonical_le(reader)?,
            rotation_sub_in_stamina: f64::read_canonical_le(reader)?,
            garbage_time_abs_diff_and_clock_ds: <[u16; 2]>::read_canonical_le(reader)?,
            intentional_foul_deficit_min_max: <[u8; 2]>::read_canonical_le(reader)?,
            intentional_foul_clock_margin_ds: u16::read_canonical_le(reader)?,
            foul_up_three_max_clock_ds: u16::read_canonical_le(reader)?,
        })
    }
}
