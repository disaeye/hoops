use serde::{Deserialize, Serialize};

use crate::canonical_le_bytes::{CanonicalLeBytes, CanonicalReader, DecodeError};
use crate::wire_schema_v1::enums::{decode_shot_clock_opt, ROSTER_IDX_NONE};

pub const PUBLIC_FRAME_BYTES: usize = 124;
pub const DEBUG_FRAME_BYTES: usize = 118;
pub const CHALLENGE_SNAPSHOT_BYTES: usize = 32;
pub const SHOT_TELEMETRY_BYTES: usize = 20;
pub const FOOTWORK_TELEMETRY_BYTES: usize = 16;

/// Away-from-zero rounding helper matching §10.1.3.
#[inline]
pub fn round_away_from_zero(x: f64) -> Result<i64, DecodeError> {
    if !x.is_finite() {
        return Err(DecodeError::NonFiniteValue);
    }
    if x == 0.0 {
        return Ok(0);
    }
    Ok(x.round() as i64)
}

#[inline]
pub fn quantize_mm(val_m: f64) -> Result<(i16, bool), DecodeError> {
    let r = round_away_from_zero(val_m * 1000.0)?;
    let clamped = r < -32767 || r > 32767;
    Ok((r.clamp(-32767, 32767) as i16, clamped))
}

#[inline]
pub fn quantize_z_mm(val_m: f64) -> Result<i16, DecodeError> {
    let r = round_away_from_zero(val_m * 1000.0)?;
    Ok(r.clamp(0, 32767) as i16)
}

#[inline]
pub fn quantize_vel_dm_s(val_m_s: f64) -> Result<i8, DecodeError> {
    let r = round_away_from_zero(val_m_s * 10.0)?;
    Ok(r.clamp(-127, 127) as i8)
}

#[inline]
pub fn quantize_facing_bin(theta_rad: f64) -> Result<u8, DecodeError> {
    if !theta_rad.is_finite() {
        return Err(DecodeError::NonFiniteValue);
    }
    let tau = 2.0 * core::f64::consts::PI;
    let norm = theta_rad - tau * libm::floor(theta_rad / tau);
    let bin = (norm / tau * 256.0).round() as i32;
    Ok(bin.rem_euclid(256) as u8)
}

