use serde::{Deserialize, Serialize};

use crate::canonical_le_bytes::{CanonicalLeBytes, CanonicalReader, DecodeError};

pub type RosterIdx = u8;
pub const ROSTER_IDX_NONE: u8 = 0xFF;

/// Shot clock 3-state encoding helper (§2.2 / §10.3.1):
/// `0..=240` => Some(ds), `0xFFFF` => None, `0xFFFE` => Unchanged (events only).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TriStateShotClock {
    Value(u16),
    None,
    Unchanged,
}

impl TriStateShotClock {
    pub fn to_wire(self) -> u16 {
        match self {
            Self::Value(v) => v,
            Self::None => 0xFFFF,
            Self::Unchanged => 0xFFFE,
        }
    }

    pub fn from_wire(v: u16, allow_unchanged: bool) -> Result<Self, DecodeError> {
        if v <= 240 {
            Ok(Self::Value(v))
        } else if v == 0xFFFF {
            Ok(Self::None)
        } else if allow_unchanged && v == 0xFFFE {
            Ok(Self::Unchanged)
        } else {
            Err(DecodeError::InvalidEnumValue {
                field: "shot_clock_ds",
                value: v as u32,
            })
        }
    }
}

pub fn encode_shot_clock_opt(opt: Option<u16>) -> u16 {
    match opt {
        Some(v) => v.min(240),
        None => 0xFFFF,
    }
}

pub fn decode_shot_clock_opt(v: u16) -> Result<Option<u16>, DecodeError> {
    if v <= 240 {
        Ok(Some(v))
    } else if v == 0xFFFF {
        Ok(None)
    } else {
        Err(DecodeError::InvalidEnumValue {
            field: "shot_clock_ds",
            value: v as u32,
        })
    }
}

/// Frozen 38 RuleCodes (`0x0000..=0x0025`, §8.4).
#[repr(u16)]
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuleCode {
    NONE = 0x0000,
    VIO_OOB = 0x0001,
    VIO_SHOT_CLOCK = 0x0002,
    VIO_8S = 0x0003,
    VIO_BACKCOURT = 0x0004,
    VIO_OFF_3S = 0x0005,
    VIO_DEF_3S = 0x0006,
    VIO_5S_INBOUND = 0x0007,
    VIO_5S_POST = 0x0008,
    VIO_5S_CLOSELY_GUARDED = 0x0009,
    VIO_03S_EXPIRED = 0x000A,
    VIO_TRAVEL = 0x000B,
    VIO_DOUBLE_DRIBBLE = 0x000C,
    VIO_KICKED_BALL = 0x000D,
    VIO_JUMP_BALL = 0x000E,
    VIO_FT_VIOLATION = 0x000F,
    VIO_GOALTEND = 0x0010,
    VIO_BASKET_INTERFERENCE = 0x0011,
    HELD_BALL = 0x0012,
    WHISTLE_INADVERTENT = 0x0013,
    FOUL_SHOOTING = 0x0014,
    FOUL_REACH_COMMON = 0x0015,
    FOUL_AWAY_FROM_PLAY = 0x0016,
    FOUL_INBOUND_BEFORE_RELEASE = 0x0017,
    FOUL_BLOCKING = 0x0018,
    FOUL_CHARGE = 0x0019,
    FOUL_ILLEGAL_SCREEN = 0x001A,
    FOUL_LOOSE_BALL = 0x001B,
    FOUL_DOUBLE = 0x001C,
    FOUL_TRANSITION_TAKE = 0x001D,
    FOUL_CLEAR_PATH = 0x001E,
    FOUL_TECH_CONDUCT = 0x001F,
    FOUL_TECH_EXCESSIVE_TIMEOUT = 0x0020,
    FOUL_FLAGRANT_1 = 0x0021,
    FOUL_FLAGRANT_2 = 0x0022,
    FOUL_UNSPORTSMANLIKE = 0x0023,
    FOUL_DISQUALIFYING = 0x0024,
    FOUL_TECH_FLOPPING = 0x0025,
}

