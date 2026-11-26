use core::fmt;
use serde::de::{self, SeqAccess, Visitor};
use serde::ser::SerializeTuple;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::canonical_le_bytes::{validate_id_string, CanonicalLeBytes, CanonicalReader, DecodeError};
use crate::wire_schema_v1::enums::*;

pub const MAX_PRETICK_COMMANDS_PER_TICK: usize = 8;
pub const MAX_WINDOW_CHALLENGE_CYCLES_PER_RULING: u8 = 2;
pub const MAX_SYSTEM_COMMANDS_PER_GAME: u16 = 8_192;
pub const MAX_PLAYER_COMMANDS_PER_GAME: u16 = 4_096;
pub const COMMIT_EVENT_RESERVE: usize = 10;
pub const MAX_EVENTS_PER_TICK: usize = 48;
pub const MAX_REPLAY_MANIFEST_BYTES: usize = 2_097_152;
pub const MAX_INIT_SPEC_BYTES: usize = 262_144;
pub const MAX_INGRESS_COMMAND_BYTES: usize = 1_024;
pub const MAX_ANCHORED_PLAYER_BYTES: usize = 182;
pub const ANCHORED_SYSTEM_BYTES: usize = 42;
pub const MAX_FUTURE_RESOLVE_HORIZON_TICKS: u32 = 600;

/// Fixed inline zero-heap `Copy` identifier (1..=64 B valid non-empty UTF-8 without `\0`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InlineId64 {
    pub len: u8,
    pub bytes: [u8; 64],
}

pub type PlayId = InlineId64;
pub type GameplanId = InlineId64;
pub type PnrRuleId = InlineId64;

impl InlineId64 {
    pub fn new(s: &str) -> Result<Self, DecodeError> {
        validate_id_string(s)?;
        let mut bytes = [0u8; 64];
        let b = s.as_bytes();
        bytes[..b.len()].copy_from_slice(b);
        Ok(Self {
            len: b.len() as u8,
            bytes,
        })
    }

    pub fn from_str_lossy(s: &str) -> Self {
        let b = s.as_bytes();
        let len = b.len().clamp(1, 64);
        let mut bytes = [0u8; 64];
        bytes[..len].copy_from_slice(&b[..len]);
        Self {
            len: len as u8,
            bytes,
        }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }
}

impl Default for InlineId64 {
    fn default() -> Self {
        Self::new("default").unwrap()
    }
}

impl fmt::Debug for InlineId64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InlineId64({:?})", self.as_str())
    }
}

impl fmt::Display for InlineId64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for InlineId64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for InlineId64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::new(&s).map_err(de::Error::custom)
    }
}

impl CanonicalLeBytes for InlineId64 {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        (self.len as u32).write_canonical_le(out);
        out.extend_from_slice(&self.bytes[..self.len as usize]);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let len = u32::read_canonical_le(reader)? as usize;
        if len == 0 || len > 64 {
            return Err(DecodeError::IdLengthOutOfRange { len });
        }
        let slice = reader.read_bytes(len)?;
        let s = core::str::from_utf8(slice).map_err(|_| DecodeError::InvalidUtf8)?;
        Self::new(s)
    }
}

/// Fixed inline zero-heap `Copy` player roster index list (`len: u8`, `items: [RosterIdx; N]`, unused tail `0xFF`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InlinePlayerList<const N: usize> {
    pub len: u8,
    pub items: [RosterIdx; N],
}

impl<const N: usize> Default for InlinePlayerList<N> {
    fn default() -> Self {
        Self {
            len: 0,
            items: [ROSTER_IDX_NONE; N],
        }
    }
}

impl<const N: usize> InlinePlayerList<N> {
    pub fn from_slice(src: &[RosterIdx]) -> Result<Self, DecodeError> {
        if src.len() > N {
            return Err(DecodeError::InvalidFieldValue {
                field: "InlinePlayerList.len",
            });
        }
        let mut items = [ROSTER_IDX_NONE; N];
        items[..src.len()].copy_from_slice(src);
        Ok(Self {
            len: src.len() as u8,
            items,
        })
    }

