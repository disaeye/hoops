use hoops_core::{
    CanonicalLeBytes, CanonicalReader, DebugOverlayFrameV1, DecodeError, GameRecordEventV1,
    PublicFrameV1,
};

pub const RAW_MAGIC: [u8; 4] = *b"HRW1";

pub fn write_raw_stream_header(out: &mut Vec<u8>, frame_count: u32, has_debug: bool, event_count: u32) {
    out.extend_from_slice(&RAW_MAGIC);
    out.extend_from_slice(&frame_count.to_le_bytes());
    out.push(u8::from(has_debug));
    out.extend_from_slice(&[0; 3]);
    out.extend_from_slice(&event_count.to_le_bytes());
}

pub fn encode_raw_stream(
    frames: &[PublicFrameV1],
    debug_frames: Option<&[DebugOverlayFrameV1]>,
    events: &[GameRecordEventV1],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + frames.len() * 130 + events.len() * 48);
    write_raw_stream_header(&mut out, frames.len() as u32, debug_frames.is_some(), events.len() as u32);
    for (i, f) in frames.iter().enumerate() {
        f.write_le(&mut out);
        if let Some(df) = debug_frames {
            df[i].write_le(&mut out);
        }
    }
    for ev in events {
        ev.write_le(&mut out);
    }
    out
}

pub fn decode_raw_stream(
    bytes: &[u8],
    roster_count: u8,
    play_dict_count: u16,
    def_rule_dict_count: u16,
) -> Result<
    (
        Vec<PublicFrameV1>,
        Option<Vec<DebugOverlayFrameV1>>,
        Vec<GameRecordEventV1>,
    ),
    DecodeError,
> {
    let mut r = CanonicalReader::new(bytes);
    let magic = r.read_bytes(4)?;
    if magic != RAW_MAGIC {
        return Err(DecodeError::InvalidMagic);
    }
    let frame_count = u32::read_canonical_le(&mut r)? as usize;
    let has_dbg = r.read_u8()?;
    if has_dbg > 1 {
        return Err(DecodeError::InvalidEnumValue {
            field: "RawStream.has_debug",
            value: has_dbg as u32,
        });
    }
    r.read_zero_pad(3)?;
    let event_count = u32::read_canonical_le(&mut r)? as usize;

    let mut frames: Vec<PublicFrameV1> = Vec::with_capacity(frame_count.min(200_000));
    let mut dbg = if has_dbg == 1 {
        Some(Vec::with_capacity(frame_count.min(200_000)))
    } else {
        None
    };

    for i in 0..frame_count {
        let pf = PublicFrameV1::read_le(&mut r, roster_count)?;
        if i == 0 && pf.tick != 0 {
            return Err(DecodeError::FrameSequenceGap {
                expected: 0,
                got: pf.tick,
            });
        } else if i > 0 && pf.tick != frames[i - 1].tick + 1 {
            return Err(DecodeError::FrameSequenceGap {
                expected: frames[i - 1].tick + 1,
                got: pf.tick,
            });
        }
        frames.push(pf);
        if let Some(ref mut dvec) = dbg {
            dvec.push(DebugOverlayFrameV1::read_le(
                &mut r,
                play_dict_count,
                def_rule_dict_count,
                &pf,
            )?);
        }
    }

    let mut events = Vec::with_capacity(event_count.min(200_000));
    let mut prev_seq = 0u64;
    for _ in 0..event_count {
        let ev = GameRecordEventV1::read_le(&mut r)?;
        if ev.event_seq != prev_seq + 1 {
            return Err(DecodeError::EventSequenceGap {
                expected: prev_seq + 1,
                got: ev.event_seq,
                tick: ev.tick,
                frame_tick: ev.tick,
            });
        }
        prev_seq = ev.event_seq;
        events.push(ev);
    }
    r.ensure_eof()?;
    Ok((frames, dbg, events))
}
