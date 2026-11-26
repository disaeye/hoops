use core::fmt;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::canonical_le_bytes::{CanonicalLeBytes, CanonicalReader, DecodeError};
use crate::engine::defense::TeamDefensePlan;
use crate::wire_schema_v1::commands::{
    AnchoredCommand, BoundedCommandQueue, BoundedDedupWindow32, PlayId,
};
use crate::wire_schema_v1::config_ir::MatchInitSpecV1;
use crate::wire_schema_v1::enums::*;
use crate::wire_schema_v1::events::GameRecordEventV1;

pub const MAX_FT_QUEUE_DEPTH: usize = 8;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommentsIILOrigin {
    pub kind: u8,
    pub start_gc_rem_10us: u32,
    pub start_sc_rem_10us: Option<u32>,
}

impl CanonicalLeBytes for CommentsIILOrigin {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.kind.write_canonical_le(out);
        self.start_gc_rem_10us.write_canonical_le(out);
        self.start_sc_rem_10us.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            kind: u8::read_canonical_le(reader)?,
            start_gc_rem_10us: u32::read_canonical_le(reader)?,
            start_sc_rem_10us: Option::<u32>::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MandatoryTimeoutState {
    pub slot_under_7m_taken: bool,
    pub slot_under_3m_taken: bool,
    pub slot_under_3m_completed: bool,
    pub first_mandatory_charged_to: Option<CommandSide>,
}

impl CanonicalLeBytes for MandatoryTimeoutState {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.slot_under_7m_taken.write_canonical_le(out);
        self.slot_under_3m_taken.write_canonical_le(out);
        self.slot_under_3m_completed.write_canonical_le(out);
        self.first_mandatory_charged_to.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            slot_under_7m_taken: bool::read_canonical_le(reader)?,
            slot_under_3m_taken: bool::read_canonical_le(reader)?,
            slot_under_3m_completed: bool::read_canonical_le(reader)?,
            first_mandatory_charged_to: Option::<CommandSide>::read_canonical_le(reader)?,
        })
    }
}

/// 33x9 RNG counter array wrapper with manual Serde & CanonicalLeBytes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RngSampleCounters(pub [[u16; 9]; 33]);

impl Default for RngSampleCounters {
    fn default() -> Self {
        Self([[0u16; 9]; 33])
    }
}

impl fmt::Debug for RngSampleCounters {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RngSampleCounters([[u16; 9]; 33])")
    }
}

impl Serialize for RngSampleCounters {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple;
        let mut t = serializer.serialize_tuple(33)?;
        for row in &self.0 {
            t.serialize_element(row)?;
        }
        t.end()
    }
}

impl<'de> Deserialize<'de> for RngSampleCounters {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = RngSampleCounters;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "[[u16; 9]; 33]")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut arr = [[0u16; 9]; 33];
                for (i, slot) in arr.iter_mut().enumerate() {
                    *slot = seq
                        .next_element::<[u16; 9]>()?
                        .ok_or_else(|| serde::de::Error::invalid_length(i, &self))?;
                }
                Ok(RngSampleCounters(arr))
            }
        }
        deserializer.deserialize_tuple(33, V)
    }
}

impl CanonicalLeBytes for RngSampleCounters {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.0.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self(<[[u16; 9]; 33]>::read_canonical_le(reader)?))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuleCallSnapshot {
    pub rule_code: RuleCode,
    pub offending_side: CommandSide,
    pub offender_roster_idx: RosterIdx,
    pub victim_roster_idx: Option<RosterIdx>,
    pub points_if_made: u8,
    pub ft_count: u8,
    pub is_shooting_foul: bool,
    pub in_shooting_motion_before_whistle: bool,
    pub foul_occurred_after_shooting_motion_started: bool,
    pub whistle_after_shooting_motion_started: bool,
    pub oob_simultaneous_with_made_fg: bool,
    pub official_not_reasonably_certain: bool,
    pub basket_made_on_play: bool,
    pub ball_in_frontcourt_at_call: bool,
    pub interruption_spot: InboundSpot,
    pub stoppage_sideline_side: CourtSideLine,
    pub restart_shot_clock_ds: Option<u16>,
    pub foul_category: u8,
}

impl Default for RuleCallSnapshot {
    fn default() -> Self {
        Self {
            rule_code: RuleCode::NONE,
            offending_side: CommandSide::Home,
            offender_roster_idx: ROSTER_IDX_NONE,
            victim_roster_idx: None,
            points_if_made: 0,
            ft_count: 0,
            is_shooting_foul: false,
            in_shooting_motion_before_whistle: false,
            foul_occurred_after_shooting_motion_started: false,
            whistle_after_shooting_motion_started: false,
            oob_simultaneous_with_made_fg: false,
            official_not_reasonably_certain: false,
            basket_made_on_play: false,
            ball_in_frontcourt_at_call: false,
            interruption_spot: InboundSpot::NearestToInterruption,
            stoppage_sideline_side: CourtSideLine::TableSide,
            restart_shot_clock_ds: None,
            foul_category: 0,
        }
    }
}

impl CanonicalLeBytes for RuleCallSnapshot {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.rule_code.write_canonical_le(out);
        self.offending_side.write_canonical_le(out);
        self.offender_roster_idx.write_canonical_le(out);
        self.victim_roster_idx.write_canonical_le(out);
        self.points_if_made.write_canonical_le(out);
        self.ft_count.write_canonical_le(out);
        self.is_shooting_foul.write_canonical_le(out);
        self.in_shooting_motion_before_whistle
            .write_canonical_le(out);
        self.foul_occurred_after_shooting_motion_started
            .write_canonical_le(out);
        self.whistle_after_shooting_motion_started
            .write_canonical_le(out);
        self.oob_simultaneous_with_made_fg.write_canonical_le(out);
        self.official_not_reasonably_certain
            .write_canonical_le(out);
        self.basket_made_on_play.write_canonical_le(out);
        self.ball_in_frontcourt_at_call.write_canonical_le(out);
        self.interruption_spot.write_canonical_le(out);
        self.stoppage_sideline_side.write_canonical_le(out);
        self.restart_shot_clock_ds.write_canonical_le(out);
        self.foul_category.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            rule_code: RuleCode::read_canonical_le(reader)?,
            offending_side: CommandSide::read_canonical_le(reader)?,
            offender_roster_idx: u8::read_canonical_le(reader)?,
            victim_roster_idx: Option::<u8>::read_canonical_le(reader)?,
            points_if_made: u8::read_canonical_le(reader)?,
            ft_count: u8::read_canonical_le(reader)?,
            is_shooting_foul: bool::read_canonical_le(reader)?,
            in_shooting_motion_before_whistle: bool::read_canonical_le(reader)?,
            foul_occurred_after_shooting_motion_started: bool::read_canonical_le(reader)?,
            whistle_after_shooting_motion_started: bool::read_canonical_le(reader)?,
            oob_simultaneous_with_made_fg: bool::read_canonical_le(reader)?,
            official_not_reasonably_certain: bool::read_canonical_le(reader)?,
            basket_made_on_play: bool::read_canonical_le(reader)?,
            ball_in_frontcourt_at_call: bool::read_canonical_le(reader)?,
            interruption_spot: InboundSpot::read_canonical_le(reader)?,
            stoppage_sideline_side: CourtSideLine::read_canonical_le(reader)?,
            restart_shot_clock_ds: Option::<u16>::read_canonical_le(reader)?,
            foul_category: u8::read_canonical_le(reader)?,
        })
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EligibleSidesMask(pub u8);

impl CanonicalLeBytes for EligibleSidesMask {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.0.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self(u8::read_canonical_le(reader)?))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingRuling {
    pub ruling_id: u64,
    pub whistle_tick: u32,
    pub window_open_tick: u32,
    pub request_seq: u32,
    pub window_epoch: u32,
    pub challenge_attempts_in_window: u8,
    pub window_state: ChallengeWindowState,
    pub window_kind: ChallengeWindowKind,
    pub perceived_call: RuleCallSnapshot,
    pub ground_truth: RuleCallSnapshot,
    pub eligible_sides: EligibleSidesMask,
    pub period_expired_after_ruling: bool,
}

