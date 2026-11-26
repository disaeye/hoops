// Pure ES Module Binary Wire Codec for Hoops Simulation Engine (v1)
// Mirrors hoops-core::wire_schema_v1 & hoops-output codecs byte-for-byte.

export const PUBLIC_FRAME_BYTES = 124;
export const DEBUG_FRAME_BYTES = 118;
export const EVENT_HEADER_BYTES = 20;
export const MAX_CHUNK_UNCOMPRESSED_BYTES = 16 * 1024 * 1024;

export class DecodeError extends Error {
  constructor(code, message, detail = {}) {
    super(`${code}: ${message}`);
    this.name = 'DecodeError';
    this.code = code;
    this.detail = detail;
  }
}

const textDecoder = new TextDecoder('utf-8', { fatal: true });
const asciiDecoder = new TextDecoder('ascii');

function toU8(input) {
  if (input instanceof Uint8Array) return input;
  if (input instanceof ArrayBuffer) return new Uint8Array(input);
  if (ArrayBuffer.isView(input)) {
    return new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
  }
  throw new TypeError('Expected Uint8Array, ArrayBuffer, or TypedArray');
}

function safeU64(big) {
  return big <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(big) : big;
}

export class CanonicalReader {
  constructor(bytes) {
    this.buf = toU8(bytes);
    this.dv = new DataView(this.buf.buffer, this.buf.byteOffset, this.buf.byteLength);
    this.pos = 0;
  }

  get remaining() {
    return this.buf.length - this.pos;
  }

  ensureAvailable(n) {
    if (this.pos + n > this.buf.length) {
      throw new DecodeError('UnexpectedEof', `unexpected end of input at offset ${this.pos}`, {
        offset: this.pos,
        needed: n,
        remaining: this.remaining,
      });
    }
  }

  readU8() {
    this.ensureAvailable(1);
    return this.buf[this.pos++];
  }

  readI8() {
    this.ensureAvailable(1);
    const v = this.dv.getInt8(this.pos);
    this.pos += 1;
    return v;
  }

  readU16() {
    this.ensureAvailable(2);
    const v = this.dv.getUint16(this.pos, true);
    this.pos += 2;
    return v;
  }

  readI16() {
    this.ensureAvailable(2);
    const v = this.dv.getInt16(this.pos, true);
    this.pos += 2;
    return v;
  }

  readU32() {
    this.ensureAvailable(4);
    const v = this.dv.getUint32(this.pos, true);
    this.pos += 4;
    return v;
  }

  readI32() {
    this.ensureAvailable(4);
    const v = this.dv.getInt32(this.pos, true);
    this.pos += 4;
    return v;
  }

  readU64BigInt() {
    this.ensureAvailable(8);
    const v = this.dv.getBigUint64(this.pos, true);
    this.pos += 8;
    return v;
  }

  readU64() {
    return safeU64(this.readU64BigInt());
  }

  readBytes(n) {
    this.ensureAvailable(n);
    const slice = this.buf.subarray(this.pos, this.pos + n);
    this.pos += n;
    return slice;
  }

  readAscii(n) {
    return asciiDecoder.decode(this.readBytes(n));
  }

  readZeroPad(n) {
    this.ensureAvailable(n);
    for (let i = 0; i < n; i++) {
      const b = this.buf[this.pos + i];
      if (b !== 0) {
        throw new DecodeError(
          'NonZeroPadding',
          `non-zero padding byte 0x${b.toString(16).padStart(2, '0')} at offset ${this.pos + i}`,
          { offset: this.pos + i, value: b },
        );
      }
    }
    this.pos += n;
  }

  readString() {
    const len = this.readU32();
    const bytes = this.readBytes(len);
    try {
      return textDecoder.decode(bytes);
    } catch {
      throw new DecodeError('InvalidUtf8', 'invalid UTF-8 string', { offset: this.pos - len });
    }
  }

  readIdString() {
    const s = this.readString();
    const byteLen = new TextEncoder().encode(s).length;
    if (byteLen < 1 || byteLen > 64 || s.includes('\0')) {
      throw new DecodeError('IdLengthOutOfRange', `identifier length ${byteLen} invalid`, {
        len: byteLen,
      });
    }
    return s;
  }

  ensureEof() {
    if (this.pos !== this.buf.length) {
      throw new DecodeError(
        'TrailingBytes',
        `trailing ${this.buf.length - this.pos} bytes after end of payload`,
        { count: this.buf.length - this.pos },
      );
    }
  }
}

export function validateShotClockOpt(sc) {
  if (sc <= 240 || sc === 0xffff) return sc;
  throw new DecodeError('InvalidEnumValue', `invalid shot_clock_ds ${sc}`, {
    field: 'shot_clock_ds',
    value: sc,
  });
}

export function validateTriStateShotClock(sc) {
  if (sc <= 240 || sc === 0xffff || sc === 0xfffe) return sc;
  throw new DecodeError('InvalidEnumValue', `invalid TriStateShotClock ${sc}`, {
    field: 'new_shot_clock_ds',
    value: sc,
  });
}

