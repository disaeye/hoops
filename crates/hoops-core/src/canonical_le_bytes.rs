use std::collections::BTreeMap;
use thiserror::Error;

/// Canonical decode errors for `CanonicalLeBytes`, `HPS1`, `HCK1`, `HTR1`, `.hreplay`, and events.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DecodeError {
    #[error("unexpected end of input at offset {offset}")]
    UnexpectedEof { offset: usize },
    #[error("trailing {count} bytes after expected end of payload")]
    TrailingBytes { count: usize },
    #[error("non-zero padding byte 0x{value:02X} at offset {offset}")]
    NonZeroPadding { offset: usize, value: u8 },
    #[error("invalid enum value {value} for {field}")]
    InvalidEnumValue { field: &'static str, value: u32 },
    #[error("invalid UTF-8 string")]
    InvalidUtf8,
    #[error("identifier length {len} out of range 1..=64 or contains NUL")]
    IdLengthOutOfRange { len: usize },
    #[error("private tactic/defense dictionary entries found in PublicOnly stream")]
    PrivateDictInPublicStream,
    #[error("frame sequence gap: expected {expected}, got {got}")]
    FrameSequenceGap { expected: u32, got: u32 },
    #[error("event sequence gap: expected {expected}, got {got} (tick={tick}, frame_tick={frame_tick})")]
    EventSequenceGap {
        expected: u64,
        got: u64,
        tick: u32,
        frame_tick: u32,
    },
    #[error("chunk payload too large: {bytes} bytes (limit {limit})")]
    ChunkPayloadTooLarge { bytes: usize, limit: usize },
    #[error("invalid magic bytes")]
    InvalidMagic,
    #[error("unsupported format version {version}")]
    UnsupportedVersion { version: u16 },
    #[error("non-finite floating point value encountered")]
    NonFiniteValue,
    #[error("invalid roster index {idx} (roster_count={roster_count})")]
    InvalidRosterIndex { idx: u8, roster_count: u8 },
    #[error("invalid empty slot payload at slot {slot}")]
    InvalidEmptySlot { slot: u8 },
    #[error("checksum or hash mismatch")]
    ChecksumMismatch,
    #[error("manifest or header size {bytes} exceeds limit {limit}")]
    ManifestTooLarge { bytes: usize, limit: usize },
    #[error("invalid field value for {field}")]
    InvalidFieldValue { field: &'static str },
}

/// Deterministic canonical little-endian serialization following §2.2 8 closed rules.
pub trait CanonicalLeBytes: Sized {
    fn write_canonical_le(&self, out: &mut Vec<u8>);
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError>;

    fn to_canonical_le_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_canonical_le(&mut out);
        out
    }

    fn from_canonical_le_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut reader = CanonicalReader::new(bytes);
        let val = Self::read_canonical_le(&mut reader)?;
        reader.ensure_eof()?;
        Ok(val)
    }
}

/// Cursor over a byte slice tracking exact offset for `DecodeError`.
pub struct CanonicalReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> CanonicalReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    pub fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        if self.pos.saturating_add(len) > self.buf.len() {
            return Err(DecodeError::UnexpectedEof { offset: self.pos });
        }
        let slice = &self.buf[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    pub fn read_u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.read_bytes(1)?[0])
    }

    pub fn read_i8(&mut self) -> Result<i8, DecodeError> {
        Ok(self.read_u8()? as i8)
    }

    pub fn read_zero_pad(&mut self, len: usize) -> Result<(), DecodeError> {
        let start = self.pos;
        let bytes = self.read_bytes(len)?;
        for (i, &b) in bytes.iter().enumerate() {
            if b != 0 {
                return Err(DecodeError::NonZeroPadding {
                    offset: start + i,
                    value: b,
                });
            }
        }
        Ok(())
    }

    pub fn ensure_eof(&self) -> Result<(), DecodeError> {
        if self.pos < self.buf.len() {
            return Err(DecodeError::TrailingBytes {
                count: self.buf.len() - self.pos,
            });
        }
        Ok(())
    }
}

