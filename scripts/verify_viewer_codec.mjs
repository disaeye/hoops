import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import { zstdDecompressSync } from 'node:zlib';

import {
  DecodeError,
  decodeHps1Header,
  decodePublicFrameV1,
  decodeSoaZstdArchive,
} from '../viewer/codec.js';

const ROOT = path.resolve(import.meta.dirname, '..');
const FIXTURES = path.join(ROOT, 'crates/hoops-output/tests/fixtures');

// 1. Verify golden PublicFrameV1 (124 B) matches Rust JSON output bit-identically
const frameBin = fs.readFileSync(path.join(FIXTURES, 'frame_v1_golden.bin'));
const frameJson = JSON.parse(fs.readFileSync(path.join(FIXTURES, 'frame_v1_golden.json'), 'utf8'));
const decodedFrame = decodePublicFrameV1(frameBin, 30, { expectEof: true });
assert.deepEqual(decodedFrame, frameJson, 'PublicFrameV1 JS decode mismatch vs golden JSON');

// 2. Verify golden SoADeltaZstd600 archive (.hframes.zst) matches Rust JSON output bit-identically
const arcBin = fs.readFileSync(path.join(FIXTURES, 'archive_soa_zstd_v1_golden.hframes.zst'));
const arcJson = JSON.parse(fs.readFileSync(path.join(FIXTURES, 'archive_soa_zstd_v1_golden.json'), 'utf8'));
const decodedArc = decodeSoaZstdArchive(arcBin, {
  decompressZstd: (payload) => zstdDecompressSync(payload),
});
assert.deepEqual(decodedArc.header, arcJson.header, 'HPS1 header mismatch');
assert.deepEqual(decodedArc.frames, arcJson.frames, 'Archive SoA frames mismatch');
assert.deepEqual(decodedArc.events, arcJson.events, 'Archive SoA events mismatch');

// 3. Negative corruption checks (strict canonical safety invariants)
const badMagic = Uint8Array.from(frameBin);
assert.throws(
  () => decodeHps1Header(badMagic),
  (e) => e instanceof DecodeError && e.code === 'InvalidMagic',
);

const badFlags = Uint8Array.from(frameBin);
badFlags[23] |= 0x80; // ball_flags bit7 MUST be 0
assert.throws(
  () => decodePublicFrameV1(badFlags, 30),
  (e) => e instanceof DecodeError && e.code === 'NonZeroPadding',
);

const badRoster = Uint8Array.from(frameBin);
badRoster[24] = 29; // slot 0 roster_idx = 29 >= roster_count 10
assert.throws(
  () => decodePublicFrameV1(badRoster, 10),
  (e) => e instanceof DecodeError && e.code === 'InvalidRosterIndex',
);

const badEmpty = Uint8Array.from(frameBin);
badEmpty[24] = 0xff; // Empty slot marker with dirty x_mm != 0
badEmpty[25] = 0x01;
assert.throws(
  () => decodePublicFrameV1(badEmpty, 30),
  (e) => e instanceof DecodeError && e.code === 'InvalidEmptySlot',
);

assert.throws(
  () => decodePublicFrameV1(frameBin.subarray(0, 123), 30),
  (e) => e instanceof DecodeError && e.code === 'UnexpectedEof',
);

const withTrailing = new Uint8Array(frameBin.length + 1);
withTrailing.set(frameBin);
assert.throws(
  () => decodePublicFrameV1(withTrailing, 30, { expectEof: true }),
  (e) => e instanceof DecodeError && e.code === 'TrailingBytes',
);

// Extract raw HPS1 slice from archive and inject private play_dict entry into PublicOnly header
const dv = new DataView(arcBin.buffer, arcBin.byteOffset, arcBin.byteLength);
const hdrLen = dv.getUint32(8, true);
const pubHdrBytes = arcBin.subarray(12, 12 + hdrLen);
// Locate trailing play_dict & def_dict count (both u32 = 0 at end of PublicOnly header)
const mutatedHdr = new Uint8Array(pubHdrBytes.length + 6 + 3);
mutatedHdr.set(pubHdrBytes.subarray(0, pubHdrBytes.length - 8));
const mDv = new DataView(mutatedHdr.buffer);
const pOff = pubHdrBytes.length - 8;
mDv.setUint32(pOff, 1, true); // play_dict len = 1
mDv.setUint16(pOff + 4, 0, true); // key = 0
mDv.setUint32(pOff + 6, 3, true); // string len = 3
mutatedHdr.set([0x70, 0x6e, 0x72], pOff + 10); // "pnr"
mDv.setUint32(pOff + 13, 0, true); // def_dict len = 0
assert.throws(
  () => decodeHps1Header(mutatedHdr),
  (e) => e instanceof DecodeError && e.code === 'PrivateDictInPublicStream',
);

console.log(
  `PASS verify_viewer_codec: frame_v1 (124B) + archive_soa_zstd (${decodedArc.frames.length} frames, ${decodedArc.events.length} events) match golden JSON; all 7 negative corruption vectors rejected cleanly.`,
);