export function decodeHps1Header(input, { expectEof = true } = {}) {
  const r = input instanceof CanonicalReader ? input : new CanonicalReader(input);
  const magic = r.readAscii(4);
  if (magic !== 'HPS1') {
    throw new DecodeError('InvalidMagic', `expected HPS1, got ${magic}`);
  }
  const format_version = r.readU16();
  if (format_version !== 1) {
    throw new DecodeError('UnsupportedVersion', `unsupported HPS1 version ${format_version}`, {
      version: format_version,
    });
  }
  const visByte = r.readU8();
  if (visByte > 1) {
    throw new DecodeError('InvalidEnumValue', `invalid stream_visibility ${visByte}`, {
      field: 'Hps1Header.stream_visibility',
      value: visByte,
    });
  }
  const stream_visibility = visByte === 0 ? 'PublicOnly' : 'AuthoritativeFull';
  const roster_count = r.readU8();
  if (roster_count > 30) {
    throw new DecodeError('InvalidFieldValue', `roster_count ${roster_count} > 30`, {
      field: 'Hps1Header.roster_count',
    });
  }
  const frame_size_bytes = r.readU16();
  if (frame_size_bytes !== PUBLIC_FRAME_BYTES) {
    throw new DecodeError('InvalidFieldValue', `frame_size_bytes ${frame_size_bytes} != 124`, {
      field: 'Hps1Header.frame_size_bytes',
    });
  }
  const debug_frame_size_bytes = r.readU16();
  r.readZeroPad(4);
  const court_length_mm = r.readU32();
  const court_width_mm = r.readU32();
  const home_team_id = r.readIdString();
  const away_team_id = r.readIdString();

  const r_len = r.readU8();
  if (r_len > 30) {
    throw new DecodeError('InvalidFieldValue', `roster_len ${r_len} > 30`, {
      field: 'Hps1Header.roster_len',
    });
  }
  const roster = [];
  for (let i = 0; i < r_len; i++) {
    const roster_idx = r.readU8();
    if (roster_idx >= roster_count) {
      throw new DecodeError(
        'InvalidRosterIndex',
        `roster_idx ${roster_idx} >= roster_count ${roster_count}`,
        { idx: roster_idx, roster_count },
      );
    }
    const jersey_number_code = r.readU8();
    if (jersey_number_code > 100) {
      throw new DecodeError('InvalidEnumValue', `jersey_number_code ${jersey_number_code} > 100`, {
        field: 'jersey_number_code',
        value: jersey_number_code,
      });
    }
    const player_id = r.readIdString();
    const display_name = r.readIdString();
    roster.push({ roster_idx, jersey_number_code, player_id, display_name });
  }

  const play_len = r.readU32();
  const play_dict = [];
  for (let i = 0; i < play_len; i++) {
    play_dict.push([r.readU16(), r.readString()]);
  }
  const def_len = r.readU32();
  const def_dict = [];
  for (let i = 0; i < def_len; i++) {
    def_dict.push([r.readU16(), r.readString()]);
  }

  if (stream_visibility === 'PublicOnly' && (play_dict.length > 0 || def_dict.length > 0)) {
    throw new DecodeError(
      'PrivateDictInPublicStream',
      'private tactic/defense dictionaries found in PublicOnly HPS1 header',
    );
  }
  if (expectEof) {
    r.ensureEof();
  }

  return {
    format_version,
    stream_visibility,
    roster_count,
    frame_size_bytes,
    debug_frame_size_bytes,
    court_length_mm,
    court_width_mm,
    home_team_id,
    away_team_id,
    roster,
    play_dict,
    def_dict,
  };
}

export function decodePlayerFrameSlotV1(r, slotIdx, rosterCount = 30) {
  const roster_idx = r.readU8();
  const x_mm = r.readI16();
  const y_mm = r.readI16();
  const vx_dm_s = r.readI8();
  const vy_dm_s = r.readI8();
  const facing_bin = r.readU8();
  const visible_action = r.readU8();
  const flagsOff = r.pos;
  const public_flags = r.readU8();

  if (roster_idx === 0xff) {
    if (
      x_mm !== 0 ||
      y_mm !== 0 ||
      vx_dm_s !== 0 ||
      vy_dm_s !== 0 ||
      facing_bin !== 0 ||
      visible_action !== 0 ||
      public_flags !== 0
    ) {
      throw new DecodeError('InvalidEmptySlot', `dirty empty slot ${slotIdx}`, { slot: slotIdx });
    }
  } else {
    if (roster_idx >= rosterCount) {
      throw new DecodeError(
        'InvalidRosterIndex',
        `roster_idx ${roster_idx} >= roster_count ${rosterCount}`,
        { idx: roster_idx, roster_count: rosterCount },
      );
    }
    if (visible_action > 19) {
      throw new DecodeError('InvalidEnumValue', `visible_action ${visible_action}`, {
        field: 'PlayerFrameSlotV1.visible_action',
        value: visible_action,
      });
    }
    if ((public_flags & 0x80) !== 0) {
      throw new DecodeError('NonZeroPadding', `PlayerFrameSlotV1 public_flags bit7 set`, {
        offset: flagsOff,
        value: public_flags,
      });
    }
  }

  return {
    roster_idx,
    x_mm,
    y_mm,
    vx_dm_s,
    vy_dm_s,
    facing_bin,
    visible_action,
    public_flags,
  };
}

export function decodePublicFrameV1(input, rosterCount = 30, { expectEof = true } = {}) {
  const r = input instanceof CanonicalReader ? input : new CanonicalReader(input);
  const tick = r.readU32();
  const quarter = r.readU8();
  const game_clock_ds = r.readU16();
  const shot_clock_ds = validateShotClockOpt(r.readU16());
  const game_phase = r.readU8();
  if (game_phase > 10) {
    throw new DecodeError('InvalidEnumValue', `game_phase ${game_phase}`, {
      field: 'PublicFrameV1.game_phase',
      value: game_phase,
    });
  }
  const possession_side = r.readU8();
  if (possession_side > 2) {
    throw new DecodeError('InvalidEnumValue', `possession_side ${possession_side}`, {
      field: 'PublicFrameV1.possession_side',
      value: possession_side,
    });
  }
  const score_home = r.readU16();
  const score_away = r.readU16();
  const ball_x_mm = r.readI16();
  const ball_y_mm = r.readI16();
  const ball_z_mm = r.readI16();
  const ball_state = r.readU8();
  if (ball_state > 5) {
    throw new DecodeError('InvalidEnumValue', `ball_state ${ball_state}`, {
      field: 'PublicFrameV1.ball_state',
      value: ball_state,
    });
  }
  const ball_owner_slot = r.readI8();
  if (ball_owner_slot < -1 || ball_owner_slot > 9) {
    throw new DecodeError('InvalidEnumValue', `ball_owner_slot ${ball_owner_slot}`, {
      field: 'PublicFrameV1.ball_owner_slot',
      value: ball_owner_slot,
    });
  }
  const ballFlagsOff = r.pos;
  const ball_flags = r.readU8();
  if ((ball_flags & 0x80) !== 0) {
    throw new DecodeError('NonZeroPadding', `PublicFrameV1 ball_flags bit7 non-zero`, {
      offset: ballFlagsOff,
      value: ball_flags,
    });
  }
  const players = [];
  for (let s = 0; s < 10; s++) {
    players.push(decodePlayerFrameSlotV1(r, s, rosterCount));
  }
  if (expectEof) {
    r.ensureEof();
  }
  return {
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
  };
}

