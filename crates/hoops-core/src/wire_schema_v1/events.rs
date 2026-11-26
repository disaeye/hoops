use serde::{Deserialize, Serialize};

use crate::canonical_le_bytes::{CanonicalLeBytes, CanonicalReader, DecodeError};
use crate::wire_schema_v1::enums::{TriStateShotClock, decode_shot_clock_opt};
use crate::wire_schema_v1::frames::{FootworkTelemetryPublic, ShotTelemetryPublic};

pub const EVENT_HEADER_BYTES: usize = 20;

/// Helper wrapper for `[u32; 30]` in `0x1A GameEnded`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FinalCourtTimeDs(pub [u32; 30]);

impl Default for FinalCourtTimeDs {
    fn default() -> Self {
        Self([0u32; 30])
    }
}

impl Serialize for FinalCourtTimeDs {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple;
        let mut t = serializer.serialize_tuple(30)?;
        for &v in &self.0 {
            t.serialize_element(&v)?;
        }
        t.end()
    }
}

impl<'de> Deserialize<'de> for FinalCourtTimeDs {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = FinalCourtTimeDs;
            fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                write!(f, "30 u32 values")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut arr = [0u32; 30];
                for (i, slot) in arr.iter_mut().enumerate() {
                    *slot = seq
                        .next_element::<u32>()?
                        .ok_or_else(|| serde::de::Error::invalid_length(i, &self))?;
                }
                Ok(FinalCourtTimeDs(arr))
            }
        }
        deserializer.deserialize_tuple(30, V)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum GameRecordEventPayload {
    /// 0x01: 6 B
    PeriodStarted {
        quarter: u8,
        start_mode: u8,
        inbounding_side: u8,
        inbound_spot: u8,
        arrow_side: u8,
    },
    /// 0x02: 4 B
    JumpBallTapped {
        tip_winner_side: u8,
        tapper_roster_idx: u8,
        ball_tapped: u8,
    },
    /// 0x03: 10 B
    PossessionEstablished {
        possession_id: u32,
        side: u8,
        handler_roster_idx: u8,
        trigger_kind: u8,
        new_shot_clock_ds: u16,
    },
    /// 0x04: 6 B
    PassReleased {
        passer_roster_idx: u8,
        target_roster_idx: u8,
        pass_kind: u8,
        is_extra_pass: u8,
        flight_ticks: u16,
    },
    /// 0x05: 4 B
    PassCompleted {
        passer_roster_idx: u8,
        catcher_roster_idx: u8,
        bobbled_recovered: u8,
    },
    /// 0x06: 4 B
    PassIntercepted {
        passer_roster_idx: u8,
        stealer_roster_idx: u8,
        is_deflection_loose: u8,
    },
    /// 0x07: 16 B
    ShotReleased {
        shot_id: u32,
        shooter_roster_idx: u8,
        shot_value: u8,
        release_zone: u8,
        defender_slot: u8,
        nearest_def_dist_mm: u16,
        c_geom_centi: u16,
        release_x_mm: i16,
        release_y_mm: i16,
    },
    /// 0x08: 36 B
    ShotResolved {
        shot_id: u32,
        shooter_roster_idx: u8,
        scorer_team: u8,
        points_awarded: u8,
        outcome_class: u8,
        rim_touched: u8,
        backboard_contact: u8,
        is_blocked: u8,
        blocker_roster_idx: u8,
        is_goaltend: u8,
        assist_roster_idx: u8,
        foul_on_shot: u8,
        telemetry: ShotTelemetryPublic,
    },
    /// 0x09: 12 B
    FreeThrowResolved {
        ft_seq_id: u32,
        shooter_roster_idx: u8,
        team_side: u8,
        made: u8,
        shot_number: u8,
        total_shots: u8,
        is_technical_or_flagrant: u8,
        post_ft_outcome_tag: u8,
        ft_flags: u8,
    },
    /// 0x0A: 8 B
    ReboundSecured {
        rebounder_roster_idx: u8,
        rebound_team: u8,
        is_offensive: u8,
        is_deadball_team_reb: u8,
        scramble_origin: u8,
        new_shot_clock_ds: u16,
    },
    /// 0x0B: 6 B
    TurnoverCommitted {
        offender_roster_idx: u8,
        team_side: u8,
        turnover_rule_code: u16,
        stealer_roster_idx: u8,
        is_live_steal: u8,
    },
    /// 0x0C: 20 B
    ViolationTriggered {
        offender_roster_idx: u8,
        team_side: u8,
        rule_code: u16,
        footwork: FootworkTelemetryPublic,
    },
    /// 0x0D: 12 B
    RefereeInitialCall {
        ruling_id: u64,
        offending_side: u8,
        offender_roster_idx: u8,
        initial_rule_code: u16,
    },
    /// 0x0E: 24 B
    ChallengeWindowOpenedRecord {
        ruling_id: u64,
        whistle_tick: u32,
        window_open_tick: u32,
        window_epoch: u32,
        initial_rule_code: u16,
        eligible_sides_mask: u8,
        window_kind_tag: u8,
    },
    /// 0x0F: 24 B
    CoachChallengeRequested {
        ruling_id: u64,
        target_tag: u8,
        requester: u8,
        request_seq: u32,
        target_event_seq: u64,
    },
    /// 0x10: 16 B
    CoachChallengePendingGrant {
        ruling_id: u64,
        requester: u8,
        request_seq: u32,
    },
    /// 0x11: 16 B
    CoachChallengeWithdrawn {
        ruling_id: u64,
        requester: u8,
        request_seq: u32,
    },
    /// 0x12: 20 B
    CoachChallengeReviewed {
        ruling_id: u64,
        requester: u8,
        review_origin: u8,
        overturned: u8,
        reason_code: u8,
        timeout_charged_side: u8,
        final_rule_code: u16,
        adjusted_game_clock_ds: u16,
        new_shot_clock_ds: u16,
    },
    /// 0x13: 20 B
    RulingFinalized {
        ruling_id: u64,
        final_rule_code: u16,
        offending_side: u8,
        offender_roster_idx: u8,
        points_delta_signed: i8,
        ft_shots_enqueued: u8,
        resulting_possession_side: u8,
        foul_out_or_ejected_flag: u8,
        restored_game_clock_ds: u16,
        new_shot_clock_ds: u16,
    },
    /// 0x14: 12 B
    UnchallengeableChallengeChargedTimeout {
        ruling_id: u64,
        requester: u8,
        timeout_charged: u8,
        remaining_timeouts: u8,
        remaining_challenges: u8,
    },
    /// 0x15: 4 B
    TimeoutGranted {
        reason_tag: u8,
        charged_side: u8,
        home_to_rem: u8,
        away_to_rem: u8,
    },
    /// 0x16: 8 B
    SubstitutionApplied {
        team_side: u8,
        out_roster_idx: u8,
        in_roster_idx: u8,
        sub_priority_tier: u8,
        out_player_cum_court_time_ds: u32,
    },
    /// 0x17: 4 B
    DeadTime {
        duration_ds: u16,
        reason_tag: u8,
    },
    /// 0x18: 12 B
    AnchoredCommandAbortedRecord {
        ingress_seq: u64,
        side: u8,
        abort_reason_code: u8,
        cmd_tag: u8,
    },
    /// 0x19: 24 B
    ScoringEventAmended {
        original_shot_event_seq: u64,
        shooter_roster_idx: u8,
        assist_roster_idx: u8,
        team_side: u8,
        amend_kind: u8,
        old_points: u8,
        new_points: u8,
        pts_delta: i8,
        fga_delta: i8,
        fgm_delta: i8,
        tpa_delta: i8,
        tpm_delta: i8,
        ast_delta: i8,
        blocker_roster_idx: u8,
        blk_delta: i8,
    },
    /// 0x1A: 132 B
    GameEnded {
        score_home_official: u16,
        score_away_official: u16,
        on_court_score_home: u16,
        on_court_score_away: u16,
        winner_side: u8,
        end_reason: u8,
        final_quarter: u8,
        final_court_time_ds: FinalCourtTimeDs,
    },
    /// 0x1B: 16 B
    FoulAssessedRecord {
        ruling_id: u64,
        rule_code: u16,
        offending_side: u8,
        offender_roster_idx: u8,
        foul_category: u8,
        counts_team_foul: u8,
        resulting_personal_fouls: u8,
        ejected_or_fouled_out_flags: u8,
    },
    /// 0x1C: 20 B
    ClockAdjusted {
        adjust_reason: u8,
        delta_game_clock_ds_signed: i16,
        new_game_clock_ds: u16,
        new_shot_clock_ds: u16,
        affected_slot_mask: u16,
        affected_roster_indices: [u8; 10],
    },
}