#[inline]
pub fn quantize_cm(val_m: f64) -> Result<i16, DecodeError> {
    let r = round_away_from_zero(val_m * 100.0)?;
    Ok(r.clamp(-32767, 32767) as i16)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PlayerFrameSlotV1 {
    pub roster_idx: u8,
    pub x_mm: i16,
    pub y_mm: i16,
    pub vx_dm_s: i8,
    pub vy_dm_s: i8,
    pub facing_bin: u8,
    pub visible_action: u8,
    pub public_flags: u8,
}

impl PlayerFrameSlotV1 {
    pub fn inactive() -> Self {
        Self {
            roster_idx: ROSTER_IDX_NONE,
            x_mm: 0,
            y_mm: 0,
            vx_dm_s: 0,
            vy_dm_s: 0,
            facing_bin: 0,
            visible_action: 0,
            public_flags: 0,
        }
    }

    pub fn write_le(&self, out: &mut Vec<u8>) {
        if self.roster_idx == ROSTER_IDX_NONE {
            out.push(ROSTER_IDX_NONE);
            out.extend_from_slice(&[0u8; 9]);
        } else {
            out.push(self.roster_idx);
            out.extend_from_slice(&self.x_mm.to_le_bytes());
            out.extend_from_slice(&self.y_mm.to_le_bytes());
            out.push(self.vx_dm_s as u8);
            out.push(self.vy_dm_s as u8);
            out.push(self.facing_bin);
            out.push(self.visible_action);
            out.push(self.public_flags);
        }
    }

    pub fn read_le(
        r: &mut CanonicalReader<'_>,
        slot_idx: u8,
        roster_count: u8,
    ) -> Result<Self, DecodeError> {
        let roster_idx = r.read_u8()?;
        let x_mm = i16::read_canonical_le(r)?;
        let y_mm = i16::read_canonical_le(r)?;
        let vx_dm_s = r.read_i8()?;
        let vy_dm_s = r.read_i8()?;
        let facing_bin = r.read_u8()?;
        let visible_action = r.read_u8()?;
        let public_flags = r.read_u8()?;
        if roster_idx == ROSTER_IDX_NONE {
            if x_mm != 0
                || y_mm != 0
                || vx_dm_s != 0
                || vy_dm_s != 0
                || facing_bin != 0
                || visible_action != 0
                || public_flags != 0
            {
                return Err(DecodeError::InvalidEmptySlot { slot: slot_idx });
            }
            return Ok(Self::inactive());
        }
        if roster_idx >= roster_count {
            return Err(DecodeError::InvalidRosterIndex {
                idx: roster_idx,
                roster_count,
            });
        }
        if visible_action > 19 {
            return Err(DecodeError::InvalidEnumValue {
                field: "visible_action",
                value: visible_action as u32,
            });
        }
        Ok(Self {
            roster_idx,
            x_mm,
            y_mm,
            vx_dm_s,
            vy_dm_s,
            facing_bin,
            visible_action,
            public_flags,
        })
    }
}

/// Fixed 124 B PublicFrameV1 (§10.1.1).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicFrameV1 {
    pub tick: u32,
    pub quarter: u8,
    pub game_clock_ds: u16,
    pub shot_clock_ds: u16,
    pub game_phase: u8,
    pub possession_side: u8,
    pub score_home: u16,
    pub score_away: u16,
    pub ball_x_mm: i16,
    pub ball_y_mm: i16,
    pub ball_z_mm: i16,
    pub ball_state: u8,
    pub ball_owner_slot: i8,
    pub ball_flags: u8,
    pub players: [PlayerFrameSlotV1; 10],
}

impl Default for PublicFrameV1 {
    fn default() -> Self {
        Self {
            tick: 0,
            quarter: 1,
            game_clock_ds: 7200,
            shot_clock_ds: 240,
            game_phase: 0,
            possession_side: 2,
            score_home: 0,
            score_away: 0,
            ball_x_mm: 0,
            ball_y_mm: 0,
            ball_z_mm: 0,
            ball_state: 5,
            ball_owner_slot: -1,
            ball_flags: 0,
            players: [PlayerFrameSlotV1::inactive(); 10],
        }
    }
}

impl PublicFrameV1 {
    pub fn to_bytes(&self) -> [u8; PUBLIC_FRAME_BYTES] {
        let mut out = Vec::with_capacity(PUBLIC_FRAME_BYTES);
        self.write_le(&mut out);
        let mut arr = [0u8; PUBLIC_FRAME_BYTES];
        arr.copy_from_slice(&out);
        arr
    }