impl CanonicalLeBytes for PendingRuling {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.ruling_id.write_canonical_le(out);
        self.whistle_tick.write_canonical_le(out);
        self.window_open_tick.write_canonical_le(out);
        self.request_seq.write_canonical_le(out);
        self.window_epoch.write_canonical_le(out);
        self.challenge_attempts_in_window.write_canonical_le(out);
        self.window_state.write_canonical_le(out);
        self.window_kind.write_canonical_le(out);
        self.perceived_call.write_canonical_le(out);
        self.ground_truth.write_canonical_le(out);
        self.eligible_sides.write_canonical_le(out);
        self.period_expired_after_ruling.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            ruling_id: u64::read_canonical_le(reader)?,
            whistle_tick: u32::read_canonical_le(reader)?,
            window_open_tick: u32::read_canonical_le(reader)?,
            request_seq: u32::read_canonical_le(reader)?,
            window_epoch: u32::read_canonical_le(reader)?,
            challenge_attempts_in_window: u8::read_canonical_le(reader)?,
            window_state: ChallengeWindowState::read_canonical_le(reader)?,
            window_kind: ChallengeWindowKind::read_canonical_le(reader)?,
            perceived_call: RuleCallSnapshot::read_canonical_le(reader)?,
            ground_truth: RuleCallSnapshot::read_canonical_le(reader)?,
            eligible_sides: EligibleSidesMask::read_canonical_le(reader)?,
            period_expired_after_ruling: bool::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingFibaHccRequest {
    pub requester: CommandSide,
    pub target_event_seq: u64,
    pub request_seq: u32,
    pub enqueued_ingress_seq: u64,
}

impl CanonicalLeBytes for PendingFibaHccRequest {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.requester.write_canonical_le(out);
        self.target_event_seq.write_canonical_le(out);
        self.request_seq.write_canonical_le(out);
        self.enqueued_ingress_seq.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            requester: CommandSide::read_canonical_le(reader)?,
            target_event_seq: u64::read_canonical_le(reader)?,
            request_seq: u32::read_canonical_le(reader)?,
            enqueued_ingress_seq: u64::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct FtSeriesSpec {
    pub shooter_roster_idx: RosterIdx,
    pub total_shots: u8,
    pub shots_remaining: u8,
    pub is_technical_or_flagrant: bool,
    pub on_complete: PostFtOutcome,
}

impl CanonicalLeBytes for FtSeriesSpec {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.shooter_roster_idx.write_canonical_le(out);
        self.total_shots.write_canonical_le(out);
        self.shots_remaining.write_canonical_le(out);
        self.is_technical_or_flagrant.write_canonical_le(out);
        self.on_complete.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            shooter_roster_idx: u8::read_canonical_le(reader)?,
            total_shots: u8::read_canonical_le(reader)?,
            shots_remaining: u8::read_canonical_le(reader)?,
            is_technical_or_flagrant: bool::read_canonical_le(reader)?,
            on_complete: PostFtOutcome::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BoundedFtQueue {
    pub items: [Option<FtSeriesSpec>; MAX_FT_QUEUE_DEPTH],
    pub len: u8,
}

impl BoundedFtQueue {
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn clear(&mut self) {
        self.items = [None; MAX_FT_QUEUE_DEPTH];
        self.len = 0;
    }
    pub fn push(&mut self, spec: FtSeriesSpec) -> bool {
        if (self.len as usize) >= MAX_FT_QUEUE_DEPTH {
            return false;
        }
        self.items[self.len as usize] = Some(spec);
        self.len += 1;
        true
    }
    pub fn top_mut(&mut self) -> Option<&mut FtSeriesSpec> {
        if self.len == 0 {
            None
        } else {
            self.items[(self.len - 1) as usize].as_mut()
        }
    }
    pub fn pop(&mut self) -> Option<FtSeriesSpec> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        self.items[self.len as usize].take()
    }
}

impl CanonicalLeBytes for BoundedFtQueue {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.items.write_canonical_le(out);
        self.len.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            items: <[Option<FtSeriesSpec>; MAX_FT_QUEUE_DEPTH]>::read_canonical_le(reader)?,
            len: u8::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeferredShotFoul {
    pub call: RuleCallSnapshot,
    pub whistle_tick: u32,
    pub whistle_game_clock_ds: u16,
}

impl CanonicalLeBytes for DeferredShotFoul {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.call.write_canonical_le(out);
        self.whistle_tick.write_canonical_le(out);
        self.whistle_game_clock_ds.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            call: RuleCallSnapshot::read_canonical_le(reader)?,
            whistle_tick: u32::read_canonical_le(reader)?,
            whistle_game_clock_ds: u16::read_canonical_le(reader)?,
        })
    }
}

/// 20-field ShotContext (§2.4 / §3.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShotContext {
    pub shot_id: u32,
    pub shooter_roster_idx: RosterIdx,
    pub assist_roster_idx: Option<RosterIdx>,
    pub blocker_roster_idx: Option<RosterIdx>,
    pub shot_value: u8,
    pub ground_truth_is_three_pointer: bool,
    pub release_zone: u8,
    pub release_x_mm: i16,
    pub release_y_mm: i16,
    pub intended_outcome: IntendedOutcomeClass,
    pub release_tick: u32,
    pub release_game_clock_ds: u16,
    pub whistle_tick: Option<u32>,
    pub whistle_game_clock_ds: Option<u16>,
    pub deferred_foul_call: Option<DeferredShotFoul>,
    pub flight_resolved_tick: Option<u32>,
    pub basket_scored_tick: Option<u32>,
    pub rim_touched: bool,
    pub backboard_contact: BackboardContact,
    pub released_before_shot_clock_zero: bool,
}

impl CanonicalLeBytes for ShotContext {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.shot_id.write_canonical_le(out);
        self.shooter_roster_idx.write_canonical_le(out);
        self.assist_roster_idx.write_canonical_le(out);
        self.blocker_roster_idx.write_canonical_le(out);
        self.shot_value.write_canonical_le(out);
        self.ground_truth_is_three_pointer.write_canonical_le(out);
        self.release_zone.write_canonical_le(out);
        self.release_x_mm.write_canonical_le(out);
        self.release_y_mm.write_canonical_le(out);
        self.intended_outcome.write_canonical_le(out);
        self.release_tick.write_canonical_le(out);
        self.release_game_clock_ds.write_canonical_le(out);
        self.whistle_tick.write_canonical_le(out);
        self.whistle_game_clock_ds.write_canonical_le(out);
        self.deferred_foul_call.write_canonical_le(out);
        self.flight_resolved_tick.write_canonical_le(out);
        self.basket_scored_tick.write_canonical_le(out);
        self.rim_touched.write_canonical_le(out);
        self.backboard_contact.write_canonical_le(out);
        self.released_before_shot_clock_zero
            .write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            shot_id: u32::read_canonical_le(reader)?,
            shooter_roster_idx: u8::read_canonical_le(reader)?,
            assist_roster_idx: Option::<u8>::read_canonical_le(reader)?,
            blocker_roster_idx: Option::<u8>::read_canonical_le(reader)?,
            shot_value: u8::read_canonical_le(reader)?,
            ground_truth_is_three_pointer: bool::read_canonical_le(reader)?,
            release_zone: u8::read_canonical_le(reader)?,
            release_x_mm: i16::read_canonical_le(reader)?,
            release_y_mm: i16::read_canonical_le(reader)?,
            intended_outcome: IntendedOutcomeClass::read_canonical_le(reader)?,
            release_tick: u32::read_canonical_le(reader)?,
            release_game_clock_ds: u16::read_canonical_le(reader)?,
            whistle_tick: Option::<u32>::read_canonical_le(reader)?,
            whistle_game_clock_ds: Option::<u16>::read_canonical_le(reader)?,
            deferred_foul_call: Option::<DeferredShotFoul>::read_canonical_le(reader)?,
            flight_resolved_tick: Option::<u32>::read_canonical_le(reader)?,
            basket_scored_tick: Option::<u32>::read_canonical_le(reader)?,
            rim_touched: bool::read_canonical_le(reader)?,
            backboard_contact: BackboardContact::read_canonical_le(reader)?,
            released_before_shot_clock_zero: bool::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboundContext {
    pub side: CommandSide,
    pub spot: InboundSpot,
    pub sideline_side: CourtSideLine,
    pub allow_endline_run: bool,
    pub stop_clock_until_touch: bool,
    pub inbound_ball_at_disposal: bool,
    pub inbound_touched_court_player: bool,
    pub inbound_5s_timer_ds: u8,
    pub original_spot: InboundSpot,
    pub pending_shot_clock_ds: Option<u16>,
    pub is_period_start_arrow_inbound: bool,
    pub allow_both_sides_advance: bool,
    pub post_made_fg: bool,
    pub post_made_last_ft: bool,
}

impl CanonicalLeBytes for InboundContext {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.side.write_canonical_le(out);
        self.spot.write_canonical_le(out);
        self.sideline_side.write_canonical_le(out);
        self.allow_endline_run.write_canonical_le(out);
        self.stop_clock_until_touch.write_canonical_le(out);
        self.inbound_ball_at_disposal.write_canonical_le(out);
        self.inbound_touched_court_player.write_canonical_le(out);
        self.inbound_5s_timer_ds.write_canonical_le(out);
        self.original_spot.write_canonical_le(out);
        self.pending_shot_clock_ds.write_canonical_le(out);
        self.is_period_start_arrow_inbound.write_canonical_le(out);
        self.allow_both_sides_advance.write_canonical_le(out);
        self.post_made_fg.write_canonical_le(out);
        self.post_made_last_ft.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            side: CommandSide::read_canonical_le(reader)?,
            spot: InboundSpot::read_canonical_le(reader)?,
            sideline_side: CourtSideLine::read_canonical_le(reader)?,
            allow_endline_run: bool::read_canonical_le(reader)?,
            stop_clock_until_touch: bool::read_canonical_le(reader)?,
            inbound_ball_at_disposal: bool::read_canonical_le(reader)?,
            inbound_touched_court_player: bool::read_canonical_le(reader)?,
            inbound_5s_timer_ds: u8::read_canonical_le(reader)?,
            original_spot: InboundSpot::read_canonical_le(reader)?,
            pending_shot_clock_ds: Option::<u16>::read_canonical_le(reader)?,
            is_period_start_arrow_inbound: bool::read_canonical_le(reader)?,
            allow_both_sides_advance: bool::read_canonical_le(reader)?,
            post_made_fg: bool::read_canonical_le(reader)?,
            post_made_last_ft: bool::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct JumpBallContext {
    pub spot: JumpBallSpot,
    pub jumpers: [u8; 2],
    pub toss_timer_ds: u8,
    pub ball_tossed: bool,
    pub ball_tapped: bool,
    pub is_opening_tip: bool,
}

impl CanonicalLeBytes for JumpBallContext {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.spot.write_canonical_le(out);
        self.jumpers.write_canonical_le(out);
        self.toss_timer_ds.write_canonical_le(out);
        self.ball_tossed.write_canonical_le(out);
        self.ball_tapped.write_canonical_le(out);
        self.is_opening_tip.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            spot: JumpBallSpot::read_canonical_le(reader)?,
            jumpers: <[u8; 2]>::read_canonical_le(reader)?,
            toss_timer_ds: u8::read_canonical_le(reader)?,
            ball_tossed: bool::read_canonical_le(reader)?,
            ball_tapped: bool::read_canonical_le(reader)?,
            is_opening_tip: bool::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeadTimeRecord {
    pub duration_ds: u16,
    pub reason_tag: u8,
}

impl CanonicalLeBytes for DeadTimeRecord {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.duration_ds.write_canonical_le(out);
        self.reason_tag.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            duration_ds: u16::read_canonical_le(reader)?,
            reason_tag: u8::read_canonical_le(reader)?,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SuspendedGameState {
    Inbound(InboundContext) = 0,
    FreeThrows = 1,
    JumpBall(JumpBallContext) = 2,
}

impl CanonicalLeBytes for SuspendedGameState {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        match *self {
            Self::Inbound(ctx) => {
                out.push(0);
                ctx.write_canonical_le(out);
            }
            Self::FreeThrows => out.push(1),
            Self::JumpBall(ctx) => {
                out.push(2);
                ctx.write_canonical_le(out);
            }
        }
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::Inbound(InboundContext::read_canonical_le(reader)?)),
            1 => Ok(Self::FreeThrows),
            2 => Ok(Self::JumpBall(JumpBallContext::read_canonical_le(reader)?)),
            v => Err(DecodeError::InvalidEnumValue {
                field: "SuspendedGameState",
                value: v as u32,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundedSuspendedStack<const N: usize> {
    pub items: [Option<SuspendedGameState>; N],
    pub len: u8,
}

impl<const N: usize> Default for BoundedSuspendedStack<N> {
    fn default() -> Self {
        Self {
            items: [None; N],
            len: 0,
        }
    }
}

impl<const N: usize> serde::Serialize for BoundedSuspendedStack<N> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("BoundedSuspendedStack", 2)?;
        let active = (self.len as usize).min(N);
        s.serialize_field("items", &&self.items[..active])?;
        s.serialize_field("len", &self.len)?;
        s.end()
    }
}

impl<'de, const N: usize> serde::Deserialize<'de> for BoundedSuspendedStack<N> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Helper {
            items: Vec<Option<SuspendedGameState>>,
            len: u8,
        }
        let h = Helper::deserialize(deserializer)?;
        if (h.len as usize) > N || h.items.len() > N {
            return Err(serde::de::Error::custom("BoundedSuspendedStack overflow"));
        }
        let mut items = [None; N];
        for (i, item) in h.items.into_iter().enumerate() {
            items[i] = item;
        }
        Ok(Self { items, len: h.len })
    }
}

impl<const N: usize> BoundedSuspendedStack<N> {
    pub fn push(&mut self, item: SuspendedGameState) -> bool {
        if (self.len as usize) >= N {
            return false;
        }
        self.items[self.len as usize] = Some(item);
        self.len += 1;
        true
    }