impl GameRecordEventPayload {
    pub fn event_type_code(&self) -> u8 {
        match self {
            Self::PeriodStarted { .. } => 0x01,
            Self::JumpBallTapped { .. } => 0x02,
            Self::PossessionEstablished { .. } => 0x03,
            Self::PassReleased { .. } => 0x04,
            Self::PassCompleted { .. } => 0x05,
            Self::PassIntercepted { .. } => 0x06,
            Self::ShotReleased { .. } => 0x07,
            Self::ShotResolved { .. } => 0x08,
            Self::FreeThrowResolved { .. } => 0x09,
            Self::ReboundSecured { .. } => 0x0A,
            Self::TurnoverCommitted { .. } => 0x0B,
            Self::ViolationTriggered { .. } => 0x0C,
            Self::RefereeInitialCall { .. } => 0x0D,
            Self::ChallengeWindowOpenedRecord { .. } => 0x0E,
            Self::CoachChallengeRequested { .. } => 0x0F,
            Self::CoachChallengePendingGrant { .. } => 0x10,
            Self::CoachChallengeWithdrawn { .. } => 0x11,
            Self::CoachChallengeReviewed { .. } => 0x12,
            Self::RulingFinalized { .. } => 0x13,
            Self::UnchallengeableChallengeChargedTimeout { .. } => 0x14,
            Self::TimeoutGranted { .. } => 0x15,
            Self::SubstitutionApplied { .. } => 0x16,
            Self::DeadTime { .. } => 0x17,
            Self::AnchoredCommandAbortedRecord { .. } => 0x18,
            Self::ScoringEventAmended { .. } => 0x19,
            Self::GameEnded { .. } => 0x1A,
            Self::FoulAssessedRecord { .. } => 0x1B,
            Self::ClockAdjusted { .. } => 0x1C,
        }
    }

    pub fn expected_payload_len(code: u8) -> Option<u16> {
        match code {
            0x01 => Some(6),
            0x02 => Some(4),
            0x03 => Some(10),
            0x04 => Some(6),
            0x05 => Some(4),
            0x06 => Some(4),
            0x07 => Some(16),
            0x08 => Some(36),
            0x09 => Some(12),
            0x0A => Some(8),
            0x0B => Some(6),
            0x0C => Some(20),
            0x0D => Some(12),
            0x0E => Some(24),
            0x0F => Some(24),
            0x10 => Some(16),
            0x11 => Some(16),
            0x12 => Some(20),
            0x13 => Some(20),
            0x14 => Some(12),
            0x15 => Some(4),
            0x16 => Some(8),
            0x17 => Some(4),
            0x18 => Some(12),
            0x19 => Some(24),
            0x1A => Some(132),
            0x1B => Some(16),
            0x1C => Some(20),
            _ => None,
        }
    }

