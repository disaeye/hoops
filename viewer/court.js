const FT = 0.3048;
const IN = FT / 12;

const NBA = Object.freeze({
  name: 'NBA', length: 94 * FT, width: 50 * FT, line: 2 * IN,
  apronX: 8 * FT, apronY: 5 * FT, circle: 6 * FT, innerCircle: 2 * FT,
  board: 4 * FT, boardWidth: 6 * FT, rim: 5.25 * FT, rimRadius: 9 * IN,
  lane: 16 * FT, freeThrow: 19 * FT, freeThrowWidth: 16 * FT,
  three: 23.75 * FT, corner: 22 * FT, restricted: 4 * FT,
});
const FIBA = Object.freeze({
  name: 'FIBA', length: 28, width: 15, line: 0.05,
  apronX: 2, apronY: 2, circle: 1.8,
  board: 1.2, boardWidth: 1.8, rim: 1.575, rimRadius: 0.225,
  lane: 4.9, freeThrow: 5.8, freeThrowWidth: 3.6,
  three: 6.75, corner: 6.6, restricted: 1.30,
});

export function courtGeometry(ruleId, header) {
  const standard = ruleId?.startsWith('fiba') || (!ruleId && header?.court_length_mm === 28000) ? FIBA : NBA;
  if (!header || (Math.round(standard.length * 1000) === header.court_length_mm &&
    Math.round(standard.width * 1000) === header.court_width_mm)) return standard;
  return { ...standard, length: header.court_length_mm / 1000, width: header.court_width_mm / 1000 };
}

export function threePointLine(court) {
  const radius = court.three - court.line / 2;
  const offset = court.corner - court.line / 2;
  return { radius, offset, angle: Math.asin(offset / radius), join: court.rim + Math.sqrt(radius ** 2 - offset ** 2) };
}

function line(ctx, x1, y1, x2, y2) {
  ctx.beginPath();
  ctx.moveTo(x1, y1);
  ctx.lineTo(x2, y2);
  ctx.stroke();
}

function arc(ctx, x, y, r, start = 0, end = 2 * Math.PI) {
  ctx.beginPath();
  ctx.arc(x, y, r, start, end);
  ctx.stroke();
}

function drawLaneMarks(ctx, court, lane) {
  const nba = court.name === 'NBA';
  for (const side of [-1, 1]) {
    const y = side * (lane + court.line / 2);
    const depth = side * (nba ? 6 * IN : 0.10);
    const marks = nba ? [7, 8, 11, 14].map((distance) => distance * FT) : [1.75, 2.60, 3.00, 3.85, 4.70];
    for (const x of marks) line(ctx, x, y, x, y + depth);
    if (nba) {
      ctx.fillStyle = '#593b32';
      ctx.fillRect(7 * FT, Math.min(y, y + depth), FT, Math.abs(depth));
      line(ctx, 0, y + side * 3 * FT, 6 * IN, y + side * 3 * FT);
      line(ctx, 13 * FT, y + side * 3 * FT, 13 * FT, y + side * 3 * FT + depth);
      line(ctx, 28 * FT, side * court.width / 2, 28 * FT, side * (court.width / 2 - 3 * FT));
    } else {
      const sideline = side * (court.width / 2 + court.line);
      line(ctx, 8.325 - court.line / 2, sideline, 8.325 - court.line / 2, sideline + side * 0.15);
    }
  }
}

function drawEnd(ctx, court, color) {
  const lane = court.lane / 2 - court.line / 2;
  const ft = court.freeThrow - court.line / 2;
  ctx.fillStyle = color;
  ctx.fillRect(0, -lane, ft, lane * 2);
  for (const side of [-1, 1]) line(ctx, 0, side * lane, ft, side * lane);
  const freeThrowHalf = court.freeThrowWidth / 2 - court.line / 2;
  if (court.name === 'FIBA') {
    ctx.save();
    ctx.globalAlpha = 0.62;
    for (const side of [-1, 1]) line(ctx, ft, side * freeThrowHalf, ft, side * lane);
    ctx.restore();
  }
  line(ctx, ft, -freeThrowHalf, ft, freeThrowHalf);
  arc(ctx, ft, 0, court.circle - court.line / 2, -Math.PI / 2, Math.PI / 2);
  if (court.name === 'NBA') {
    ctx.setLineDash([0.30, 0.28]);
    arc(ctx, ft, 0, court.circle - court.line / 2, Math.PI / 2, 3 * Math.PI / 2);
    ctx.setLineDash([]);
  }
  drawLaneMarks(ctx, court, lane);

  const { radius, offset, angle, join } = threePointLine(court);
  ctx.beginPath();
  ctx.moveTo(0, -offset);
  ctx.lineTo(join, -offset);
  ctx.arc(court.rim, 0, radius, -angle, angle);
  ctx.lineTo(0, offset);
  ctx.stroke();

  const restricted = court.restricted - court.line / 2;
  ctx.beginPath();
  ctx.moveTo(court.board, -restricted);
  ctx.lineTo(court.rim, -restricted);
  ctx.arc(court.rim, 0, restricted, -Math.PI / 2, Math.PI / 2);
  ctx.lineTo(court.board, restricted);
  ctx.stroke();

  ctx.lineWidth = 0.09;
  ctx.strokeStyle = '#fff';
  line(ctx, court.board, -court.boardWidth / 2, court.board, court.boardWidth / 2);
  ctx.strokeStyle = '#b54511';
  ctx.lineWidth = 0.02;
  arc(ctx, court.rim, 0, court.rimRadius + ctx.lineWidth / 2);
}

export function drawCourt(ctx, court, ox, oy, scale) {
  const hx = court.length / 2;
  const hy = court.width / 2;
  ctx.save();
  ctx.translate(ox, oy);
  ctx.scale(scale, scale);
  ctx.fillStyle = '#503b32';
  ctx.fillRect(-hx - court.apronX, -hy - court.apronY, court.length + 2 * court.apronX, court.width + 2 * court.apronY);
  for (let y = -hy, i = 0; y < hy; y += 0.46, i += 1) {
    ctx.fillStyle = i % 2 ? '#c89460' : '#ce9b66';
    ctx.fillRect(-hx, y, court.length, Math.min(0.46, hy - y));
  }
  ctx.strokeStyle = '#fff7e8';
  ctx.lineWidth = court.line;
  const halfLine = court.line / 2;
  ctx.strokeRect(-hx - halfLine, -hy - halfLine, court.length + court.line, court.width + court.line);
  const extension = court.name === 'FIBA' ? 0.15 : 0;
  line(ctx, 0, -hy - extension, 0, hy + extension);
  arc(ctx, 0, 0, court.circle - halfLine);
  if (court.name === 'NBA') {
    arc(ctx, 0, 0, court.innerCircle + halfLine);
    for (const x of [-4 * FT, 4 * FT]) line(ctx, x, hy + court.line, x, hy + court.line + 3 * FT);
  } else {
    for (const side of [-1, 1]) {
      for (const offset of [5, hx]) {
        line(ctx, side * offset, hy + court.line, side * offset, hy + 2);
      }
    }
  }
  for (const side of [-1, 1]) {
    ctx.save();
    ctx.translate(side * hx, 0);
    ctx.scale(-side, 1);
    drawEnd(ctx, court, side < 0 ? '#94675c' : '#756d73');
    ctx.restore();
  }
  ctx.restore();
}