    pub fn pop(&mut self) -> Option<SuspendedGameState> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        self.items[self.len as usize].take()
    }

    pub fn clear(&mut self) {
        self.items = [None; N];
        self.len = 0;
    }
}

impl<const N: usize> CanonicalLeBytes for BoundedSuspendedStack<N> {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.items.write_canonical_le(out);
        self.len.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            items: <[Option<SuspendedGameState>; N]>::read_canonical_le(reader)?,
            len: u8::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ReviewableEventEntry {
    pub event_seq: u64,
    pub tick: u32,
    pub quarter: u8,
    pub game_clock_ds: u16,
    pub points_awarded: u8,
    pub original_rule_code: u16,
    pub scorer_side: Option<CommandSide>,
    pub shooter_roster_idx: Option<RosterIdx>,
    pub assist_roster_idx: Option<RosterIdx>,
    pub blocker_roster_idx: Option<RosterIdx>,
    pub original_is_three_pointer: bool,
    pub original_fga_counted: bool,
    pub stoppage_occurred_since: bool,
    pub shot_clock_expired_before_release: bool,
    pub ground_truth_is_three_pointer: bool,
    pub ground_truth_oob_by_side: Option<CommandSide>,
    pub ground_truth_goaltend_violation: bool,
    pub ground_truth_def_foul_after_motion_start: bool,
    pub official_not_reasonably_certain: bool,
}

impl CanonicalLeBytes for ReviewableEventEntry {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.event_seq.write_canonical_le(out);
        self.tick.write_canonical_le(out);
        self.quarter.write_canonical_le(out);
        self.game_clock_ds.write_canonical_le(out);
        self.points_awarded.write_canonical_le(out);
        self.original_rule_code.write_canonical_le(out);
        self.scorer_side.write_canonical_le(out);
        self.shooter_roster_idx.write_canonical_le(out);
        self.assist_roster_idx.write_canonical_le(out);
        self.blocker_roster_idx.write_canonical_le(out);
        self.original_is_three_pointer.write_canonical_le(out);
        self.original_fga_counted.write_canonical_le(out);
        self.stoppage_occurred_since.write_canonical_le(out);
        self.shot_clock_expired_before_release
            .write_canonical_le(out);
        self.ground_truth_is_three_pointer.write_canonical_le(out);
        self.ground_truth_oob_by_side.write_canonical_le(out);
        self.ground_truth_goaltend_violation.write_canonical_le(out);
        self.ground_truth_def_foul_after_motion_start
            .write_canonical_le(out);
        self.official_not_reasonably_certain
            .write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            event_seq: u64::read_canonical_le(reader)?,
            tick: u32::read_canonical_le(reader)?,
            quarter: u8::read_canonical_le(reader)?,
            game_clock_ds: u16::read_canonical_le(reader)?,
            points_awarded: u8::read_canonical_le(reader)?,
            original_rule_code: u16::read_canonical_le(reader)?,
            scorer_side: Option::<CommandSide>::read_canonical_le(reader)?,
            shooter_roster_idx: Option::<u8>::read_canonical_le(reader)?,
            assist_roster_idx: Option::<u8>::read_canonical_le(reader)?,
            blocker_roster_idx: Option::<u8>::read_canonical_le(reader)?,
            original_is_three_pointer: bool::read_canonical_le(reader)?,
            original_fga_counted: bool::read_canonical_le(reader)?,
            stoppage_occurred_since: bool::read_canonical_le(reader)?,
            shot_clock_expired_before_release: bool::read_canonical_le(reader)?,
            ground_truth_is_three_pointer: bool::read_canonical_le(reader)?,
            ground_truth_oob_by_side: Option::<CommandSide>::read_canonical_le(reader)?,
            ground_truth_goaltend_violation: bool::read_canonical_le(reader)?,
            ground_truth_def_foul_after_motion_start: bool::read_canonical_le(reader)?,
            official_not_reasonably_certain: bool::read_canonical_le(reader)?,
        })
    }
}

/// 256-slot bounded reviewable event ring with 2-tier eviction (§2.3 / §3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedReviewableEventRing<const N: usize> {
    pub head: u16,
    pub len: u16,
    pub entries: Box<[ReviewableEventEntry; N]>,
}

impl<const N: usize> Default for BoundedReviewableEventRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> BoundedReviewableEventRing<N> {
    pub fn new() -> Self {
        Self {
            head: 0,
            len: 0,
            entries: Box::new([ReviewableEventEntry::default(); N]),
        }
    }

    pub fn find_by_event_seq(&self, event_seq: u64) -> Option<&ReviewableEventEntry> {
        for i in 0..(self.len as usize).min(N) {
            if self.entries[i].event_seq == event_seq {
                return Some(&self.entries[i]);
            }
        }
        None
    }

    pub fn find_by_event_seq_mut(&mut self, event_seq: u64) -> Option<&mut ReviewableEventEntry> {
        let len = (self.len as usize).min(N);
        for i in 0..len {
            if self.entries[i].event_seq == event_seq {
                return Some(&mut self.entries[i]);
            }
        }
        None
    }

    pub fn mark_stoppage_occurred(&mut self, up_to_event_seq: u64) {
        for i in 0..(self.len as usize).min(N) {
            if self.entries[i].event_seq <= up_to_event_seq {
                self.entries[i].stoppage_occurred_since = true;
            }
        }
    }

