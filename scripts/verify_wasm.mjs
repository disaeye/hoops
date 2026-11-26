import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';

const ROOT = path.resolve(import.meta.dirname, '..');
const WASM_PATH = path.join(ROOT, 'target/wasm32-unknown-unknown/release/hoops_core.wasm');
const SPEC_PATH = process.argv[2] ?? path.join(ROOT, 'target/verify/hoops_wasm_spec.bin');
const HTR_PATH = process.argv[3] ?? path.join(ROOT, 'target/verify/hoops_native.htr');

const EXPECTED_EXPORTS = [
  'memory',
  'hoops_wasm_abi_version',
  'hoops_wasm_alloc',
  'hoops_wasm_dealloc',
  'hoops_engine_from_manifest',
  'hoops_engine_destroy',
  'hoops_engine_enqueue_command',
  'hoops_engine_replay_anchored_command',
  'hoops_engine_step',
  'hoops_engine_last_public_frame_ptr',
  'hoops_engine_last_debug_frame_ptr',
  'hoops_engine_last_events_ptr',
  'hoops_engine_active_window_ptr',
  'hoops_engine_last_hashes_ptr',
  'hoops_engine_capture_checkpoint_ptr',
];

const wasmBytes = fs.readFileSync(WASM_PATH);
const mod = await WebAssembly.compile(wasmBytes);
const inst = await WebAssembly.instantiate(mod, {});
const ex = inst.exports;

for (const name of EXPECTED_EXPORTS) {
  assert.ok(name in ex, `Missing frozen WASM export: ${name}`);
}
assert.equal(ex.hoops_wasm_abi_version(), 1);

const mem = ex.memory;
const initPages = mem.buffer.byteLength / 65536;
console.log(`Initial WASM memory pages: ${initPages} (${(mem.buffer.byteLength / 1048576).toFixed(2)} MiB)`);
assert.ok(initPages <= 64, `WASM initial pages ${initPages} exceeds 64-page (4.0 MiB) ceiling`);

const specBytes = fs.readFileSync(SPEC_PATH);
const htrBytes = fs.readFileSync(HTR_PATH);

// Parse HTR1 header (80 bytes) + records (100 bytes each)
const htrView = new DataView(htrBytes.buffer, htrBytes.byteOffset, htrBytes.byteLength);
assert.equal(htrBytes.subarray(0, 4).toString(), 'HTR1');
const tickCount = htrView.getUint32(12, true);

// Test 0xFFFF_FFFE capacity probe zero-mutation retry + AlreadyAccepted idempotency BEFORE parity engine & page baseline
const cmdBuf = new Uint8Array(8 + 1 + 1 + 1 + 1 + 1 + 2 + 1);
const cmdDv = new DataView(cmdBuf.buffer);
cmdDv.setBigUint64(0, 1n, true);
cmdBuf[8] = 0; // apply_at_tick = None
cmdBuf[9] = 0; // PreTick
cmdBuf[10] = 0; // Home
cmdBuf[11] = 0; // Player
cmdBuf[12] = 6; // SetPaceAndPressure
cmdBuf[13] = 2; // PatchField::Set
cmdBuf[14] = 70; // pace=70
cmdBuf[15] = 0; // PatchField::Keep

const h2SpecPtr = ex.hoops_wasm_alloc(specBytes.length);
new Uint8Array(mem.buffer, h2SpecPtr, specBytes.length).set(specBytes);
const h2 = ex.hoops_engine_from_manifest(h2SpecPtr, specBytes.length);
ex.hoops_wasm_dealloc(h2SpecPtr, specBytes.length);
assert.notEqual(h2, 0);

const inPtr = ex.hoops_wasm_alloc(cmdBuf.length);
new Uint8Array(mem.buffer, inPtr, cmdBuf.length).set(cmdBuf);
const tinyOutPtr = ex.hoops_wasm_alloc(4);
const rcTooSmall = ex.hoops_engine_enqueue_command(h2, inPtr, cmdBuf.length, tinyOutPtr, 4);
assert.equal(rcTooSmall >>> 0, 0xFFFFFFFE, 'Expected 0xFFFF_FFFE buffer too small probe');
const needed = new DataView(mem.buffer, tinyOutPtr, 4).getUint32(0, true);
ex.hoops_wasm_dealloc(tinyOutPtr, 4);
assert.ok(needed > 4);

const exactOutPtr = ex.hoops_wasm_alloc(needed);
const rcOk1 = ex.hoops_engine_enqueue_command(h2, inPtr, cmdBuf.length, exactOutPtr, needed);
assert.equal(rcOk1, 0, 'Retry with exact capacity must succeed after 0xFFFF_FFFE zero-mutation probe');
const anchored1 = new Uint8Array(mem.buffer, exactOutPtr, needed).slice();