    pub fn write_le(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.tick.to_le_bytes());
        out.push(self.quarter);
        out.extend_from_slice(&self.game_clock_ds.to_le_bytes());
        out.extend_from_slice(&self.shot_clock_ds.to_le_bytes());
        out.push(self.game_phase);
        out.push(self.possession_side);
        out.extend_from_slice(&self.score_home.to_le_bytes());
        out.extend_from_slice(&self.score_away.to_le_bytes());
        out.extend_from_slice(&self.ball_x_mm.to_le_bytes());
        out.extend_from_slice(&self.ball_y_mm.to_le_bytes());
        out.extend_from_slice(&self.ball_z_mm.to_le_bytes());
        out.push(self.ball_state);
        out.push(self.ball_owner_slot as u8);
        out.push(self.ball_flags & 0x7F);
        for p in &self.players {
            p.write_le(out);
        }
    }

    pub fn read_le(r: &mut CanonicalReader<'_>, roster_count: u8) -> Result<Self, DecodeError> {
        let tick = u32::read_canonical_le(r)?;
        let quarter = r.read_u8()?;
        let game_clock_ds = u16::read_canonical_le(r)?;
        let shot_clock_ds = u16::read_canonical_le(r)?;
        let _ = decode_shot_clock_opt(shot_clock_ds)?;
        let game_phase = r.read_u8()?;
        if game_phase > 10 {
            return Err(DecodeError::InvalidEnumValue {
                field: "PublicFrameV1.game_phase",
                value: game_phase as u32,
            });
        }
        let possession_side = r.read_u8()?;
        if possession_side > 2 {
            return Err(DecodeError::InvalidEnumValue {
                field: "PublicFrameV1.possession_side",
                value: possession_side as u32,
            });
        }
        let score_home = u16::read_canonical_le(r)?;
        let score_away = u16::read_canonical_le(r)?;
        let ball_x_mm = i16::read_canonical_le(r)?;
        let ball_y_mm = i16::read_canonical_le(r)?;
        let ball_z_mm = i16::read_canonical_le(r)?;
        let ball_state = r.read_u8()?;
        if ball_state > 5 {
            return Err(DecodeError::InvalidEnumValue {
                field: "PublicFrameV1.ball_state",
                value: ball_state as u32,
            });
        }
        let ball_owner_slot = r.read_i8()?;
        if ball_owner_slot < -1 || ball_owner_slot > 9 {
            return Err(DecodeError::InvalidEnumValue {
                field: "PublicFrameV1.ball_owner_slot",
                value: ball_owner_slot as i32 as u32,
            });
        }
        let flags_offset = r.position();
        let ball_flags = r.read_u8()?;
        if (ball_flags & 0x80) != 0 {
            return Err(DecodeError::NonZeroPadding {
                offset: flags_offset,
                value: ball_flags,
            });
        }
        let mut players = [PlayerFrameSlotV1::inactive(); 10];
        for (i, slot) in players.iter_mut().enumerate() {
            *slot = PlayerFrameSlotV1::read_le(r, i as u8, roster_count)?;
        }
        Ok(Self {
            tick,
            quarter,
            game_clock_ds,
            shot_clock_ds,
            game_phase,
            possession_side,
            score_home,
            score_away,
            ball_x_mm,
            ball_y_mm,
            ball_z_mm,
            ball_state,
            ball_owner_slot,
            ball_flags,
            players,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlayerDebugSlotV1 {
    pub target_x_cm: i16,
    pub target_y_cm: i16,
    pub defender_slot: u8,
    pub burst_energy: u8,
    pub cum_stamina: u8,
    pub rhythm_quant: i8,
    pub epv_centi: i16,
    pub debug_flags: u8,
}

impl Default for PlayerDebugSlotV1 {
    fn default() -> Self {
        Self::inactive()
    }
}

impl PlayerDebugSlotV1 {
    pub fn inactive() -> Self {
        Self {
            target_x_cm: 0,
            target_y_cm: 0,
            defender_slot: 0xFF,
            burst_energy: 0,
            cum_stamina: 0,
            rhythm_quant: 0,
            epv_centi: 0,
            debug_flags: 0,
        }
    }

    pub fn write_le(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.target_x_cm.to_le_bytes());
        out.extend_from_slice(&self.target_y_cm.to_le_bytes());
        out.push(self.defender_slot);
        out.push(self.burst_energy);
        out.push(self.cum_stamina);
        out.push(self.rhythm_quant as u8);
        out.extend_from_slice(&self.epv_centi.to_le_bytes());
        out.push(self.debug_flags & 0x7F);
    }

    pub fn read_le(
        r: &mut CanonicalReader<'_>,
        slot_idx: u8,
        is_empty_slot: bool,
    ) -> Result<Self, DecodeError> {
        let target_x_cm = i16::read_canonical_le(r)?;
        let target_y_cm = i16::read_canonical_le(r)?;
        let defender_slot = r.read_u8()?;
        let burst_energy = r.read_u8()?;
        let cum_stamina = r.read_u8()?;
        let rhythm_quant = r.read_i8()?;
        let epv_centi = i16::read_canonical_le(r)?;
        let flags_offset = r.position();
        let debug_flags = r.read_u8()?;
        if (debug_flags & 0x80) != 0 {
            return Err(DecodeError::NonZeroPadding {
                offset: flags_offset,
                value: debug_flags,
            });
        }
        if is_empty_slot
            && (target_x_cm != 0
                || target_y_cm != 0
                || defender_slot != 0xFF
                || burst_energy != 0
                || cum_stamina != 0
                || rhythm_quant != 0
                || epv_centi != 0
                || debug_flags != 0)
        {
            return Err(DecodeError::InvalidEmptySlot { slot: slot_idx });
        }
        if defender_slot > 9 && defender_slot != 0xFF {
            return Err(DecodeError::InvalidEnumValue {
                field: "defender_slot",
                value: defender_slot as u32,
            });
        }
        Ok(Self {
            target_x_cm,
            target_y_cm,
            defender_slot,
            burst_energy,
            cum_stamina,
            rhythm_quant,
            epv_centi,
            debug_flags,
        })
    }
}

/// Fixed 118 B DebugOverlayFrameV1 (§10.1.2).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DebugOverlayFrameV1 {
    pub tick: u32,
    pub active_play_idx: u16,
    pub active_def_rule_idx: u16,
    pub player_debug: [PlayerDebugSlotV1; 10],
}

impl Default for DebugOverlayFrameV1 {
    fn default() -> Self {
        Self {
            tick: 0,
            active_play_idx: 0xFFFF,
            active_def_rule_idx: 0xFFFF,
            player_debug: [PlayerDebugSlotV1::inactive(); 10],
        }
    }
}

impl DebugOverlayFrameV1 {
    pub fn to_bytes(&self) -> [u8; DEBUG_FRAME_BYTES] {
        let mut out = Vec::with_capacity(DEBUG_FRAME_BYTES);
        self.write_le(&mut out);
        let mut arr = [0u8; DEBUG_FRAME_BYTES];
        arr.copy_from_slice(&out);
        arr
    }

    pub fn write_le(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.tick.to_le_bytes());
        out.extend_from_slice(&self.active_play_idx.to_le_bytes());
        out.extend_from_slice(&self.active_def_rule_idx.to_le_bytes());
        for p in &self.player_debug {
            p.write_le(out);
        }
    }

    pub fn read_le(
        r: &mut CanonicalReader<'_>,
        play_dict_count: u16,
        def_rule_dict_count: u16,
        public_frame: &PublicFrameV1,
    ) -> Result<Self, DecodeError> {
        let tick = u32::read_canonical_le(r)?;
        if tick != public_frame.tick {
            return Err(DecodeError::FrameSequenceGap {
                expected: public_frame.tick,
                got: tick,
            });
        }
        let active_play_idx = u16::read_canonical_le(r)?;
        if active_play_idx != 0xFFFF && active_play_idx >= play_dict_count {
            return Err(DecodeError::InvalidEnumValue {
                field: "active_play_idx",
                value: active_play_idx as u32,
            });
        }
        let active_def_rule_idx = u16::read_canonical_le(r)?;
        if active_def_rule_idx != 0xFFFF && active_def_rule_idx >= def_rule_dict_count {
            return Err(DecodeError::InvalidEnumValue {
                field: "active_def_rule_idx",
                value: active_def_rule_idx as u32,
            });
        }
        let mut player_debug = [PlayerDebugSlotV1::inactive(); 10];
        for (i, slot) in player_debug.iter_mut().enumerate() {
            let empty = public_frame.players[i].roster_idx == ROSTER_IDX_NONE;
            *slot = PlayerDebugSlotV1::read_le(r, i as u8, empty)?;
        }
        Ok(Self {
            tick,
            active_play_idx,
            active_def_rule_idx,
            player_debug,
        })
    }
}