    pub fn as_slice(&self) -> &[RosterIdx] {
        &self.items[..self.len as usize]
    }

    pub fn validate(&self) -> Result<(), DecodeError> {
        let l = self.len as usize;
        if l > N {
            return Err(DecodeError::InvalidEnumValue {
                field: "InlinePlayerList.len",
                value: self.len as u32,
            });
        }
        for &v in &self.items[l..N] {
            if v != ROSTER_IDX_NONE {
                return Err(DecodeError::InvalidEnumValue {
                    field: "InlinePlayerList.unused_tail",
                    value: v as u32,
                });
            }
        }
        Ok(())
    }
}

impl<const N: usize> Serialize for InlinePlayerList<N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_tuple(1 + N)?;
        seq.serialize_element(&self.len)?;
        for item in &self.items {
            seq.serialize_element(item)?;
        }
        seq.end()
    }
}

impl<'de, const N: usize> Deserialize<'de> for InlinePlayerList<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ListVisitor<const M: usize>;
        impl<'de, const M: usize> Visitor<'de> for ListVisitor<M> {
            type Value = InlinePlayerList<M>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "InlinePlayerList<{}> tuple", M)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let len = seq
                    .next_element::<u8>()?
                    .ok_or_else(|| de::Error::invalid_length(0, &self))?;
                let mut items = [ROSTER_IDX_NONE; M];
                for (i, slot) in items.iter_mut().enumerate() {
                    *slot = seq
                        .next_element::<u8>()?
                        .ok_or_else(|| de::Error::invalid_length(1 + i, &self))?;
                }
                let list = InlinePlayerList { len, items };
                list.validate().map_err(de::Error::custom)?;
                Ok(list)
            }
        }
        deserializer.deserialize_tuple(1 + N, ListVisitor::<N>)
    }
}

impl<const N: usize> CanonicalLeBytes for InlinePlayerList<N> {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.len.write_canonical_le(out);
        let l = (self.len as usize).min(N);
        for i in 0..N {
            if i < l {
                self.items[i].write_canonical_le(out);
            } else {
                ROSTER_IDX_NONE.write_canonical_le(out);
            }
        }
    }

    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let len = reader.read_u8()?;
        let mut items = [ROSTER_IDX_NONE; N];
        for slot in items.iter_mut() {
            *slot = reader.read_u8()?;
        }
        let list = Self { len, items };
        list.validate()?;
        Ok(list)
    }
}

/// 182 B fixed array wrapper with manual Serde for `N > 32`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct EncodedAnchoredBytes(pub [u8; 182]);

impl Default for EncodedAnchoredBytes {
    fn default() -> Self {
        Self([0u8; 182])
    }
}

impl fmt::Debug for EncodedAnchoredBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EncodedAnchoredBytes([182 B])")
    }
}

impl Serialize for EncodedAnchoredBytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut tup = serializer.serialize_tuple(182)?;
        for b in &self.0 {
            tup.serialize_element(b)?;
        }
        tup.end()
    }
}