    /// Push entry; if full, apply Tier-1 (`stoppage_occurred_since && !pinned`) then Tier-2 (oldest `!pinned`).
    /// Returns `Some(evicted_event_seq)` if an entry was evicted.
    pub fn push_with_eviction(
        &mut self,
        entry: ReviewableEventEntry,
        pinned_event_seqs: &[u64],
    ) -> Option<u64> {
        let cur_len = self.len as usize;
        if cur_len < N {
            self.entries[cur_len] = entry;
            self.len += 1;
            return None;
        }
        // Tier 1: oldest with stoppage_occurred_since && !pinned
        let mut best_idx: Option<usize> = None;
        let mut best_seq = u64::MAX;
        for i in 0..N {
            let e = &self.entries[i];
            if e.stoppage_occurred_since && !pinned_event_seqs.contains(&e.event_seq) && e.event_seq < best_seq {
                best_seq = e.event_seq;
                best_idx = Some(i);
            }
        }
        // Tier 2: oldest unpinned
        if best_idx.is_none() {
            for i in 0..N {
                let e = &self.entries[i];
                if !pinned_event_seqs.contains(&e.event_seq) && e.event_seq < best_seq {
                    best_seq = e.event_seq;
                    best_idx = Some(i);
                }
            }
        }
        let idx = best_idx.unwrap_or(0);
        let evicted_seq = self.entries[idx].event_seq;
        self.entries[idx] = entry;
        self.head = ((idx + 1) % N) as u16;
        Some(evicted_seq)
    }
}

impl<const N: usize> Serialize for BoundedReviewableEventRing<N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("BoundedReviewableEventRing", 3)?;
        s.serialize_field("head", &self.head)?;
        s.serialize_field("len", &self.len)?;
        let active: Vec<&ReviewableEventEntry> = self.entries.iter().collect();
        s.serialize_field("entries", &active)?;
        s.end()
    }
}

impl<'de, const N: usize> Deserialize<'de> for BoundedReviewableEventRing<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Helper {
            head: u16,
            len: u16,
            entries: Vec<ReviewableEventEntry>,
        }
        let h = Helper::deserialize(deserializer)?;
        if h.entries.len() != N {
            return Err(serde::de::Error::invalid_length(h.entries.len(), &"ring size N"));
        }
        let mut arr = Box::new([ReviewableEventEntry::default(); N]);
        for (i, item) in h.entries.into_iter().enumerate() {
            arr[i] = item;
        }
        Ok(Self {
            head: h.head,
            len: h.len,
            entries: arr,
        })
    }
}

impl<const N: usize> CanonicalLeBytes for BoundedReviewableEventRing<N> {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        let active = (self.len as usize).min(N);
        self.head.write_canonical_le(out);
        (active as u16).write_canonical_le(out);
        for e in &self.entries[..active] {
            e.write_canonical_le(out);
        }
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let head = u16::read_canonical_le(reader)?;
        let len = u16::read_canonical_le(reader)?;
        if (len as usize) > N || ((head as usize) >= N && N > 0) {
            return Err(DecodeError::InvalidFieldValue {
                field: "BoundedReviewableEventRing.len",
            });
        }
        let mut entries = Box::new([ReviewableEventEntry::default(); N]);
        for e in &mut entries[..(len as usize)] {
            *e = ReviewableEventEntry::read_canonical_le(reader)?;
        }
        Ok(Self { head, len, entries })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundedSubQueue<const N: usize> {
    pub len: u8,
    pub entries: [(CommandSide, u8, u8); N],
}

impl<const N: usize> Default for BoundedSubQueue<N> {
    fn default() -> Self {
        Self {
            len: 0,
            entries: [(CommandSide::Home, 0, 0); N],
        }
    }
}

impl<const N: usize> serde::Serialize for BoundedSubQueue<N> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("BoundedSubQueue", 2)?;
        let active = (self.len as usize).min(N);
        s.serialize_field("len", &self.len)?;
        s.serialize_field("entries", &&self.entries[..active])?;
        s.end()
    }
}

impl<'de, const N: usize> serde::Deserialize<'de> for BoundedSubQueue<N> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Helper {
            len: u8,
            entries: Vec<(CommandSide, u8, u8)>,
        }
        let h = Helper::deserialize(deserializer)?;
        if (h.len as usize) > N || h.entries.len() > N {
            return Err(serde::de::Error::custom("BoundedSubQueue overflow"));
        }
        let mut entries = [(CommandSide::Home, 0, 0); N];
        for (i, e) in h.entries.into_iter().enumerate() {
            entries[i] = e;
        }
        Ok(Self { len: h.len, entries })
    }
}

impl<const N: usize> BoundedSubQueue<N> {
    pub fn new() -> Self {
        Self {
            len: 0,
            entries: [(CommandSide::Home, ROSTER_IDX_NONE, ROSTER_IDX_NONE); N],
        }
    }

    pub fn push(&mut self, side: CommandSide, out_r: u8, in_r: u8) -> bool {
        if (self.len as usize) >= N {
            return false;
        }
        self.entries[self.len as usize] = (side, out_r, in_r);
        self.len += 1;
        true
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.entries = [(CommandSide::Home, ROSTER_IDX_NONE, ROSTER_IDX_NONE); N];
    }
}

impl<const N: usize> CanonicalLeBytes for BoundedSubQueue<N> {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.len.write_canonical_le(out);
        self.entries.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            len: u8::read_canonical_le(reader)?,
            entries: <[(CommandSide, u8, u8); N]>::read_canonical_le(reader)?,
        })
    }
}

/// 12-field FootworkRecord (§6.1.1).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FootworkRecord {
    pub dribble_status: DribbleStatus,
    pub gather_tick: Option<u32>,
    pub gather_game_clock_ds: Option<u16>,
    pub gather_on_air_catch: bool,
    pub pivot_foot: PivotFoot,
    pub pivot_pos_mm: Option<(i16, i16)>,
    pub steps_since_gather: u8,
    pub landed_simultaneously_after_step1: bool,
    pub jump_tick: Option<u32>,
    pub release_tick: Option<u32>,
    pub pivot_lifted_for_shot_or_pass: bool,
    pub foot_touched_down_while_lifted: bool,
}

impl Default for DribbleStatus {
    fn default() -> Self {
        Self::None
    }
}

impl Default for PivotFoot {
    fn default() -> Self {
        Self::Unset
    }
}