/// Fixed 32 B ChallengeWindowSnapshotV1 (§2.2).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChallengeWindowSnapshotV1 {
    pub ruling_id: u64,
    pub whistle_tick: u32,
    pub window_open_tick: u32,
    pub window_epoch: u32,
    pub request_seq: u32,
    pub initial_rule_code: u16,
    pub window_state: u8,
    pub window_kind_tag: u8,
    pub eligible_sides_mask: u8,
    pub grant_requester_side: u8,
}

impl ChallengeWindowSnapshotV1 {
    pub fn to_bytes(&self) -> [u8; CHALLENGE_SNAPSHOT_BYTES] {
        let mut arr = [0u8; CHALLENGE_SNAPSHOT_BYTES];
        arr[0..8].copy_from_slice(&self.ruling_id.to_le_bytes());
        arr[8..12].copy_from_slice(&self.whistle_tick.to_le_bytes());
        arr[12..16].copy_from_slice(&self.window_open_tick.to_le_bytes());
        arr[16..20].copy_from_slice(&self.window_epoch.to_le_bytes());
        arr[20..24].copy_from_slice(&self.request_seq.to_le_bytes());
        arr[24..26].copy_from_slice(&self.initial_rule_code.to_le_bytes());
        arr[26] = self.window_state;
        arr[27] = self.window_kind_tag;
        arr[28] = self.eligible_sides_mask;
        arr[29] = self.grant_requester_side;
        arr[30..32].copy_from_slice(&[0u8; 2]);
        arr
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, DecodeError> {
        let mut r = CanonicalReader::new(data);
        let ruling_id = u64::read_canonical_le(&mut r)?;
        let whistle_tick = u32::read_canonical_le(&mut r)?;
        let window_open_tick = u32::read_canonical_le(&mut r)?;
        let window_epoch = u32::read_canonical_le(&mut r)?;
        let request_seq = u32::read_canonical_le(&mut r)?;
        let initial_rule_code = u16::read_canonical_le(&mut r)?;
        let window_state = r.read_u8()?;
        if window_state > 3 {
            return Err(DecodeError::InvalidEnumValue {
                field: "ChallengeWindowSnapshotV1.window_state",
                value: window_state as u32,
            });
        }
        let window_kind_tag = r.read_u8()?;
        if window_kind_tag > 4 {
            return Err(DecodeError::InvalidEnumValue {
                field: "ChallengeWindowSnapshotV1.window_kind_tag",
                value: window_kind_tag as u32,
            });
        }
        let eligible_sides_mask = r.read_u8()?;
        let grant_requester_side = r.read_u8()?;
        if grant_requester_side > 1 && grant_requester_side != 0xFF {
            return Err(DecodeError::InvalidEnumValue {
                field: "ChallengeWindowSnapshotV1.grant_requester_side",
                value: grant_requester_side as u32,
            });
        }
        r.read_zero_pad(2)?;
        r.ensure_eof()?;
        Ok(Self {
            ruling_id,
            whistle_tick,
            window_open_tick,
            window_epoch,
            request_seq,
            initial_rule_code,
            window_state,
            window_kind_tag,
            eligible_sides_mask,
            grant_requester_side,
        })
    }
}

/// Fixed 20 B ShotTelemetryPublic (§10.3).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShotTelemetryPublic {
    pub release_tick: u32,
    pub release_game_clock_ds: u16,
    pub whistle_game_clock_ds: u16,
    pub whistle_tick: u32,
    pub flight_resolved_tick: u32,
    pub basket_scored_tick: u32,
}