impl<'de> Deserialize<'de> for EncodedAnchoredBytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ArrVisitor;
        impl<'de> Visitor<'de> for ArrVisitor {
            type Value = EncodedAnchoredBytes;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "182-byte array")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut arr = [0u8; 182];
                for (i, slot) in arr.iter_mut().enumerate() {
                    *slot = seq
                        .next_element::<u8>()?
                        .ok_or_else(|| de::Error::invalid_length(i, &self))?;
                }
                Ok(EncodedAnchoredBytes(arr))
            }
        }
        deserializer.deserialize_tuple(182, ArrVisitor)
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlayerCoachCommand {
    CallTimeout {
        queue_if_live_disallowed: bool,
        advance: AdvanceOption,
    } = 0,
    ChallengeCall {
        target: ChallengeTarget,
        expected_window_epoch: Option<u32>,
    } = 1,
    WithdrawChallenge {
        target: ChallengeTarget,
        expected_request_seq: u32,
        expected_window_epoch: Option<u32>,
    } = 2,
    Substitute {
        out_player: RosterIdx,
        in_player: RosterIdx,
    } = 3,
    SetOffensivePlay {
        play_id: PlayId,
        force_immediate: bool,
    } = 4,
    SetDefensiveGameplan {
        gameplan_id: PatchField<GameplanId>,
        protect_targets: PatchField<InlinePlayerList<5>>,
        pnr_rule_override: PatchField<PnrRuleId>,
        matchup_override: PatchField<(RosterIdx, RosterIdx)>,
    } = 5,
    SetPaceAndPressure {
        pace: PatchField<u8>,
        full_court_press: PatchField<bool>,
    } = 6,
    SetTimeoutAdvance {
        advance: AdvanceOption,
        expected_window_epoch: Option<u32>,
    } = 7,
    ForfeitMatch = 8,
}

impl PlayerCoachCommand {
    pub fn cmd_tag(self) -> u8 {
        match self {
            Self::CallTimeout { .. } => 0,
            Self::ChallengeCall { .. } => 1,
            Self::WithdrawChallenge { .. } => 2,
            Self::Substitute { .. } => 3,
            Self::SetOffensivePlay { .. } => 4,
            Self::SetDefensiveGameplan { .. } => 5,
            Self::SetPaceAndPressure { .. } => 6,
            Self::SetTimeoutAdvance { .. } => 7,
            Self::ForfeitMatch => 8,
        }
    }

    pub fn max_immediate_events(self) -> usize {
        match self {
            Self::ForfeitMatch => 3,
            Self::CallTimeout { .. } => 2,
            Self::ChallengeCall { .. } => 4,
            Self::WithdrawChallenge { .. } => 2,
            Self::Substitute { .. } => 2,
            _ => 1,
        }
    }
}