/// Normalize `f32` per Rule 2: `-0.0 -> +0.0`, any NaN -> `0x7fc0_0000`.
#[inline]
pub fn canonicalize_f32(v: f32) -> u32 {
    if v.is_nan() {
        0x7fc0_0000_u32
    } else if v == 0.0 {
        0.0_f32.to_bits()
    } else {
        v.to_bits()
    }
}

/// Normalize `f64` per Rule 2: `-0.0 -> +0.0`, any NaN -> `0x7ff8_0000_0000_0000`.
#[inline]
pub fn canonicalize_f64(v: f64) -> u64 {
    if v.is_nan() {
        0x7ff8_0000_0000_0000_u64
    } else if v == 0.0 {
        0.0_f64.to_bits()
    } else {
        v.to_bits()
    }
}

impl CanonicalLeBytes for bool {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.push(if *self { 1 } else { 0 });
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            v => Err(DecodeError::InvalidEnumValue {
                field: "bool",
                value: v as u32,
            }),
        }
    }
}

macro_rules! impl_canonical_int {
    ($($t:ty, $n:expr);+ $(;)?) => {
        $(
            impl CanonicalLeBytes for $t {
                fn write_canonical_le(&self, out: &mut Vec<u8>) {
                    out.extend_from_slice(&self.to_le_bytes());
                }
                fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
                    let b = reader.read_bytes($n)?;
                    let mut arr = [0u8; $n];
                    arr.copy_from_slice(b);
                    Ok(<$t>::from_le_bytes(arr))
                }
            }
        )+
    };
}

impl_canonical_int!(
    u8, 1;
    i8, 1;
    u16, 2;
    i16, 2;
    u32, 4;
    i32, 4;
    u64, 8;
    i64, 8;
);

impl CanonicalLeBytes for f32 {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&canonicalize_f32(*self).to_le_bytes());
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let bits = u32::read_canonical_le(reader)?;
        let v = f32::from_bits(bits);
        if !v.is_finite() {
            return Err(DecodeError::NonFiniteValue);
        }
        Ok(v)
    }
}

impl CanonicalLeBytes for f64 {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&canonicalize_f64(*self).to_le_bytes());
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let bits = u64::read_canonical_le(reader)?;
        let v = f64::from_bits(bits);
        if !v.is_finite() {
            return Err(DecodeError::NonFiniteValue);
        }
        Ok(v)
    }
}

impl<T: CanonicalLeBytes> CanonicalLeBytes for Option<T> {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        match self {
            None => out.push(0x00),
            Some(v) => {
                out.push(0x01);
                v.write_canonical_le(out);
            }
        }
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        match reader.read_u8()? {
            0 => Ok(None),
            1 => Ok(Some(T::read_canonical_le(reader)?)),
            v => Err(DecodeError::InvalidEnumValue {
                field: "Option",
                value: v as u32,
            }),
        }
    }
}

impl<T: CanonicalLeBytes, const N: usize> CanonicalLeBytes for [T; N] {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        for elem in self.iter() {
            elem.write_canonical_le(out);
        }
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let mut vec = Vec::with_capacity(N);
        for _ in 0..N {
            vec.push(T::read_canonical_le(reader)?);
        }
        vec.try_into().map_err(|_| DecodeError::InvalidFieldValue {
            field: "fixed_array",
        })
    }
}

impl<T: CanonicalLeBytes> CanonicalLeBytes for Vec<T> {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        (self.len() as u32).write_canonical_le(out);
        for elem in self {
            elem.write_canonical_le(out);
        }
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let len = u32::read_canonical_le(reader)? as usize;
        if len > 1_048_576 {
            return Err(DecodeError::ManifestTooLarge {
                bytes: len,
                limit: 1_048_576,
            });
        }
        let mut vec = Vec::with_capacity(len.min(4096));
        for _ in 0..len {
            vec.push(T::read_canonical_le(reader)?);
        }
        Ok(vec)
    }
}

