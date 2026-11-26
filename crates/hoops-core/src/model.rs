use crate::wire_schema_v1::*;

pub const BALL_RADIUS_M: f64 = 0.1213;
pub const GRAVITY_M_S2: f64 = 9.80665;
pub const RIM_RADIUS_M: f64 = 0.2286;
pub const SUB_DS_UNITS: u32 = 10_000;

#[derive(Debug, Clone, PartialEq)]
pub struct CcdSubSegment {
    pub tau_start: f64,
    pub tau_end: f64,
    pub positions: [(f64, f64); 10],
    pub velocities: [(f64, f64); 10],
    pub radii: [f64; 10],
    pub active_mask: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShotAuditSample {
    pub tick: u32,
    pub shooter_slot: u8,
    pub is_three: bool,
    pub c_geom: f64,
    pub dist_to_rim_m: f64,
    pub outcome: IntendedOutcomeClass,
    pub is_rim_or_close: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PnrAuditEvent {
    pub tick: u32,
    pub possession_id: u32,
    pub offense_side: CommandSide,
    pub coverage: PnrCoverageKind,
    pub handler_pullup_3: bool,
    pub passed_out_of_blitz: bool,
    pub roller_drew_help: bool,
    pub extra_pass_to_corner: bool,
    pub open_corner_3_attempt: bool,
    pub handler_tov: bool,
    pub pre_switch_triggered: bool,
    pub big_forced_iso: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditGroundTruthRecord {
    pub tick: u32,
    pub warnings: Vec<AuditWarning>,
    pub ccd_sub_segments: Vec<CcdSubSegment>,
    pub shots: Vec<ShotAuditSample>,
    pub pnr_events: Vec<PnrAuditEvent>,
    pub fiba_def_paint_gt_3s_legal: u32,
    pub spacing_min_dist_m: Option<f64>,
    pub wing_def_dist_m: Option<f64>,
    pub weak_rim_dist_m: Option<f64>,
}

/// Zero-allocation simulation output sink trait (§3.3).
pub trait SimSink {
    fn on_public_frame(&mut self, frame: &PublicFrameV1);
    fn on_debug_frame(&mut self, frame: &DebugOverlayFrameV1);
    fn on_game_event(&mut self, event: &GameRecordEventV1);
    fn on_audit(&mut self, record: &AuditGroundTruthRecord);
    fn on_anchored_command(&mut self, cmd: &AnchoredCommand);
    fn on_tick_hashes(
        &mut self,
        tick: u32,
        state_hash: [u8; 32],
        event_hash: [u8; 32],
        rolling_hash: [u8; 32],
    );
}

/// Null sink for headless fast benchmarks.
#[derive(Debug, Default)]
pub struct NullSimSink;

impl SimSink for NullSimSink {
    fn on_public_frame(&mut self, _frame: &PublicFrameV1) {}
    fn on_debug_frame(&mut self, _frame: &DebugOverlayFrameV1) {}
    fn on_game_event(&mut self, _event: &GameRecordEventV1) {}
    fn on_audit(&mut self, _record: &AuditGroundTruthRecord) {}
    fn on_anchored_command(&mut self, _cmd: &AnchoredCommand) {}
    fn on_tick_hashes(
        &mut self,
        _tick: u32,
        _state_hash: [u8; 32],
        _event_hash: [u8; 32],
        _rolling_hash: [u8; 32],
    ) {
    }
}

/// Fixed-capacity in-memory collector sink for tests & full-trace recording.
#[derive(Debug, Default, Clone)]
pub struct VecSimSink {
    pub public_frames: Vec<PublicFrameV1>,
    pub debug_frames: Vec<DebugOverlayFrameV1>,
    pub events: Vec<GameRecordEventV1>,
    pub audits: Vec<AuditGroundTruthRecord>,
    pub anchored_commands: Vec<AnchoredCommand>,
    pub hashes: Vec<(u32, [u8; 32], [u8; 32], [u8; 32])>,
}

impl SimSink for VecSimSink {
    fn on_public_frame(&mut self, frame: &PublicFrameV1) {
        self.public_frames.push(*frame);
    }
    fn on_debug_frame(&mut self, frame: &DebugOverlayFrameV1) {
        self.debug_frames.push(*frame);
    }
    fn on_game_event(&mut self, event: &GameRecordEventV1) {
        self.events.push(event.clone());
    }
    fn on_audit(&mut self, record: &AuditGroundTruthRecord) {
        self.audits.push(record.clone());
    }
    fn on_anchored_command(&mut self, cmd: &AnchoredCommand) {
        self.anchored_commands.push(*cmd);
    }
    fn on_tick_hashes(
        &mut self,
        tick: u32,
        state_hash: [u8; 32],
        event_hash: [u8; 32],
        rolling_hash: [u8; 32],
    ) {
        self.hashes
            .push((tick, state_hash, event_hash, rolling_hash));
    }
}

/// Pre-allocated per-tick event buffer (`<= 48`, §3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedTickEventBuf<const N: usize> {
    items: Vec<GameRecordEventV1>,
}

impl<const N: usize> Default for BoundedTickEventBuf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> BoundedTickEventBuf<N> {
    pub fn new() -> Self {
        Self {
            items: Vec::with_capacity(N),
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn as_slice(&self) -> &[GameRecordEventV1] {
        &self.items
    }

    pub fn push(&mut self, ev: GameRecordEventV1) -> bool {
        if self.items.len() >= N {
            return false;
        }
        self.items.push(ev);
        true
    }

    pub fn from_vec(v: Vec<GameRecordEventV1>) -> Self {
        let mut buf = Self::new();
        for item in v.into_iter().take(N) {
            buf.items.push(item);
        }
        buf
    }

    pub fn to_vec(&self) -> Vec<GameRecordEventV1> {
        self.items.clone()
    }
}

/// Convert `u32` 10µs remaining units to ceiling display deci-seconds (`u16`).
#[inline]
pub fn rem_10us_to_ds(rem_10us: u32) -> u16 {
    ((rem_10us.saturating_add(9_999)) / SUB_DS_UNITS) as u16
}
