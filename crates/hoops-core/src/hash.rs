use sha2::{Digest, Sha256};

use crate::canonical_le_bytes::CanonicalLeBytes;

pub const DOMAIN_STATE_V1: &[u8] = b"HOOPS_STATE_V1\0";
pub const DOMAIN_EVENT_V1: &[u8] = b"HOOPS_EVENT_V1\0";
pub const DOMAIN_TICK_V1: &[u8] = b"HOOPS_TICK_V1\0";
pub const DOMAIN_ROLL_V1: &[u8] = b"HOOPS_ROLL_V1\0";
pub const DOMAIN_REQ_FP_V1: &[u8] = b"HOOPS_REQ_FP_V1\0";

#[inline]
pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

pub fn compute_state_hash<S: CanonicalLeBytes>(state: &S) -> [u8; 32] {
    let mut scratch = Vec::with_capacity(8192);
    compute_state_hash_with_buf(state, &mut scratch)
}

pub fn compute_state_hash_with_buf<S: CanonicalLeBytes>(
    state: &S,
    scratch: &mut Vec<u8>,
) -> [u8; 32] {
    scratch.clear();
    state.write_canonical_le(scratch);
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN_STATE_V1);
    hasher.update(&*scratch);
    hasher.finalize().into()
}

pub fn compute_event_hash(event_count: u16, events_wire_bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN_EVENT_V1);
    if event_count == 0 {
        hasher.update([0u8; 6]);
    } else {
        hasher.update(event_count.to_le_bytes());
        hasher.update((events_wire_bytes.len() as u32).to_le_bytes());
        hasher.update(events_wire_bytes);
    }
    hasher.finalize().into()
}

pub fn compute_tick_digest(
    tick: u32,
    state_hash: [u8; 32],
    event_hash: [u8; 32],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN_TICK_V1);
    hasher.update(tick.to_le_bytes());
    hasher.update(state_hash);
    hasher.update(event_hash);
    hasher.finalize().into()
}

pub fn compute_rolling_hash(prev_rolling_hash: [u8; 32], tick_digest: [u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN_ROLL_V1);
    hasher.update(prev_rolling_hash);
    hasher.update(tick_digest);
    hasher.finalize().into()
}

pub fn compute_request_fingerprint<T: CanonicalLeBytes>(tuple_val: &T) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN_REQ_FP_V1);
    hasher.update(tuple_val.to_canonical_le_bytes());
    hasher.finalize().into()
}
