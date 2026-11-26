use serde::{Deserialize, Serialize};

use crate::canonical_le_bytes::{CanonicalLeBytes, CanonicalReader, DecodeError};

/// Philox-4x32-10 standard constants (§2.3).
const PHILOX_M0: u32 = 0xD251_1F53;
const PHILOX_M1: u32 = 0xCD9E_8D57;
const PHILOX_W0: u32 = 0x9E37_79B9;
const PHILOX_W1: u32 = 0xBB67_AE85;

/// Frozen 9-variant RNG purpose tag with deterministic per-tick per-slot budgets.
#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RngPurpose {
    ShotOutcome = 0,
    ShotScatterAndReboundVec = 1,
    ReboundBounce = 2,
    StealRoll = 3,
    DecisionSoftmax = 4,
    ContactAndFoulRoll = 5,
    RefereeNoise = 6,
    InjuryAndMomentumRoll = 7,
    SymmetricPerturbation = 8,
}

impl RngPurpose {
    pub const COUNT: usize = 9;

    pub const fn budget(self) -> u16 {
        match self {
            RngPurpose::ShotOutcome => 2,
            RngPurpose::ShotScatterAndReboundVec => 4,
            RngPurpose::ReboundBounce => 2,
            RngPurpose::StealRoll => 4,
            RngPurpose::DecisionSoftmax => 1,
            RngPurpose::ContactAndFoulRoll => 64,
            RngPurpose::RefereeNoise => 4,
            RngPurpose::InjuryAndMomentumRoll => 2,
            RngPurpose::SymmetricPerturbation => 1,
        }
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::ShotOutcome),
            1 => Some(Self::ShotScatterAndReboundVec),
            2 => Some(Self::ReboundBounce),
            3 => Some(Self::StealRoll),
            4 => Some(Self::DecisionSoftmax),
            5 => Some(Self::ContactAndFoulRoll),
            6 => Some(Self::RefereeNoise),
            7 => Some(Self::InjuryAndMomentumRoll),
            8 => Some(Self::SymmetricPerturbation),
            _ => None,
        }
    }
}

impl CanonicalLeBytes for RngPurpose {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let v = reader.read_u8()?;
        Self::from_u8(v).ok_or(DecodeError::InvalidEnumValue {
            field: "RngPurpose",
            value: v as u32,
        })
    }
}

/// Pure Rust counter-based Philox-4x32-10 PRNG (§2.3).
#[derive(Debug, Clone, Copy)]
pub struct Philox4x32_10;

impl Philox4x32_10 {
    #[inline]
    pub fn key_from_seed(seed: u64) -> [u32; 2] {
        [seed as u32, (seed >> 32) as u32]
    }

    #[inline]
    fn mulhilo32(a: u32, b: u32) -> (u32, u32) {
        let prod = (a as u64) * (b as u64);
        ((prod >> 32) as u32, prod as u32)
    }

    #[inline]
    fn single_round(ctr: [u32; 4], key: [u32; 2]) -> [u32; 4] {
        let (hi0, lo0) = Self::mulhilo32(PHILOX_M0, ctr[0]);
        let (hi1, lo1) = Self::mulhilo32(PHILOX_M1, ctr[2]);
        [
            hi1 ^ ctr[1] ^ key[0],
            lo1,
            hi0 ^ ctr[3] ^ key[1],
            lo0,
        ]
    }

    #[inline]
    pub fn generate(key: [u32; 2], counter: [u32; 4]) -> [u32; 4] {
        let mut c = counter;
        let mut k = key;
        for round in 0..10 {
            c = Self::single_round(c, k);
            if round < 9 {
                k[0] = k[0].wrapping_add(PHILOX_W0);
                k[1] = k[1].wrapping_add(PHILOX_W1);
            }
        }
        c
    }

    /// Extract 53-bit uniform float in `[0.0, 1.0)` from low two words `out[0..2]`.
    #[inline]
    pub fn words_to_u53_low(out: [u32; 4]) -> f64 {
        let bits53 = (((out[1] & 0x001F_FFFF) as u64) << 32) | (out[0] as u64);
        (bits53 as f64) * (1.0 / 9_007_199_254_740_992.0)
    }

