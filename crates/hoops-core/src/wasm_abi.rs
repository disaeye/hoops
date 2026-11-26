use core::slice;
use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::canonical_le_bytes::CanonicalLeBytes;
use crate::engine::SimulationEngine;
use crate::model::SimSink;
use crate::wire_schema_v1::*;

pub const WASM_ABI_VERSION: u32 = 1;
pub const WASM_ERR_BUFFER_TOO_SMALL: u32 = 0xFFFF_FFFE;
pub const WASM_ERR_INVALID_HANDLE_OR_CODEC: u32 = 0xFFFF_FFFF;

/// Isolated WASM output staging sink stored outside `GameState` (§2.2).
#[derive(Debug, Clone)]
pub struct WasmSimSink {
    pub last_call_has_public_frame: bool,
    pub last_call_public_frame: [u8; PUBLIC_FRAME_BYTES],
    pub last_call_has_debug_frame: bool,
    pub last_call_debug_frame: [u8; DEBUG_FRAME_BYTES],
    pub last_call_events_wire: Vec<u8>,
    pub last_call_event_count: u16,
    pub last_hashes: [u8; 96],
    pub last_checkpoint_bytes: Vec<u8>,
    pub staging_events: Vec<GameRecordEventV1>,
    staging_payload_scratch: Vec<u8>,
}

impl Default for WasmSimSink {
    fn default() -> Self {
        Self {
            last_call_has_public_frame: false,
            last_call_public_frame: [0u8; PUBLIC_FRAME_BYTES],
            last_call_has_debug_frame: false,
            last_call_debug_frame: [0u8; DEBUG_FRAME_BYTES],
            last_call_events_wire: Vec::with_capacity(4096),
            last_call_event_count: 0,
            last_hashes: [0u8; 96],
            last_checkpoint_bytes: Vec::with_capacity(131_072),
            staging_events: Vec::with_capacity(MAX_EVENTS_PER_TICK),
            staging_payload_scratch: Vec::with_capacity(4096),
        }
    }
}

impl WasmSimSink {
    pub fn reset_for_mutating_call(&mut self) {
        self.last_call_has_public_frame = false;
        self.last_call_has_debug_frame = false;
        self.staging_events.clear();
        self.last_call_event_count = 0;
        self.finalize_events_wire();
    }

    pub fn finalize_events_wire(&mut self) {
        let count = self.staging_events.len() as u16;
        self.last_call_event_count = count;
        self.staging_payload_scratch.clear();
        for ev in &self.staging_events {
            ev.write_le(&mut self.staging_payload_scratch);
        }
        let payload_len = self.staging_payload_scratch.len();
        self.last_call_events_wire.clear();
        self.last_call_events_wire
            .extend_from_slice(&count.to_le_bytes());
        self.last_call_events_wire
            .extend_from_slice(&(payload_len as u32).to_le_bytes());
        self.last_call_events_wire
            .extend_from_slice(&self.staging_payload_scratch);
    }
}

impl SimSink for WasmSimSink {
    fn on_public_frame(&mut self, frame: &PublicFrameV1) {
        self.last_call_has_public_frame = true;
        self.last_call_public_frame = frame.to_bytes();
    }
    fn on_debug_frame(&mut self, frame: &DebugOverlayFrameV1) {
        self.last_call_has_debug_frame = true;
        self.last_call_debug_frame = frame.to_bytes();
    }
    fn on_game_event(&mut self, event: &GameRecordEventV1) {
        self.staging_events.push(event.clone());
    }
    fn on_audit(&mut self, _record: &crate::model::AuditGroundTruthRecord) {}
    fn on_anchored_command(&mut self, _cmd: &AnchoredCommand) {}
    fn on_tick_hashes(
        &mut self,
        _tick: u32,
        state_hash: [u8; 32],
        event_hash: [u8; 32],
        rolling_hash: [u8; 32],
    ) {
        self.last_hashes[0..32].copy_from_slice(&state_hash);
        self.last_hashes[32..64].copy_from_slice(&event_hash);
        self.last_hashes[64..96].copy_from_slice(&rolling_hash);
    }
}

pub struct WasmEngineHandle {
    pub engine: SimulationEngine,
    pub wasm_sink: WasmSimSink,
    pub active_window_buf: [u8; CHALLENGE_SNAPSHOT_BYTES],
}

static HANDLES: Mutex<(u32, BTreeMap<u32, WasmEngineHandle>)> = Mutex::new((1, BTreeMap::new()));

