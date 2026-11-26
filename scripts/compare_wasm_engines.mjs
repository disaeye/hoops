// 逐拍比较两份 hoops_core.wasm（重构前/后）在同一 MatchInitSpecV1 下的公开帧、调试帧与事件字节流。
// 用法：node scripts/compare_wasm_engines.mjs <baseline.wasm> <candidate.wasm> [spec.bin] [--max-ticks N]
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';

const ROOT = path.resolve(import.meta.dirname, '..');
const args = process.argv.slice(2).filter((a) => !a.startsWith('--'));
const flags = process.argv.slice(2).filter((a) => a.startsWith('--'));
const BASELINE = args[0];
const CANDIDATE = args[1];
const SPEC_PATH = args[2] ?? path.join(ROOT, 'target/verify/hoops_wasm_spec.bin');
const MAX_TICKS = Number(flags.find((f) => f.startsWith('--max-ticks='))?.split('=')[1] ?? 200_000);
const PUBLIC_FRAME_BYTES = 124;
const DEBUG_FRAME_BYTES = 118;

assert.ok(BASELINE && CANDIDATE, 'usage: node scripts/compare_wasm_engines.mjs <baseline.wasm> <candidate.wasm> [spec.bin] [--max-ticks=N]');
const specBytes = fs.readFileSync(SPEC_PATH);

async function load(file) {
  const bytes = fs.readFileSync(file);
  const mod = await WebAssembly.compile(bytes);
  const inst = await WebAssembly.instantiate(mod, {});
  return inst.exports;
}

function makeDriver(ex) {
  const mem = ex.memory;
  const specPtr = ex.hoops_wasm_alloc(specBytes.length);
  new Uint8Array(mem.buffer, specPtr, specBytes.length).set(specBytes);
  const handle = ex.hoops_engine_from_manifest(specPtr, specBytes.length);
  ex.hoops_wasm_dealloc(specPtr, specBytes.length);
  assert.notEqual(handle, 0, 'hoops_engine_from_manifest failed');

  const winLenPtr = ex.hoops_wasm_alloc(4);
  const sysInPtr = ex.hoops_wasm_alloc(25);
  const sysOutPtr = ex.hoops_wasm_alloc(256);
  const evLenPtr = ex.hoops_wasm_alloc(4);
  const sysBuf = new Uint8Array(25);
  const sv = new DataView(sysBuf.buffer);
  let nextIngressSeq = 1n;

  const copy = (ptr, len) => Buffer.from(new Uint8Array(mem.buffer, ptr, len));
  return {
    publicFrame: () => copy(ex.hoops_engine_last_public_frame_ptr(handle, 0), PUBLIC_FRAME_BYTES),
    debugFrame: () => copy(ex.hoops_engine_last_debug_frame_ptr(handle, 0), DEBUG_FRAME_BYTES),
    events: () => copy(ex.hoops_engine_last_events_ptr(handle, evLenPtr), new DataView(mem.buffer, evLenPtr, 4).getUint32(0, true)),
    // 与 Native `step_with_batch_ai` 相同的确定性 PostWhistle 驱动
    step() {
      let st = ex.hoops_engine_step(handle);
      while (st === 2) {
        const winPtr = ex.hoops_engine_active_window_ptr(handle, winLenPtr);
        assert.equal(new DataView(mem.buffer, winLenPtr, 4).getUint32(0, true), 32);
        const wv = new DataView(mem.buffer, winPtr, 32);
        const rulingId = wv.getBigUint64(0, true);
        const windowEpoch = wv.getUint32(16, true);
        const reqSeq = wv.getUint32(20, true);
        const wState = new Uint8Array(mem.buffer, winPtr, 32)[26];
        sv.setBigUint64(0, nextIngressSeq++, true);
        sysBuf[8] = 0;
        sysBuf[9] = 1;
        sysBuf[10] = 2;
        sysBuf[11] = 1;
        sysBuf[12] = wState === 1 ? 0 : 1;
        sv.setBigUint64(13, rulingId, true);
        sv.setUint32(21, wState === 1 ? reqSeq : windowEpoch, true);
        new Uint8Array(mem.buffer, sysInPtr, sysBuf.length).set(sysBuf);
        assert.equal(ex.hoops_engine_enqueue_command(handle, sysInPtr, sysBuf.length, sysOutPtr, 256), 0);
        st = ex.hoops_engine_step(handle);
      }
      return st;
    },
  };
}

const base = makeDriver(await load(BASELINE));
const cand = makeDriver(await load(CANDIDATE));

const report = (t, kind, a, b) => {
  let at = 0;
  while (at < a.length && a[at] === b[at]) at++;
  throw new Error(`${kind} mismatch at tick ${t}: first differing byte offset ${at}, baseline=${a.toString('hex')} candidate=${b.toString('hex')}`);
};

assert.deepEqual(base.publicFrame(), cand.publicFrame(), 'tick 0 public frame mismatch');
assert.deepEqual(base.debugFrame(), cand.debugFrame(), 'tick 0 debug frame mismatch');
assert.deepEqual(base.events(), cand.events(), 'tick 0 events mismatch');

let t = 0;
let lastStatus = 0;
for (;;) {
  const stBase = base.step();
  const stCand = cand.step();
  assert.equal(stCand, stBase, `step status mismatch after tick ${t}`);
  lastStatus = stBase;
  t += 1;
  const pubA = base.publicFrame();
  const pubB = cand.publicFrame();
  if (!pubA.equals(pubB)) report(t, 'public frame', pubA, pubB);
  const dbgA = base.debugFrame();
  const dbgB = cand.debugFrame();
  if (!dbgA.equals(dbgB)) report(t, 'debug frame', dbgA, dbgB);
  const evA = base.events();
  const evB = cand.events();
  if (!evA.equals(evB)) report(t, 'events', evA, evB);
  if (stBase === 3 || stBase === 4 || t >= MAX_TICKS) break;
}
console.log(`PASS compare_wasm_engines: ${t} ticks compared, public/debug frames and events byte-identical, final status ${lastStatus}`);