impl CanonicalLeBytes for FootworkRecord {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.dribble_status.write_canonical_le(out);
        self.gather_tick.write_canonical_le(out);
        self.gather_game_clock_ds.write_canonical_le(out);
        self.gather_on_air_catch.write_canonical_le(out);
        self.pivot_foot.write_canonical_le(out);
        self.pivot_pos_mm.write_canonical_le(out);
        self.steps_since_gather.write_canonical_le(out);
        self.landed_simultaneously_after_step1
            .write_canonical_le(out);
        self.jump_tick.write_canonical_le(out);
        self.release_tick.write_canonical_le(out);
        self.pivot_lifted_for_shot_or_pass.write_canonical_le(out);
        self.foot_touched_down_while_lifted.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            dribble_status: DribbleStatus::read_canonical_le(reader)?,
            gather_tick: Option::<u32>::read_canonical_le(reader)?,
            gather_game_clock_ds: Option::<u16>::read_canonical_le(reader)?,
            gather_on_air_catch: bool::read_canonical_le(reader)?,
            pivot_foot: PivotFoot::read_canonical_le(reader)?,
            pivot_pos_mm: Option::<(i16, i16)>::read_canonical_le(reader)?,
            steps_since_gather: u8::read_canonical_le(reader)?,
            landed_simultaneously_after_step1: bool::read_canonical_le(reader)?,
            jump_tick: Option::<u32>::read_canonical_le(reader)?,
            release_tick: Option::<u32>::read_canonical_le(reader)?,
            pivot_lifted_for_shot_or_pass: bool::read_canonical_le(reader)?,
            foot_touched_down_while_lifted: bool::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdaptiveCoverageState {
    pub active_coverage: PnrCoverageKind,
    pub n_possessions: u16,
    pub observed_points: u32,
    pub escalated: bool,
    pub cooldown_remaining: u8,
}

impl Default for AdaptiveCoverageState {
    fn default() -> Self {
        Self {
            active_coverage: PnrCoverageKind::Drop,
            n_possessions: 0,
            observed_points: 0,
            escalated: false,
            cooldown_remaining: 0,
        }
    }
}

impl CanonicalLeBytes for AdaptiveCoverageState {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.active_coverage.write_canonical_le(out);
        self.n_possessions.write_canonical_le(out);
        self.observed_points.write_canonical_le(out);
        self.escalated.write_canonical_le(out);
        self.cooldown_remaining.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            active_coverage: PnrCoverageKind::read_canonical_le(reader)?,
            n_possessions: u16::read_canonical_le(reader)?,
            observed_points: u32::read_canonical_le(reader)?,
            escalated: bool::read_canonical_le(reader)?,
            cooldown_remaining: u8::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct PassFlightContext {
    pub passer_slot: u8,
    pub target_slot: u8,
    pub pass_kind: u8,
    pub is_extra_pass: bool,
    pub ticks_elapsed: u16,
    pub total_flight_ticks: u16,
    pub start_pos_m: [f64; 3],
    pub target_pos_m: [f64; 3],
    pub intercepted_by: Option<u8>,
    pub will_bobble: bool,
}

impl CanonicalLeBytes for PassFlightContext {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.passer_slot.write_canonical_le(out);
        self.target_slot.write_canonical_le(out);
        self.pass_kind.write_canonical_le(out);
        self.is_extra_pass.write_canonical_le(out);
        self.ticks_elapsed.write_canonical_le(out);
        self.total_flight_ticks.write_canonical_le(out);
        self.start_pos_m.write_canonical_le(out);
        self.target_pos_m.write_canonical_le(out);
        self.intercepted_by.write_canonical_le(out);
        self.will_bobble.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            passer_slot: u8::read_canonical_le(reader)?,
            target_slot: u8::read_canonical_le(reader)?,
            pass_kind: u8::read_canonical_le(reader)?,
            is_extra_pass: bool::read_canonical_le(reader)?,
            ticks_elapsed: u16::read_canonical_le(reader)?,
            total_flight_ticks: u16::read_canonical_le(reader)?,
            start_pos_m: <[f64; 3]>::read_canonical_le(reader)?,
            target_pos_m: <[f64; 3]>::read_canonical_le(reader)?,
            intercepted_by: Option::<u8>::read_canonical_le(reader)?,
            will_bobble: bool::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct PlayerRuntimeState {
    pub is_present: bool,
    pub on_court_slot: Option<u8>,
    pub x_m: f64,
    pub y_m: f64,
    pub vx_m_s: f64,
    pub vy_m_s: f64,
    pub facing_rad: f64,
    pub action_state: PlayerActionState,
    pub action_timer_ds: u16,
    pub footwork: FootworkRecord,
    pub burst_energy: f64,
    pub cumulative_stamina: f64,
    pub rhythm_score: f64,
    pub personal_fouls: u8,
    pub foul_out_seq: Option<u8>,
    pub tech_unsportsmanlike_count: u8,
    pub tech_non_unsportsmanlike_count: u8,
    pub unsportsmanlike_fouls: u8,
    pub flagrant1_count: u8,
    pub flagrant2_count: u8,
    pub disqualifying_count: u8,
    pub ejected: bool,
    pub fouled_out_staying_on_court: bool,
    pub locked_out_for_game: bool,
    pub stint_lock_remaining_live_ticks: u16,
    pub fiba_sub_reentry_lock: FibaSubReentryLock,
    pub injury_state: PlayerInjuryState,
    pub court_time_live_ds: u32,
    pub paint_timer_ds: u16,
    pub exiting_paint_grace_ticks: u8,
    pub feet_set_ticks: u16,
    pub lgp_established_tick: Option<u32>,
    pub caught_ball_inside_ldb: bool,
    pub z_m: f64,
    pub vz_m_s: f64,
    pub oob_reentry_pending: bool,
    pub dribbles_since_catch: u8,
    pub offball_cut_kind: u8,
    pub offball_cut_timer_ds: u8,
    pub offball_cut_side: i8,
    pub help_reaction_timer_ds: u8,
    pub help_target_valid: bool,
    pub help_committed_xy: [f64; 2],
}

impl Default for PlayerRuntimeState {
    fn default() -> Self {
        Self {
            is_present: false,
            on_court_slot: None,
            x_m: 0.0,
            y_m: 0.0,
            vx_m_s: 0.0,
            vy_m_s: 0.0,
            facing_rad: 0.0,
            action_state: PlayerActionState::Idle,
            action_timer_ds: 0,
            footwork: FootworkRecord::default(),
            burst_energy: 100.0,
            cumulative_stamina: 100.0,
            rhythm_score: 0.0,
            personal_fouls: 0,
            foul_out_seq: None,
            tech_unsportsmanlike_count: 0,
            tech_non_unsportsmanlike_count: 0,
            unsportsmanlike_fouls: 0,
            flagrant1_count: 0,
            flagrant2_count: 0,
            disqualifying_count: 0,
            ejected: false,
            fouled_out_staying_on_court: false,
            locked_out_for_game: false,
            stint_lock_remaining_live_ticks: 0,
            fiba_sub_reentry_lock: FibaSubReentryLock::None,
            injury_state: PlayerInjuryState::Healthy,
            court_time_live_ds: 0,
            paint_timer_ds: 0,
            exiting_paint_grace_ticks: 0,
            feet_set_ticks: 0,
            lgp_established_tick: None,
            caught_ball_inside_ldb: false,
            z_m: 0.0,
            vz_m_s: 0.0,
            oob_reentry_pending: false,
            dribbles_since_catch: 0,
            offball_cut_kind: 0,
            offball_cut_timer_ds: 0,
            offball_cut_side: 1,
            help_reaction_timer_ds: 0,
            help_target_valid: false,
            help_committed_xy: [0.0, 0.0],
        }
    }
}

impl CanonicalLeBytes for PlayerRuntimeState {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.is_present.write_canonical_le(out);
        self.on_court_slot.write_canonical_le(out);
        self.x_m.write_canonical_le(out);
        self.y_m.write_canonical_le(out);
        self.vx_m_s.write_canonical_le(out);
        self.vy_m_s.write_canonical_le(out);
        self.facing_rad.write_canonical_le(out);
        self.action_state.write_canonical_le(out);
        self.action_timer_ds.write_canonical_le(out);
        self.footwork.write_canonical_le(out);
        self.burst_energy.write_canonical_le(out);
        self.cumulative_stamina.write_canonical_le(out);
        self.rhythm_score.write_canonical_le(out);
        self.personal_fouls.write_canonical_le(out);
        self.foul_out_seq.write_canonical_le(out);
        self.tech_unsportsmanlike_count.write_canonical_le(out);
        self.tech_non_unsportsmanlike_count.write_canonical_le(out);
        self.unsportsmanlike_fouls.write_canonical_le(out);
        self.flagrant1_count.write_canonical_le(out);
        self.flagrant2_count.write_canonical_le(out);
        self.disqualifying_count.write_canonical_le(out);
        self.ejected.write_canonical_le(out);
        self.fouled_out_staying_on_court.write_canonical_le(out);
        self.locked_out_for_game.write_canonical_le(out);
        self.stint_lock_remaining_live_ticks
            .write_canonical_le(out);
        self.fiba_sub_reentry_lock.write_canonical_le(out);
        self.injury_state.write_canonical_le(out);
        self.court_time_live_ds.write_canonical_le(out);
        self.paint_timer_ds.write_canonical_le(out);
        self.exiting_paint_grace_ticks.write_canonical_le(out);
        self.feet_set_ticks.write_canonical_le(out);
        self.lgp_established_tick.write_canonical_le(out);
        self.caught_ball_inside_ldb.write_canonical_le(out);
        self.z_m.write_canonical_le(out);
        self.vz_m_s.write_canonical_le(out);
        self.oob_reentry_pending.write_canonical_le(out);
        self.dribbles_since_catch.write_canonical_le(out);
        self.offball_cut_kind.write_canonical_le(out);
        self.offball_cut_timer_ds.write_canonical_le(out);
        self.offball_cut_side.write_canonical_le(out);
        self.help_reaction_timer_ds.write_canonical_le(out);
        self.help_target_valid.write_canonical_le(out);
        self.help_committed_xy.write_canonical_le(out);
    }

    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            is_present: bool::read_canonical_le(reader)?,
            on_court_slot: Option::<u8>::read_canonical_le(reader)?,
            x_m: f64::read_canonical_le(reader)?,
            y_m: f64::read_canonical_le(reader)?,
            vx_m_s: f64::read_canonical_le(reader)?,
            vy_m_s: f64::read_canonical_le(reader)?,
            facing_rad: f64::read_canonical_le(reader)?,
            action_state: PlayerActionState::read_canonical_le(reader)?,
            action_timer_ds: u16::read_canonical_le(reader)?,
            footwork: FootworkRecord::read_canonical_le(reader)?,
            burst_energy: f64::read_canonical_le(reader)?,
            cumulative_stamina: f64::read_canonical_le(reader)?,
            rhythm_score: f64::read_canonical_le(reader)?,
            personal_fouls: u8::read_canonical_le(reader)?,
            foul_out_seq: Option::<u8>::read_canonical_le(reader)?,
            tech_unsportsmanlike_count: u8::read_canonical_le(reader)?,
            tech_non_unsportsmanlike_count: u8::read_canonical_le(reader)?,
            unsportsmanlike_fouls: u8::read_canonical_le(reader)?,
            flagrant1_count: u8::read_canonical_le(reader)?,
            flagrant2_count: u8::read_canonical_le(reader)?,
            disqualifying_count: u8::read_canonical_le(reader)?,
            ejected: bool::read_canonical_le(reader)?,
            fouled_out_staying_on_court: bool::read_canonical_le(reader)?,
            locked_out_for_game: bool::read_canonical_le(reader)?,
            stint_lock_remaining_live_ticks: u16::read_canonical_le(reader)?,
            fiba_sub_reentry_lock: FibaSubReentryLock::read_canonical_le(reader)?,
            injury_state: PlayerInjuryState::read_canonical_le(reader)?,
            court_time_live_ds: u32::read_canonical_le(reader)?,
            paint_timer_ds: u16::read_canonical_le(reader)?,
            exiting_paint_grace_ticks: u8::read_canonical_le(reader)?,
            feet_set_ticks: u16::read_canonical_le(reader)?,
            lgp_established_tick: Option::<u32>::read_canonical_le(reader)?,
            caught_ball_inside_ldb: bool::read_canonical_le(reader)?,
            z_m: f64::read_canonical_le(reader)?,
            vz_m_s: f64::read_canonical_le(reader)?,
            oob_reentry_pending: bool::read_canonical_le(reader)?,
            dribbles_since_catch: u8::read_canonical_le(reader)?,
            offball_cut_kind: u8::read_canonical_le(reader)?,
            offball_cut_timer_ds: u8::read_canonical_le(reader)?,
            offball_cut_side: i8::read_canonical_le(reader)?,
            help_reaction_timer_ds: u8::read_canonical_le(reader)?,
            help_target_valid: bool::read_canonical_le(reader)?,
            help_committed_xy: <[f64; 2]>::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BallRuntimeState {
    pub position_m: [f64; 3],
    pub velocity_m_s: [f64; 3],
    pub ball_state: BallState,
    pub owner_slot: Option<u8>,
    pub last_touched_by_slot: Option<u8>,
    pub last_team_control_side: Option<CommandSide>,
    pub shot_ctx: Option<ShotContext>,
    pub pass_ctx: Option<PassFlightContext>,
    pub live_ball_03s_restricted: bool,
    pub rim_touched_this_shot: bool,
    pub backboard_contact_this_shot: BackboardContact,
    pub passes_over_backboard_top_plane: bool,
    pub last_passer_roster_idx: Option<u8>,
    pub last_pass_completed_tick: u32,
}

impl Default for BallRuntimeState {
    fn default() -> Self {
        Self {
            position_m: [0.0, 0.0, 2.5],
            velocity_m_s: [0.0, 0.0, 0.0],
            ball_state: BallState::Dead,
            owner_slot: None,
            last_touched_by_slot: None,
            last_team_control_side: None,
            shot_ctx: None,
            pass_ctx: None,
            live_ball_03s_restricted: false,
            rim_touched_this_shot: false,
            backboard_contact_this_shot: BackboardContact::None,
            passes_over_backboard_top_plane: false,
            last_passer_roster_idx: None,
            last_pass_completed_tick: 0,
        }
    }
}

impl CanonicalLeBytes for BallRuntimeState {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.position_m.write_canonical_le(out);
        self.velocity_m_s.write_canonical_le(out);
        self.ball_state.write_canonical_le(out);
        self.owner_slot.write_canonical_le(out);
        self.last_touched_by_slot.write_canonical_le(out);
        self.last_team_control_side.write_canonical_le(out);
        self.shot_ctx.write_canonical_le(out);
        self.pass_ctx.write_canonical_le(out);
        self.live_ball_03s_restricted.write_canonical_le(out);
        self.rim_touched_this_shot.write_canonical_le(out);
        self.backboard_contact_this_shot.write_canonical_le(out);
        self.passes_over_backboard_top_plane.write_canonical_le(out);
        self.last_passer_roster_idx.write_canonical_le(out);
        self.last_pass_completed_tick.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            position_m: <[f64; 3]>::read_canonical_le(reader)?,
            velocity_m_s: <[f64; 3]>::read_canonical_le(reader)?,
            ball_state: BallState::read_canonical_le(reader)?,
            owner_slot: Option::<u8>::read_canonical_le(reader)?,
            last_touched_by_slot: Option::<u8>::read_canonical_le(reader)?,
            last_team_control_side: Option::<CommandSide>::read_canonical_le(reader)?,
            shot_ctx: Option::<ShotContext>::read_canonical_le(reader)?,
            pass_ctx: Option::<PassFlightContext>::read_canonical_le(reader)?,
            live_ball_03s_restricted: bool::read_canonical_le(reader)?,
            rim_touched_this_shot: bool::read_canonical_le(reader)?,
            backboard_contact_this_shot: BackboardContact::read_canonical_le(reader)?,
            passes_over_backboard_top_plane: bool::read_canonical_le(reader)?,
            last_passer_roster_idx: Option::<u8>::read_canonical_le(reader)?,
            last_pass_completed_tick: u32::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct TeamRuntimeState {
    pub score_on_court: u16,
    pub score_official_override: Option<u16>,
    pub period_team_fouls: u8,
    pub l2m_period_fouls: u8,
    pub timeouts_remaining: u8,
    pub challenges_remaining: u8,
    pub first_challenge_used_and_won: bool,
    pub consecutive_timeouts_used_in_deadball: u8,
    pub team_momentum: f64,
    pub on_court_chemistry: f64,
    pub intentional_foul_mode: bool,
    pub consecutive_scoreless_possessions: u8,
    pub active_lineup_slots: [u8; 5],
}

impl Default for TeamRuntimeState {
    fn default() -> Self {
        Self {
            score_on_court: 0,
            score_official_override: None,
            period_team_fouls: 0,
            l2m_period_fouls: 0,
            timeouts_remaining: 7,
            challenges_remaining: 1,
            first_challenge_used_and_won: false,
            consecutive_timeouts_used_in_deadball: 0,
            team_momentum: 0.0,
            on_court_chemistry: 1.0,
            intentional_foul_mode: false,
            consecutive_scoreless_possessions: 0,
            active_lineup_slots: [0, 1, 2, 3, 4],
        }
    }
}

impl CanonicalLeBytes for TeamRuntimeState {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.score_on_court.write_canonical_le(out);
        self.score_official_override.write_canonical_le(out);
        self.period_team_fouls.write_canonical_le(out);
        self.l2m_period_fouls.write_canonical_le(out);
        self.timeouts_remaining.write_canonical_le(out);
        self.challenges_remaining.write_canonical_le(out);
        self.first_challenge_used_and_won.write_canonical_le(out);
        self.consecutive_timeouts_used_in_deadball
            .write_canonical_le(out);
        self.team_momentum.write_canonical_le(out);
        self.on_court_chemistry.write_canonical_le(out);
        self.intentional_foul_mode.write_canonical_le(out);
        self.consecutive_scoreless_possessions
            .write_canonical_le(out);
        self.active_lineup_slots.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            score_on_court: u16::read_canonical_le(reader)?,
            score_official_override: Option::<u16>::read_canonical_le(reader)?,
            period_team_fouls: u8::read_canonical_le(reader)?,
            l2m_period_fouls: u8::read_canonical_le(reader)?,
            timeouts_remaining: u8::read_canonical_le(reader)?,
            challenges_remaining: u8::read_canonical_le(reader)?,
            first_challenge_used_and_won: bool::read_canonical_le(reader)?,
            consecutive_timeouts_used_in_deadball: u8::read_canonical_le(reader)?,
            team_momentum: f64::read_canonical_le(reader)?,
            on_court_chemistry: f64::read_canonical_le(reader)?,
            intentional_foul_mode: bool::read_canonical_le(reader)?,
            consecutive_scoreless_possessions: u8::read_canonical_le(reader)?,
            active_lineup_slots: <[u8; 5]>::read_canonical_le(reader)?,
        })
    }
}

/// Complete 7-category deterministic `GameState` (§2.3 / §2.4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GameState {
    // 1. Clocks & Global Counters
    pub sim_tick: u32,
    pub quarter: u8,
    pub game_clock_rem_10us: u32,
    pub game_clock_ds: u16,
    pub game_clock_running: bool,
    pub game_clock_ticked_this_step: bool,
    pub q1_game_clock_ever_started: bool,
    pub shot_clock_rem_10us: Option<u32>,
    pub shot_clock_ds: Option<u16>,
    pub shot_clock_running: bool,
    pub ball_in_frontcourt: bool,
    pub backcourt_8s_timer_ds: u8,
    pub closely_guarded_timer_ds: u8,
    pub back_to_basket_timer_ds: u8,
    pub comments_iil_origin_clocks: Option<CommentsIILOrigin>,
    pub possession_id: u32,
    pub possession_tick: u16,
    pub possession_side: Option<CommandSide>,
    pub possession_arrow: CommandSide,
    pub opening_tip_winner: Option<CommandSide>,
    pub mandatory_timeout_state: MandatoryTimeoutState,
    pub rng_sample_counters: RngSampleCounters,
    pub next_ingress_seq: u64,
    pub player_commands_accepted_count: u16,
    pub system_commands_accepted_count: u16,
    pub replay_manifest_projected_bytes: u32,
    pub next_ruling_id: u64,
    pub next_event_seq: u64,
    pub next_shot_id: u32,
    pub next_foul_out_seq: u8,
    pub last_stoppage_resume_event_seq: u64,
    pub reviewable_ring_overflow_watermark_event_seq: u64,
    pub emitted_tick_event_cursor: u16,