impl Default for ShotTelemetryPublic {
    fn default() -> Self {
        Self {
            release_tick: 0xFFFF_FFFF,
            release_game_clock_ds: 0xFFFF,
            whistle_game_clock_ds: 0xFFFF,
            whistle_tick: 0xFFFF_FFFF,
            flight_resolved_tick: 0xFFFF_FFFF,
            basket_scored_tick: 0xFFFF_FFFF,
        }
    }
}

impl ShotTelemetryPublic {
    pub fn write_le(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.release_tick.to_le_bytes());
        out.extend_from_slice(&self.release_game_clock_ds.to_le_bytes());
        out.extend_from_slice(&self.whistle_game_clock_ds.to_le_bytes());
        out.extend_from_slice(&self.whistle_tick.to_le_bytes());
        out.extend_from_slice(&self.flight_resolved_tick.to_le_bytes());
        out.extend_from_slice(&self.basket_scored_tick.to_le_bytes());
    }

    pub fn read_le(r: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            release_tick: u32::read_canonical_le(r)?,
            release_game_clock_ds: u16::read_canonical_le(r)?,
            whistle_game_clock_ds: u16::read_canonical_le(r)?,
            whistle_tick: u32::read_canonical_le(r)?,
            flight_resolved_tick: u32::read_canonical_le(r)?,
            basket_scored_tick: u32::read_canonical_le(r)?,
        })
    }
}