export function decodeDebugOverlayFrameV1(
  input,
  playDictCount,
  defDictCount,
  publicFrame,
  { expectEof = false } = {},
) {
  const r = input instanceof CanonicalReader ? input : new CanonicalReader(input);
  const tick = r.readU32();
  if (publicFrame && tick !== publicFrame.tick) {
    throw new DecodeError('FrameSequenceGap', `debug tick ${tick} != public tick ${publicFrame.tick}`, {
      expected: publicFrame.tick,
      got: tick,
    });
  }
  const active_play_idx = r.readU16();
  if (active_play_idx !== 0xffff && active_play_idx >= playDictCount) {
    throw new DecodeError('InvalidEnumValue', `active_play_idx ${active_play_idx}`, {
      field: 'active_play_idx',
      value: active_play_idx,
    });
  }
  const active_def_rule_idx = r.readU16();
  if (active_def_rule_idx !== 0xffff && active_def_rule_idx >= defDictCount) {
    throw new DecodeError('InvalidEnumValue', `active_def_rule_idx ${active_def_rule_idx}`, {
      field: 'active_def_rule_idx',
      value: active_def_rule_idx,
    });
  }
  const player_debug = [];
  for (let s = 0; s < 10; s++) {
    const target_x_cm = r.readI16();
    const target_y_cm = r.readI16();
    const defender_slot = r.readU8();
    const burst_energy = r.readU8();
    const cum_stamina = r.readU8();
    const rhythm_quant = r.readI8();
    const epv_centi = r.readI16();
    const dfOff = r.pos;
    const debug_flags = r.readU8();
    const empty = publicFrame ? publicFrame.players[s].roster_idx === 0xff : false;
    if (empty) {
      if (
        target_x_cm !== 0 ||
        target_y_cm !== 0 ||
        defender_slot !== 0xff ||
        burst_energy !== 0 ||
        cum_stamina !== 0 ||
        rhythm_quant !== 0 ||
        epv_centi !== 0 ||
        debug_flags !== 0
      ) {
        throw new DecodeError('InvalidEmptySlot', `dirty debug empty slot ${s}`, { slot: s });
      }
    } else {
      if (defender_slot !== 0xff && defender_slot > 9) {
        throw new DecodeError('InvalidEnumValue', `defender_slot ${defender_slot}`, {
          field: 'PlayerDebugSlotV1.defender_slot',
          value: defender_slot,
        });
      }
      if ((debug_flags & 0x80) !== 0) {
        throw new DecodeError('NonZeroPadding', `PlayerDebugSlotV1 debug_flags bit7 set`, {
          offset: dfOff,
          value: debug_flags,
        });
      }
    }
    player_debug.push({
      target_x_cm,
      target_y_cm,
      defender_slot,
      burst_energy,
      cum_stamina,
      rhythm_quant,
      epv_centi,
      debug_flags,
    });
  }
  if (expectEof) r.ensureEof();
  return { tick, active_play_idx, active_def_rule_idx, player_debug };
}

const EXPECTED_EVENT_LEN = new Map([
  [0x01, 6],
  [0x02, 4],
  [0x03, 10],
  [0x04, 6],
  [0x05, 4],
  [0x06, 4],
  [0x07, 16],
  [0x08, 36],
  [0x09, 12],
  [0x0a, 8],
  [0x0b, 6],
  [0x0c, 20],
  [0x0d, 12],
  [0x0e, 24],
  [0x0f, 24],
  [0x10, 16],
  [0x11, 16],
  [0x12, 20],
  [0x13, 20],
  [0x14, 12],
  [0x15, 4],
  [0x16, 8],
  [0x17, 4],
  [0x18, 12],
  [0x19, 24],
  [0x1a, 132],
  [0x1b, 16],
  [0x1c, 20],
]);

function readShotTelemetryPublic(r) {
  return {
    release_tick: r.readU32(),
    release_game_clock_ds: r.readU16(),
    whistle_game_clock_ds: r.readU16(),
    whistle_tick: r.readU32(),
    flight_resolved_tick: r.readU32(),
    basket_scored_tick: r.readU32(),
  };
}

function readFootworkTelemetryPublic(r) {
  const dribble_status = r.readU8();
  if (dribble_status > 4) {
    throw new DecodeError('InvalidEnumValue', `dribble_status ${dribble_status}`);
  }
  const pivot_foot = r.readU8();
  if (pivot_foot > 3) {
    throw new DecodeError('InvalidEnumValue', `pivot_foot ${pivot_foot}`);
  }
  const steps_since_gather = r.readU8();
  const flagsOff = r.pos;
  const footwork_flags = r.readU8();
  if ((footwork_flags & 0xc0) !== 0) {
    throw new DecodeError('NonZeroPadding', `footwork_flags bit6..7 non-zero`, {
      offset: flagsOff,
      value: footwork_flags,
    });
  }
  const pivot_x_mm = r.readI16();
  const pivot_y_mm = r.readI16();
  const xMin = pivot_x_mm === -32768;
  const yMin = pivot_y_mm === -32768;
  if (xMin !== yMin || (pivot_foot === 0 && !(xMin && yMin))) {
    throw new DecodeError('InvalidEnumValue', `FootworkTelemetryPublic invalid pivot pos`);
  }
  const gather_game_clock_ds = r.readU16();
  const closely_guarded_timer_ds = r.readU16();
  const gather_tick = r.readU32();
  return {
    dribble_status,
    pivot_foot,
    steps_since_gather,
    footwork_flags,
    pivot_x_mm,
    pivot_y_mm,
    gather_game_clock_ds,
    closely_guarded_timer_ds,
    gather_tick,
  };
}