impl RuleCode {
    pub const COUNT: usize = 38;

    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            0x0000 => Some(Self::NONE),
            0x0001 => Some(Self::VIO_OOB),
            0x0002 => Some(Self::VIO_SHOT_CLOCK),
            0x0003 => Some(Self::VIO_8S),
            0x0004 => Some(Self::VIO_BACKCOURT),
            0x0005 => Some(Self::VIO_OFF_3S),
            0x0006 => Some(Self::VIO_DEF_3S),
            0x0007 => Some(Self::VIO_5S_INBOUND),
            0x0008 => Some(Self::VIO_5S_POST),
            0x0009 => Some(Self::VIO_5S_CLOSELY_GUARDED),
            0x000A => Some(Self::VIO_03S_EXPIRED),
            0x000B => Some(Self::VIO_TRAVEL),
            0x000C => Some(Self::VIO_DOUBLE_DRIBBLE),
            0x000D => Some(Self::VIO_KICKED_BALL),
            0x000E => Some(Self::VIO_JUMP_BALL),
            0x000F => Some(Self::VIO_FT_VIOLATION),
            0x0010 => Some(Self::VIO_GOALTEND),
            0x0011 => Some(Self::VIO_BASKET_INTERFERENCE),
            0x0012 => Some(Self::HELD_BALL),
            0x0013 => Some(Self::WHISTLE_INADVERTENT),
            0x0014 => Some(Self::FOUL_SHOOTING),
            0x0015 => Some(Self::FOUL_REACH_COMMON),
            0x0016 => Some(Self::FOUL_AWAY_FROM_PLAY),
            0x0017 => Some(Self::FOUL_INBOUND_BEFORE_RELEASE),
            0x0018 => Some(Self::FOUL_BLOCKING),
            0x0019 => Some(Self::FOUL_CHARGE),
            0x001A => Some(Self::FOUL_ILLEGAL_SCREEN),
            0x001B => Some(Self::FOUL_LOOSE_BALL),
            0x001C => Some(Self::FOUL_DOUBLE),
            0x001D => Some(Self::FOUL_TRANSITION_TAKE),
            0x001E => Some(Self::FOUL_CLEAR_PATH),
            0x001F => Some(Self::FOUL_TECH_CONDUCT),
            0x0020 => Some(Self::FOUL_TECH_EXCESSIVE_TIMEOUT),
            0x0021 => Some(Self::FOUL_FLAGRANT_1),
            0x0022 => Some(Self::FOUL_FLAGRANT_2),
            0x0023 => Some(Self::FOUL_UNSPORTSMANLIKE),
            0x0024 => Some(Self::FOUL_DISQUALIFYING),
            0x0025 => Some(Self::FOUL_TECH_FLOPPING),
            _ => None,
        }
    }

    pub fn is_foul(self) -> bool {
        (self as u16) >= 0x0014
    }

    pub fn is_personal_foul(self) -> bool {
        matches!(
            self,
            Self::FOUL_SHOOTING
                | Self::FOUL_REACH_COMMON
                | Self::FOUL_AWAY_FROM_PLAY
                | Self::FOUL_INBOUND_BEFORE_RELEASE
                | Self::FOUL_BLOCKING
                | Self::FOUL_CHARGE
                | Self::FOUL_ILLEGAL_SCREEN
                | Self::FOUL_LOOSE_BALL
                | Self::FOUL_DOUBLE
                | Self::FOUL_TRANSITION_TAKE
                | Self::FOUL_CLEAR_PATH
                | Self::FOUL_FLAGRANT_1
                | Self::FOUL_FLAGRANT_2
                | Self::FOUL_UNSPORTSMANLIKE
                | Self::FOUL_DISQUALIFYING
        )
    }

    pub fn is_offensive_foul(self) -> bool {
        matches!(self, Self::FOUL_CHARGE | Self::FOUL_ILLEGAL_SCREEN)
    }
}

impl CanonicalLeBytes for RuleCode {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        (*self as u16).write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = u16::read_canonical_le(reader)?;
        Self::from_u16(v).ok_or(DecodeError::InvalidEnumValue {
            field: "RuleCode",
            value: v as u32,
        })
    }
}

