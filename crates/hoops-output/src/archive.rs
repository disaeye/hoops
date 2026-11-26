use std::io::Write;

use hoops_core::{
    CanonicalLeBytes, CanonicalReader, DecodeError, GameRecordEventV1, PlayerFrameSlotV1,
    PublicFrameV1,
};

use crate::error::OutputError;
use crate::header::Hps1Header;

pub const ARCHIVE_MAGIC: [u8; 4] = *b"HSAD";
pub const CHUNK_MAGIC: [u8; 4] = *b"HCK1";
pub const CHUNK_VERSION: u16 = 1;
pub const SEEK_MAGIC: [u8; 4] = *b"SKX1";
pub const FOOTER_END_MAGIC: [u8; 4] = *b"END1";
pub const CHUNK_MAX_TICKS: usize = 600;
pub const MAX_CHUNK_UNCOMPRESSED_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkSeekEntry {
    pub start_tick: u32,
    pub tick_count: u16,
    pub first_event_seq: u64,
    pub last_event_seq: u64,
    pub file_offset: u64,
    pub chunk_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoaArchiveDecoded {
    pub header: Hps1Header,
    pub frames: Vec<PublicFrameV1>,
    pub events: Vec<GameRecordEventV1>,
    pub seek_index: Vec<ChunkSeekEntry>,
}

fn encode_chunk_raw_soa(
    frames: &[PublicFrameV1],
    events: &[GameRecordEventV1],
) -> Vec<u8> {
    let n = frames.len();
    let mut raw = Vec::with_capacity(n * 124 + events.len() * 48 + 32);
    // Tick sequence base + deltas (allows checking sequential continuity on decode)
    frames[0].tick.write_canonical_le(&mut raw);
    for i in 1..n {
        let delta = frames[i].tick.wrapping_sub(frames[i - 1].tick);
        delta.write_canonical_le(&mut raw);
    }
    // Frame scalar columns SoA
    for f in frames {
        raw.push(f.quarter);
    }
    // Delta-encoded i16 clock columns
    frames[0].game_clock_ds.write_canonical_le(&mut raw);
    for i in 1..n {
        let d = (frames[i].game_clock_ds as i16).wrapping_sub(frames[i - 1].game_clock_ds as i16);
        d.write_canonical_le(&mut raw);
    }
    for f in frames {
        f.shot_clock_ds.write_canonical_le(&mut raw);
    }
    for f in frames {
        raw.push(f.game_phase);
    }
    for f in frames {
        raw.push(f.possession_side);
    }
    frames[0].score_home.write_canonical_le(&mut raw);
    for i in 1..n {
        let d = frames[i].score_home.wrapping_sub(frames[i - 1].score_home);
        d.write_canonical_le(&mut raw);
    }
    frames[0].score_away.write_canonical_le(&mut raw);
    for i in 1..n {
        let d = frames[i].score_away.wrapping_sub(frames[i - 1].score_away);
        d.write_canonical_le(&mut raw);
    }
    // Ball delta-encoded columns
    frames[0].ball_x_mm.write_canonical_le(&mut raw);
    for i in 1..n {
        frames[i].ball_x_mm.wrapping_sub(frames[i - 1].ball_x_mm).write_canonical_le(&mut raw);
    }
    frames[0].ball_y_mm.write_canonical_le(&mut raw);
    for i in 1..n {
        frames[i].ball_y_mm.wrapping_sub(frames[i - 1].ball_y_mm).write_canonical_le(&mut raw);
    }
    frames[0].ball_z_mm.write_canonical_le(&mut raw);
    for i in 1..n {
        frames[i].ball_z_mm.wrapping_sub(frames[i - 1].ball_z_mm).write_canonical_le(&mut raw);
    }
    for f in frames {
        raw.push(f.ball_state);
    }
    for f in frames {
        raw.push(f.ball_owner_slot as u8);
    }
    for f in frames {
        raw.push(f.ball_flags & 0x7F);
    }

    // Player slots 0..10 SoA columns
    for s in 0..10 {
        for f in frames {
            raw.push(f.players[s].roster_idx);
        }
        frames[0].players[s].x_mm.write_canonical_le(&mut raw);
        for i in 1..n {
            frames[i].players[s]
                .x_mm
                .wrapping_sub(frames[i - 1].players[s].x_mm)
                .write_canonical_le(&mut raw);
        }
        frames[0].players[s].y_mm.write_canonical_le(&mut raw);
        for i in 1..n {
            frames[i].players[s]
                .y_mm
                .wrapping_sub(frames[i - 1].players[s].y_mm)
                .write_canonical_le(&mut raw);
        }
        for f in frames {
            raw.push(f.players[s].vx_dm_s as u8);
        }
        for f in frames {
            raw.push(f.players[s].vy_dm_s as u8);
        }
        for f in frames {
            raw.push(f.players[s].facing_bin);
        }
        for f in frames {
            raw.push(f.players[s].visible_action);
        }
        for f in frames {
            raw.push(f.players[s].public_flags & 0x7F);
        }
    }

    // Events at end of chunk raw stream
    for ev in events {
        ev.write_le(&mut raw);
    }
    raw
}

fn decode_chunk_raw_soa(
    raw: &[u8],
    tick_count: usize,
    event_count: usize,
    expected_start_tick: u32,
    roster_count: u8,
) -> Result<(Vec<PublicFrameV1>, Vec<GameRecordEventV1>), DecodeError> {
    if tick_count == 0 {
        return Err(DecodeError::InvalidFieldValue {
            field: "Hck1Chunk.tick_count",
        });
    }
    let mut r = CanonicalReader::new(raw);
    let mut ticks = Vec::with_capacity(tick_count);
    let t0 = u32::read_canonical_le(&mut r)?;
    if t0 != expected_start_tick {
        return Err(DecodeError::FrameSequenceGap {
            expected: expected_start_tick,
            got: t0,
        });
    }
    ticks.push(t0);
    for i in 1..tick_count {
        let delta = u32::read_canonical_le(&mut r)?;
        let cur = ticks[i - 1].wrapping_add(delta);
        if cur != ticks[i - 1] + 1 {
            return Err(DecodeError::FrameSequenceGap {
                expected: ticks[i - 1] + 1,
                got: cur,
            });
        }
        ticks.push(cur);
    }

    let quarters = r.read_bytes(tick_count)?.to_vec();

    let mut game_clock_ds = Vec::with_capacity(tick_count);
    game_clock_ds.push(u16::read_canonical_le(&mut r)?);
    for i in 1..tick_count {
        let d = i16::read_canonical_le(&mut r)?;
        game_clock_ds.push((game_clock_ds[i - 1] as i16).wrapping_add(d) as u16);
    }

    let mut shot_clock_ds = Vec::with_capacity(tick_count);
    for _ in 0..tick_count {
        let sc = u16::read_canonical_le(&mut r)?;
        let _ = hoops_core::decode_shot_clock_opt(sc)?;
        shot_clock_ds.push(sc);
    }

    let game_phases = r.read_bytes(tick_count)?.to_vec();
    for &gp in &game_phases {
        if gp > 10 {
            return Err(DecodeError::InvalidEnumValue {
                field: "PublicFrameV1.game_phase",
                value: gp as u32,
            });
        }
    }
    let poss_sides = r.read_bytes(tick_count)?.to_vec();
    for &ps in &poss_sides {
        if ps > 2 {
            return Err(DecodeError::InvalidEnumValue {
                field: "PublicFrameV1.possession_side",
                value: ps as u32,
            });
        }
    }

    let mut score_home = Vec::with_capacity(tick_count);
    score_home.push(u16::read_canonical_le(&mut r)?);
    for i in 1..tick_count {
        score_home.push(score_home[i - 1].wrapping_add(u16::read_canonical_le(&mut r)?));
    }
    let mut score_away = Vec::with_capacity(tick_count);
    score_away.push(u16::read_canonical_le(&mut r)?);
    for i in 1..tick_count {
        score_away.push(score_away[i - 1].wrapping_add(u16::read_canonical_le(&mut r)?));
    }

    let mut bx = Vec::with_capacity(tick_count);
    bx.push(i16::read_canonical_le(&mut r)?);
    for i in 1..tick_count {
        bx.push(bx[i - 1].wrapping_add(i16::read_canonical_le(&mut r)?));
    }
    let mut by = Vec::with_capacity(tick_count);
    by.push(i16::read_canonical_le(&mut r)?);
    for i in 1..tick_count {
        by.push(by[i - 1].wrapping_add(i16::read_canonical_le(&mut r)?));
    }
    let mut bz = Vec::with_capacity(tick_count);
    bz.push(i16::read_canonical_le(&mut r)?);
    for i in 1..tick_count {
        bz.push(bz[i - 1].wrapping_add(i16::read_canonical_le(&mut r)?));
    }

    let bstate = r.read_bytes(tick_count)?.to_vec();
    for &bs in &bstate {
        if bs > 5 {
            return Err(DecodeError::InvalidEnumValue {
                field: "PublicFrameV1.ball_state",
                value: bs as u32,
            });
        }
    }
    let bowner = r.read_bytes(tick_count)?.to_vec();
    for &bo in &bowner {
        let s = bo as i8;
        if !(-1..=9).contains(&s) {
            return Err(DecodeError::InvalidEnumValue {
                field: "PublicFrameV1.ball_owner_slot",
                value: s as i32 as u32,
            });
        }
    }
    let bflags_pos = r.position();
    let bflags = r.read_bytes(tick_count)?.to_vec();
    for (idx, &bf) in bflags.iter().enumerate() {
        if (bf & 0x80) != 0 {
            return Err(DecodeError::NonZeroPadding {
                offset: bflags_pos + idx,
                value: bf,
            });
        }
    }

    let mut frames = Vec::with_capacity(tick_count);
    for i in 0..tick_count {
        frames.push(PublicFrameV1 {
            tick: ticks[i],
            quarter: quarters[i],
            game_clock_ds: game_clock_ds[i],
            shot_clock_ds: shot_clock_ds[i],
            game_phase: game_phases[i],
            possession_side: poss_sides[i],
            score_home: score_home[i],
            score_away: score_away[i],
            ball_x_mm: bx[i],
            ball_y_mm: by[i],
            ball_z_mm: bz[i],
            ball_state: bstate[i],
            ball_owner_slot: bowner[i] as i8,
            ball_flags: bflags[i],
            players: [PlayerFrameSlotV1::inactive(); 10],
        });
    }

    for s in 0..10 {
        let r_indices = r.read_bytes(tick_count)?.to_vec();
        let mut xs = Vec::with_capacity(tick_count);
        xs.push(i16::read_canonical_le(&mut r)?);
        for i in 1..tick_count {
            xs.push(xs[i - 1].wrapping_add(i16::read_canonical_le(&mut r)?));
        }
        let mut ys = Vec::with_capacity(tick_count);
        ys.push(i16::read_canonical_le(&mut r)?);
        for i in 1..tick_count {
            ys.push(ys[i - 1].wrapping_add(i16::read_canonical_le(&mut r)?));
        }
        let vxs = r.read_bytes(tick_count)?.to_vec();
        let vys = r.read_bytes(tick_count)?.to_vec();
        let facings = r.read_bytes(tick_count)?.to_vec();
        let actions = r.read_bytes(tick_count)?.to_vec();
        let pflags_pos = r.position();
        let pflags = r.read_bytes(tick_count)?.to_vec();

        for i in 0..tick_count {
            let ri = r_indices[i];
            if ri != 0xFF && ri >= roster_count {
                return Err(DecodeError::InvalidRosterIndex {
                    idx: ri,
                    roster_count,
                });
            }
            if (pflags[i] & 0x80) != 0 {
                return Err(DecodeError::NonZeroPadding {
                    offset: pflags_pos + i,
                    value: pflags[i],
                });
            }
            if actions[i] > 19 {
                return Err(DecodeError::InvalidEnumValue {
                    field: "PlayerFrameSlotV1.visible_action",
                    value: actions[i] as u32,
                });
            }
            frames[i].players[s] = PlayerFrameSlotV1 {
                roster_idx: ri,
                x_mm: xs[i],
                y_mm: ys[i],
                vx_dm_s: vxs[i] as i8,
                vy_dm_s: vys[i] as i8,
                facing_bin: facings[i],
                visible_action: actions[i],
                public_flags: pflags[i],
            };
        }
    }

    let mut events = Vec::with_capacity(event_count);
    for _ in 0..event_count {
        events.push(GameRecordEventV1::read_le(&mut r)?);
    }
    r.ensure_eof()?;
    Ok((frames, events))
}

/// Encode full game into `SoADeltaZstd600` container with Zstd chunks & half-split guard (`MAX_CHUNK_UNCOMPRESSED_BYTES`).
pub fn encode_soa_zstd_archive(
    header: &Hps1Header,
    frames: &[PublicFrameV1],
    events: &[GameRecordEventV1],
    zstd_level: i32,
) -> Result<Vec<u8>, OutputError> {
    encode_soa_zstd_archive_with_limit(
        header,
        frames,
        events,
        zstd_level,
        MAX_CHUNK_UNCOMPRESSED_BYTES,
    )
}

pub fn encode_soa_zstd_archive_with_limit(
    header: &Hps1Header,
    frames: &[PublicFrameV1],
    events: &[GameRecordEventV1],
    zstd_level: i32,
    max_raw_chunk_bytes: usize,
) -> Result<Vec<u8>, OutputError> {
    let hdr_bytes = header.to_bytes();
    let mut out = Vec::with_capacity(hdr_bytes.len() + frames.len() * 64 + events.len() * 32 + 128);
    out.extend_from_slice(&ARCHIVE_MAGIC);
    out.extend_from_slice(&1u16.to_le_bytes()); // archive version
    out.extend_from_slice(&[0u8; 2]); // reserved
    (hdr_bytes.len() as u32).write_canonical_le(&mut out);
    out.extend_from_slice(&hdr_bytes);

    let mut seek_entries = Vec::new();
    let mut idx = 0usize;
    let mut chunk_size = CHUNK_MAX_TICKS;

    while idx < frames.len() {
        let end = (idx + chunk_size).min(frames.len());
        let f_slice = &frames[idx..end];
        let t_start = f_slice[0].tick;
        let t_end = f_slice[f_slice.len() - 1].tick;
        let e_slice: Vec<GameRecordEventV1> = events
            .iter()
            .filter(|e| e.tick >= t_start && e.tick <= t_end)
            .cloned()
            .collect();

        let raw = encode_chunk_raw_soa(f_slice, &e_slice);
        if raw.len() > max_raw_chunk_bytes {
            if f_slice.len() > 1 {
                chunk_size = (f_slice.len() / 2).max(1);
                continue;
            }
            return Err(OutputError::Decode(DecodeError::ChunkPayloadTooLarge {
                bytes: raw.len(),
                limit: max_raw_chunk_bytes,
            }));
        }

        let compressed = zstd::bulk::compress(&raw, zstd_level)
            .map_err(|e| OutputError::Zstd(e.to_string()))?;

        let offset = out.len() as u64;
        out.extend_from_slice(&CHUNK_MAGIC);
        out.extend_from_slice(&CHUNK_VERSION.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // flags: bit0=1 => zstd compressed
        t_start.write_canonical_le(&mut out);
        (f_slice.len() as u16).write_canonical_le(&mut out);
        (e_slice.len() as u16).write_canonical_le(&mut out);
        (raw.len() as u32).write_canonical_le(&mut out);
        (compressed.len() as u32).write_canonical_le(&mut out);
        out.extend_from_slice(&compressed);

        let chunk_bytes = (out.len() as u64 - offset) as u32;
        let first_es = e_slice.first().map_or(0, |e| e.event_seq);
        let last_es = e_slice.last().map_or(0, |e| e.event_seq);
        seek_entries.push(ChunkSeekEntry {
            start_tick: t_start,
            tick_count: f_slice.len() as u16,
            first_event_seq: first_es,
            last_event_seq: last_es,
            file_offset: offset,
            chunk_bytes,
        });

        idx = end;
        chunk_size = CHUNK_MAX_TICKS;
    }

    // Write SeekIndexFooterV1
    let footer_start = out.len() as u64;
    out.extend_from_slice(&SEEK_MAGIC);
    (seek_entries.len() as u32).write_canonical_le(&mut out);
    for se in &seek_entries {
        se.start_tick.write_canonical_le(&mut out);
        se.tick_count.write_canonical_le(&mut out);
        se.first_event_seq.write_canonical_le(&mut out);
        se.last_event_seq.write_canonical_le(&mut out);
        se.file_offset.write_canonical_le(&mut out);
        se.chunk_bytes.write_canonical_le(&mut out);
    }
    footer_start.write_canonical_le(&mut out);
    out.extend_from_slice(&FOOTER_END_MAGIC);
    Ok(out)
}

/// 增量拼装完整 `SoADeltaZstd600` 归档：按 SeekIndex 顺序转写已封口分块归档中的压缩分块，全程只驻留当前分块
pub struct SoaArchiveAssembler<'a> {
    writer: &'a mut dyn Write,
    offset: u64,
    seek_entries: Vec<ChunkSeekEntry>,
    last_tick: Option<u32>,
}

impl<'a> SoaArchiveAssembler<'a> {
    pub fn new(header: &Hps1Header, writer: &'a mut dyn Write) -> Result<Self, OutputError> {
        let hdr_bytes = header.to_bytes();
        let mut head = Vec::with_capacity(12 + hdr_bytes.len());
        head.extend_from_slice(&ARCHIVE_MAGIC);
        head.extend_from_slice(&1u16.to_le_bytes());
        head.extend_from_slice(&[0u8; 2]);
        (hdr_bytes.len() as u32).write_canonical_le(&mut head);
        head.extend_from_slice(&hdr_bytes);
        writer
            .write_all(&head)
            .map_err(|e| OutputError::Io(e.to_string()))?;
        Ok(Self {
            writer,
            offset: head.len() as u64,
            seek_entries: Vec::new(),
            last_tick: None,
        })
    }

    /// 分块必须按 tick 严格递增且互不重叠；允许因落盘失败留下缺口
    pub fn append_chunk_archive(&mut self, chunk_archive: &[u8]) -> Result<(), OutputError> {
        let (_, entries, _) = parse_seek_index(chunk_archive)?;
        for entry in entries {
            let last_tick = entry
                .start_tick
                .saturating_add(u32::from(entry.tick_count).saturating_sub(1));
            if self.last_tick.is_some_and(|prev| entry.start_tick <= prev) {
                return Err(OutputError::Decode(DecodeError::FrameSequenceGap {
                    expected: self.last_tick.unwrap_or(0).saturating_add(1),
                    got: entry.start_tick,
                }));
            }
            let start = entry.file_offset as usize;
            let end = start.saturating_add(entry.chunk_bytes as usize);
            let record = chunk_archive
                .get(start..end)
                .ok_or(DecodeError::UnexpectedEof { offset: end })?;
            self.writer
                .write_all(record)
                .map_err(|e| OutputError::Io(e.to_string()))?;
            let mut relocated = entry.clone();
            relocated.file_offset = self.offset;
            self.offset += record.len() as u64;
            self.seek_entries.push(relocated);
            self.last_tick = Some(last_tick);
        }
        Ok(())
    }

    pub fn finish(self) -> Result<(), OutputError> {
        let footer_start = self.offset;
        let mut footer = Vec::with_capacity(16 + self.seek_entries.len() * 30);
        footer.extend_from_slice(&SEEK_MAGIC);
        (self.seek_entries.len() as u32).write_canonical_le(&mut footer);
        for se in &self.seek_entries {
            se.start_tick.write_canonical_le(&mut footer);
            se.tick_count.write_canonical_le(&mut footer);
            se.first_event_seq.write_canonical_le(&mut footer);
            se.last_event_seq.write_canonical_le(&mut footer);
            se.file_offset.write_canonical_le(&mut footer);
            se.chunk_bytes.write_canonical_le(&mut footer);
        }
        footer_start.write_canonical_le(&mut footer);
        footer.extend_from_slice(&FOOTER_END_MAGIC);
        self.writer
            .write_all(&footer)
            .map_err(|e| OutputError::Io(e.to_string()))?;
        Ok(())
    }
}

pub fn parse_seek_index(bytes: &[u8]) -> Result<(Hps1Header, Vec<ChunkSeekEntry>, usize), OutputError> {
    if bytes.len() < 24 {
        return Err(OutputError::Decode(DecodeError::UnexpectedEof {
            offset: bytes.len(),
        }));
    }
    if &bytes[0..4] != ARCHIVE_MAGIC {
        return Err(OutputError::Decode(DecodeError::InvalidMagic));
    }
    let ver = u16::from_le_bytes([bytes[4], bytes[5]]);
    if ver != 1 {
        return Err(OutputError::Decode(DecodeError::UnsupportedVersion {
            version: ver,
        }));
    }
    if bytes[6] != 0 || bytes[7] != 0 {
        return Err(OutputError::Decode(DecodeError::NonZeroPadding {
            offset: 6,
            value: bytes[6],
        }));
    }
    let hdr_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    if 12 + hdr_len > bytes.len() {
        return Err(OutputError::Decode(DecodeError::UnexpectedEof {
            offset: bytes.len(),
        }));
    }
    let header = Hps1Header::from_bytes(&bytes[12..12 + hdr_len])?;

    let end_len = bytes.len();
    if &bytes[end_len - 4..end_len] != FOOTER_END_MAGIC {
        return Err(OutputError::Decode(DecodeError::InvalidMagic));
    }
    let footer_start = u64::from_le_bytes(
        bytes[end_len - 12..end_len - 4]
            .try_into()
            .unwrap(),
    ) as usize;
    if footer_start < 12 + hdr_len || footer_start >= end_len - 12 {
        return Err(OutputError::Decode(DecodeError::InvalidFieldValue {
            field: "SeekIndexFooterV1.footer_start",
        }));
    }
    let mut fr = CanonicalReader::new(&bytes[footer_start..end_len - 12]);
    let sm = fr.read_bytes(4)?;
    if sm != SEEK_MAGIC {
        return Err(OutputError::Decode(DecodeError::InvalidMagic));
    }
    let num_chunks = u32::read_canonical_le(&mut fr)? as usize;
    let mut entries = Vec::with_capacity(num_chunks);
    for _ in 0..num_chunks {
        entries.push(ChunkSeekEntry {
            start_tick: u32::read_canonical_le(&mut fr)?,
            tick_count: u16::read_canonical_le(&mut fr)?,
            first_event_seq: u64::read_canonical_le(&mut fr)?,
            last_event_seq: u64::read_canonical_le(&mut fr)?,
            file_offset: u64::read_canonical_le(&mut fr)?,
            chunk_bytes: u32::read_canonical_le(&mut fr)?,
        });
    }
    fr.ensure_eof()?;
    Ok((header, entries, footer_start))
}

pub fn decode_soa_zstd_archive(bytes: &[u8]) -> Result<SoaArchiveDecoded, OutputError> {
    let decoded = decode_soa_zstd_archive_range(bytes, 0, u32::MAX)?;
    if let Some(first) = decoded.events.first().filter(|event| event.event_seq != 1) {
        return Err(OutputError::Decode(DecodeError::EventSequenceGap {
            expected: 1,
            got: first.event_seq,
            tick: first.tick,
            frame_tick: first.tick,
        }));
    }
    Ok(decoded)
}

pub fn decode_soa_zstd_archive_range(
    bytes: &[u8],
    from_tick: u32,
    to_tick: u32,
) -> Result<SoaArchiveDecoded, OutputError> {
    let (header, seek_index, _) = parse_seek_index(bytes)?;
    let mut all_frames = Vec::new();
    let mut all_events = Vec::new();

    for entry in &seek_index {
        let last_tick = entry.start_tick.saturating_add(u32::from(entry.tick_count).saturating_sub(1));
        if entry.start_tick > to_tick || last_tick < from_tick {
            continue;
        }
        let start = entry.file_offset as usize;
        let end = start + (entry.chunk_bytes as usize);
        if end > bytes.len() {
            return Err(OutputError::Decode(DecodeError::UnexpectedEof {
                offset: bytes.len(),
            }));
        }
        let mut cr = CanonicalReader::new(&bytes[start..end]);
        if cr.read_bytes(4)? != CHUNK_MAGIC {
            return Err(OutputError::Decode(DecodeError::InvalidMagic));
        }
        let cver = u16::read_canonical_le(&mut cr)?;
        if cver != CHUNK_VERSION {
            return Err(OutputError::Decode(DecodeError::UnsupportedVersion {
                version: cver,
            }));
        }
        let flags = u16::read_canonical_le(&mut cr)?;
        if (flags & !1) != 0 {
            return Err(OutputError::Decode(DecodeError::NonZeroPadding {
                offset: start + 6,
                value: (flags >> 8) as u8,
            }));
        }
        let start_tick = u32::read_canonical_le(&mut cr)?;
        let tick_count = u16::read_canonical_le(&mut cr)? as usize;
        let event_count = u16::read_canonical_le(&mut cr)? as usize;
        let raw_len = u32::read_canonical_le(&mut cr)? as usize;
        if raw_len > MAX_CHUNK_UNCOMPRESSED_BYTES {
            return Err(OutputError::Decode(DecodeError::ChunkPayloadTooLarge {
                bytes: raw_len,
                limit: MAX_CHUNK_UNCOMPRESSED_BYTES,
            }));
        }
        let zlen = u32::read_canonical_le(&mut cr)? as usize;
        let payload = cr.read_bytes(zlen)?;
        cr.ensure_eof()?;

        let raw = if (flags & 1) != 0 {
            zstd::bulk::decompress(payload, raw_len)
                .map_err(|e| OutputError::Zstd(e.to_string()))?
        } else {
            payload.to_vec()
        };
        let (frames, events) =
            decode_chunk_raw_soa(&raw, tick_count, event_count, start_tick, header.roster_count)?;
        all_frames.extend(frames.into_iter().filter(|frame| (from_tick..=to_tick).contains(&frame.tick)));
        all_events.extend(events.into_iter().filter(|event| (from_tick..=to_tick).contains(&event.tick)));
    }

    let mut prev_seq = all_events.first().map_or(0, |event| event.event_seq.saturating_sub(1));
    for ev in &all_events {
        if ev.event_seq != prev_seq + 1 {
            return Err(OutputError::Decode(DecodeError::EventSequenceGap {
                expected: prev_seq + 1,
                got: ev.event_seq,
                tick: ev.tick,
                frame_tick: ev.tick,
            }));
        }
        prev_seq = ev.event_seq;
    }

    Ok(SoaArchiveDecoded {
        header,
        frames: all_frames,
        events: all_events,
        seek_index,
    })
}

/// Extract post-disconnect frames & events for Phase B reconnect (`from_tick`, `after_event_seq`).
pub fn extract_archive_slice(
    bytes: &[u8],
    from_tick: u32,
    after_event_seq: u64,
) -> Result<(Hps1Header, Vec<PublicFrameV1>, Vec<GameRecordEventV1>), OutputError> {
    let decoded = decode_soa_zstd_archive(bytes)?;
    let frames = decoded
        .frames
        .into_iter()
        .filter(|f| f.tick >= from_tick)
        .collect();
    let events = decoded
        .events
        .into_iter()
        .filter(|e| e.event_seq > after_event_seq)
        .collect();
    Ok((decoded.header, frames, events))
}