function decodeEventPayload(r, code) {
  switch (code) {
    case 0x01: {
      const quarter = r.readU8();
      const start_mode = r.readU8();
      if (start_mode > 2) throw new DecodeError('InvalidEnumValue', `start_mode ${start_mode}`);
      const inbounding_side = r.readU8();
      const inbound_spot = r.readU8();
      if (inbound_spot > 6) throw new DecodeError('InvalidEnumValue', `inbound_spot ${inbound_spot}`);
      const arrow_side = r.readU8();
      if (arrow_side > 1 && arrow_side !== 0xff) {
        throw new DecodeError('InvalidEnumValue', `arrow_side ${arrow_side}`);
      }
      r.readZeroPad(1);
      return { PeriodStarted: { quarter, start_mode, inbounding_side, inbound_spot, arrow_side } };
    }
    case 0x02: {
      const tip_winner_side = r.readU8();
      const tapper_roster_idx = r.readU8();
      const ball_tapped = r.readU8();
      r.readZeroPad(1);
      return { JumpBallTapped: { tip_winner_side, tapper_roster_idx, ball_tapped } };
    }
    case 0x03: {
      const possession_id = r.readU32();
      const side = r.readU8();
      const handler_roster_idx = r.readU8();
      const trigger_kind = r.readU8();
      if (trigger_kind > 4) throw new DecodeError('InvalidEnumValue', `trigger_kind ${trigger_kind}`);
      r.readZeroPad(1);
      const new_shot_clock_ds = validateTriStateShotClock(r.readU16());
      return {
        PossessionEstablished: {
          possession_id,
          side,
          handler_roster_idx,
          trigger_kind,
          new_shot_clock_ds,
        },
      };
    }
    case 0x04: {
      const passer_roster_idx = r.readU8();
      const target_roster_idx = r.readU8();
      const pass_kind = r.readU8();
      if (pass_kind > 4) throw new DecodeError('InvalidEnumValue', `pass_kind ${pass_kind}`);
      const is_extra_pass = r.readU8();
      const flight_ticks = r.readU16();
      return {
        PassReleased: {
          passer_roster_idx,
          target_roster_idx,
          pass_kind,
          is_extra_pass,
          flight_ticks,
        },
      };
    }
    case 0x05: {
      const passer_roster_idx = r.readU8();
      const catcher_roster_idx = r.readU8();
      const bobbled_recovered = r.readU8();
      r.readZeroPad(1);
      return { PassCompleted: { passer_roster_idx, catcher_roster_idx, bobbled_recovered } };
    }
    case 0x06: {
      const passer_roster_idx = r.readU8();
      const stealer_roster_idx = r.readU8();
      const is_deflection_loose = r.readU8();
      r.readZeroPad(1);
      return { PassIntercepted: { passer_roster_idx, stealer_roster_idx, is_deflection_loose } };
    }
    case 0x07: {
      const shot_id = r.readU32();
      const shooter_roster_idx = r.readU8();
      const shot_value = r.readU8();
      if (shot_value !== 2 && shot_value !== 3) {
        throw new DecodeError('InvalidEnumValue', `shot_value ${shot_value}`);
      }
      const release_zone = r.readU8();
      if (release_zone > 4) throw new DecodeError('InvalidEnumValue', `release_zone ${release_zone}`);
      const defender_slot = r.readU8();
      const nearest_def_dist_mm = r.readU16();
      const c_geom_centi = r.readU16();
      const release_x_mm = r.readI16();
      const release_y_mm = r.readI16();
      return {
        ShotReleased: {
          shot_id,
          shooter_roster_idx,
          shot_value,
          release_zone,
          defender_slot,
          nearest_def_dist_mm,
          c_geom_centi,
          release_x_mm,
          release_y_mm,
        },
      };
    }
    case 0x08: {
      const shot_id = r.readU32();
      const shooter_roster_idx = r.readU8();
      const scorer_team = r.readU8();
      const points_awarded = r.readU8();
      const outcome_class = r.readU8();
      if (outcome_class > 9) throw new DecodeError('InvalidEnumValue', `outcome_class ${outcome_class}`);
      const rim_touched = r.readU8();
      const backboard_contact = r.readU8();
      if (backboard_contact > 2) {
        throw new DecodeError('InvalidEnumValue', `backboard_contact ${backboard_contact}`);
      }
      const is_blocked = r.readU8();
      const blocker_roster_idx = r.readU8();
      const is_goaltend = r.readU8();
      const assist_roster_idx = r.readU8();
      const foul_on_shot = r.readU8();
      r.readZeroPad(1);
      const telemetry = readShotTelemetryPublic(r);
      return {
        ShotResolved: {
          shot_id,
          shooter_roster_idx,
          scorer_team,
          points_awarded,
          outcome_class,
          rim_touched,
          backboard_contact,
          is_blocked,
          blocker_roster_idx,
          is_goaltend,
          assist_roster_idx,
          foul_on_shot,
          telemetry,
        },
      };
    }
    case 0x09: {
      const ft_seq_id = r.readU32();
      const shooter_roster_idx = r.readU8();
      const team_side = r.readU8();
      const made = r.readU8();
      const shot_number = r.readU8();
      const total_shots = r.readU8();
      const is_technical_or_flagrant = r.readU8();
      const post_ft_outcome_tag = r.readU8();
      if (post_ft_outcome_tag > 5) {
        throw new DecodeError('InvalidEnumValue', `post_ft_outcome_tag ${post_ft_outcome_tag}`);
      }
      const flagsOff = r.pos;
      const ft_flags = r.readU8();
      if ((ft_flags & 0xfc) !== 0) {
        throw new DecodeError('NonZeroPadding', `ft_flags padding bits set`, {
          offset: flagsOff,
          value: ft_flags,
        });
      }
      return {
        FreeThrowResolved: {
          ft_seq_id,
          shooter_roster_idx,
          team_side,
          made,
          shot_number,
          total_shots,
          is_technical_or_flagrant,
          post_ft_outcome_tag,
          ft_flags,
        },
      };
    }
    case 0x0a: {
      const rebounder_roster_idx = r.readU8();
      const rebound_team = r.readU8();
      const is_offensive = r.readU8();
      const is_deadball_team_reb = r.readU8();
      const scramble_origin = r.readU8();
      if (scramble_origin > 3) throw new DecodeError('InvalidEnumValue', `scramble_origin ${scramble_origin}`);
      const new_shot_clock_ds = validateTriStateShotClock(r.readU16());
      r.readZeroPad(1);
      return {
        ReboundSecured: {
          rebounder_roster_idx,
          rebound_team,
          is_offensive,
          is_deadball_team_reb,
          scramble_origin,
          new_shot_clock_ds,
        },
      };
    }
    case 0x0b: {
      return {
        TurnoverCommitted: {
          offender_roster_idx: r.readU8(),
          team_side: r.readU8(),
          turnover_rule_code: r.readU16(),
          stealer_roster_idx: r.readU8(),
          is_live_steal: r.readU8(),
        },
      };
    }
    case 0x0c: {
      return {
        ViolationTriggered: {
          offender_roster_idx: r.readU8(),
          team_side: r.readU8(),
          rule_code: r.readU16(),
          footwork: readFootworkTelemetryPublic(r),
        },
      };
    }
    case 0x0d: {
      return {
        RefereeInitialCall: {
          ruling_id: r.readU64(),
          offending_side: r.readU8(),
          offender_roster_idx: r.readU8(),
          initial_rule_code: r.readU16(),
        },
      };
    }
    case 0x0e: {
      const ruling_id = r.readU64();
      const whistle_tick = r.readU32();
      const window_open_tick = r.readU32();
      const window_epoch = r.readU32();
      const initial_rule_code = r.readU16();
      const eligible_sides_mask = r.readU8();
      const window_kind_tag = r.readU8();
      if (window_kind_tag > 4) throw new DecodeError('InvalidEnumValue', `window_kind_tag ${window_kind_tag}`);
      return {
        ChallengeWindowOpenedRecord: {
          ruling_id,
          whistle_tick,
          window_open_tick,
          window_epoch,
          initial_rule_code,
          eligible_sides_mask,
          window_kind_tag,
        },
      };
    }
    case 0x0f: {
      const ruling_id = r.readU64();
      const target_tag = r.readU8();
      if (target_tag > 1) throw new DecodeError('InvalidEnumValue', `target_tag ${target_tag}`);
      const requester = r.readU8();
      r.readZeroPad(2);
      const request_seq = r.readU32();
      const target_event_seq = r.readU64();
      return {
        CoachChallengeRequested: {
          ruling_id,
          target_tag,
          requester,
          request_seq,
          target_event_seq,
        },
      };
    }
    case 0x10: {
      const ruling_id = r.readU64();
      const requester = r.readU8();
      r.readZeroPad(3);
      const request_seq = r.readU32();
      return { CoachChallengePendingGrant: { ruling_id, requester, request_seq } };
    }
    case 0x11: {
      const ruling_id = r.readU64();
      const requester = r.readU8();
      r.readZeroPad(3);
      const request_seq = r.readU32();
      return { CoachChallengeWithdrawn: { ruling_id, requester, request_seq } };
    }
    case 0x12: {
      const ruling_id = r.readU64();
      const requester = r.readU8();
      const review_origin = r.readU8();
      const overturned = r.readU8();
      const reason_code = r.readU8();
      if (reason_code > 4) throw new DecodeError('InvalidEnumValue', `reason_code ${reason_code}`);
      const timeout_charged_side = r.readU8();
      r.readZeroPad(1);
      const final_rule_code = r.readU16();
      const adjusted_game_clock_ds = r.readU16();
      const new_shot_clock_ds = validateTriStateShotClock(r.readU16());
      return {
        CoachChallengeReviewed: {
          ruling_id,
          requester,
          review_origin,
          overturned,
          reason_code,
          timeout_charged_side,
          final_rule_code,
          adjusted_game_clock_ds,
          new_shot_clock_ds,
        },
      };
    }
    case 0x13: {
      const ruling_id = r.readU64();
      const final_rule_code = r.readU16();
      const offending_side = r.readU8();
      const offender_roster_idx = r.readU8();
      const points_delta_signed = r.readI8();
      const ft_shots_enqueued = r.readU8();
      const resulting_possession_side = r.readU8();
      const foul_out_or_ejected_flag = r.readU8();
      const restored_game_clock_ds = r.readU16();
      const new_shot_clock_ds = validateTriStateShotClock(r.readU16());
      return {
        RulingFinalized: {
          ruling_id,
          final_rule_code,
          offending_side,
          offender_roster_idx,
          points_delta_signed,
          ft_shots_enqueued,
          resulting_possession_side,
          foul_out_or_ejected_flag,
          restored_game_clock_ds,
          new_shot_clock_ds,
        },
      };
    }
    case 0x14: {
      return {
        UnchallengeableChallengeChargedTimeout: {
          ruling_id: r.readU64(),
          requester: r.readU8(),
          timeout_charged: r.readU8(),
          remaining_timeouts: r.readU8(),
          remaining_challenges: r.readU8(),
        },
      };
    }
    case 0x15: {
      const reason_tag = r.readU8();
      if (reason_tag > 4) throw new DecodeError('InvalidEnumValue', `TimeoutGranted.reason_tag ${reason_tag}`);
      return {
        TimeoutGranted: {
          reason_tag,
          charged_side: r.readU8(),
          home_to_rem: r.readU8(),
          away_to_rem: r.readU8(),
        },
      };
    }
    case 0x16: {
      const team_side = r.readU8();
      const out_roster_idx = r.readU8();
      const in_roster_idx = r.readU8();
      const sub_priority_tier = r.readU8();
      if (sub_priority_tier > 5) throw new DecodeError('InvalidEnumValue', `sub_priority_tier ${sub_priority_tier}`);
      const out_player_cum_court_time_ds = r.readU32();
      return {
        SubstitutionApplied: {
          team_side,
          out_roster_idx,
          in_roster_idx,
          sub_priority_tier,
          out_player_cum_court_time_ds,
        },
      };
    }
    case 0x17: {
      const duration_ds = r.readU16();
      const reason_tag = r.readU8();
      if (reason_tag > 4) throw new DecodeError('InvalidEnumValue', `DeadTime.reason_tag ${reason_tag}`);
      r.readZeroPad(1);
      return { DeadTime: { duration_ds, reason_tag } };
    }
    case 0x18: {
      const ingress_seq = r.readU64();
      const side = r.readU8();
      const abort_reason_code = r.readU8();
      if (abort_reason_code === 0 || abort_reason_code > 0x0b) {
        throw new DecodeError('InvalidEnumValue', `abort_reason_code ${abort_reason_code}`);
      }
      const cmd_tag = r.readU8();
      r.readZeroPad(1);
      return {
        AnchoredCommandAbortedRecord: { ingress_seq, side, abort_reason_code, cmd_tag },
      };
    }
    case 0x19: {
      const original_shot_event_seq = r.readU64();
      const shooter_roster_idx = r.readU8();
      const assist_roster_idx = r.readU8();
      const team_side = r.readU8();
      const amend_kind = r.readU8();
      if (amend_kind > 4) throw new DecodeError('InvalidEnumValue', `amend_kind ${amend_kind}`);
      const old_points = r.readU8();
      const new_points = r.readU8();
      const pts_delta = r.readI8();
      const fga_delta = r.readI8();
      const fgm_delta = r.readI8();
      const tpa_delta = r.readI8();
      const tpm_delta = r.readI8();
      const ast_delta = r.readI8();
      const blocker_roster_idx = r.readU8();
      const blk_delta = r.readI8();
      r.readZeroPad(2);
      return {
        ScoringEventAmended: {
          original_shot_event_seq,
          shooter_roster_idx,
          assist_roster_idx,
          team_side,
          amend_kind,
          old_points,
          new_points,
          pts_delta,
          fga_delta,
          fgm_delta,
          tpa_delta,
          tpm_delta,
          ast_delta,
          blocker_roster_idx,
          blk_delta,
        },
      };
    }
    case 0x1a: {
      const score_home_official = r.readU16();
      const score_away_official = r.readU16();
      const on_court_score_home = r.readU16();
      const on_court_score_away = r.readU16();
      const winner_side = r.readU8();
      if (winner_side > 1 && winner_side !== 0xff) {
        throw new DecodeError('InvalidEnumValue', `winner_side ${winner_side}`);
      }
      const end_reason = r.readU8();
      if (end_reason > 5) throw new DecodeError('InvalidEnumValue', `end_reason ${end_reason}`);
      const final_quarter = r.readU8();
      r.readZeroPad(1);
      const final_court_time_ds = [];
      for (let i = 0; i < 30; i++) final_court_time_ds.push(r.readU32());
      return {
        GameEnded: {
          score_home_official,
          score_away_official,
          on_court_score_home,
          on_court_score_away,
          winner_side,
          end_reason,
          final_quarter,
          final_court_time_ds,
        },
      };
    }
    case 0x1b: {
      const ruling_id = r.readU64();
      const rule_code = r.readU16();
      const offending_side = r.readU8();
      const offender_roster_idx = r.readU8();
      const foul_category = r.readU8();
      if (foul_category > 6) throw new DecodeError('InvalidEnumValue', `foul_category ${foul_category}`);
      return {
        FoulAssessedRecord: {
          ruling_id,
          rule_code,
          offending_side,
          offender_roster_idx,
          foul_category,
          counts_team_foul: r.readU8(),
          resulting_personal_fouls: r.readU8(),
          ejected_or_fouled_out_flags: r.readU8(),
        },
      };
    }
    case 0x1c: {
      const adjust_reason = r.readU8();
      if (adjust_reason > 3) throw new DecodeError('InvalidEnumValue', `adjust_reason ${adjust_reason}`);
      const delta_game_clock_ds_signed = r.readI16();
      const new_game_clock_ds = r.readU16();
      const new_shot_clock_ds = validateTriStateShotClock(r.readU16());
      const maskOff = r.pos;
      const affected_slot_mask = r.readU16();
      if ((affected_slot_mask & 0xfc00) !== 0) {
        throw new DecodeError('NonZeroPadding', `affected_slot_mask upper bits non-zero`, {
          offset: maskOff + 1,
          value: (affected_slot_mask >> 8) & 0xfc,
        });
      }
      r.readZeroPad(1);
      const affected_roster_indices = Array.from(r.readBytes(10));
      return {
        ClockAdjusted: {
          adjust_reason,
          delta_game_clock_ds_signed,
          new_game_clock_ds,
          new_shot_clock_ds,
          affected_slot_mask,
          affected_roster_indices,
        },
      };
    }
    default:
      throw new DecodeError('InvalidEnumValue', `unknown event_type_code 0x${code.toString(16)}`, {
        field: 'event_type_code',
        value: code,
      });
  }
}

