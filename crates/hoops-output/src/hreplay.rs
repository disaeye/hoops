use std::io::Read;

use hoops_core::{
    CanonicalLeBytes, CanonicalReader, CommandPhase, DecodeError, GameEndReason, HashTraceV1,
    PublicFrameV1, ReplayManifest, SimulationEngine, StepStatus, VecSimSink,
};

use crate::error::OutputError;

pub const HREPLAY_MAGIC: [u8; 4] = *b"HRPL";
pub const HREPLAY_VERSION: u16 = 1;
pub const HREPLAY_COMPRESSION_ZSTD: u8 = 1;
pub const MAX_HREPLAY_MANIFEST_BYTES: usize = 2_097_152;

#[derive(Debug, Clone)]
pub struct HReplayContainer {
    pub manifest: ReplayManifest,
    pub hash_trace: Option<HashTraceV1>,
}

#[derive(Debug, Clone)]
pub struct StandaloneReplayOutcome {
    pub final_tick: u32,
    pub final_rolling_hash: [u8; 32],
    pub final_state_hash: [u8; 32],
    pub score_home: u16,
    pub score_away: u16,
    pub end_reason: Option<GameEndReason>,
    pub frames: Vec<PublicFrameV1>,
}

impl HReplayContainer {
    pub fn to_bytes(&self) -> Vec<u8> {
        let m_bytes = self.manifest.to_canonical_le_bytes();
        let compressed = zstd::encode_all(&m_bytes[..], 3).expect("zstd compress replay manifest");

        let mut out = Vec::with_capacity(16 + compressed.len());
        out.extend_from_slice(&HREPLAY_MAGIC);
        out.extend_from_slice(&HREPLAY_VERSION.to_le_bytes());
        out.push(HREPLAY_COMPRESSION_ZSTD);
        out.push(0); // reserved == 0
        out.extend_from_slice(&(m_bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(&compressed);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, OutputError> {
        if bytes.len() < 16 {
            return Err(OutputError::Decode(DecodeError::UnexpectedEof {
                offset: bytes.len(),
            }));
        }
        if bytes.len() > MAX_HREPLAY_MANIFEST_BYTES {
            return Err(OutputError::Decode(DecodeError::ManifestTooLarge {
                bytes: bytes.len(),
                limit: MAX_HREPLAY_MANIFEST_BYTES,
            }));
        }

        let mut r = CanonicalReader::new(bytes);
        if r.read_bytes(4)? != HREPLAY_MAGIC {
            return Err(OutputError::Decode(DecodeError::InvalidMagic));
        }
        let ver = u16::read_canonical_le(&mut r)?;
        if ver != HREPLAY_VERSION {
            return Err(OutputError::Decode(DecodeError::UnsupportedVersion {
                version: ver,
            }));
        }
        let compression_mode = r.read_u8()?;
        if compression_mode != HREPLAY_COMPRESSION_ZSTD {
            return Err(OutputError::Decode(DecodeError::InvalidEnumValue {
                field: "HReplay.compression_mode",
                value: compression_mode as u32,
            }));
        }
        r.read_zero_pad(1)?;
        let uncompressed_len = u64::read_canonical_le(&mut r)?;
        if uncompressed_len > MAX_HREPLAY_MANIFEST_BYTES as u64 {
            return Err(OutputError::Decode(DecodeError::InvalidFieldValue {
                field: "HReplay.uncompressed_manifest_bytes",
            }));
        }

        let compressed = &bytes[16..];
        let decoder = zstd::stream::read::Decoder::new(compressed)
            .map_err(|e| OutputError::Zstd(e.to_string()))?;
        let mut decompressed =
            Vec::with_capacity((uncompressed_len as usize).min(MAX_HREPLAY_MANIFEST_BYTES));
        decoder
            .take(uncompressed_len.saturating_add(1))
            .read_to_end(&mut decompressed)
            .map_err(|e| OutputError::Zstd(e.to_string()))?;
        if decompressed.len() as u64 != uncompressed_len {
            return Err(OutputError::Decode(DecodeError::InvalidFieldValue {
                field: "HReplay.uncompressed_manifest_bytes",
            }));
        }

        let manifest = ReplayManifest::from_canonical_le_bytes(&decompressed)?;
        Ok(Self {
            manifest,
            hash_trace: None,
        })
    }

    /// Execute self-contained offline deterministic replay cleanly with ZERO external files needed.
    pub fn replay_standalone(&self) -> Result<StandaloneReplayOutcome, OutputError> {
        let mut sink = VecSimSink::default();
        let mut eng = SimulationEngine::new(&self.manifest.init_spec, &mut sink)?;
        let cmds = &self.manifest.applied_commands;
        let mut ci = 0usize;
        let mut guard = 0usize;

        while !matches!(eng.state.cursor, hoops_core::EngineCursor::GameFinished)
            && guard < 220_000
        {
            guard += 1;
            while ci < cmds.len()
                && cmds[ci].resolved_tick == eng.state.sim_tick
                && cmds[ci].resolved_phase == CommandPhase::PreTick
            {
                eng.replay_anchored_command(cmds[ci], &mut sink)?;
                ci += 1;
            }
            let mut st = eng.step(&mut sink);
            while let StepStatus::AwaitingPostWhistle { .. } = st {
                while ci < cmds.len()
                    && cmds[ci].resolved_tick == eng.state.sim_tick
                    && cmds[ci].resolved_phase == CommandPhase::PostWhistle
                {
                    eng.replay_anchored_command(cmds[ci], &mut sink)?;
                    ci += 1;
                }
                st = eng.step(&mut sink);
            }
            if matches!(st, StepStatus::Finished { .. } | StepStatus::Aborted { .. }) {
                break;
            }
        }

        let (final_sh, _eh, final_rh) = eng.last_hashes;
        if let Some(ref ver) = self.manifest.verification {
            if eng.state.sim_tick != ver.final_tick {
                return Err(OutputError::VerificationFailed(format!(
                    "final tick mismatch: expected {}, got {}",
                    ver.final_tick, eng.state.sim_tick
                )));
            }
            if final_sh != ver.final_state_hash {
                return Err(OutputError::VerificationFailed(
                    "final_state_hash mismatch".to_owned(),
                ));
            }
            for &(cp_tick, cp_hash) in &ver.state_hash_checkpoints {
                if let Some(&(_, sh, _, _)) = sink.hashes.iter().find(|&&(t, _, _, _)| t == cp_tick) {
                    if sh != cp_hash {
                        return Err(OutputError::VerificationFailed(format!(
                            "checkpoint hash mismatch at tick {cp_tick}"
                        )));
                    }
                }
            }
        }

        Ok(StandaloneReplayOutcome {
            final_tick: eng.state.sim_tick,
            final_rolling_hash: final_rh,
            final_state_hash: final_sh,
            score_home: eng.state.team_states[0]
                .score_official_override
                .unwrap_or(eng.state.team_states[0].score_on_court),
            score_away: eng.state.team_states[1]
                .score_official_override
                .unwrap_or(eng.state.team_states[1].score_on_court),
            end_reason: eng.state.end_reason,
            frames: sink.public_frames,
        })
    }
}