/// Fixed 16 B FootworkTelemetryPublic (§10.3).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct FootworkTelemetryPublic {
    pub dribble_status: u8,
    pub pivot_foot: u8,
    pub steps_since_gather: u8,
    pub footwork_flags: u8,
    pub pivot_x_mm: i16,
    pub pivot_y_mm: i16,
    pub gather_game_clock_ds: u16,
    pub closely_guarded_timer_ds: u16,
    pub gather_tick: u32,
}

impl Default for FootworkTelemetryPublic {
    fn default() -> Self {
        Self {
            dribble_status: 0,
            pivot_foot: 0,
            steps_since_gather: 0,
            footwork_flags: 0,
            pivot_x_mm: i16::MIN,
            pivot_y_mm: i16::MIN,
            gather_game_clock_ds: 0xFFFF,
            closely_guarded_timer_ds: 0,
            gather_tick: 0xFFFF_FFFF,
        }
    }
}

impl FootworkTelemetryPublic {
    pub fn write_le(&self, out: &mut Vec<u8>) {
        out.push(self.dribble_status);
        out.push(self.pivot_foot);
        out.push(self.steps_since_gather);
        out.push(self.footwork_flags & 0x3F);
        out.extend_from_slice(&self.pivot_x_mm.to_le_bytes());
        out.extend_from_slice(&self.pivot_y_mm.to_le_bytes());
        out.extend_from_slice(&self.gather_game_clock_ds.to_le_bytes());
        out.extend_from_slice(&self.closely_guarded_timer_ds.to_le_bytes());
        out.extend_from_slice(&self.gather_tick.to_le_bytes());
    }

    pub fn read_le(r: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let dribble_status = r.read_u8()?;
        if dribble_status > 4 {
            return Err(DecodeError::InvalidEnumValue {
                field: "FootworkTelemetryPublic.dribble_status",
                value: dribble_status as u32,
            });
        }
        let pivot_foot = r.read_u8()?;
        if pivot_foot > 3 {
            return Err(DecodeError::InvalidEnumValue {
                field: "FootworkTelemetryPublic.pivot_foot",
                value: pivot_foot as u32,
            });
        }
        let steps_since_gather = r.read_u8()?;
        let flags_off = r.position();
        let footwork_flags = r.read_u8()?;
        if (footwork_flags & 0xC0) != 0 {
            return Err(DecodeError::NonZeroPadding {
                offset: flags_off,
                value: footwork_flags,
            });
        }
        let pivot_x_mm = i16::read_canonical_le(r)?;
        let pivot_y_mm = i16::read_canonical_le(r)?;
        let x_min = pivot_x_mm == i16::MIN;
        let y_min = pivot_y_mm == i16::MIN;
        if x_min != y_min || (pivot_foot == 0 && !(x_min && y_min)) {
            return Err(DecodeError::InvalidEnumValue {
                field: "FootworkTelemetryPublic.pivot_pos",
                value: 0xFFFF,
            });
        }
        let gather_game_clock_ds = u16::read_canonical_le(r)?;
        let closely_guarded_timer_ds = u16::read_canonical_le(r)?;
        let gather_tick = u32::read_canonical_le(r)?;
        Ok(Self {
            dribble_status,
            pivot_foot,
            steps_since_gather,
            footwork_flags,
            pivot_x_mm,
            pivot_y_mm,
            gather_game_clock_ds,
            closely_guarded_timer_ds,
            gather_tick,
        })
    }
}