export function decodeGameRecordEventV1(input, { expectEof = false } = {}) {
  const r = input instanceof CanonicalReader ? input : new CanonicalReader(input);
  const event_seq = r.readU64();
  const tick = r.readU32();
  const game_clock_ds = r.readU16();
  const shot_clock_ds = validateShotClockOpt(r.readU16());
  const quarter = r.readU8();
  const event_type_code = r.readU8();
  const payload_byte_len = r.readU16();

  const expected = EXPECTED_EVENT_LEN.get(event_type_code);
  if (expected === undefined) {
    throw new DecodeError('InvalidEnumValue', `invalid event_type_code ${event_type_code}`, {
      field: 'event_type_code',
      value: event_type_code,
    });
  }
  if (payload_byte_len !== expected) {
    throw new DecodeError(
      'InvalidFieldValue',
      `payload_byte_len ${payload_byte_len} != expected ${expected}`,
      { field: 'payload_byte_len' },
    );
  }
  const payload = decodeEventPayload(r, event_type_code);
  if (expectEof) r.ensureEof();
  return {
    event_seq,
    tick,
    game_clock_ds,
    shot_clock_ds,
    quarter,
    payload,
  };
}

export function decodeRawStream(input, header) {
  const r = input instanceof CanonicalReader ? input : new CanonicalReader(input);
  const magic = r.readAscii(4);
  if (magic !== 'HRW1') {
    throw new DecodeError('InvalidMagic', `expected HRW1, got ${magic}`);
  }
  const frameCount = r.readU32();
  const hasDbg = r.readU8();
  if (hasDbg > 1) {
    throw new DecodeError('InvalidEnumValue', `RawStream has_debug ${hasDbg}`, {
      field: 'RawStream.has_debug',
      value: hasDbg,
    });
  }
  r.readZeroPad(3);
  const eventCount = r.readU32();

  const rosterCount = header ? header.roster_count : 30;
  const playCnt = header ? header.play_dict.length : 0;
  const defCnt = header ? header.def_dict.length : 0;

  const frames = [];
  const debugFrames = hasDbg === 1 ? [] : null;
  for (let i = 0; i < frameCount; i++) {
    const pf = decodePublicFrameV1(r, rosterCount, { expectEof: false });
    if (i > 0 && pf.tick !== frames[i - 1].tick + 1) {
      throw new DecodeError(
        'FrameSequenceGap',
        `expected tick ${frames[i - 1].tick + 1}, got ${pf.tick}`,
        { expected: frames[i - 1].tick + 1, got: pf.tick },
      );
    }
    frames.push(pf);
    if (debugFrames) {
      debugFrames.push(decodeDebugOverlayFrameV1(r, playCnt, defCnt, pf, { expectEof: false }));
    }
  }

  const events = [];
  let prevSeq = 0;
  for (let i = 0; i < eventCount; i++) {
    const ev = decodeGameRecordEventV1(r, { expectEof: false });
    if (ev.event_seq !== prevSeq + 1 && prevSeq !== 0) {
      throw new DecodeError('EventSequenceGap', `expected ${prevSeq + 1}, got ${ev.event_seq}`);
    }
    prevSeq = ev.event_seq;
    events.push(ev);
  }
  r.ensureEof();
  return { frames, debugFrames, events };
}