    // 2. State Machine & Suspended Contexts
    pub cursor: EngineCursor,
    pub game_phase: GamePhase,
    pub active_pending_ruling: Option<PendingRuling>,
    pub pending_post_commit_timeout: Option<(CommandSide, AdvanceOption)>,
    pub pending_fiba_timeout_request: [bool; 2],
    pub scramble_origin: Option<ScrambleOrigin>,
    pub deferred_def_3s_violation: Option<(u8, bool)>,
    pub deferred_flopping_tech: Option<(CommandSide, u8)>,
    pub ft_series_queue: BoundedFtQueue,
    pub inbound_context: Option<InboundContext>,
    pub jump_ball_context: Option<JumpBallContext>,
    pub suspended_states: BoundedSuspendedStack<4>,
    pub pending_fiba_hcc_requests: [Option<PendingFibaHccRequest>; 2],
    pub fiba_hcc_generation: [u32; 2],
    pub recent_reviewable_events: BoundedReviewableEventRing<256>,
    pub dead_time_pending: Option<DeadTimeRecord>,
    pub end_reason: Option<GameEndReason>,

    // 3. Commands, Reservations & Dedup Watermarks
    pub command_queue: BoundedCommandQueue,
    pub pending_deadball_subs: BoundedSubQueue<10>,
    pub pending_deadball_timeouts: [Option<AdvanceOption>; 2],
    pub client_dedup_ring: [BoundedDedupWindow32; 2],
    pub highest_client_request_id: [u64; 2],