impl CanonicalLeBytes for String {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        let bytes = self.as_bytes();
        (bytes.len() as u32).write_canonical_le(out);
        out.extend_from_slice(bytes);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let len = u32::read_canonical_le(reader)? as usize;
        if len > 65_536 {
            return Err(DecodeError::ManifestTooLarge {
                bytes: len,
                limit: 65_536,
            });
        }
        let bytes = reader.read_bytes(len)?;
        let s = core::str::from_utf8(bytes).map_err(|_| DecodeError::InvalidUtf8)?;
        if s.as_bytes().contains(&0) {
            return Err(DecodeError::IdLengthOutOfRange { len });
        }
        Ok(s.to_owned())
    }
}

impl<K: CanonicalLeBytes + Ord, V: CanonicalLeBytes> CanonicalLeBytes for BTreeMap<K, V> {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        let mut encoded_entries: Vec<(Vec<u8>, &V)> = Vec::with_capacity(self.len());
        for (k, v) in self.iter() {
            encoded_entries.push((k.to_canonical_le_bytes(), v));
        }
        encoded_entries.sort_by(|a, b| a.0.cmp(&b.0));
        (encoded_entries.len() as u32).write_canonical_le(out);
        for (kb, v) in encoded_entries {
            out.extend_from_slice(&kb);
            v.write_canonical_le(out);
        }
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let len = u32::read_canonical_le(reader)? as usize;
        let mut map = BTreeMap::new();
        for _ in 0..len {
            let k = K::read_canonical_le(reader)?;
            let v = V::read_canonical_le(reader)?;
            map.insert(k, v);
        }
        Ok(map)
    }
}

impl CanonicalLeBytes for () {
    fn write_canonical_le(&self, _out: &mut Vec<u8>) {}
    fn read_canonical_le(_reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok(())
    }
}

impl<A: CanonicalLeBytes, B: CanonicalLeBytes> CanonicalLeBytes for (A, B) {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.0.write_canonical_le(out);
        self.1.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok((
            A::read_canonical_le(reader)?,
            B::read_canonical_le(reader)?,
        ))
    }
}

impl<A: CanonicalLeBytes, B: CanonicalLeBytes, C: CanonicalLeBytes> CanonicalLeBytes for (A, B, C) {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.0.write_canonical_le(out);
        self.1.write_canonical_le(out);
        self.2.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok((
            A::read_canonical_le(reader)?,
            B::read_canonical_le(reader)?,
            C::read_canonical_le(reader)?,
        ))
    }
}

impl<A: CanonicalLeBytes, B: CanonicalLeBytes, C: CanonicalLeBytes, D: CanonicalLeBytes>
    CanonicalLeBytes for (A, B, C, D)
{
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.0.write_canonical_le(out);
        self.1.write_canonical_le(out);
        self.2.write_canonical_le(out);
        self.3.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok((
            A::read_canonical_le(reader)?,
            B::read_canonical_le(reader)?,
            C::read_canonical_le(reader)?,
            D::read_canonical_le(reader)?,
        ))
    }
}

impl<
        A: CanonicalLeBytes,
        B: CanonicalLeBytes,
        C: CanonicalLeBytes,
        D: CanonicalLeBytes,
        E: CanonicalLeBytes,
        F: CanonicalLeBytes,
        G: CanonicalLeBytes,
    > CanonicalLeBytes for (A, B, C, D, E, F, G)
{
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        self.0.write_canonical_le(out);
        self.1.write_canonical_le(out);
        self.2.write_canonical_le(out);
        self.3.write_canonical_le(out);
        self.4.write_canonical_le(out);
        self.5.write_canonical_le(out);
        self.6.write_canonical_le(out);
    }
    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        Ok((
            A::read_canonical_le(reader)?,
            B::read_canonical_le(reader)?,
            C::read_canonical_le(reader)?,
            D::read_canonical_le(reader)?,
            E::read_canonical_le(reader)?,
            F::read_canonical_le(reader)?,
            G::read_canonical_le(reader)?,
        ))
    }
}

/// Validate a 1..=64 byte UTF-8 identifier string with no NUL byte.
pub fn validate_id_string(s: &str) -> Result<(), DecodeError> {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 || bytes.contains(&0) {
        return Err(DecodeError::IdLengthOutOfRange { len: bytes.len() });
    }
    Ok(())
}