export function decodeHps1PlusRawBundle(bytes) {
  const r = new CanonicalReader(bytes);
  const hdrLen = r.readU32();
  const hdrBytes = r.readBytes(hdrLen);
  const header = decodeHps1Header(hdrBytes, { expectEof: true });
  const rawSlice = r.readBytes(r.remaining);
  const { frames, debugFrames, events } = decodeRawStream(rawSlice, header);
  return { header, frames, debugFrames, events };
}

function signExtendI16(u) {
  return (u << 16) >> 16;
}
function signExtendI8(u) {
  return (u << 24) >> 24;
}

export function decodeSoaChunkRaw(rawBytes, tickCount, eventCount, expectedStartTick, rosterCount) {
  if (tickCount === 0) {
    throw new DecodeError('InvalidFieldValue', 'Hck1Chunk tick_count == 0');
  }
  const r = new CanonicalReader(rawBytes);

  const ticks = [r.readU32()];
  if (ticks[0] !== expectedStartTick) {
    throw new DecodeError('FrameSequenceGap', `expected ${expectedStartTick}, got ${ticks[0]}`, {
      expected: expectedStartTick,
      got: ticks[0],
    });
  }
  for (let i = 1; i < tickCount; i++) {
    const delta = r.readU32();
    const cur = (ticks[i - 1] + delta) >>> 0;
    if (cur !== ticks[i - 1] + 1) {
      throw new DecodeError('FrameSequenceGap', `expected ${ticks[i - 1] + 1}, got ${cur}`);
    }
    ticks.push(cur);
  }

  const quarters = r.readBytes(tickCount);

  const gameClock = [r.readU16()];
  for (let i = 1; i < tickCount; i++) {
    const d = r.readI16();
    gameClock.push((signExtendI16(gameClock[i - 1]) + d) & 0xffff);
  }

  const shotClock = [];
  for (let i = 0; i < tickCount; i++) {
    shotClock.push(validateShotClockOpt(r.readU16()));
  }

  const gamePhases = r.readBytes(tickCount);
  for (const gp of gamePhases) {
    if (gp > 10) throw new DecodeError('InvalidEnumValue', `game_phase ${gp}`);
  }
  const possSides = r.readBytes(tickCount);
  for (const ps of possSides) {
    if (ps > 2) throw new DecodeError('InvalidEnumValue', `possession_side ${ps}`);
  }

  const scoreHome = [r.readU16()];
  for (let i = 1; i < tickCount; i++) scoreHome.push((scoreHome[i - 1] + r.readU16()) & 0xffff);
  const scoreAway = [r.readU16()];
  for (let i = 1; i < tickCount; i++) scoreAway.push((scoreAway[i - 1] + r.readU16()) & 0xffff);

  const bx = [r.readI16()];
  for (let i = 1; i < tickCount; i++) bx.push(signExtendI16(bx[i - 1] + r.readI16()));
  const by = [r.readI16()];
  for (let i = 1; i < tickCount; i++) by.push(signExtendI16(by[i - 1] + r.readI16()));
  const bz = [r.readI16()];
  for (let i = 1; i < tickCount; i++) bz.push(signExtendI16(bz[i - 1] + r.readI16()));

  const bstate = r.readBytes(tickCount);
  for (const bs of bstate) {
    if (bs > 5) throw new DecodeError('InvalidEnumValue', `ball_state ${bs}`);
  }
  const bownerRaw = r.readBytes(tickCount);
  const bowner = [];
  for (const bo of bownerRaw) {
    const s = signExtendI8(bo);
    if (s < -1 || s > 9) throw new DecodeError('InvalidEnumValue', `ball_owner_slot ${s}`);
    bowner.push(s);
  }
  const bflagsPos = r.pos;
  const bflags = r.readBytes(tickCount);
  for (let i = 0; i < tickCount; i++) {
    if ((bflags[i] & 0x80) !== 0) {
      throw new DecodeError('NonZeroPadding', `ball_flags bit7 set`, {
        offset: bflagsPos + i,
        value: bflags[i],
      });
    }
  }

  const frames = [];
  for (let i = 0; i < tickCount; i++) {
    frames.push({
      tick: ticks[i],
      quarter: quarters[i],
      game_clock_ds: gameClock[i],
      shot_clock_ds: shotClock[i],
      game_phase: gamePhases[i],
      possession_side: possSides[i],
      score_home: scoreHome[i],
      score_away: scoreAway[i],
      ball_x_mm: bx[i],
      ball_y_mm: by[i],
      ball_z_mm: bz[i],
      ball_state: bstate[i],
      ball_owner_slot: bowner[i],
      ball_flags: bflags[i],
      players: Array.from({ length: 10 }, () => ({
        roster_idx: 0xff,
        x_mm: 0,
        y_mm: 0,
        vx_dm_s: 0,
        vy_dm_s: 0,
        facing_bin: 0,
        visible_action: 0,
        public_flags: 0,
      })),
    });
  }

  for (let s = 0; s < 10; s++) {
    const rIndices = r.readBytes(tickCount);
    const xs = [r.readI16()];
    for (let i = 1; i < tickCount; i++) xs.push(signExtendI16(xs[i - 1] + r.readI16()));
    const ys = [r.readI16()];
    for (let i = 1; i < tickCount; i++) ys.push(signExtendI16(ys[i - 1] + r.readI16()));
    const vxs = r.readBytes(tickCount);
    const vys = r.readBytes(tickCount);
    const facings = r.readBytes(tickCount);
    const actions = r.readBytes(tickCount);
    const pflagsPos = r.pos;
    const pflags = r.readBytes(tickCount);

    for (let i = 0; i < tickCount; i++) {
      const ri = rIndices[i];
      if (ri !== 0xff && ri >= rosterCount) {
        throw new DecodeError('InvalidRosterIndex', `roster_idx ${ri} >= ${rosterCount}`, {
          idx: ri,
          roster_count: rosterCount,
        });
      }
      if ((pflags[i] & 0x80) !== 0) {
        throw new DecodeError('NonZeroPadding', `pflags bit7 set`, {
          offset: pflagsPos + i,
          value: pflags[i],
        });
      }
      if (actions[i] > 19) {
        throw new DecodeError('InvalidEnumValue', `visible_action ${actions[i]}`);
      }
      frames[i].players[s] = {
        roster_idx: ri,
        x_mm: xs[i],
        y_mm: ys[i],
        vx_dm_s: signExtendI8(vxs[i]),
        vy_dm_s: signExtendI8(vys[i]),
        facing_bin: facings[i],
        visible_action: actions[i],
        public_flags: pflags[i],
      };
    }
  }

  const events = [];
  for (let i = 0; i < eventCount; i++) {
    events.push(decodeGameRecordEventV1(r, { expectEof: false }));
  }
  r.ensureEof();
  return { frames, events };
}

