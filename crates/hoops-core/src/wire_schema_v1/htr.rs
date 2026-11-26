use crate::canonical_le_bytes::{CanonicalLeBytes, CanonicalReader, DecodeError};

pub const HTR1_MAGIC: [u8; 4] = *b"HTR1";
pub const HTR1_VERSION: u16 = 1;
pub const HTR1_HEADER_BYTES: usize = 80;
pub const HTR1_RECORD_BYTES: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashTraceRecordV1 {
    pub tick: u32,
    pub state_hash: [u8; 32],
    pub event_hash: [u8; 32],
    pub rolling_hash: [u8; 32],
}

/// Frozen `HTR1` per-tick reference hash trace (§2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashTraceV1 {
    pub format_version: u16,
    pub start_tick: u32,
    pub tick_count: u32,
    pub init_spec_sha256: [u8; 32],
    pub applied_commands_sha256: [u8; 32],
    pub records: Vec<HashTraceRecordV1>,
}

impl HashTraceV1 {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            HTR1_HEADER_BYTES + self.records.len() * HTR1_RECORD_BYTES,
        );
        out.extend_from_slice(&HTR1_MAGIC);
        out.extend_from_slice(&self.format_version.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // reserved == 0
        out.extend_from_slice(&self.start_tick.to_le_bytes());
        out.extend_from_slice(&(self.records.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.init_spec_sha256);
        out.extend_from_slice(&self.applied_commands_sha256);
        for rec in &self.records {
            out.extend_from_slice(&rec.tick.to_le_bytes());
            out.extend_from_slice(&rec.state_hash);
            out.extend_from_slice(&rec.event_hash);
            out.extend_from_slice(&rec.rolling_hash);
        }
        out
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, DecodeError> {
        if data.len() < HTR1_HEADER_BYTES {
            return Err(DecodeError::UnexpectedEof { offset: data.len() });
        }
        let mut r = CanonicalReader::new(data);
        let magic = r.read_bytes(4)?;
        if magic != HTR1_MAGIC {
            return Err(DecodeError::InvalidMagic);
        }
        let format_version = u16::read_canonical_le(&mut r)?;
        if format_version != HTR1_VERSION {
            return Err(DecodeError::UnsupportedVersion {
                version: format_version,
            });
        }
        r.read_zero_pad(2)?;
        let start_tick = u32::read_canonical_le(&mut r)?;
        let tick_count = u32::read_canonical_le(&mut r)?;
        let init_spec_sha256 = <[u8; 32]>::read_canonical_le(&mut r)?;
        let applied_commands_sha256 = <[u8; 32]>::read_canonical_le(&mut r)?;

        let expected_len = HTR1_HEADER_BYTES + (tick_count as usize) * HTR1_RECORD_BYTES;
        if data.len() < expected_len {
            return Err(DecodeError::UnexpectedEof { offset: data.len() });
        }

        let mut records = Vec::with_capacity((tick_count as usize).min(200_000));
        for i in 0..tick_count {
            let tick = u32::read_canonical_le(&mut r)?;
            if tick != start_tick.saturating_add(i) {
                return Err(DecodeError::FrameSequenceGap {
                    expected: start_tick.saturating_add(i),
                    got: tick,
                });
            }
            let state_hash = <[u8; 32]>::read_canonical_le(&mut r)?;
            let event_hash = <[u8; 32]>::read_canonical_le(&mut r)?;
            let rolling_hash = <[u8; 32]>::read_canonical_le(&mut r)?;
            records.push(HashTraceRecordV1 {
                tick,
                state_hash,
                event_hash,
                rolling_hash,
            });
        }
        r.ensure_eof()?;

        Ok(Self {
            format_version,
            start_tick,
            tick_count,
            init_spec_sha256,
            applied_commands_sha256,
            records,
        })
    }
}