    /// Extract 53-bit uniform float in `[0.0, 1.0)` from high two words `out[2..4]`.
    #[inline]
    pub fn words_to_u53_high(out: [u32; 4]) -> f64 {
        let bits53 = (((out[3] & 0x001F_FFFF) as u64) << 32) | (out[2] as u64);
        (bits53 as f64) * (1.0 / 9_007_199_254_740_992.0)
    }

    /// Standard Box-Muller Gaussian N(0, 1) using `libm` transcendental functions.
    #[inline]
    pub fn words_to_gaussian(out: [u32; 4]) -> f64 {
        let min_u = 1.0 / 9_007_199_254_740_992.0;
        let u1 = Self::words_to_u53_low(out).max(min_u);
        let u2 = Self::words_to_u53_high(out);
        let r = libm::sqrt(-2.0 * libm::log(u1));
        let theta = 2.0 * core::f64::consts::PI * u2;
        r * libm::cos(theta)
    }

    #[inline]
    pub fn build_counter(
        possession_id: u32,
        possession_tick: u16,
        canonical_actor_id: u8,
        purpose: RngPurpose,
        sample_idx: u16,
    ) -> [u32; 4] {
        [
            possession_id,
            ((possession_tick as u32) << 16)
                | ((canonical_actor_id as u32) << 8)
                | (purpose as u32),
            sample_idx as u32,
            0u32,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RngBudgetExceeded {
    pub actor_slot: u8,
    pub purpose: RngPurpose,
    pub budget: u16,
}

#[inline]
pub fn actor_slot_index(canonical_actor_id: u8) -> usize {
    if canonical_actor_id == 0xFF {
        32
    } else {
        let team = ((canonical_actor_id >> 4) as usize).min(1);
        let rank = (canonical_actor_id & 0x0F) as usize;
        team * 16 + rank
    }
}

/// Deterministic sampling helper enforcing Debug+Release uniform budget check.
pub fn sample_u53_checked(
    philox_key: [u32; 2],
    possession_id: u32,
    possession_tick: u16,
    counters: &mut [[u16; 9]; 33],
    canonical_actor_id: u8,
    purpose: RngPurpose,
) -> Result<f64, RngBudgetExceeded> {
    let slot = actor_slot_index(canonical_actor_id);
    let p_idx = purpose as usize;
    let old_cnt = counters[slot][p_idx];
    let budget = purpose.budget();
    if old_cnt >= budget {
        return Err(RngBudgetExceeded {
            actor_slot: slot as u8,
            purpose,
            budget,
        });
    }
    counters[slot][p_idx] = old_cnt + 1;
    let ctr = Philox4x32_10::build_counter(
        possession_id,
        possession_tick,
        canonical_actor_id,
        purpose,
        old_cnt,
    );
    let words = Philox4x32_10::generate(philox_key, ctr);
    Ok(Philox4x32_10::words_to_u53_low(words))
}

pub fn sample_gaussian_checked(
    philox_key: [u32; 2],
    possession_id: u32,
    possession_tick: u16,
    counters: &mut [[u16; 9]; 33],
    canonical_actor_id: u8,
    purpose: RngPurpose,
) -> Result<f64, RngBudgetExceeded> {
    let slot = actor_slot_index(canonical_actor_id);
    let p_idx = purpose as usize;
    let old_cnt = counters[slot][p_idx];
    let budget = purpose.budget();
    if old_cnt >= budget {
        return Err(RngBudgetExceeded {
            actor_slot: slot as u8,
            purpose,
            budget,
        });
    }
    counters[slot][p_idx] = old_cnt + 1;
    let ctr = Philox4x32_10::build_counter(
        possession_id,
        possession_tick,
        canonical_actor_id,
        purpose,
        old_cnt,
    );
    let words = Philox4x32_10::generate(philox_key, ctr);
    Ok(Philox4x32_10::words_to_gaussian(words))
}