pub fn host_abi_version() -> u32 {
    WASM_ABI_VERSION
}

pub fn host_alloc(size: usize) -> *mut u8 {
    if size == 0 {
        return core::ptr::NonNull::<u8>::dangling().as_ptr();
    }
    let mut vec = Vec::<u8>::with_capacity(size);
    let ptr = vec.as_mut_ptr();
    core::mem::forget(vec);
    ptr
}

pub unsafe fn host_dealloc(ptr: *mut u8, size: usize) {
    if size == 0 || ptr.is_null() {
        return;
    }
    let _ = Vec::from_raw_parts(ptr, 0, size);
}

pub fn host_engine_from_manifest(spec_bytes: &[u8]) -> u32 {
    let init_spec = match MatchInitSpecV1::from_canonical_le_bytes(spec_bytes) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let mut wasm_sink = WasmSimSink::default();
    wasm_sink.reset_for_mutating_call();
    let engine = match SimulationEngine::new(&init_spec, &mut wasm_sink) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    wasm_sink.finalize_events_wire();
    let mut guard = HANDLES.lock().unwrap();
    let h = guard.0;
    guard.0 = h.wrapping_add(1).max(1);
    guard.1.insert(
        h,
        WasmEngineHandle {
            engine,
            wasm_sink,
            active_window_buf: [0u8; CHALLENGE_SNAPSHOT_BYTES],
        },
    );
    h
}

pub fn host_engine_destroy(handle: u32) {
    let mut guard = HANDLES.lock().unwrap();
    guard.1.remove(&handle);
}

/// Strictly checks output buffer capacity BEFORE any state or `wasm_sink` mutation (§2.2).
pub fn host_engine_enqueue_command(
    handle: u32,
    ingress_bytes: &[u8],
    out_anchored: &mut [u8],
) -> u32 {
    let cmd = match IngressCommand::from_canonical_le_bytes(ingress_bytes) {
        Ok(c) => c,
        Err(_) => return CommandRejectReason::InvalidCodecOrPadding as u32,
    };
    let mut guard = HANDLES.lock().unwrap();
    let h = match guard.1.get_mut(&handle) {
        Some(v) => v,
        None => return WASM_ERR_INVALID_HANDLE_OR_CODEC,
    };

    // Phase 1: Pure read-only prepare_command
    let outcome = match h.engine.prepare_command(cmd) {
        Ok(o) => o,
        Err(reason) => return reason as u32,
    };

    let (enc_bytes, enc_len) = match &outcome {
        PreparedOutcome::AlreadyAccepted {
            encoded_bytes,
            encoded_len,
            ..
        } => (encoded_bytes.0, *encoded_len as usize),
        PreparedOutcome::NewCommand(p) => (p.encoded_bytes.0, p.encoded_len as usize),
    };

    // Capacity check BEFORE mutating GameState or wasm_sink!
    let needed = 4 + enc_len;
    if out_anchored.len() < needed {
        if out_anchored.len() >= 4 {
            out_anchored[0..4].copy_from_slice(&(needed as u32).to_le_bytes());
        }
        return WASM_ERR_BUFFER_TOO_SMALL;
    }

    // Phase 2: Atomic commit + reset wasm_sink
    h.wasm_sink.reset_for_mutating_call();
    match outcome {
        PreparedOutcome::AlreadyAccepted { .. } => {
            // Zero mutation to GameState, refresh last_hashes
            let (sh, eh, rh) = h.engine.last_hashes;
            h.wasm_sink.on_tick_hashes(h.engine.state.sim_tick, sh, eh, rh);
        }
        PreparedOutcome::NewCommand(prep) => {
            let _ = h.engine.commit_prepared_command(prep, &mut h.wasm_sink);
        }
    }
    h.wasm_sink.finalize_events_wire();

    out_anchored[0..4].copy_from_slice(&(enc_len as u32).to_le_bytes());
    out_anchored[4..4 + enc_len].copy_from_slice(&enc_bytes[..enc_len]);
    0
}