    pub fn write_le(&self, out: &mut Vec<u8>) {
        match self {
            Self::PeriodStarted {
                quarter,
                start_mode,
                inbounding_side,
                inbound_spot,
                arrow_side,
            } => {
                out.push(*quarter);
                out.push(*start_mode);
                out.push(*inbounding_side);
                out.push(*inbound_spot);
                out.push(*arrow_side);
                out.push(0);
            }
            Self::JumpBallTapped {
                tip_winner_side,
                tapper_roster_idx,
                ball_tapped,
            } => {
                out.push(*tip_winner_side);
                out.push(*tapper_roster_idx);
                out.push(*ball_tapped);
                out.push(0);
            }
            Self::PossessionEstablished {
                possession_id,
                side,
                handler_roster_idx,
                trigger_kind,
                new_shot_clock_ds,
            } => {
                out.extend_from_slice(&possession_id.to_le_bytes());
                out.push(*side);
                out.push(*handler_roster_idx);
                out.push(*trigger_kind);
                out.push(0);
                out.extend_from_slice(&new_shot_clock_ds.to_le_bytes());
            }
            Self::PassReleased {
                passer_roster_idx,
                target_roster_idx,
                pass_kind,
                is_extra_pass,
                flight_ticks,
            } => {
                out.push(*passer_roster_idx);
                out.push(*target_roster_idx);
                out.push(*pass_kind);
                out.push(*is_extra_pass);
                out.extend_from_slice(&flight_ticks.to_le_bytes());
            }
            Self::PassCompleted {
                passer_roster_idx,
                catcher_roster_idx,
                bobbled_recovered,
            } => {
                out.push(*passer_roster_idx);
                out.push(*catcher_roster_idx);
                out.push(*bobbled_recovered);
                out.push(0);
            }
            Self::PassIntercepted {
                passer_roster_idx,
                stealer_roster_idx,
                is_deflection_loose,
            } => {
                out.push(*passer_roster_idx);
                out.push(*stealer_roster_idx);
                out.push(*is_deflection_loose);
                out.push(0);
            }
            Self::ShotReleased {
                shot_id,
                shooter_roster_idx,
                shot_value,
                release_zone,
                defender_slot,
                nearest_def_dist_mm,
                c_geom_centi,
                release_x_mm,
                release_y_mm,
            } => {
                out.extend_from_slice(&shot_id.to_le_bytes());
                out.push(*shooter_roster_idx);
                out.push(*shot_value);
                out.push(*release_zone);
                out.push(*defender_slot);
                out.extend_from_slice(&nearest_def_dist_mm.to_le_bytes());
                out.extend_from_slice(&c_geom_centi.to_le_bytes());
                out.extend_from_slice(&release_x_mm.to_le_bytes());
                out.extend_from_slice(&release_y_mm.to_le_bytes());
            }
            Self::ShotResolved {
                shot_id,
                shooter_roster_idx,
                scorer_team,
                points_awarded,
                outcome_class,
                rim_touched,
                backboard_contact,
                is_blocked,
                blocker_roster_idx,
                is_goaltend,
                assist_roster_idx,
                foul_on_shot,
                telemetry,
            } => {
                out.extend_from_slice(&shot_id.to_le_bytes());
                out.push(*shooter_roster_idx);
                out.push(*scorer_team);
                out.push(*points_awarded);
                out.push(*outcome_class);
                out.push(*rim_touched);
                out.push(*backboard_contact);
                out.push(*is_blocked);
                out.push(*blocker_roster_idx);
                out.push(*is_goaltend);
                out.push(*assist_roster_idx);
                out.push(*foul_on_shot);
                out.push(0);
                telemetry.write_le(out);
            }
            Self::FreeThrowResolved {
                ft_seq_id,
                shooter_roster_idx,
                team_side,
                made,
                shot_number,
                total_shots,
                is_technical_or_flagrant,
                post_ft_outcome_tag,
                ft_flags,
            } => {
                out.extend_from_slice(&ft_seq_id.to_le_bytes());
                out.push(*shooter_roster_idx);
                out.push(*team_side);
                out.push(*made);
                out.push(*shot_number);
                out.push(*total_shots);
                out.push(*is_technical_or_flagrant);
                out.push(*post_ft_outcome_tag);
                out.push(*ft_flags & 0x03);
            }
            Self::ReboundSecured {
                rebounder_roster_idx,
                rebound_team,
                is_offensive,
                is_deadball_team_reb,
                scramble_origin,
                new_shot_clock_ds,
            } => {
                out.push(*rebounder_roster_idx);
                out.push(*rebound_team);
                out.push(*is_offensive);
                out.push(*is_deadball_team_reb);
                out.push(*scramble_origin);
                out.extend_from_slice(&new_shot_clock_ds.to_le_bytes());
                out.push(0);
            }
            Self::TurnoverCommitted {
                offender_roster_idx,
                team_side,
                turnover_rule_code,
                stealer_roster_idx,
                is_live_steal,
            } => {
                out.push(*offender_roster_idx);
                out.push(*team_side);
                out.extend_from_slice(&turnover_rule_code.to_le_bytes());
                out.push(*stealer_roster_idx);
                out.push(*is_live_steal);
            }
            Self::ViolationTriggered {
                offender_roster_idx,
                team_side,
                rule_code,
                footwork,
            } => {
                out.push(*offender_roster_idx);
                out.push(*team_side);
                out.extend_from_slice(&rule_code.to_le_bytes());
                footwork.write_le(out);
            }
            Self::RefereeInitialCall {
                ruling_id,
                offending_side,
                offender_roster_idx,
                initial_rule_code,
            } => {
                out.extend_from_slice(&ruling_id.to_le_bytes());
                out.push(*offending_side);
                out.push(*offender_roster_idx);
                out.extend_from_slice(&initial_rule_code.to_le_bytes());
            }
            Self::ChallengeWindowOpenedRecord {
                ruling_id,
                whistle_tick,
                window_open_tick,
                window_epoch,
                initial_rule_code,
                eligible_sides_mask,
                window_kind_tag,
            } => {
                out.extend_from_slice(&ruling_id.to_le_bytes());
                out.extend_from_slice(&whistle_tick.to_le_bytes());
                out.extend_from_slice(&window_open_tick.to_le_bytes());
                out.extend_from_slice(&window_epoch.to_le_bytes());
                out.extend_from_slice(&initial_rule_code.to_le_bytes());
                out.push(*eligible_sides_mask);
                out.push(*window_kind_tag);
            }
            Self::CoachChallengeRequested {
                ruling_id,
                target_tag,
                requester,
                request_seq,
                target_event_seq,
            } => {
                out.extend_from_slice(&ruling_id.to_le_bytes());
                out.push(*target_tag);
                out.push(*requester);
                out.extend_from_slice(&[0u8; 2]);
                out.extend_from_slice(&request_seq.to_le_bytes());
                out.extend_from_slice(&target_event_seq.to_le_bytes());
            }
            Self::CoachChallengePendingGrant {
                ruling_id,
                requester,
                request_seq,
            } => {
                out.extend_from_slice(&ruling_id.to_le_bytes());
                out.push(*requester);
                out.extend_from_slice(&[0u8; 3]);
                out.extend_from_slice(&request_seq.to_le_bytes());
            }
            Self::CoachChallengeWithdrawn {
                ruling_id,
                requester,
                request_seq,
            } => {
                out.extend_from_slice(&ruling_id.to_le_bytes());
                out.push(*requester);
                out.extend_from_slice(&[0u8; 3]);
                out.extend_from_slice(&request_seq.to_le_bytes());
            }
            Self::CoachChallengeReviewed {
                ruling_id,
                requester,
                review_origin,
                overturned,
                reason_code,
                timeout_charged_side,
                final_rule_code,
                adjusted_game_clock_ds,
                new_shot_clock_ds,
            } => {
                out.extend_from_slice(&ruling_id.to_le_bytes());
                out.push(*requester);
                out.push(*review_origin);
                out.push(*overturned);
                out.push(*reason_code);
                out.push(*timeout_charged_side);
                out.push(0);
                out.extend_from_slice(&final_rule_code.to_le_bytes());
                out.extend_from_slice(&adjusted_game_clock_ds.to_le_bytes());
                out.extend_from_slice(&new_shot_clock_ds.to_le_bytes());
            }
            Self::RulingFinalized {
                ruling_id,
                final_rule_code,
                offending_side,
                offender_roster_idx,
                points_delta_signed,
                ft_shots_enqueued,
                resulting_possession_side,
                foul_out_or_ejected_flag,
                restored_game_clock_ds,
                new_shot_clock_ds,
            } => {
                out.extend_from_slice(&ruling_id.to_le_bytes());
                out.extend_from_slice(&final_rule_code.to_le_bytes());
                out.push(*offending_side);
                out.push(*offender_roster_idx);
                out.push(*points_delta_signed as u8);
                out.push(*ft_shots_enqueued);
                out.push(*resulting_possession_side);
                out.push(*foul_out_or_ejected_flag);
                out.extend_from_slice(&restored_game_clock_ds.to_le_bytes());
                out.extend_from_slice(&new_shot_clock_ds.to_le_bytes());
            }
            Self::UnchallengeableChallengeChargedTimeout {
                ruling_id,
                requester,
                timeout_charged,
                remaining_timeouts,
                remaining_challenges,
            } => {
                out.extend_from_slice(&ruling_id.to_le_bytes());
                out.push(*requester);
                out.push(*timeout_charged);
                out.push(*remaining_timeouts);
                out.push(*remaining_challenges);
            }
            Self::TimeoutGranted {
                reason_tag,
                charged_side,
                home_to_rem,
                away_to_rem,
            } => {
                out.push(*reason_tag);
                out.push(*charged_side);
                out.push(*home_to_rem);
                out.push(*away_to_rem);
            }
            Self::SubstitutionApplied {
                team_side,
                out_roster_idx,
                in_roster_idx,
                sub_priority_tier,
                out_player_cum_court_time_ds,
            } => {
                out.push(*team_side);
                out.push(*out_roster_idx);
                out.push(*in_roster_idx);
                out.push(*sub_priority_tier);
                out.extend_from_slice(&out_player_cum_court_time_ds.to_le_bytes());
            }
            Self::DeadTime {
                duration_ds,
                reason_tag,
            } => {
                out.extend_from_slice(&duration_ds.to_le_bytes());
                out.push(*reason_tag);
                out.push(0);
            }
            Self::AnchoredCommandAbortedRecord {
                ingress_seq,
                side,
                abort_reason_code,
                cmd_tag,
            } => {
                out.extend_from_slice(&ingress_seq.to_le_bytes());
                out.push(*side);
                out.push(*abort_reason_code);
                out.push(*cmd_tag);
                out.push(0);
            }
            Self::ScoringEventAmended {
                original_shot_event_seq,
                shooter_roster_idx,
                assist_roster_idx,
                team_side,
                amend_kind,
                old_points,
                new_points,
                pts_delta,
                fga_delta,
                fgm_delta,
                tpa_delta,
                tpm_delta,
                ast_delta,
                blocker_roster_idx,
                blk_delta,
            } => {
                out.extend_from_slice(&original_shot_event_seq.to_le_bytes());
                out.push(*shooter_roster_idx);
                out.push(*assist_roster_idx);
                out.push(*team_side);
                out.push(*amend_kind);
                out.push(*old_points);
                out.push(*new_points);
                out.push(*pts_delta as u8);
                out.push(*fga_delta as u8);
                out.push(*fgm_delta as u8);
                out.push(*tpa_delta as u8);
                out.push(*tpm_delta as u8);
                out.push(*ast_delta as u8);
                out.push(*blocker_roster_idx);
                out.push(*blk_delta as u8);
                out.extend_from_slice(&[0u8; 2]);
            }
            Self::GameEnded {
                score_home_official,
                score_away_official,
                on_court_score_home,
                on_court_score_away,
                winner_side,
                end_reason,
                final_quarter,
                final_court_time_ds,
            } => {
                out.extend_from_slice(&score_home_official.to_le_bytes());
                out.extend_from_slice(&score_away_official.to_le_bytes());
                out.extend_from_slice(&on_court_score_home.to_le_bytes());
                out.extend_from_slice(&on_court_score_away.to_le_bytes());
                out.push(*winner_side);
                out.push(*end_reason);
                out.push(*final_quarter);
                out.push(0);
                for &v in &final_court_time_ds.0 {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
            Self::FoulAssessedRecord {
                ruling_id,
                rule_code,
                offending_side,
                offender_roster_idx,
                foul_category,
                counts_team_foul,
                resulting_personal_fouls,
                ejected_or_fouled_out_flags,
            } => {
                out.extend_from_slice(&ruling_id.to_le_bytes());
                out.extend_from_slice(&rule_code.to_le_bytes());
                out.push(*offending_side);
                out.push(*offender_roster_idx);
                out.push(*foul_category);
                out.push(*counts_team_foul);
                out.push(*resulting_personal_fouls);
                out.push(*ejected_or_fouled_out_flags);
            }
            Self::ClockAdjusted {
                adjust_reason,
                delta_game_clock_ds_signed,
                new_game_clock_ds,
                new_shot_clock_ds,
                affected_slot_mask,
                affected_roster_indices,
            } => {
                out.push(*adjust_reason);
                out.extend_from_slice(&delta_game_clock_ds_signed.to_le_bytes());
                out.extend_from_slice(&new_game_clock_ds.to_le_bytes());
                out.extend_from_slice(&new_shot_clock_ds.to_le_bytes());
                out.extend_from_slice(&(affected_slot_mask & 0x03FF).to_le_bytes());
                out.push(0);
                out.extend_from_slice(affected_roster_indices);
            }
        }
    }

    pub fn read_le(r: &mut CanonicalReader<'_>, code: u8) -> Result<Self, DecodeError> {
        match code {
            0x01 => {
                let quarter = r.read_u8()?;
                let start_mode = r.read_u8()?;
                if start_mode > 2 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "PeriodStarted.start_mode",
                        value: start_mode as u32,
                    });
                }
                let inbounding_side = r.read_u8()?;
                let inbound_spot = r.read_u8()?;
                if inbound_spot > 6 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "PeriodStarted.inbound_spot",
                        value: inbound_spot as u32,
                    });
                }
                let arrow_side = r.read_u8()?;
                if arrow_side > 1 && arrow_side != 0xFF {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "PeriodStarted.arrow_side",
                        value: arrow_side as u32,
                    });
                }
                r.read_zero_pad(1)?;
                Ok(Self::PeriodStarted {
                    quarter,
                    start_mode,
                    inbounding_side,
                    inbound_spot,
                    arrow_side,
                })
            }
            0x02 => {
                let tip_winner_side = r.read_u8()?;
                let tapper_roster_idx = r.read_u8()?;
                let ball_tapped = r.read_u8()?;
                r.read_zero_pad(1)?;
                Ok(Self::JumpBallTapped {
                    tip_winner_side,
                    tapper_roster_idx,
                    ball_tapped,
                })
            }
            0x03 => {
                let possession_id = u32::read_canonical_le(r)?;
                let side = r.read_u8()?;
                let handler_roster_idx = r.read_u8()?;
                let trigger_kind = r.read_u8()?;
                if trigger_kind > 4 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "PossessionEstablished.trigger_kind",
                        value: trigger_kind as u32,
                    });
                }
                r.read_zero_pad(1)?;
                let new_shot_clock_ds = u16::read_canonical_le(r)?;
                let _ = TriStateShotClock::from_wire(new_shot_clock_ds, true)?;
                Ok(Self::PossessionEstablished {
                    possession_id,
                    side,
                    handler_roster_idx,
                    trigger_kind,
                    new_shot_clock_ds,
                })
            }
            0x04 => {
                let passer_roster_idx = r.read_u8()?;
                let target_roster_idx = r.read_u8()?;
                let pass_kind = r.read_u8()?;
                if pass_kind > 4 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "PassReleased.pass_kind",
                        value: pass_kind as u32,
                    });
                }
                let is_extra_pass = r.read_u8()?;
                let flight_ticks = u16::read_canonical_le(r)?;
                Ok(Self::PassReleased {
                    passer_roster_idx,
                    target_roster_idx,
                    pass_kind,
                    is_extra_pass,
                    flight_ticks,
                })
            }
            0x05 => {
                let passer_roster_idx = r.read_u8()?;
                let catcher_roster_idx = r.read_u8()?;
                let bobbled_recovered = r.read_u8()?;
                r.read_zero_pad(1)?;
                Ok(Self::PassCompleted {
                    passer_roster_idx,
                    catcher_roster_idx,
                    bobbled_recovered,
                })
            }
            0x06 => {
                let passer_roster_idx = r.read_u8()?;
                let stealer_roster_idx = r.read_u8()?;
                let is_deflection_loose = r.read_u8()?;
                r.read_zero_pad(1)?;
                Ok(Self::PassIntercepted {
                    passer_roster_idx,
                    stealer_roster_idx,
                    is_deflection_loose,
                })
            }
            0x07 => {
                let shot_id = u32::read_canonical_le(r)?;
                let shooter_roster_idx = r.read_u8()?;
                let shot_value = r.read_u8()?;
                if shot_value != 2 && shot_value != 3 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "ShotReleased.shot_value",
                        value: shot_value as u32,
                    });
                }
                let release_zone = r.read_u8()?;
                if release_zone > 4 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "ShotReleased.release_zone",
                        value: release_zone as u32,
                    });
                }
                let defender_slot = r.read_u8()?;
                let nearest_def_dist_mm = u16::read_canonical_le(r)?;
                let c_geom_centi = u16::read_canonical_le(r)?;
                let release_x_mm = i16::read_canonical_le(r)?;
                let release_y_mm = i16::read_canonical_le(r)?;
                Ok(Self::ShotReleased {
                    shot_id,
                    shooter_roster_idx,
                    shot_value,
                    release_zone,
                    defender_slot,
                    nearest_def_dist_mm,
                    c_geom_centi,
                    release_x_mm,
                    release_y_mm,
                })
            }
            0x08 => {
                let shot_id = u32::read_canonical_le(r)?;
                let shooter_roster_idx = r.read_u8()?;
                let scorer_team = r.read_u8()?;
                let points_awarded = r.read_u8()?;
                let outcome_class = r.read_u8()?;
                if outcome_class > 9 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "ShotResolved.outcome_class",
                        value: outcome_class as u32,
                    });
                }
                let rim_touched = r.read_u8()?;
                let backboard_contact = r.read_u8()?;
                if backboard_contact > 2 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "ShotResolved.backboard_contact",
                        value: backboard_contact as u32,
                    });
                }
                let is_blocked = r.read_u8()?;
                let blocker_roster_idx = r.read_u8()?;
                let is_goaltend = r.read_u8()?;
                let assist_roster_idx = r.read_u8()?;
                let foul_on_shot = r.read_u8()?;
                r.read_zero_pad(1)?;
                let telemetry = ShotTelemetryPublic::read_le(r)?;
                Ok(Self::ShotResolved {
                    shot_id,
                    shooter_roster_idx,
                    scorer_team,
                    points_awarded,
                    outcome_class,
                    rim_touched,
                    backboard_contact,
                    is_blocked,
                    blocker_roster_idx,
                    is_goaltend,
                    assist_roster_idx,
                    foul_on_shot,
                    telemetry,
                })
            }
            0x09 => {
                let ft_seq_id = u32::read_canonical_le(r)?;
                let shooter_roster_idx = r.read_u8()?;
                let team_side = r.read_u8()?;
                let made = r.read_u8()?;
                let shot_number = r.read_u8()?;
                let total_shots = r.read_u8()?;
                let is_technical_or_flagrant = r.read_u8()?;
                let post_ft_outcome_tag = r.read_u8()?;
                if post_ft_outcome_tag > 5 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "FreeThrowResolved.post_ft_outcome_tag",
                        value: post_ft_outcome_tag as u32,
                    });
                }
                let flags_off = r.position();
                let ft_flags = r.read_u8()?;
                if (ft_flags & 0xFC) != 0 {
                    return Err(DecodeError::NonZeroPadding {
                        offset: flags_off,
                        value: ft_flags,
                    });
                }
                Ok(Self::FreeThrowResolved {
                    ft_seq_id,
                    shooter_roster_idx,
                    team_side,
                    made,
                    shot_number,
                    total_shots,
                    is_technical_or_flagrant,
                    post_ft_outcome_tag,
                    ft_flags,
                })
            }
            0x0A => {
                let rebounder_roster_idx = r.read_u8()?;
                let rebound_team = r.read_u8()?;
                let is_offensive = r.read_u8()?;
                let is_deadball_team_reb = r.read_u8()?;
                let scramble_origin = r.read_u8()?;
                if scramble_origin > 3 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "ReboundSecured.scramble_origin",
                        value: scramble_origin as u32,
                    });
                }
                let new_shot_clock_ds = u16::read_canonical_le(r)?;
                let _ = TriStateShotClock::from_wire(new_shot_clock_ds, true)?;
                r.read_zero_pad(1)?;
                Ok(Self::ReboundSecured {
                    rebounder_roster_idx,
                    rebound_team,
                    is_offensive,
                    is_deadball_team_reb,
                    scramble_origin,
                    new_shot_clock_ds,
                })
            }
            0x0B => Ok(Self::TurnoverCommitted {
                offender_roster_idx: r.read_u8()?,
                team_side: r.read_u8()?,
                turnover_rule_code: u16::read_canonical_le(r)?,
                stealer_roster_idx: r.read_u8()?,
                is_live_steal: r.read_u8()?,
            }),
            0x0C => Ok(Self::ViolationTriggered {
                offender_roster_idx: r.read_u8()?,
                team_side: r.read_u8()?,
                rule_code: u16::read_canonical_le(r)?,
                footwork: FootworkTelemetryPublic::read_le(r)?,
            }),
            0x0D => Ok(Self::RefereeInitialCall {
                ruling_id: u64::read_canonical_le(r)?,
                offending_side: r.read_u8()?,
                offender_roster_idx: r.read_u8()?,
                initial_rule_code: u16::read_canonical_le(r)?,
            }),
            0x0E => {
                let ruling_id = u64::read_canonical_le(r)?;
                let whistle_tick = u32::read_canonical_le(r)?;
                let window_open_tick = u32::read_canonical_le(r)?;
                let window_epoch = u32::read_canonical_le(r)?;
                let initial_rule_code = u16::read_canonical_le(r)?;
                let eligible_sides_mask = r.read_u8()?;
                let window_kind_tag = r.read_u8()?;
                if window_kind_tag > 4 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "ChallengeWindowOpenedRecord.window_kind_tag",
                        value: window_kind_tag as u32,
                    });
                }
                Ok(Self::ChallengeWindowOpenedRecord {
                    ruling_id,
                    whistle_tick,
                    window_open_tick,
                    window_epoch,
                    initial_rule_code,
                    eligible_sides_mask,
                    window_kind_tag,
                })
            }
            0x0F => {
                let ruling_id = u64::read_canonical_le(r)?;
                let target_tag = r.read_u8()?;
                if target_tag > 1 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "CoachChallengeRequested.target_tag",
                        value: target_tag as u32,
                    });
                }
                let requester = r.read_u8()?;
                r.read_zero_pad(2)?;
                let request_seq = u32::read_canonical_le(r)?;
                let target_event_seq = u64::read_canonical_le(r)?;
                Ok(Self::CoachChallengeRequested {
                    ruling_id,
                    target_tag,
                    requester,
                    request_seq,
                    target_event_seq,
                })
            }
            0x10 => {
                let ruling_id = u64::read_canonical_le(r)?;
                let requester = r.read_u8()?;
                r.read_zero_pad(3)?;
                let request_seq = u32::read_canonical_le(r)?;
                Ok(Self::CoachChallengePendingGrant {
                    ruling_id,
                    requester,
                    request_seq,
                })
            }
            0x11 => {
                let ruling_id = u64::read_canonical_le(r)?;
                let requester = r.read_u8()?;
                r.read_zero_pad(3)?;
                let request_seq = u32::read_canonical_le(r)?;
                Ok(Self::CoachChallengeWithdrawn {
                    ruling_id,
                    requester,
                    request_seq,
                })
            }
            0x12 => {
                let ruling_id = u64::read_canonical_le(r)?;
                let requester = r.read_u8()?;
                let review_origin = r.read_u8()?;
                let overturned = r.read_u8()?;
                let reason_code = r.read_u8()?;
                if reason_code > 4 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "CoachChallengeReviewed.reason_code",
                        value: reason_code as u32,
                    });
                }
                let timeout_charged_side = r.read_u8()?;
                r.read_zero_pad(1)?;
                let final_rule_code = u16::read_canonical_le(r)?;
                let adjusted_game_clock_ds = u16::read_canonical_le(r)?;
                let new_shot_clock_ds = u16::read_canonical_le(r)?;
                let _ = TriStateShotClock::from_wire(new_shot_clock_ds, true)?;
                Ok(Self::CoachChallengeReviewed {
                    ruling_id,
                    requester,
                    review_origin,
                    overturned,
                    reason_code,
                    timeout_charged_side,
                    final_rule_code,
                    adjusted_game_clock_ds,
                    new_shot_clock_ds,
                })
            }
            0x13 => {
                let ruling_id = u64::read_canonical_le(r)?;
                let final_rule_code = u16::read_canonical_le(r)?;
                let offending_side = r.read_u8()?;
                let offender_roster_idx = r.read_u8()?;
                let points_delta_signed = r.read_i8()?;
                let ft_shots_enqueued = r.read_u8()?;
                let resulting_possession_side = r.read_u8()?;
                let foul_out_or_ejected_flag = r.read_u8()?;
                let restored_game_clock_ds = u16::read_canonical_le(r)?;
                let new_shot_clock_ds = u16::read_canonical_le(r)?;
                let _ = TriStateShotClock::from_wire(new_shot_clock_ds, true)?;
                Ok(Self::RulingFinalized {
                    ruling_id,
                    final_rule_code,
                    offending_side,
                    offender_roster_idx,
                    points_delta_signed,
                    ft_shots_enqueued,
                    resulting_possession_side,
                    foul_out_or_ejected_flag,
                    restored_game_clock_ds,
                    new_shot_clock_ds,
                })
            }
            0x14 => Ok(Self::UnchallengeableChallengeChargedTimeout {
                ruling_id: u64::read_canonical_le(r)?,
                requester: r.read_u8()?,
                timeout_charged: r.read_u8()?,
                remaining_timeouts: r.read_u8()?,
                remaining_challenges: r.read_u8()?,
            }),
            0x15 => {
                let reason_tag = r.read_u8()?;
                if reason_tag > 4 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "TimeoutGranted.reason_tag",
                        value: reason_tag as u32,
                    });
                }
                Ok(Self::TimeoutGranted {
                    reason_tag,
                    charged_side: r.read_u8()?,
                    home_to_rem: r.read_u8()?,
                    away_to_rem: r.read_u8()?,
                })
            }
            0x16 => {
                let team_side = r.read_u8()?;
                let out_roster_idx = r.read_u8()?;
                let in_roster_idx = r.read_u8()?;
                let sub_priority_tier = r.read_u8()?;
                if sub_priority_tier > 5 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "SubstitutionApplied.sub_priority_tier",
                        value: sub_priority_tier as u32,
                    });
                }
                let out_player_cum_court_time_ds = u32::read_canonical_le(r)?;
                Ok(Self::SubstitutionApplied {
                    team_side,
                    out_roster_idx,
                    in_roster_idx,
                    sub_priority_tier,
                    out_player_cum_court_time_ds,
                })
            }
            0x17 => {
                let duration_ds = u16::read_canonical_le(r)?;
                let reason_tag = r.read_u8()?;
                if reason_tag > 4 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "DeadTime.reason_tag",
                        value: reason_tag as u32,
                    });
                }
                r.read_zero_pad(1)?;
                Ok(Self::DeadTime {
                    duration_ds,
                    reason_tag,
                })
            }
            0x18 => {
                let ingress_seq = u64::read_canonical_le(r)?;
                let side = r.read_u8()?;
                let abort_reason_code = r.read_u8()?;
                if abort_reason_code == 0 || abort_reason_code > 0x0B {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "AnchoredCommandAbortedRecord.abort_reason_code",
                        value: abort_reason_code as u32,
                    });
                }
                let cmd_tag = r.read_u8()?;
                r.read_zero_pad(1)?;
                Ok(Self::AnchoredCommandAbortedRecord {
                    ingress_seq,
                    side,
                    abort_reason_code,
                    cmd_tag,
                })
            }
            0x19 => {
                let original_shot_event_seq = u64::read_canonical_le(r)?;
                let shooter_roster_idx = r.read_u8()?;
                let assist_roster_idx = r.read_u8()?;
                let team_side = r.read_u8()?;
                let amend_kind = r.read_u8()?;
                if amend_kind > 4 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "ScoringEventAmended.amend_kind",
                        value: amend_kind as u32,
                    });
                }
                let old_points = r.read_u8()?;
                let new_points = r.read_u8()?;
                let pts_delta = r.read_i8()?;
                let fga_delta = r.read_i8()?;
                let fgm_delta = r.read_i8()?;
                let tpa_delta = r.read_i8()?;
                let tpm_delta = r.read_i8()?;
                let ast_delta = r.read_i8()?;
                let blocker_roster_idx = r.read_u8()?;
                let blk_delta = r.read_i8()?;
                r.read_zero_pad(2)?;
                Ok(Self::ScoringEventAmended {
                    original_shot_event_seq,
                    shooter_roster_idx,
                    assist_roster_idx,
                    team_side,
                    amend_kind,
                    old_points,
                    new_points,
                    pts_delta,
                    fga_delta,
                    fgm_delta,
                    tpa_delta,
                    tpm_delta,
                    ast_delta,
                    blocker_roster_idx,
                    blk_delta,
                })
            }
            0x1A => {
                let score_home_official = u16::read_canonical_le(r)?;
                let score_away_official = u16::read_canonical_le(r)?;
                let on_court_score_home = u16::read_canonical_le(r)?;
                let on_court_score_away = u16::read_canonical_le(r)?;
                let winner_side = r.read_u8()?;
                if winner_side > 1 && winner_side != 0xFF {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "GameEnded.winner_side",
                        value: winner_side as u32,
                    });
                }
                let end_reason = r.read_u8()?;
                if end_reason > 5 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "GameEnded.end_reason",
                        value: end_reason as u32,
                    });
                }
                let final_quarter = r.read_u8()?;
                r.read_zero_pad(1)?;
                let mut arr = [0u32; 30];
                for slot in arr.iter_mut() {
                    *slot = u32::read_canonical_le(r)?;
                }
                Ok(Self::GameEnded {
                    score_home_official,
                    score_away_official,
                    on_court_score_home,
                    on_court_score_away,
                    winner_side,
                    end_reason,
                    final_quarter,
                    final_court_time_ds: FinalCourtTimeDs(arr),
                })
            }
            0x1B => {
                let ruling_id = u64::read_canonical_le(r)?;
                let rule_code = u16::read_canonical_le(r)?;
                let offending_side = r.read_u8()?;
                let offender_roster_idx = r.read_u8()?;
                let foul_category = r.read_u8()?;
                if foul_category > 6 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "FoulAssessedRecord.foul_category",
                        value: foul_category as u32,
                    });
                }
                let counts_team_foul = r.read_u8()?;
                let resulting_personal_fouls = r.read_u8()?;
                let ejected_or_fouled_out_flags = r.read_u8()?;
                Ok(Self::FoulAssessedRecord {
                    ruling_id,
                    rule_code,
                    offending_side,
                    offender_roster_idx,
                    foul_category,
                    counts_team_foul,
                    resulting_personal_fouls,
                    ejected_or_fouled_out_flags,
                })
            }
            0x1C => {
                let adjust_reason = r.read_u8()?;
                if adjust_reason > 3 {
                    return Err(DecodeError::InvalidEnumValue {
                        field: "ClockAdjusted.adjust_reason",
                        value: adjust_reason as u32,
                    });
                }
                let delta_game_clock_ds_signed = i16::read_canonical_le(r)?;
                let new_game_clock_ds = u16::read_canonical_le(r)?;
                let new_shot_clock_ds = u16::read_canonical_le(r)?;
                let _ = TriStateShotClock::from_wire(new_shot_clock_ds, true)?;
                let mask_off = r.position();
                let affected_slot_mask = u16::read_canonical_le(r)?;
                if (affected_slot_mask & 0xFC00) != 0 {
                    return Err(DecodeError::NonZeroPadding {
                        offset: mask_off + 1,
                        value: ((affected_slot_mask >> 8) as u8) & 0xFC,
                    });
                }
                r.read_zero_pad(1)?;
                let mut affected_roster_indices = [0xFFu8; 10];
                let bytes = r.read_bytes(10)?;
                affected_roster_indices.copy_from_slice(bytes);
                Ok(Self::ClockAdjusted {
                    adjust_reason,
                    delta_game_clock_ds_signed,
                    new_game_clock_ds,
                    new_shot_clock_ds,
                    affected_slot_mask,
                    affected_roster_indices,
                })
            }
            other => Err(DecodeError::InvalidEnumValue {
                field: "GameRecordEventV1.event_type_code",
                value: other as u32,
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameRecordEventV1 {
    pub event_seq: u64,
    pub tick: u32,
    pub game_clock_ds: u16,
    pub shot_clock_ds: u16,
    pub quarter: u8,
    pub payload: GameRecordEventPayload,
}

impl GameRecordEventV1 {
    pub fn write_le(&self, out: &mut Vec<u8>) {
        let code = self.payload.event_type_code();
        let payload_len = GameRecordEventPayload::expected_payload_len(code).unwrap();
        out.extend_from_slice(&self.event_seq.to_le_bytes());
        out.extend_from_slice(&self.tick.to_le_bytes());
        out.extend_from_slice(&self.game_clock_ds.to_le_bytes());
        out.extend_from_slice(&self.shot_clock_ds.to_le_bytes());
        out.push(self.quarter);
        out.push(code);
        out.extend_from_slice(&payload_len.to_le_bytes());
        self.payload.write_le(out);
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(EVENT_HEADER_BYTES + 132);
        self.write_le(&mut out);
        out
    }

    pub fn read_le(r: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let event_seq = u64::read_canonical_le(r)?;
        let tick = u32::read_canonical_le(r)?;
        let game_clock_ds = u16::read_canonical_le(r)?;
        let shot_clock_ds = u16::read_canonical_le(r)?;
        let _ = decode_shot_clock_opt(shot_clock_ds)?;
        let quarter = r.read_u8()?;
        let event_type_code = r.read_u8()?;
        let payload_byte_len = u16::read_canonical_le(r)?;
        let expected = GameRecordEventPayload::expected_payload_len(event_type_code).ok_or(
            DecodeError::InvalidEnumValue {
                field: "event_type_code",
                value: event_type_code as u32,
            },
        )?;
        if payload_byte_len != expected {
            return Err(DecodeError::InvalidFieldValue {
                field: "payload_byte_len",
            });
        }
        let payload = GameRecordEventPayload::read_le(r, event_type_code)?;
        Ok(Self {
            event_seq,
            tick,
            game_clock_ds,
            shot_clock_ds,
            quarter,
            payload,
        })
    }
}

impl CanonicalLeBytes for GameRecordEventV1 {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.write_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Self::read_le(reader)
    }
}