    // 4. Tactics & Adaptive State
    pub active_play_id: [PlayId; 2],
    pub pending_next_play_id: [Option<PlayId>; 2],
    pub play_phase_idx: [u8; 2],
    pub play_phase_elapsed_ds: [u16; 2],
    pub play_mirrored: [bool; 2],
    pub slot_bindings: [[u8; 5]; 2],
    pub defense_plans: [TeamDefensePlan; 2],
    pub active_pace: [u8; 2],
    pub full_court_press: [bool; 2],
    pub active_drive_corridor: u8,
    pub play_screen_contact_ds: [u8; 2],
    pub transition_window_ds: [u8; 2],
    pub shot_sub_stage: u8,
    pub shot_stage_ticks: u8,
    pub shot_gc_expired_pending: bool,
    pub scramble_ticks: u8,
    pub inbound_sub_stage: u8,
    pub inbound_stage_ticks: u8,
    pub inbound_pass_start_m: [f64; 3],
    pub inbound_pass_target_m: [f64; 3],
    pub ft_sub_stage: u8,
    pub ft_stage_ticks: u8,
    pub ft_pending_made: bool,
    pub tip_flight_ticks: u8,
    pub ball_fx_flags: u8,
    pub ball_fx_timer_ds: u8,
    pub slot_contact_timer_ds: [u8; 10],
    pub oob_spot_xy: [f64; 2],
    pub oob_is_endline: bool,
    pub last_timeout_tick: [u32; 2],
    pub scoring_run_pts: [u8; 2],
    pub screen_ready_ticks: [u8; 2],
    pub active_drive_target_xy: [f64; 2],
    pub pnr_read_branch: [u8; 2],
    pub xout_rotator_slot: [u8; 2],
    pub xout_timer_ds: [u8; 2],

    // 5. Full Roster Players (30 fixed slots)
    pub players: [PlayerRuntimeState; 30],

    // 6. Ball State
    pub ball: BallRuntimeState,

    // 7. Team States (Home=0, Away=1)
    pub team_states: [TeamRuntimeState; 2],
}