pub fn host_engine_replay_anchored_command(handle: u32, anchored_bytes: &[u8]) -> u32 {
    if anchored_bytes.len() > MAX_INGRESS_COMMAND_BYTES {
        return CommandRejectReason::PayloadTooLarge as u32;
    }
    let anchored = match AnchoredCommand::from_canonical_le_bytes(anchored_bytes) {
        Ok(c) => c,
        Err(_) => return CommandRejectReason::InvalidCodecOrPadding as u32,
    };
    let mut guard = HANDLES.lock().unwrap();
    let h = match guard.1.get_mut(&handle) {
        Some(v) => v,
        None => return WASM_ERR_INVALID_HANDLE_OR_CODEC,
    };
    h.wasm_sink.reset_for_mutating_call();
    let res = h
        .engine
        .replay_anchored_command(anchored, &mut h.wasm_sink);
    h.wasm_sink.finalize_events_wire();
    match res {
        Ok(()) => 0,
        Err(_) => WASM_ERR_INVALID_HANDLE_OR_CODEC,
    }
}

pub fn host_engine_step(handle: u32) -> u32 {
    let mut guard = HANDLES.lock().unwrap();
    let h = match guard.1.get_mut(&handle) {
        Some(v) => v,
        None => return 0,
    };
    h.wasm_sink.reset_for_mutating_call();
    let st = h.engine.step(&mut h.wasm_sink);
    h.wasm_sink.finalize_events_wire();
    match st {
        StepStatus::TickCompleted { .. } => 1,
        StepStatus::AwaitingPostWhistle { .. } => 2,
        StepStatus::Finished { .. } => 3,
        StepStatus::Aborted { .. } => 4,
    }
}

pub fn host_engine_last_public_frame(handle: u32) -> Option<(bool, [u8; PUBLIC_FRAME_BYTES])> {
    let guard = HANDLES.lock().unwrap();
    let h = guard.1.get(&handle)?;
    Some((
        h.wasm_sink.last_call_has_public_frame,
        h.wasm_sink.last_call_public_frame,
    ))
}

pub fn host_engine_last_debug_frame(handle: u32) -> Option<(bool, [u8; DEBUG_FRAME_BYTES])> {
    let guard = HANDLES.lock().unwrap();
    let h = guard.1.get(&handle)?;
    Some((
        h.wasm_sink.last_call_has_debug_frame,
        h.wasm_sink.last_call_debug_frame,
    ))
}

pub fn host_engine_last_events_wire(handle: u32) -> Option<Vec<u8>> {
    let guard = HANDLES.lock().unwrap();
    let h = guard.1.get(&handle)?;
    Some(h.wasm_sink.last_call_events_wire.clone())
}

pub fn host_engine_last_hashes(handle: u32) -> Option<[u8; 96]> {
    let guard = HANDLES.lock().unwrap();
    let h = guard.1.get(&handle)?;
    Some(h.wasm_sink.last_hashes)
}

pub fn host_engine_capture_checkpoint(handle: u32) -> Option<Vec<u8>> {
    let mut guard = HANDLES.lock().unwrap();
    let h = guard.1.get_mut(&handle)?;
    let bytes = h.engine.capture_checkpoint_snapshot().to_canonical_le_bytes();
    h.wasm_sink.last_checkpoint_bytes = bytes.clone();
    Some(bytes)
}

// ============================================================================
// Frozen 14 WASM C-ABI Exports (§2.2)
// ============================================================================

#[no_mangle]
pub extern "C" fn hoops_wasm_abi_version() -> u32 {
    host_abi_version()
}

#[no_mangle]
pub extern "C" fn hoops_wasm_alloc(size: u32) -> *mut u8 {
    host_alloc(size as usize)
}

#[no_mangle]
pub unsafe extern "C" fn hoops_wasm_dealloc(ptr: *mut u8, size: u32) {
    host_dealloc(ptr, size as usize);
}

#[no_mangle]
pub unsafe extern "C" fn hoops_engine_from_manifest(
    init_spec_ptr: *const u8,
    init_spec_len: u32,
) -> u32 {
    if init_spec_ptr.is_null() {
        return 0;
    }
    let slice = slice::from_raw_parts(init_spec_ptr, init_spec_len as usize);
    host_engine_from_manifest(slice)
}

#[no_mangle]
pub extern "C" fn hoops_engine_destroy(handle: u32) {
    host_engine_destroy(handle);
}

#[no_mangle]
pub unsafe extern "C" fn hoops_engine_enqueue_command(
    handle: u32,
    ingress_ptr: *const u8,
    ingress_len: u32,
    out_anchored_ptr: *mut u8,
    out_anchored_cap: u32,
) -> u32 {
    if ingress_ptr.is_null() || (out_anchored_cap > 0 && out_anchored_ptr.is_null()) {
        return WASM_ERR_INVALID_HANDLE_OR_CODEC;
    }
    let in_slice = slice::from_raw_parts(ingress_ptr, ingress_len as usize);
    let out_slice = if out_anchored_cap == 0 {
        &mut []
    } else {
        slice::from_raw_parts_mut(out_anchored_ptr, out_anchored_cap as usize)
    };
    host_engine_enqueue_command(handle, in_slice, out_slice)
}