/// Frozen 28 CommandRejectReason items (`0x0001..=0x001C`, §2.2).
#[repr(u16)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommandRejectReason {
    InvalidCodecOrPadding = 0x0001,
    PayloadTooLarge = 0x0002,
    RateLimited = 0x0003,
    QueueFull = 0x0004,
    GameCommandLimitExceeded = 0x0005,
    CommandLogStorageStalled = 0x0006,
    ClientRequestIdTooOld = 0x0007,
    IdempotencyPayloadMismatch = 0x0008,
    ForbiddenSystemCommand = 0x0009,
    InvalidPhase = 0x000A,
    Expired = 0x000B,
    StaleWindowEpoch = 0x000C,
    StaleRequestSeq = 0x000D,
    ChallengeDisabled = 0x000E,
    UnchallengeableRuling = 0x000F,
    NoChallengesRemaining = 0x0010,
    ChallengeAlreadyGranted = 0x0011,
    RulingIdMismatch = 0x0012,
    JumpBallAlreadyTossed = 0x0013,
    NotInPlayerControl = 0x0014,
    TickCommandSlotFull = 0x0015,
    ChallengeTargetPastFirstStoppage = 0x0016,
    WindowChallengeCycleLimitReached = 0x0017,
    TickEventCapacityReserved = 0x0018,
    MatchAlreadyEnded = 0x0019,
    ReviewableEventEvicted = 0x001A,
    ReplayManifestCapacityExceeded = 0x001B,
    SubIneligibleUntilClockRun = 0x001C,
}

impl CommandRejectReason {
    pub const COUNT: usize = 28;

    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            0x0001 => Some(Self::InvalidCodecOrPadding),
            0x0002 => Some(Self::PayloadTooLarge),
            0x0003 => Some(Self::RateLimited),
            0x0004 => Some(Self::QueueFull),
            0x0005 => Some(Self::GameCommandLimitExceeded),
            0x0006 => Some(Self::CommandLogStorageStalled),
            0x0007 => Some(Self::ClientRequestIdTooOld),
            0x0008 => Some(Self::IdempotencyPayloadMismatch),
            0x0009 => Some(Self::ForbiddenSystemCommand),
            0x000A => Some(Self::InvalidPhase),
            0x000B => Some(Self::Expired),
            0x000C => Some(Self::StaleWindowEpoch),
            0x000D => Some(Self::StaleRequestSeq),
            0x000E => Some(Self::ChallengeDisabled),
            0x000F => Some(Self::UnchallengeableRuling),
            0x0010 => Some(Self::NoChallengesRemaining),
            0x0011 => Some(Self::ChallengeAlreadyGranted),
            0x0012 => Some(Self::RulingIdMismatch),
            0x0013 => Some(Self::JumpBallAlreadyTossed),
            0x0014 => Some(Self::NotInPlayerControl),
            0x0015 => Some(Self::TickCommandSlotFull),
            0x0016 => Some(Self::ChallengeTargetPastFirstStoppage),
            0x0017 => Some(Self::WindowChallengeCycleLimitReached),
            0x0018 => Some(Self::TickEventCapacityReserved),
            0x0019 => Some(Self::MatchAlreadyEnded),
            0x001A => Some(Self::ReviewableEventEvicted),
            0x001B => Some(Self::ReplayManifestCapacityExceeded),
            0x001C => Some(Self::SubIneligibleUntilClockRun),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for CommandRejectReason {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        (*self as u16).write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = u16::read_canonical_le(reader)?;
        Self::from_u16(v).ok_or(DecodeError::InvalidEnumValue {
            field: "CommandRejectReason",
            value: v as u32,
        })
    }
}

/// Frozen 11 CommandAbortReason items (`0x01..=0x0B`, §3.3 / §10.3.1).
#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommandAbortReason {
    MootedByPriorOverturn = 0x01,
    ActorInjuredOrEjected = 0x02,
    ActorFouledOut = 0x03,
    StaleWindowEpochAtExecute = 0x04,
    StaleRequestSeqAtExecute = 0x05,
    PrerequisiteTimeoutUnavailable = 0x06,
    ChallengeAlreadyExhausted = 0x07,
    TargetOutOfStoppageScope = 0x08,
    MatchEndedBeforeExecution = 0x09,
    SupercededByLaterForfeit = 0x0A,
    SubIneligibleUntilClockRunAtExecute = 0x0B,
}

impl CommandAbortReason {
    pub const COUNT: usize = 11;

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::MootedByPriorOverturn),
            0x02 => Some(Self::ActorInjuredOrEjected),
            0x03 => Some(Self::ActorFouledOut),
            0x04 => Some(Self::StaleWindowEpochAtExecute),
            0x05 => Some(Self::StaleRequestSeqAtExecute),
            0x06 => Some(Self::PrerequisiteTimeoutUnavailable),
            0x07 => Some(Self::ChallengeAlreadyExhausted),
            0x08 => Some(Self::TargetOutOfStoppageScope),
            0x09 => Some(Self::MatchEndedBeforeExecution),
            0x0A => Some(Self::SupercededByLaterForfeit),
            0x0B => Some(Self::SubIneligibleUntilClockRunAtExecute),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for CommandAbortReason {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "CommandAbortReason",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommandSide {
    Home = 0,
    Away = 1,
    System = 2,
}

impl CommandSide {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Home),
            1 => Some(Self::Away),
            2 => Some(Self::System),
            _ => None,
        }
    }

    pub fn team_index(self) -> Option<usize> {
        match self {
            Self::Home => Some(0),
            Self::Away => Some(1),
            Self::System => None,
        }
    }

    pub fn opponent(self) -> Self {
        match self {
            Self::Home => Self::Away,
            Self::Away => Self::Home,
            Self::System => Self::System,
        }
    }
}