impl CanonicalLeBytes for GameState {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        // 1
        self.sim_tick.write_canonical_le(out);
        self.quarter.write_canonical_le(out);
        self.game_clock_rem_10us.write_canonical_le(out);
        self.game_clock_ds.write_canonical_le(out);
        self.game_clock_running.write_canonical_le(out);
        self.game_clock_ticked_this_step.write_canonical_le(out);
        self.q1_game_clock_ever_started.write_canonical_le(out);
        self.shot_clock_rem_10us.write_canonical_le(out);
        self.shot_clock_ds.write_canonical_le(out);
        self.shot_clock_running.write_canonical_le(out);
        self.ball_in_frontcourt.write_canonical_le(out);
        self.backcourt_8s_timer_ds.write_canonical_le(out);
        self.closely_guarded_timer_ds.write_canonical_le(out);
        self.back_to_basket_timer_ds.write_canonical_le(out);
        self.comments_iil_origin_clocks.write_canonical_le(out);
        self.possession_id.write_canonical_le(out);
        self.possession_tick.write_canonical_le(out);
        self.possession_side.write_canonical_le(out);
        self.possession_arrow.write_canonical_le(out);
        self.opening_tip_winner.write_canonical_le(out);
        self.mandatory_timeout_state.write_canonical_le(out);
        self.rng_sample_counters.write_canonical_le(out);
        self.next_ingress_seq.write_canonical_le(out);
        self.player_commands_accepted_count.write_canonical_le(out);
        self.system_commands_accepted_count.write_canonical_le(out);
        self.replay_manifest_projected_bytes.write_canonical_le(out);
        self.next_ruling_id.write_canonical_le(out);
        self.next_event_seq.write_canonical_le(out);
        self.next_shot_id.write_canonical_le(out);
        self.next_foul_out_seq.write_canonical_le(out);
        self.last_stoppage_resume_event_seq.write_canonical_le(out);
        self.reviewable_ring_overflow_watermark_event_seq
            .write_canonical_le(out);
        self.emitted_tick_event_cursor.write_canonical_le(out);
        // 2
        self.cursor.write_canonical_le(out);
        self.game_phase.write_canonical_le(out);
        self.active_pending_ruling.write_canonical_le(out);
        self.pending_post_commit_timeout.write_canonical_le(out);
        self.pending_fiba_timeout_request.write_canonical_le(out);
        self.scramble_origin.write_canonical_le(out);
        self.deferred_def_3s_violation.write_canonical_le(out);
        self.deferred_flopping_tech.write_canonical_le(out);
        self.ft_series_queue.write_canonical_le(out);
        self.inbound_context.write_canonical_le(out);
        self.jump_ball_context.write_canonical_le(out);
        self.suspended_states.write_canonical_le(out);
        self.pending_fiba_hcc_requests.write_canonical_le(out);
        self.fiba_hcc_generation.write_canonical_le(out);
        self.recent_reviewable_events.write_canonical_le(out);
        self.dead_time_pending.write_canonical_le(out);
        self.end_reason.write_canonical_le(out);
        // 3
        self.command_queue.write_canonical_le(out);
        self.pending_deadball_subs.write_canonical_le(out);
        self.pending_deadball_timeouts.write_canonical_le(out);
        self.client_dedup_ring.write_canonical_le(out);
        self.highest_client_request_id.write_canonical_le(out);
        // 4
        self.active_play_id.write_canonical_le(out);
        self.pending_next_play_id.write_canonical_le(out);
        self.play_phase_idx.write_canonical_le(out);
        self.play_phase_elapsed_ds.write_canonical_le(out);
        self.play_mirrored.write_canonical_le(out);
        self.slot_bindings.write_canonical_le(out);
        self.defense_plans.write_canonical_le(out);
        self.active_pace.write_canonical_le(out);
        self.full_court_press.write_canonical_le(out);
        self.active_drive_corridor.write_canonical_le(out);
        self.play_screen_contact_ds.write_canonical_le(out);
        self.transition_window_ds.write_canonical_le(out);
        self.shot_sub_stage.write_canonical_le(out);
        self.shot_stage_ticks.write_canonical_le(out);
        self.shot_gc_expired_pending.write_canonical_le(out);
        self.scramble_ticks.write_canonical_le(out);
        self.inbound_sub_stage.write_canonical_le(out);
        self.inbound_stage_ticks.write_canonical_le(out);
        self.inbound_pass_start_m.write_canonical_le(out);
        self.inbound_pass_target_m.write_canonical_le(out);
        self.ft_sub_stage.write_canonical_le(out);
        self.ft_stage_ticks.write_canonical_le(out);
        self.ft_pending_made.write_canonical_le(out);
        self.tip_flight_ticks.write_canonical_le(out);
        self.ball_fx_flags.write_canonical_le(out);
        self.ball_fx_timer_ds.write_canonical_le(out);
        self.slot_contact_timer_ds.write_canonical_le(out);
        self.oob_spot_xy.write_canonical_le(out);
        self.oob_is_endline.write_canonical_le(out);
        self.last_timeout_tick.write_canonical_le(out);
        self.scoring_run_pts.write_canonical_le(out);
        self.screen_ready_ticks.write_canonical_le(out);
        self.active_drive_target_xy.write_canonical_le(out);
        self.pnr_read_branch.write_canonical_le(out);
        self.xout_rotator_slot.write_canonical_le(out);
        self.xout_timer_ds.write_canonical_le(out);
        // 5
        self.players.write_canonical_le(out);
        // 6
        self.ball.write_canonical_le(out);
        // 7
        self.team_states.write_canonical_le(out);
    }

    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            sim_tick: u32::read_canonical_le(reader)?,
            quarter: u8::read_canonical_le(reader)?,
            game_clock_rem_10us: u32::read_canonical_le(reader)?,
            game_clock_ds: u16::read_canonical_le(reader)?,
            game_clock_running: bool::read_canonical_le(reader)?,
            game_clock_ticked_this_step: bool::read_canonical_le(reader)?,
            q1_game_clock_ever_started: bool::read_canonical_le(reader)?,
            shot_clock_rem_10us: Option::<u32>::read_canonical_le(reader)?,
            shot_clock_ds: Option::<u16>::read_canonical_le(reader)?,
            shot_clock_running: bool::read_canonical_le(reader)?,
            ball_in_frontcourt: bool::read_canonical_le(reader)?,
            backcourt_8s_timer_ds: u8::read_canonical_le(reader)?,
            closely_guarded_timer_ds: u8::read_canonical_le(reader)?,
            back_to_basket_timer_ds: u8::read_canonical_le(reader)?,
            comments_iil_origin_clocks: Option::<CommentsIILOrigin>::read_canonical_le(reader)?,
            possession_id: u32::read_canonical_le(reader)?,
            possession_tick: u16::read_canonical_le(reader)?,
            possession_side: Option::<CommandSide>::read_canonical_le(reader)?,
            possession_arrow: CommandSide::read_canonical_le(reader)?,
            opening_tip_winner: Option::<CommandSide>::read_canonical_le(reader)?,
            mandatory_timeout_state: MandatoryTimeoutState::read_canonical_le(reader)?,
            rng_sample_counters: RngSampleCounters::read_canonical_le(reader)?,
            next_ingress_seq: u64::read_canonical_le(reader)?,
            player_commands_accepted_count: u16::read_canonical_le(reader)?,
            system_commands_accepted_count: u16::read_canonical_le(reader)?,
            replay_manifest_projected_bytes: u32::read_canonical_le(reader)?,
            next_ruling_id: u64::read_canonical_le(reader)?,
            next_event_seq: u64::read_canonical_le(reader)?,
            next_shot_id: u32::read_canonical_le(reader)?,
            next_foul_out_seq: u8::read_canonical_le(reader)?,
            last_stoppage_resume_event_seq: u64::read_canonical_le(reader)?,
            reviewable_ring_overflow_watermark_event_seq: u64::read_canonical_le(reader)?,
            emitted_tick_event_cursor: u16::read_canonical_le(reader)?,
            cursor: EngineCursor::read_canonical_le(reader)?,
            game_phase: GamePhase::read_canonical_le(reader)?,
            active_pending_ruling: Option::<PendingRuling>::read_canonical_le(reader)?,
            pending_post_commit_timeout: Option::<(CommandSide, AdvanceOption)>::read_canonical_le(
                reader,
            )?,
            pending_fiba_timeout_request: <[bool; 2]>::read_canonical_le(reader)?,
            scramble_origin: Option::<ScrambleOrigin>::read_canonical_le(reader)?,
            deferred_def_3s_violation: Option::<(u8, bool)>::read_canonical_le(reader)?,
            deferred_flopping_tech: Option::<(CommandSide, u8)>::read_canonical_le(reader)?,
            ft_series_queue: BoundedFtQueue::read_canonical_le(reader)?,
            inbound_context: Option::<InboundContext>::read_canonical_le(reader)?,
            jump_ball_context: Option::<JumpBallContext>::read_canonical_le(reader)?,
            suspended_states: BoundedSuspendedStack::<4>::read_canonical_le(reader)?,
            pending_fiba_hcc_requests: <[Option<PendingFibaHccRequest>; 2]>::read_canonical_le(
                reader,
            )?,
            fiba_hcc_generation: <[u32; 2]>::read_canonical_le(reader)?,
            recent_reviewable_events: BoundedReviewableEventRing::<256>::read_canonical_le(reader)?,
            dead_time_pending: Option::<DeadTimeRecord>::read_canonical_le(reader)?,
            end_reason: Option::<GameEndReason>::read_canonical_le(reader)?,
            command_queue: BoundedCommandQueue::read_canonical_le(reader)?,
            pending_deadball_subs: BoundedSubQueue::<10>::read_canonical_le(reader)?,
            pending_deadball_timeouts: <[Option<AdvanceOption>; 2]>::read_canonical_le(reader)?,
            client_dedup_ring: <[BoundedDedupWindow32; 2]>::read_canonical_le(reader)?,
            highest_client_request_id: <[u64; 2]>::read_canonical_le(reader)?,
            active_play_id: <[PlayId; 2]>::read_canonical_le(reader)?,
            pending_next_play_id: <[Option<PlayId>; 2]>::read_canonical_le(reader)?,
            play_phase_idx: <[u8; 2]>::read_canonical_le(reader)?,
            play_phase_elapsed_ds: <[u16; 2]>::read_canonical_le(reader)?,
            play_mirrored: <[bool; 2]>::read_canonical_le(reader)?,
            slot_bindings: <[[u8; 5]; 2]>::read_canonical_le(reader)?,
            defense_plans: <[TeamDefensePlan; 2]>::read_canonical_le(reader)?,
            active_pace: <[u8; 2]>::read_canonical_le(reader)?,
            full_court_press: <[bool; 2]>::read_canonical_le(reader)?,
            active_drive_corridor: u8::read_canonical_le(reader)?,
            play_screen_contact_ds: <[u8; 2]>::read_canonical_le(reader)?,
            transition_window_ds: <[u8; 2]>::read_canonical_le(reader)?,
            shot_sub_stage: u8::read_canonical_le(reader)?,
            shot_stage_ticks: u8::read_canonical_le(reader)?,
            shot_gc_expired_pending: bool::read_canonical_le(reader)?,
            scramble_ticks: u8::read_canonical_le(reader)?,
            inbound_sub_stage: u8::read_canonical_le(reader)?,
            inbound_stage_ticks: u8::read_canonical_le(reader)?,
            inbound_pass_start_m: <[f64; 3]>::read_canonical_le(reader)?,
            inbound_pass_target_m: <[f64; 3]>::read_canonical_le(reader)?,
            ft_sub_stage: u8::read_canonical_le(reader)?,
            ft_stage_ticks: u8::read_canonical_le(reader)?,
            ft_pending_made: bool::read_canonical_le(reader)?,
            tip_flight_ticks: u8::read_canonical_le(reader)?,
            ball_fx_flags: u8::read_canonical_le(reader)?,
            ball_fx_timer_ds: u8::read_canonical_le(reader)?,
            slot_contact_timer_ds: <[u8; 10]>::read_canonical_le(reader)?,
            oob_spot_xy: <[f64; 2]>::read_canonical_le(reader)?,
            oob_is_endline: bool::read_canonical_le(reader)?,
            last_timeout_tick: <[u32; 2]>::read_canonical_le(reader)?,
            scoring_run_pts: <[u8; 2]>::read_canonical_le(reader)?,
            screen_ready_ticks: <[u8; 2]>::read_canonical_le(reader)?,
            active_drive_target_xy: <[f64; 2]>::read_canonical_le(reader)?,
            pnr_read_branch: <[u8; 2]>::read_canonical_le(reader)?,
            xout_rotator_slot: <[u8; 2]>::read_canonical_le(reader)?,
            xout_timer_ds: <[u8; 2]>::read_canonical_le(reader)?,
            players: <[PlayerRuntimeState; 30]>::read_canonical_le(reader)?,
            ball: BallRuntimeState::read_canonical_le(reader)?,
            team_states: <[TeamRuntimeState; 2]>::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EngineCheckpointSnapshotV1 {
    pub state: GameState,
    pub prev_rolling_hash: [u8; 32],
    pub current_tick_events: Vec<GameRecordEventV1>,
}

impl CanonicalLeBytes for EngineCheckpointSnapshotV1 {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.state.write_canonical_le(out);
        self.prev_rolling_hash.write_canonical_le(out);
        self.current_tick_events.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            state: GameState::read_canonical_le(reader)?,
            prev_rolling_hash: <[u8; 32]>::read_canonical_le(reader)?,
            current_tick_events: Vec::<GameRecordEventV1>::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayVerificationV1 {
    pub state_hash_checkpoints: Vec<(u32, [u8; 32])>,
    pub final_tick: u32,
    pub final_state_hash: [u8; 32],
}

impl CanonicalLeBytes for ReplayVerificationV1 {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.state_hash_checkpoints.write_canonical_le(out);
        self.final_tick.write_canonical_le(out);
        self.final_state_hash.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            state_hash_checkpoints: Vec::<(u32, [u8; 32])>::read_canonical_le(reader)?,
            final_tick: u32::read_canonical_le(reader)?,
            final_state_hash: <[u8; 32]>::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReplayManifest {
    pub init_spec: MatchInitSpecV1,
    pub applied_commands: Vec<AnchoredCommand>,
    pub verification: Option<ReplayVerificationV1>,
}

impl CanonicalLeBytes for ReplayManifest {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.init_spec.write_canonical_le(out);
        self.applied_commands.write_canonical_le(out);
        self.verification.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            init_spec: MatchInitSpecV1::read_canonical_le(reader)?,
            applied_commands: Vec::<AnchoredCommand>::read_canonical_le(reader)?,
            verification: Option::<ReplayVerificationV1>::read_canonical_le(reader)?,
        })
    }
}