// Idempotent retry must succeed and emit identical anchored bytes
const exactOutPtr2 = ex.hoops_wasm_alloc(needed);
const rcOk2 = ex.hoops_engine_enqueue_command(h2, inPtr, cmdBuf.length, exactOutPtr2, needed);
assert.equal(rcOk2, 0, 'Idempotent retry should return 0 (AlreadyAccepted)');
const anchored2 = new Uint8Array(mem.buffer, exactOutPtr2, needed).slice();
assert.deepEqual(anchored1, anchored2);

ex.hoops_wasm_dealloc(exactOutPtr, needed);
ex.hoops_wasm_dealloc(exactOutPtr2, needed);
ex.hoops_wasm_dealloc(inPtr, cmdBuf.length);
ex.hoops_engine_destroy(h2);

// Create parity engine and pre-allocate fixed scratch buffers BEFORE capturing post-init page baseline
const specPtr = ex.hoops_wasm_alloc(specBytes.length);
new Uint8Array(mem.buffer, specPtr, specBytes.length).set(specBytes);
const handle = ex.hoops_engine_from_manifest(specPtr, specBytes.length);
ex.hoops_wasm_dealloc(specPtr, specBytes.length);
assert.notEqual(handle, 0, 'hoops_engine_from_manifest failed');

const winLenPtr = ex.hoops_wasm_alloc(4);
const sysInPtr = ex.hoops_wasm_alloc(25);
const sysOutPtr = ex.hoops_wasm_alloc(256);

const pagesAfterInit = mem.buffer.byteLength / 65536;
assert.ok(pagesAfterInit <= 64, `Pages after init ${pagesAfterInit} > 64`);

// Verify tick0 + remaining ticks match Native HTR1 rolling_hash[t] bit-identically and 0 post-init page growth
const checkRollingHash = (t) => {
  const recOffset = 80 + t * 100;
  const nativeTick = htrView.getUint32(recOffset, true);
  assert.equal(nativeTick, t);
  const nativeRoll = htrBytes.subarray(recOffset + 68, recOffset + 100);
  const wasmHashPtr = ex.hoops_engine_last_hashes_ptr(handle);
  const wasmRoll = Buffer.from(mem.buffer, wasmHashPtr + 64, 32);
  assert.ok(nativeRoll.equals(wasmRoll), `Rolling hash mismatch at tick ${t}: native=${nativeRoll.toString('hex')} wasm=${wasmRoll.toString('hex')}`);
};

let nextIngressSeq = 1n;
const sysBuf = new Uint8Array(25);
const sv = new DataView(sysBuf.buffer);

checkRollingHash(0);
for (let t = 1; t < tickCount; t++) {
  let st = ex.hoops_engine_step(handle);
  while (st === 2) {
    // Handle PostWhistle AI Confirm/Close via SystemCommand using pre-allocated scratch buffers (0 allocations in tick loop)
    const winPtr = ex.hoops_engine_active_window_ptr(handle, winLenPtr);
    const winLen = new DataView(mem.buffer, winLenPtr, 4).getUint32(0, true);
    assert.equal(winLen, 32);
    const wv = new DataView(mem.buffer, winPtr, winLen);
    const rulingId = wv.getBigUint64(0, true);
    const windowEpoch = wv.getUint32(16, true);
    const reqSeq = wv.getUint32(20, true);
    const wState = new Uint8Array(mem.buffer, winPtr, winLen)[26];
    // CloseChallengeWindow or ConfirmChallengeGrant
    sv.setBigUint64(0, nextIngressSeq++, true);
    sysBuf[8] = 0; // None
    sysBuf[9] = 1; // PostWhistle
    sysBuf[10] = 2; // System
    sysBuf[11] = 1; // CommandPayload::System
    sysBuf[12] = wState === 1 ? 0 : 1; // Confirm(0) or Close(1)
    sv.setBigUint64(13, rulingId, true);
    sv.setUint32(21, wState === 1 ? reqSeq : windowEpoch, true);
    new Uint8Array(mem.buffer, sysInPtr, sysBuf.length).set(sysBuf);
    assert.equal(ex.hoops_engine_enqueue_command(handle, sysInPtr, sysBuf.length, sysOutPtr, 256), 0);
    st = ex.hoops_engine_step(handle);
  }
  checkRollingHash(t);
}

const finalPages = mem.buffer.byteLength / 65536;
assert.equal(finalPages, pagesAfterInit, `Post-init memory.grow occurred: ${pagesAfterInit} -> ${finalPages}`);
ex.hoops_wasm_dealloc(winLenPtr, 4);
ex.hoops_wasm_dealloc(sysInPtr, 25);
ex.hoops_wasm_dealloc(sysOutPtr, 256);
ex.hoops_engine_destroy(handle);

console.log(`PASS verify_wasm: 14 exports OK, memory=${finalPages} pages (<= 64), 0 post-init memory.grow, 0xFFFF_FFFE retry+idempotency OK, bit-exact rolling_hash[0..${tickCount - 1}] vs Native.`);