#[no_mangle]
pub unsafe extern "C" fn hoops_engine_replay_anchored_command(
    handle: u32,
    anchored_ptr: *const u8,
    anchored_len: u32,
) -> u32 {
    if anchored_ptr.is_null() {
        return WASM_ERR_INVALID_HANDLE_OR_CODEC;
    }
    let slice = slice::from_raw_parts(anchored_ptr, anchored_len as usize);
    host_engine_replay_anchored_command(handle, slice)
}

#[no_mangle]
pub extern "C" fn hoops_engine_step(handle: u32) -> u32 {
    host_engine_step(handle)
}

#[no_mangle]
pub unsafe extern "C" fn hoops_engine_last_public_frame_ptr(
    handle: u32,
    out_has_new_frame: *mut u8,
) -> *const u8 {
    let guard = HANDLES.lock().unwrap();
    match guard.1.get(&handle) {
        Some(h) => {
            if !out_has_new_frame.is_null() {
                *out_has_new_frame = if h.wasm_sink.last_call_has_public_frame { 1 } else { 0 };
            }
            h.wasm_sink.last_call_public_frame.as_ptr()
        }
        None => {
            if !out_has_new_frame.is_null() {
                *out_has_new_frame = 0;
            }
            core::ptr::null()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn hoops_engine_last_debug_frame_ptr(
    handle: u32,
    out_has_new_frame: *mut u8,
) -> *const u8 {
    let guard = HANDLES.lock().unwrap();
    match guard.1.get(&handle) {
        Some(h) => {
            if !out_has_new_frame.is_null() {
                *out_has_new_frame = if h.wasm_sink.last_call_has_debug_frame { 1 } else { 0 };
            }
            h.wasm_sink.last_call_debug_frame.as_ptr()
        }
        None => {
            if !out_has_new_frame.is_null() {
                *out_has_new_frame = 0;
            }
            core::ptr::null()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn hoops_engine_last_events_ptr(
    handle: u32,
    out_len: *mut u32,
) -> *const u8 {
    let guard = HANDLES.lock().unwrap();
    match guard.1.get(&handle) {
        Some(h) => {
            if !out_len.is_null() {
                *out_len = h.wasm_sink.last_call_events_wire.len() as u32;
            }
            h.wasm_sink.last_call_events_wire.as_ptr()
        }
        None => {
            if !out_len.is_null() {
                *out_len = 0;
            }
            core::ptr::null()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn hoops_engine_active_window_ptr(
    handle: u32,
    out_len: *mut u32,
) -> *const u8 {
    let mut guard = HANDLES.lock().unwrap();
    match guard.1.get_mut(&handle) {
        Some(h) => {
            if let Some(snap) = h.engine.active_challenge_window_snapshot() {
                h.active_window_buf = snap.to_bytes();
                if !out_len.is_null() {
                    *out_len = CHALLENGE_SNAPSHOT_BYTES as u32;
                }
                h.active_window_buf.as_ptr()
            } else {
                if !out_len.is_null() {
                    *out_len = 0;
                }
                core::ptr::null()
            }
        }
        None => {
            if !out_len.is_null() {
                *out_len = 0;
            }
            core::ptr::null()
        }
    }
}

#[no_mangle]
pub extern "C" fn hoops_engine_last_hashes_ptr(handle: u32) -> *const u8 {
    let guard = HANDLES.lock().unwrap();
    match guard.1.get(&handle) {
        Some(h) => h.wasm_sink.last_hashes.as_ptr(),
        None => core::ptr::null(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn hoops_engine_capture_checkpoint_ptr(
    handle: u32,
    out_len: *mut u32,
) -> *const u8 {
    let mut guard = HANDLES.lock().unwrap();
    match guard.1.get_mut(&handle) {
        Some(h) => {
            let bytes = h.engine.capture_checkpoint_snapshot().to_canonical_le_bytes();
            h.wasm_sink.last_checkpoint_bytes = bytes;
            if !out_len.is_null() {
                *out_len = h.wasm_sink.last_checkpoint_bytes.len() as u32;
            }
            h.wasm_sink.last_checkpoint_bytes.as_ptr()
        }
        None => {
            if !out_len.is_null() {
                *out_len = 0;
            }
            core::ptr::null()
        }
    }
}