impl CanonicalLeBytes for PlayerCoachCommand {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        match self {
            Self::CallTimeout {
                queue_if_live_disallowed,
                advance,
            } => {
                out.push(0);
                queue_if_live_disallowed.write_canonical_le(out);
                advance.write_canonical_le(out);
            }
            Self::ChallengeCall {
                target,
                expected_window_epoch,
            } => {
                out.push(1);
                target.write_canonical_le(out);
                expected_window_epoch.write_canonical_le(out);
            }
            Self::WithdrawChallenge {
                target,
                expected_request_seq,
                expected_window_epoch,
            } => {
                out.push(2);
                target.write_canonical_le(out);
                expected_request_seq.write_canonical_le(out);
                expected_window_epoch.write_canonical_le(out);
            }
            Self::Substitute {
                out_player,
                in_player,
            } => {
                out.push(3);
                out_player.write_canonical_le(out);
                in_player.write_canonical_le(out);
            }
            Self::SetOffensivePlay {
                play_id,
                force_immediate,
            } => {
                out.push(4);
                play_id.write_canonical_le(out);
                force_immediate.write_canonical_le(out);
            }
            Self::SetDefensiveGameplan {
                gameplan_id,
                protect_targets,
                pnr_rule_override,
                matchup_override,
            } => {
                out.push(5);
                gameplan_id.write_canonical_le(out);
                protect_targets.write_canonical_le(out);
                pnr_rule_override.write_canonical_le(out);
                matchup_override.write_canonical_le(out);
            }
            Self::SetPaceAndPressure {
                pace,
                full_court_press,
            } => {
                out.push(6);
                pace.write_canonical_le(out);
                full_court_press.write_canonical_le(out);
            }
            Self::SetTimeoutAdvance {
                advance,
                expected_window_epoch,
            } => {
                out.push(7);
                advance.write_canonical_le(out);
                expected_window_epoch.write_canonical_le(out);
            }
            Self::ForfeitMatch => out.push(8),
        }
    }

    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::CallTimeout {
                queue_if_live_disallowed: bool::read_canonical_le(reader)?,
                advance: AdvanceOption::read_canonical_le(reader)?,
            }),
            1 => Ok(Self::ChallengeCall {
                target: ChallengeTarget::read_canonical_le(reader)?,
                expected_window_epoch: Option::<u32>::read_canonical_le(reader)?,
            }),
            2 => Ok(Self::WithdrawChallenge {
                target: ChallengeTarget::read_canonical_le(reader)?,
                expected_request_seq: u32::read_canonical_le(reader)?,
                expected_window_epoch: Option::<u32>::read_canonical_le(reader)?,
            }),
            3 => Ok(Self::Substitute {
                out_player: u8::read_canonical_le(reader)?,
                in_player: u8::read_canonical_le(reader)?,
            }),
            4 => Ok(Self::SetOffensivePlay {
                play_id: PlayId::read_canonical_le(reader)?,
                force_immediate: bool::read_canonical_le(reader)?,
            }),
            5 => Ok(Self::SetDefensiveGameplan {
                gameplan_id: PatchField::<GameplanId>::read_canonical_le(reader)?,
                protect_targets: PatchField::<InlinePlayerList<5>>::read_canonical_le(reader)?,
                pnr_rule_override: PatchField::<PnrRuleId>::read_canonical_le(reader)?,
                matchup_override: PatchField::<(RosterIdx, RosterIdx)>::read_canonical_le(reader)?,
            }),
            6 => Ok(Self::SetPaceAndPressure {
                pace: PatchField::<u8>::read_canonical_le(reader)?,
                full_court_press: PatchField::<bool>::read_canonical_le(reader)?,
            }),
            7 => Ok(Self::SetTimeoutAdvance {
                advance: AdvanceOption::read_canonical_le(reader)?,
                expected_window_epoch: Option::<u32>::read_canonical_le(reader)?,
            }),
            8 => Ok(Self::ForfeitMatch),
            v => Err(DecodeError::InvalidEnumValue {
                field: "PlayerCoachCommand",
                value: v as u32,
            }),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SystemCommand {
    ConfirmChallengeGrant { ruling_id: u64, request_seq: u32 } = 0,
    CloseChallengeWindow { ruling_id: u64, window_epoch: u32 } = 1,
}

impl SystemCommand {
    pub fn cmd_tag(self) -> u8 {
        match self {
            Self::ConfirmChallengeGrant { .. } => 0x80,
            Self::CloseChallengeWindow { .. } => 0x81,
        }
    }

    pub fn max_immediate_events(self) -> usize {
        2
    }
}

impl CanonicalLeBytes for SystemCommand {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        match *self {
            Self::ConfirmChallengeGrant {
                ruling_id,
                request_seq,
            } => {
                out.push(0);
                ruling_id.write_canonical_le(out);
                request_seq.write_canonical_le(out);
            }
            Self::CloseChallengeWindow {
                ruling_id,
                window_epoch,
            } => {
                out.push(1);
                ruling_id.write_canonical_le(out);
                window_epoch.write_canonical_le(out);
            }
        }
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::ConfirmChallengeGrant {
                ruling_id: u64::read_canonical_le(reader)?,
                request_seq: u32::read_canonical_le(reader)?,
            }),
            1 => Ok(Self::CloseChallengeWindow {
                ruling_id: u64::read_canonical_le(reader)?,
                window_epoch: u32::read_canonical_le(reader)?,
            }),
            v => Err(DecodeError::InvalidEnumValue {
                field: "SystemCommand",
                value: v as u32,
            }),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CommandPayload {
    Player(PlayerCoachCommand) = 0,
    System(SystemCommand) = 1,
}

impl CommandPayload {
    pub fn cmd_tag(self) -> u8 {
        match self {
            Self::Player(p) => p.cmd_tag(),
            Self::System(s) => s.cmd_tag(),
        }
    }

    pub fn max_immediate_events(self) -> usize {
        match self {
            Self::Player(p) => p.max_immediate_events(),
            Self::System(s) => s.max_immediate_events(),
        }
    }