export function decodeSoaZstdArchive(input, { decompressZstd } = {}) {
  const bytes = toU8(input);
  if (bytes.length < 24) {
    throw new DecodeError('UnexpectedEof', `archive truncated at ${bytes.length}`);
  }
  if (asciiDecoder.decode(bytes.subarray(0, 4)) !== 'HSAD') {
    throw new DecodeError('InvalidMagic', 'expected HSAD archive magic');
  }
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const ver = dv.getUint16(4, true);
  if (ver !== 1) {
    throw new DecodeError('UnsupportedVersion', `unsupported HSAD version ${ver}`, { version: ver });
  }
  if (bytes[6] !== 0 || bytes[7] !== 0) {
    throw new DecodeError('NonZeroPadding', 'HSAD header reserved bytes non-zero', {
      offset: 6,
      value: bytes[6],
    });
  }
  const hdrLen = dv.getUint32(8, true);
  if (12 + hdrLen > bytes.length) {
    throw new DecodeError('UnexpectedEof', 'HPS1 header truncated');
  }
  const header = decodeHps1Header(bytes.subarray(12, 12 + hdrLen), { expectEof: true });

  const endLen = bytes.length;
  if (asciiDecoder.decode(bytes.subarray(endLen - 4, endLen)) !== 'END1') {
    throw new DecodeError('InvalidMagic', 'expected END1 footer terminator');
  }
  const footerStart = safeU64(dv.getBigUint64(endLen - 12, true));
  if (footerStart < 12 + hdrLen || footerStart >= endLen - 12) {
    throw new DecodeError('InvalidFieldValue', 'invalid SeekIndexFooterV1 footer_start');
  }

  const fr = new CanonicalReader(bytes.subarray(footerStart, endLen - 12));
  if (fr.readAscii(4) !== 'SKX1') {
    throw new DecodeError('InvalidMagic', 'expected SKX1 seek index');
  }
  const numChunks = fr.readU32();
  const seekIndex = [];
  for (let i = 0; i < numChunks; i++) {
    seekIndex.push({
      start_tick: fr.readU32(),
      tick_count: fr.readU16(),
      first_event_seq: fr.readU64(),
      last_event_seq: fr.readU64(),
      file_offset: fr.readU64(),
      chunk_bytes: fr.readU32(),
    });
  }
  fr.ensureEof();

  const allFrames = [];
  const allEvents = [];
  for (const entry of seekIndex) {
    const start = entry.file_offset;
    const end = start + entry.chunk_bytes;
    if (end > bytes.length) {
      throw new DecodeError('UnexpectedEof', 'chunk truncated');
    }
    const cr = new CanonicalReader(bytes.subarray(start, end));
    if (cr.readAscii(4) !== 'HCK1') {
      throw new DecodeError('InvalidMagic', 'expected HCK1 chunk magic');
    }
    const cver = cr.readU16();
    if (cver !== 1) {
      throw new DecodeError('UnsupportedVersion', `unsupported chunk version ${cver}`, {
        version: cver,
      });
    }
    const flags = cr.readU16();
    if ((flags & ~1) !== 0) {
      throw new DecodeError('NonZeroPadding', 'HCK1 reserved flags non-zero', {
        offset: start + 6,
        value: flags >> 8,
      });
    }
    const startTick = cr.readU32();
    const tickCount = cr.readU16();
    const eventCount = cr.readU16();
    const rawLen = cr.readU32();
    if (rawLen > MAX_CHUNK_UNCOMPRESSED_BYTES) {
      throw new DecodeError('ChunkPayloadTooLarge', `raw_len ${rawLen} exceeds ceiling`);
    }
    const zlen = cr.readU32();
    const payload = cr.readBytes(zlen);
    cr.ensureEof();

    let raw;
    if ((flags & 1) !== 0) {
      if (typeof decompressZstd !== 'function') {
        throw new DecodeError('ZstdUnavailable', 'decompressZstd function required for compressed HCK1');
      }
      raw = toU8(decompressZstd(payload, rawLen));
    } else {
      raw = payload;
    }
    const { frames, events } = decodeSoaChunkRaw(
      raw,
      tickCount,
      eventCount,
      startTick,
      header.roster_count,
    );
    allFrames.push(...frames);
    allEvents.push(...events);
  }

  let prevSeq = 0;
  for (const ev of allEvents) {
    if (ev.event_seq !== prevSeq + 1) {
      throw new DecodeError('EventSequenceGap', `expected ${prevSeq + 1}, got ${ev.event_seq}`);
    }
    prevSeq = ev.event_seq;
  }

  return { header, frames: allFrames, events: allEvents, seek_index: seekIndex };
}
