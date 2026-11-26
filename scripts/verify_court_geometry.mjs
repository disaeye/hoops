import assert from 'node:assert/strict';
import { courtGeometry, threePointLine } from '../viewer/court.js';

const close = (actual, expected) => assert(Math.abs(actual - expected) < 1e-9, `${actual} != ${expected}`);
const nba = courtGeometry('nba_2023_24');
const fiba = courtGeometry('fiba_2024');
assert.equal(nba.name, 'NBA');
assert.equal(fiba.name, 'FIBA');
for (const [court, dimensions] of [
  [nba, { length: 28.6512, width: 15.24, line: 0.0508, rim: 1.6002, lane: 4.8768, freeThrow: 5.7912, three: 7.239, corner: 6.7056, restricted: 1.2192 }],
  [fiba, { length: 28, width: 15, line: 0.05, rim: 1.575, lane: 4.9, freeThrow: 5.8, three: 6.75, corner: 6.6, restricted: 1.30 }],
]) {
  for (const [field, value] of Object.entries(dimensions)) close(court[field], value);
  const { radius, offset, angle, join } = threePointLine(court);
  close(radius + court.line / 2, court.three);
  close(offset + court.line / 2, court.corner);
  close(radius * Math.sin(angle), offset);
  close(court.rim + radius * Math.cos(angle), join);
  assert(join < court.freeThrow);
}
assert.equal(courtGeometry('nba', { court_length_mm: 28651, court_width_mm: 15240 }), nba);
assert.equal(courtGeometry(undefined, { court_length_mm: 28000, court_width_mm: 15000 }), fiba);
console.log('Court: NBA and FIBA dimensions, line offsets, and arc joins passed.');