    pub fn is_system(self) -> bool {
        matches!(self, Self::System(_))
    }

    pub fn is_forfeit(self) -> bool {
        matches!(self, Self::Player(PlayerCoachCommand::ForfeitMatch))
    }
}

impl CanonicalLeBytes for CommandPayload {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        match self {
            Self::Player(p) => {
                out.push(0);
                p.write_canonical_le(out);
            }
            Self::System(s) => {
                out.push(1);
                s.write_canonical_le(out);
            }
        }
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(Self::Player(PlayerCoachCommand::read_canonical_le(reader)?)),
            1 => Ok(Self::System(SystemCommand::read_canonical_le(reader)?)),
            v => Err(DecodeError::InvalidEnumValue {
                field: "CommandPayload",
                value: v as u32,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngressCommand {
    pub client_request_id: u64,
    pub apply_at_tick: Option<u32>,
    pub phase: CommandPhase,
    pub side: CommandSide,
    pub payload: CommandPayload,
}

impl CanonicalLeBytes for IngressCommand {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.client_request_id.write_canonical_le(out);
        self.apply_at_tick.write_canonical_le(out);
        self.phase.write_canonical_le(out);
        self.side.write_canonical_le(out);
        self.payload.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            client_request_id: u64::read_canonical_le(reader)?,
            apply_at_tick: Option::<u32>::read_canonical_le(reader)?,
            phase: CommandPhase::read_canonical_le(reader)?,
            side: CommandSide::read_canonical_le(reader)?,
            payload: CommandPayload::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnchoredCommand {
    pub client_request_id: u64,
    pub enqueued_at_tick: u32,
    pub enqueued_phase: CommandPhase,
    pub requested_apply_at_tick: Option<u32>,
    pub resolved_tick: u32,
    pub resolved_phase: CommandPhase,
    pub ingress_seq: u64,
    pub side: CommandSide,
    pub payload: CommandPayload,
}

impl CanonicalLeBytes for AnchoredCommand {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.client_request_id.write_canonical_le(out);
        self.enqueued_at_tick.write_canonical_le(out);
        self.enqueued_phase.write_canonical_le(out);
        self.requested_apply_at_tick.write_canonical_le(out);
        self.resolved_tick.write_canonical_le(out);
        self.resolved_phase.write_canonical_le(out);
        self.ingress_seq.write_canonical_le(out);
        self.side.write_canonical_le(out);
        self.payload.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            client_request_id: u64::read_canonical_le(reader)?,
            enqueued_at_tick: u32::read_canonical_le(reader)?,
            enqueued_phase: CommandPhase::read_canonical_le(reader)?,
            requested_apply_at_tick: Option::<u32>::read_canonical_le(reader)?,
            resolved_tick: u32::read_canonical_le(reader)?,
            resolved_phase: CommandPhase::read_canonical_le(reader)?,
            ingress_seq: u64::read_canonical_le(reader)?,
            side: CommandSide::read_canonical_le(reader)?,
            payload: CommandPayload::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedCommand {
    pub ingress: IngressCommand,
    pub expected_enqueued_at_tick: u32,
    pub expected_enqueued_phase: CommandPhase,
    pub expected_ingress_seq: u64,
    pub candidate_anchored: AnchoredCommand,
    pub request_fingerprint: [u8; 32],
    pub projected_manifest_delta_bytes: u32,
    pub encoded_bytes: EncodedAnchoredBytes,
    pub encoded_len: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PreparedOutcome {
    AlreadyAccepted {
        existing_anchored: AnchoredCommand,
        encoded_bytes: EncodedAnchoredBytes,
        encoded_len: u16,
    },
    NewCommand(PreparedCommand),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DedupEntry {
    pub client_request_id: u64,
    pub request_fingerprint: [u8; 32],
    pub anchored: AnchoredCommand,
}

impl CanonicalLeBytes for DedupEntry {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.client_request_id.write_canonical_le(out);
        self.request_fingerprint.write_canonical_le(out);
        self.anchored.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            client_request_id: u64::read_canonical_le(reader)?,
            request_fingerprint: <[u8; 32]>::read_canonical_le(reader)?,
            anchored: AnchoredCommand::read_canonical_le(reader)?,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoundedDedupWindow32 {
    pub head: u8,
    pub len: u8,
    pub entries: [Option<DedupEntry>; 32],
}

impl Default for BoundedDedupWindow32 {
    fn default() -> Self {
        Self {
            head: 0,
            len: 0,
            entries: [None; 32],
        }
    }
}

impl BoundedDedupWindow32 {
    pub fn find(&self, client_request_id: u64) -> Option<&DedupEntry> {
        for i in 0..(self.len as usize).min(32) {
            if let Some(ref e) = self.entries[i] {
                if e.client_request_id == client_request_id {
                    return Some(e);
                }
            }
        }
        None
    }

    pub fn insert(&mut self, entry: DedupEntry) {
        if let Some(pos) = (0..(self.len as usize).min(32)).find(|&i| {
            self.entries[i]
                .as_ref()
                .map_or(false, |e| e.client_request_id == entry.client_request_id)
        }) {
            self.entries[pos] = Some(entry);
            return;
        }
        if (self.len as usize) < 32 {
            let idx = self.len as usize;
            self.entries[idx] = Some(entry);
            self.len += 1;
        } else {
            let idx = (self.head as usize) % 32;
            self.entries[idx] = Some(entry);
            self.head = ((idx + 1) % 32) as u8;
        }
    }
}

impl CanonicalLeBytes for BoundedDedupWindow32 {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        let len = self.len.min(32);
        self.head.write_canonical_le(out);
        len.write_canonical_le(out);
        for i in 0..(len as usize) {
            self.entries[i].write_canonical_le(out);
        }
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let head = u8::read_canonical_le(reader)?;
        let len = u8::read_canonical_le(reader)?;
        if len > 32 || (len > 0 && head >= len && len == 32 && head >= 32) {
            return Err(DecodeError::InvalidFieldValue {
                field: "BoundedDedupWindow32.len",
            });
        }
        let mut entries = [None; 32];
        for i in 0..(len as usize) {
            entries[i] = Option::<DedupEntry>::read_canonical_le(reader)?;
        }
        Ok(Self {
            head,
            len,
            entries,
        })
    }
}

/// 68-slot bounded command queue (64 player + 4 system reserve) with manual Serde & CanonicalLeBytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedCommandQueue {
    pub entries: Box<[Option<AnchoredCommand>; 68]>,
    pub order: [u8; 68],
    pub len: u8,
    pub player_len: u8,
    pub system_reserved_len: u8,
}

impl Default for BoundedCommandQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl BoundedCommandQueue {
    pub fn new() -> Self {
        Self {
            entries: Box::new([None; 68]),
            order: [0xFF; 68],
            len: 0,
            player_len: 0,
            system_reserved_len: 0,
        }
    }

    pub fn clear(&mut self) {
        for slot in self.entries.iter_mut() {
            *slot = None;
        }
        self.order = [0xFF; 68];
        self.len = 0;
        self.player_len = 0;
        self.system_reserved_len = 0;
    }

    pub fn can_push(&self, is_system: bool) -> bool {
        if is_system {
            (self.system_reserved_len as usize) < 4 && (self.len as usize) < 68
        } else {
            (self.player_len as usize) < 64 && (self.len as usize) < 68
        }
    }

    pub fn pretick_count_at(&self, tick: u32, side: CommandSide) -> usize {
        let mut cnt = 0;
        for i in 0..(self.len as usize) {
            let slot = self.order[i] as usize;
            if let Some(cmd) = self.entries[slot] {
                if cmd.resolved_tick == tick
                    && cmd.resolved_phase == CommandPhase::PreTick
                    && cmd.side == side
                {
                    cnt += 1;
                }
            }
        }
        cnt
    }

    pub fn push(&mut self, cmd: AnchoredCommand) -> Result<(), CommandRejectReason> {
        let is_sys = cmd.payload.is_system();
        if !self.can_push(is_sys) {
            return Err(CommandRejectReason::QueueFull);
        }
        let free_idx = (0..68)
            .find(|&i| self.entries[i].is_none())
            .ok_or(CommandRejectReason::QueueFull)?;
        self.entries[free_idx] = Some(cmd);
        let cur_len = self.len as usize;
        self.order[cur_len] = free_idx as u8;
        self.len += 1;
        if is_sys {
            self.system_reserved_len += 1;
        } else {
            self.player_len += 1;
        }
        // Sort order[0..self.len] by (resolved_tick, resolved_phase, ingress_seq)
        let n = self.len as usize;
        let entries = &self.entries;
        self.order[..n].sort_by_key(|&slot| {
            let c = entries[slot as usize].unwrap();
            (c.resolved_tick, c.resolved_phase, c.ingress_seq)
        });
        Ok(())
    }

    pub fn pop_ready_for_tick_phase(
        &mut self,
        tick: u32,
        phase: CommandPhase,
    ) -> Vec<AnchoredCommand> {
        let mut ready = Vec::new();
        let mut keep = Vec::new();
        let n = self.len as usize;
        for i in 0..n {
            let slot = self.order[i] as usize;
            if let Some(cmd) = self.entries[slot] {
                if cmd.resolved_tick == tick && cmd.resolved_phase == phase {
                    ready.push(cmd);
                    self.entries[slot] = None;
                    if cmd.payload.is_system() {
                        self.system_reserved_len = self.system_reserved_len.saturating_sub(1);
                    } else {
                        self.player_len = self.player_len.saturating_sub(1);
                    }
                } else {
                    keep.push(slot as u8);
                }
            }
        }
        self.order = [0xFF; 68];
        for (i, &s) in keep.iter().enumerate() {
            self.order[i] = s;
        }
        self.len = keep.len() as u8;
        ready
    }
}

impl Serialize for BoundedCommandQueue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut vec = Vec::with_capacity(self.len as usize);
        for i in 0..(self.len as usize).min(68) {
            let idx = self.order[i] as usize;
            if idx < 68 {
                if let Some(cmd) = self.entries[idx] {
                    vec.push(cmd);
                }
            }
        }
        vec.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BoundedCommandQueue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let vec = Vec::<AnchoredCommand>::deserialize(deserializer)?;
        let mut q = Self::new();
        for cmd in vec {
            q.push(cmd).map_err(|e| de::Error::custom(format!("{:?}", e)))?;
        }
        Ok(q)
    }
}

impl CanonicalLeBytes for BoundedCommandQueue {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.len.write_canonical_le(out);
        self.player_len.write_canonical_le(out);
        self.system_reserved_len.write_canonical_le(out);
        for i in 0..(self.len as usize).min(68) {
            let slot = self.order[i] as usize;
            if let Some(cmd) = self.entries[slot] {
                cmd.write_canonical_le(out);
            }
        }
    }

    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let len = reader.read_u8()?;
        let player_len = reader.read_u8()?;
        let system_reserved_len = reader.read_u8()?;
        if len as usize > 68 || player_len as usize > 64 || system_reserved_len as usize > 4 {
            return Err(DecodeError::InvalidFieldValue {
                field: "BoundedCommandQueue.len",
            });
        }
        let mut q = Self::new();
        for _ in 0..len {
            let cmd = AnchoredCommand::read_canonical_le(reader)?;
            q.push(cmd)
                .map_err(|_| DecodeError::InvalidFieldValue { field: "BoundedCommandQueue" })?;
        }
        if q.player_len != player_len || q.system_reserved_len != system_reserved_len {
            return Err(DecodeError::InvalidFieldValue {
                field: "BoundedCommandQueue.counts",
            });
        }
        Ok(q)
    }
}