impl CanonicalLeBytes for CommandSide {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "CommandSide",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommandPhase {
    PreTick = 0,
    PostWhistle = 1,
}

impl CommandPhase {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::PreTick),
            1 => Some(Self::PostWhistle),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for CommandPhase {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "CommandPhase",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EngineCursor {
    ReadyForPreTick = 0,
    PausedInPostWhistle {
        window_open_tick: u32,
        ruling_id: u64,
    } = 1,
    PendingForfeitFinalize {
        forfeiting_side: CommandSide,
        cleared_ruling_id: Option<u64>,
    } = 2,
    GameFinished = 3,
}

impl CanonicalLeBytes for EngineCursor {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        match *self {
            Self::ReadyForPreTick => out.push(0),
            Self::PausedInPostWhistle {
                window_open_tick,
                ruling_id,
            } => {
                out.push(1);
                window_open_tick.write_canonical_le(out);
                ruling_id.write_canonical_le(out);
            }
            Self::PendingForfeitFinalize {
                forfeiting_side,
                cleared_ruling_id,
            } => {
                out.push(2);
                forfeiting_side.write_canonical_le(out);
                cleared_ruling_id.write_canonical_le(out);
            }
            Self::GameFinished => out.push(3),
        }
    }

    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::ReadyForPreTick),
            1 => Ok(Self::PausedInPostWhistle {
                window_open_tick: u32::read_canonical_le(reader)?,
                ruling_id: u64::read_canonical_le(reader)?,
            }),
            2 => Ok(Self::PendingForfeitFinalize {
                forfeiting_side: CommandSide::read_canonical_le(reader)?,
                cleared_ruling_id: Option::<u64>::read_canonical_le(reader)?,
            }),
            3 => Ok(Self::GameFinished),
            v => Err(DecodeError::InvalidEnumValue {
                field: "EngineCursor",
                value: v as u32,
            }),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DeterminismViolationKind {
    RngCounterSaturated = 0,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StepStatus {
    TickCompleted {
        tick: u32,
    } = 1,
    AwaitingPostWhistle {
        window_open_tick: u32,
        ruling_id: u64,
        window_state: ChallengeWindowState,
    } = 2,
    Finished {
        tick: u32,
    } = 3,
    Aborted {
        tick: u32,
        reason: DeterminismViolationKind,
    } = 4,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum GamePhase {
    JumpBall = 0,
    LiveBall = 1,
    ShotInFlight = 2,
    ReboundScramble = 3,
    WhistlePendingCommit = 4,
    CoachChallengeReview = 5,
    FreeThrows = 6,
    InboundSetup = 7,
    Timeout = 8,
    PeriodBreak = 9,
    GameOver = 10,
}

impl GamePhase {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::JumpBall),
            1 => Some(Self::LiveBall),
            2 => Some(Self::ShotInFlight),
            3 => Some(Self::ReboundScramble),
            4 => Some(Self::WhistlePendingCommit),
            5 => Some(Self::CoachChallengeReview),
            6 => Some(Self::FreeThrows),
            7 => Some(Self::InboundSetup),
            8 => Some(Self::Timeout),
            9 => Some(Self::PeriodBreak),
            10 => Some(Self::GameOver),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for GamePhase {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "GamePhase",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum GameEndReason {
    Normal = 0,
    FibaLossByDefault = 1,
    ForfeitHome = 2,
    ForfeitAway = 3,
    MaxSimTicksExceeded = 4,
    DeterminismInvariantViolation = 5,
}

impl GameEndReason {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Normal),
            1 => Some(Self::FibaLossByDefault),
            2 => Some(Self::ForfeitHome),
            3 => Some(Self::ForfeitAway),
            4 => Some(Self::MaxSimTicksExceeded),
            5 => Some(Self::DeterminismInvariantViolation),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for GameEndReason {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "GameEndReason",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlayerActionState {
    Idle = 0,
    Jog = 1,
    Sprint = 2,
    Dribble = 3,
    Drive = 4,
    PostBackdown = 5,
    SetScreen = 6,
    NavScreen = 7,
    Pass = 8,
    Catch = 9,
    ShootJumper = 10,
    Layup = 11,
    Dunk = 12,
    DefSlide = 13,
    Closeout = 14,
    ContestJump = 15,
    BoxOut = 16,
    ReboundJump = 17,
    HustleDive = 18,
    Fallen = 19,
}

impl PlayerActionState {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Idle),
            1 => Some(Self::Jog),
            2 => Some(Self::Sprint),
            3 => Some(Self::Dribble),
            4 => Some(Self::Drive),
            5 => Some(Self::PostBackdown),
            6 => Some(Self::SetScreen),
            7 => Some(Self::NavScreen),
            8 => Some(Self::Pass),
            9 => Some(Self::Catch),
            10 => Some(Self::ShootJumper),
            11 => Some(Self::Layup),
            12 => Some(Self::Dunk),
            13 => Some(Self::DefSlide),
            14 => Some(Self::Closeout),
            15 => Some(Self::ContestJump),
            16 => Some(Self::BoxOut),
            17 => Some(Self::ReboundJump),
            18 => Some(Self::HustleDive),
            19 => Some(Self::Fallen),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for PlayerActionState {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "PlayerActionState",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlayerInjuryState {
    Healthy = 0,
    ConcussionEvaluation = 1,
    MinorOut { recovery_remaining_live_ticks: u16 } = 2,
    OutForGame = 3,
}

impl CanonicalLeBytes for PlayerInjuryState {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        match *self {
            Self::Healthy => out.push(0),
            Self::ConcussionEvaluation => out.push(1),
            Self::MinorOut {
                recovery_remaining_live_ticks,
            } => {
                out.push(2);
                recovery_remaining_live_ticks.write_canonical_le(out);
            }
            Self::OutForGame => out.push(3),
        }
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::Healthy),
            1 => Ok(Self::ConcussionEvaluation),
            2 => Ok(Self::MinorOut {
                recovery_remaining_live_ticks: u16::read_canonical_le(reader)?,
            }),
            3 => Ok(Self::OutForGame),
            v => Err(DecodeError::InvalidEnumValue {
                field: "PlayerInjuryState",
                value: v as u32,
            }),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FibaSubReentryLock {
    None = 0,
    SubbedOutCannotReenter = 1,
    SubbedInCannotLeave = 2,
}

impl FibaSubReentryLock {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::SubbedOutCannotReenter),
            2 => Some(Self::SubbedInCannotLeave),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for FibaSubReentryLock {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "FibaSubReentryLock",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BallState {
    Held = 0,
    Dribble = 1,
    PassFlight = 2,
    ShotFlight = 3,
    Loose = 4,
    Dead = 5,
}

impl BallState {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Held),
            1 => Some(Self::Dribble),
            2 => Some(Self::PassFlight),
            3 => Some(Self::ShotFlight),
            4 => Some(Self::Loose),
            5 => Some(Self::Dead),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for BallState {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "BallState",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DribbleStatus {
    None = 0,
    LiveTripleThreat = 1,
    Dribbling = 2,
    Gathered = 3,
    AirborneWithBall = 4,
}

impl DribbleStatus {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::LiveTripleThreat),
            2 => Some(Self::Dribbling),
            3 => Some(Self::Gathered),
            4 => Some(Self::AirborneWithBall),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for DribbleStatus {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "DribbleStatus",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PivotFoot {
    Unset = 0,
    Left = 1,
    Right = 2,
    LockedBothFeetAfterStep1JumpStop = 3,
}

impl PivotFoot {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Unset),
            1 => Some(Self::Left),
            2 => Some(Self::Right),
            3 => Some(Self::LockedBothFeetAfterStep1JumpStop),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for PivotFoot {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "PivotFoot",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BackboardContact {
    None = 0,
    BelowRimLevel = 1,
    AtOrAboveRim = 2,
}

impl BackboardContact {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::BelowRimLevel),
            2 => Some(Self::AtOrAboveRim),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for BackboardContact {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "BackboardContact",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ScrambleOrigin {
    FromRimContact { prior_offense: CommandSide } = 0,
    FromFreeThrowMiss { prior_offense: CommandSide } = 1,
    FromNoRimMissOrBlock { prior_offense: CommandSide } = 2,
}

impl ScrambleOrigin {
    pub fn prior_offense(self) -> CommandSide {
        match self {
            Self::FromRimContact { prior_offense }
            | Self::FromFreeThrowMiss { prior_offense }
            | Self::FromNoRimMissOrBlock { prior_offense } => prior_offense,
        }
    }

    pub fn tag(self) -> u8 {
        match self {
            Self::FromRimContact { .. } => 0,
            Self::FromFreeThrowMiss { .. } => 1,
            Self::FromNoRimMissOrBlock { .. } => 2,
        }
    }
}

impl CanonicalLeBytes for ScrambleOrigin {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(self.tag());
        self.prior_offense().write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let tag = reader.read_u8()?;
        let side = CommandSide::read_canonical_le(reader)?;
        match tag {
            0 => Ok(Self::FromRimContact {
                prior_offense: side,
            }),
            1 => Ok(Self::FromFreeThrowMiss {
                prior_offense: side,
            }),
            2 => Ok(Self::FromNoRimMissOrBlock {
                prior_offense: side,
            }),
            v => Err(DecodeError::InvalidEnumValue {
                field: "ScrambleOrigin",
                value: v as u32,
            }),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum InboundSpot {
    BackcourtEndline = 0,
    CenterLineExtendedOppositeTable = 1,
    FrontcourtSideline28ft = 2,
    FreeThrowLineExtended = 3,
    NearestToInterruption = 4,
    FibaFrontcourtThrowInLine = 5,
    OriginalInboundSpot = 6,
}

impl InboundSpot {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::BackcourtEndline),
            1 => Some(Self::CenterLineExtendedOppositeTable),
            2 => Some(Self::FrontcourtSideline28ft),
            3 => Some(Self::FreeThrowLineExtended),
            4 => Some(Self::NearestToInterruption),
            5 => Some(Self::FibaFrontcourtThrowInLine),
            6 => Some(Self::OriginalInboundSpot),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for InboundSpot {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "InboundSpot",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CourtSideLine {
    TableSide = 0,
    OppositeTableSide = 1,
}

impl CourtSideLine {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::TableSide),
            1 => Some(Self::OppositeTableSide),
            _ => None,
        }
    }

    pub fn from_y_m(y_m: f64) -> Self {
        if y_m <= 0.0 {
            Self::TableSide
        } else {
            Self::OppositeTableSide
        }
    }
}

impl CanonicalLeBytes for CourtSideLine {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "CourtSideLine",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum JumpBallSpot {
    CenterCircle = 0,
    HomeFreeThrowCircle = 1,
    AwayFreeThrowCircle = 2,
    NearestCircle = 3,
}

impl JumpBallSpot {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::CenterCircle),
            1 => Some(Self::HomeFreeThrowCircle),
            2 => Some(Self::AwayFreeThrowCircle),
            3 => Some(Self::NearestCircle),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for JumpBallSpot {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "JumpBallSpot",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TimeoutReason {
    TeamCalled { side: CommandSide } = 0,
    Mandatory { team_side: CommandSide } = 1,
}

impl CanonicalLeBytes for TimeoutReason {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        match *self {
            Self::TeamCalled { side } => {
                out.push(0);
                side.write_canonical_le(out);
            }
            Self::Mandatory { team_side } => {
                out.push(1);
                team_side.write_canonical_le(out);
            }
        }
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::TeamCalled {
                side: CommandSide::read_canonical_le(reader)?,
            }),
            1 => Ok(Self::Mandatory {
                team_side: CommandSide::read_canonical_le(reader)?,
            }),
            v => Err(DecodeError::InvalidEnumValue {
                field: "TimeoutReason",
                value: v as u32,
            }),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChallengeWindowState {
    Open = 0,
    PendingRefereeGrant { requester: CommandSide } = 1,
    UnderReview = 2,
    ClosedCommitted = 3,
}

impl ChallengeWindowState {
    pub fn tag(self) -> u8 {
        match self {
            Self::Open => 0,
            Self::PendingRefereeGrant { .. } => 1,
            Self::UnderReview => 2,
            Self::ClosedCommitted => 3,
        }
    }

    pub fn requester(self) -> Option<CommandSide> {
        match self {
            Self::PendingRefereeGrant { requester } => Some(requester),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for ChallengeWindowState {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        match *self {
            Self::Open => out.push(0),
            Self::PendingRefereeGrant { requester } => {
                out.push(1);
                requester.write_canonical_le(out);
            }
            Self::UnderReview => out.push(2),
            Self::ClosedCommitted => out.push(3),
        }
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::Open),
            1 => Ok(Self::PendingRefereeGrant {
                requester: CommandSide::read_canonical_le(reader)?,
            }),
            2 => Ok(Self::UnderReview),
            3 => Ok(Self::ClosedCommitted),
            v => Err(DecodeError::InvalidEnumValue {
                field: "ChallengeWindowState",
                value: v as u32,
            }),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChallengeWindowKind {
    LiveDeadball = 0,
    TimeoutPending { reason: TimeoutReason } = 1,
}

impl ChallengeWindowKind {
    pub fn window_kind_tag(self) -> u8 {
        match self {
            Self::LiveDeadball => 0,
            Self::TimeoutPending {
                reason: TimeoutReason::TeamCalled { side },
            } => match side {
                CommandSide::Home => 1,
                _ => 2,
            },
            Self::TimeoutPending {
                reason: TimeoutReason::Mandatory { team_side },
            } => match team_side {
                CommandSide::Home => 3,
                _ => 4,
            },
        }
    }
}

impl CanonicalLeBytes for ChallengeWindowKind {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        match *self {
            Self::LiveDeadball => out.push(0),
            Self::TimeoutPending { reason } => {
                out.push(1);
                reason.write_canonical_le(out);
            }
        }
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::LiveDeadball),
            1 => Ok(Self::TimeoutPending {
                reason: TimeoutReason::read_canonical_le(reader)?,
            }),
            v => Err(DecodeError::InvalidEnumValue {
                field: "ChallengeWindowKind",
                value: v as u32,
            }),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChallengeTarget {
    Ruling(u64) = 0,
    RecentScoringOrClockEvent { event_seq: u64 } = 1,
}

impl CanonicalLeBytes for ChallengeTarget {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        match *self {
            Self::Ruling(id) => {
                out.push(0);
                id.write_canonical_le(out);
            }
            Self::RecentScoringOrClockEvent { event_seq } => {
                out.push(1);
                event_seq.write_canonical_le(out);
            }
        }
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::Ruling(u64::read_canonical_le(reader)?)),
            1 => Ok(Self::RecentScoringOrClockEvent {
                event_seq: u64::read_canonical_le(reader)?,
            }),
            v => Err(DecodeError::InvalidEnumValue {
                field: "ChallengeTarget",
                value: v as u32,
            }),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AdvanceOption {
    StayBackcourt = 0,
    AdvanceTableSide = 1,
    AdvanceOppositeSide = 2,
}

impl AdvanceOption {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::StayBackcourt),
            1 => Some(Self::AdvanceTableSide),
            2 => Some(Self::AdvanceOppositeSide),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for AdvanceOption {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "AdvanceOption",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PatchField<T> {
    Keep = 0,
    Clear = 1,
    Set(T) = 2,
}

impl<T: CanonicalLeBytes> CanonicalLeBytes for PatchField<T> {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        match self {
            Self::Keep => out.push(0),
            Self::Clear => out.push(1),
            Self::Set(v) => {
                out.push(2);
                v.write_canonical_le(out);
            }
        }
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::Keep),
            1 => Ok(Self::Clear),
            2 => Ok(Self::Set(T::read_canonical_le(reader)?)),
            v => Err(DecodeError::InvalidEnumValue {
                field: "PatchField",
                value: v as u32,
            }),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum IntendedOutcomeClass {
    Swish = 0,
    RimIn = 1,
    BankMake = 2,
    RimOut = 3,
    BankMiss = 4,
    BackboardOnlyMiss = 5,
    Airball = 6,
    Blocked = 7,
    Goaltended = 8,
    InterceptedLive = 9,
}

impl IntendedOutcomeClass {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Swish),
            1 => Some(Self::RimIn),
            2 => Some(Self::BankMake),
            3 => Some(Self::RimOut),
            4 => Some(Self::BankMiss),
            5 => Some(Self::BackboardOnlyMiss),
            6 => Some(Self::Airball),
            7 => Some(Self::Blocked),
            8 => Some(Self::Goaltended),
            9 => Some(Self::InterceptedLive),
            _ => None,
        }
    }

    pub fn is_make(self) -> bool {
        matches!(
            self,
            Self::Swish | Self::RimIn | Self::BankMake | Self::Goaltended
        )
    }

    pub fn touches_rim(self) -> bool {
        matches!(self, Self::RimIn | Self::BankMake | Self::RimOut | Self::BankMiss)
    }

    pub fn touches_backboard(self) -> bool {
        matches!(
            self,
            Self::BankMake | Self::BankMiss | Self::BackboardOnlyMiss
        )
    }
}

impl CanonicalLeBytes for IntendedOutcomeClass {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "IntendedOutcomeClass",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PnrCoverageKind {
    Drop = 0,
    HighShowAndRecover = 1,
    Blitz = 2,
    Switch = 3,
    Ice = 4,
}

impl PnrCoverageKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Drop),
            1 => Some(Self::HighShowAndRecover),
            2 => Some(Self::Blitz),
            3 => Some(Self::Switch),
            4 => Some(Self::Ice),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for PnrCoverageKind {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "PnrCoverageKind",
            value: v as u32,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StateHashMode {
    Disabled = 0,
    PerTickRolling = 1,
}

impl CanonicalLeBytes for StateHashMode {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::Disabled),
            1 => Ok(Self::PerTickRolling),
            v => Err(DecodeError::InvalidEnumValue {
                field: "StateHashMode",
                value: v as u32,
            }),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum IntentionalFoulPolicy {
    Auto = 0,
    Disabled = 1,
    ForceEnabled = 2,
}

impl CanonicalLeBytes for IntentionalFoulPolicy {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::Auto),
            1 => Ok(Self::Disabled),
            2 => Ok(Self::ForceEnabled),
            v => Err(DecodeError::InvalidEnumValue {
                field: "IntentionalFoulPolicy",
                value: v as u32,
            }),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FoulUpThreePolicy {
    Never = 0,
    Under6sIfBonus = 1,
}

impl CanonicalLeBytes for FoulUpThreePolicy {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::Never),
            1 => Ok(Self::Under6sIfBonus),
            v => Err(DecodeError::InvalidEnumValue {
                field: "FoulUpThreePolicy",
                value: v as u32,
            }),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ShotClockResetPolicy {
    NbaRule7 = 0,
    FibaArt29 = 1,
}

impl CanonicalLeBytes for ShotClockResetPolicy {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::NbaRule7),
            1 => Ok(Self::FibaArt29),
            v => Err(DecodeError::InvalidEnumValue {
                field: "ShotClockResetPolicy",
                value: v as u32,
            }),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MandatoryFirstChargePolicy {
    HomeFirst = 0,
    CanonicalAlphaFirst = 1,
}

impl CanonicalLeBytes for MandatoryFirstChargePolicy {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::HomeFirst),
            1 => Ok(Self::CanonicalAlphaFirst),
            v => Err(DecodeError::InvalidEnumValue {
                field: "MandatoryFirstChargePolicy",
                value: v as u32,
            }),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PostFtOutcome {
    LiveReboundOnLastMiss = 0,
    RetainedInbound {
        side: CommandSide,
        spot: InboundSpot,
    } = 1,
    ResumeJumpBall {
        spot: JumpBallSpot,
    } = 2,
    ResumeInterruptedFtSeries = 3,
}

impl CanonicalLeBytes for PostFtOutcome {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        match *self {
            Self::LiveReboundOnLastMiss => out.push(0),
            Self::RetainedInbound { side, spot } => {
                out.push(1);
                side.write_canonical_le(out);
                spot.write_canonical_le(out);
            }
            Self::ResumeJumpBall { spot } => {
                out.push(2);
                spot.write_canonical_le(out);
            }
            Self::ResumeInterruptedFtSeries => out.push(3),
        }
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::LiveReboundOnLastMiss),
            1 => Ok(Self::RetainedInbound {
                side: CommandSide::read_canonical_le(reader)?,
                spot: InboundSpot::read_canonical_le(reader)?,
            }),
            2 => Ok(Self::ResumeJumpBall {
                spot: JumpBallSpot::read_canonical_le(reader)?,
            }),
            3 => Ok(Self::ResumeInterruptedFtSeries),
            v => Err(DecodeError::InvalidEnumValue {
                field: "PostFtOutcome",
                value: v as u32,
            }),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AuditWarning {
    RngCounterSaturated {
        actor_slot: u8,
        purpose: u8,
        budget: u16,
    } = 0,
    DepenetrationFallbackUsed {
        tick: u32,
        max_pen_um: u32,
    } = 1,
    CoordinateClamped {
        tick: u32,
        slot: u8,
    } = 2,
    SystemCommandQuotaExhaustedWindowBypassed {
        tick: u32,
        ruling_id: u64,
    } = 3,
}
