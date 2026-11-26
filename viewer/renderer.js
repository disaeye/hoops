import { courtGeometry, drawCourt } from './court.js';

export const ACTION_LABELS = [
  '站位', '慢跑', '冲刺', '运球', '突破', '背打', '掩护', '绕掩护',
  '传球', '接球', '跳投', '上篮', '扣篮', '滑步', '扑防', '封堵',
  '卡位', '篮板', '扑抢', '倒地',
];

export const PHASE_NAMES = [
  '中圈跳球', '活球进攻', '投篮飞行', '篮板争抢', '鸣哨待裁',
  '挑战复核', '罚球回合', '发球回合', '暂停中', '节间休息', '比赛结束',
];

const PLAYBOOK_LABELS = {
  spread_pnr: '拉开空间挡拆',
  horns_flare: '牛角外弹掩护',
  split_action: '弱侧无球传切',
  high_post_hub: '高位策应',
  five_out_delay: '五外手递手',
  spain_pnr: '西班牙挡拆',
  post_iso: '低位单打',
  ato_quick_three: '暂停后三分',
  freelance: '自由流动进攻',
};

const DEFENSE_LABELS = {
  drop_base: '基础沉退',
  switch_all: '无限换防',
  blitz_heavy: '上线夹击',
  zone_2_3: '2-3 联防',
};

function lerp(a, b, t) {
  return a + (b - a) * t;
}

function lerpAngle(angA, angB, t) {
  let diff = angB - angA;
  while (diff > Math.PI) diff -= Math.PI * 2;
  while (diff < -Math.PI) diff += Math.PI * 2;
  return angA + diff * t;
}

export class CourtRenderer {
  constructor(canvas) {
    this.canvas = canvas;
    this.ctx = canvas.getContext('2d');
    this.floor = document.createElement('canvas');
    this.court = null;
    this.header = null;
    this.rosterMap = new Map();
    this.showDebug = false;
    this.fx = [];
    this.prevTick = -1;
    this.prevBallFlags = 0;
    this.prevScores = [0, 0];
    this.prevBallZ = 0;
    this.prevBallState = 5;
    this.setRule('nba');
  }

  setRule(ruleId) {
    this.ruleId = ruleId;
    this.updateGeometry(null);
  }

  updateGeometry(header) {
    this.header = header;
    this.rosterMap = new Map((header?.roster ?? []).map((player) => [player.roster_idx, player]));
    this.court = courtGeometry(this.ruleId, header);
    const { length, width, apronX, apronY } = this.court;
    this.canvas.style.setProperty('--court-aspect', (length + 2 * apronX) / (width + 2 * apronY));
    this.floor.width = 0;
  }

  render(frame, header, debugFrame = null) {
    this.renderInterpolated(frame, frame, 0, header, debugFrame, []);
  }

  renderInterpolated(frameA, frameB, alpha, header, debugFrame = null, trail = []) {
    if (header !== this.header) this.updateGeometry(header);
    const ctx = this.ctx;
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const cssW = this.canvas.clientWidth;
    const cssH = this.canvas.clientHeight;
    if (!cssW || !cssH) return;
    const w = Math.round(cssW * dpr);
    const h = Math.round(cssH * dpr);
    const compact = cssW < 600;
    const { length, width, apronX, apronY } = this.court;
    const scale = Math.min(w / (length + apronX * 2), h / (width + apronY * 2));
    const ox = w / 2;
    const oy = h / 2;
    const toPx = (xm, ym) => [ox + xm * scale, oy - ym * scale];
    if (this.canvas.width !== w || this.canvas.height !== h) {
      this.canvas.width = w;
      this.canvas.height = h;
    }
    if (this.floor.width !== w || this.floor.height !== h) {
      this.floor.width = w;
      this.floor.height = h;
      drawCourt(this.floor.getContext('2d'), this.court, ox, oy, scale);
    }
    ctx.clearRect(0, 0, w, h);
    ctx.drawImage(this.floor, 0, 0);

    const baseFrame = (alpha < 0.5 ? frameA : frameB) ?? frameA ?? frameB;
    if (!baseFrame) return;

    const fA = frameA || baseFrame;
    const fB = frameB || baseFrame;
    const t = fA === fB ? 0 : Math.max(0, Math.min(1, alpha));

    // Interpolated player states
    const interpPlayers = [];
    for (let s = 0; s < 10; s++) {
      const pB = fB.players[s];
      const pA = fA.players[s];
      const pCur = t < 0.5 ? pA : pB;
      if (!pCur || pCur.roster_idx === 0xff) {
        interpPlayers.push(null);
        continue;
      }
      let xm = pCur.x_mm / 1000;
      let ym = pCur.y_mm / 1000;
      let ang = (pCur.facing_bin / 256) * Math.PI * 2;
      if (pA && pB && pA.roster_idx === pB.roster_idx && pA.roster_idx !== 0xff) {
        const ax = pA.x_mm / 1000;
        const ay = pA.y_mm / 1000;
        const bx = pB.x_mm / 1000;
        const by = pB.y_mm / 1000;
        if (Math.hypot(bx - ax, by - ay) < 3.5) {
          xm = lerp(ax, bx, t);
          ym = lerp(ay, by, t);
          const angA = (pA.facing_bin / 256) * Math.PI * 2;
          const angB = (pB.facing_bin / 256) * Math.PI * 2;
          ang = lerpAngle(angA, angB, t);
        }
      }
      interpPlayers.push({ slot: s, p: pCur, xm, ym, ang });
    }

    // Film-room debug target lines
    if (this.showDebug && debugFrame) {
      for (const item of interpPlayers) {
        if (!item) continue;
        const ds = debugFrame.player_debug[item.slot];
        if (!ds) continue;
        const [px, py] = toPx(item.xm, item.ym);
        const [tx, ty] = toPx(ds.target_x_cm / 100, ds.target_y_cm / 100);
        ctx.strokeStyle = item.p.roster_idx < 15 ? 'rgba(96,165,250,0.45)' : 'rgba(248,113,113,0.45)';
        ctx.setLineDash([4 * dpr, 4 * dpr]);
        ctx.beginPath();
        ctx.moveTo(px, py);
        ctx.lineTo(tx, ty);
        ctx.stroke();
        ctx.setLineDash([]);
      }
    }

    // Track frame-to-frame micro-physics FX (rim clank, glass flash, net swish, floor bounce)
    const nowMs = performance.now();
    const curZ = (baseFrame.ball_z_mm ?? 0) / 1000;
    const curFlags = baseFrame.ball_flags ?? 0;
    if (this.prevTick < 0 || baseFrame.tick < this.prevTick || baseFrame.tick - this.prevTick > 10) {
      this.fx.length = 0;
    } else if (baseFrame.tick > this.prevTick) {
      const endSide = baseFrame.ball_x_mm >= 0 ? 1 : -1;
      const rimX = endSide * (length / 2 - this.court.rim);
      const boardX = endSide * (length / 2 - this.court.board);
      if ((curFlags & 0x02) !== 0 && (this.prevBallFlags & 0x02) === 0) {
        this.fx.push({ kind: 'board', x: boardX, y: 0, t0: nowMs, dur: 380 });
      }
      if ((curFlags & 0x01) !== 0 && (this.prevBallFlags & 0x01) === 0) {
        this.fx.push({ kind: 'rim', x: rimX, y: 0, t0: nowMs, dur: 420 });
      }
      const dHome = (baseFrame.score_home ?? 0) - this.prevScores[0];
      const dAway = (baseFrame.score_away ?? 0) - this.prevScores[1];
      const scoreDelta = Math.max(0, dHome) + Math.max(0, dAway);
      const scoredRising = (curFlags & 0x10) !== 0 && (this.prevBallFlags & 0x10) === 0;
      if (scoredRising || scoreDelta > 0) {
        const recent = this.fx.find((f) => f.kind === 'swish' && nowMs - f.t0 < 250);
        if (recent) {
          if (scoreDelta > 0) recent.pts = scoreDelta;
        } else {
          this.fx.push({ kind: 'swish', x: rimX, y: 0, pts: scoreDelta || 0, t0: nowMs, dur: 850 });
        }
      }
      if (baseFrame.ball_state === 4 && this.prevBallState === 4 && this.prevBallZ > 0.32 && curZ <= 0.22) {
        this.fx.push({ kind: 'bounce', x: baseFrame.ball_x_mm / 1000, y: baseFrame.ball_y_mm / 1000, t0: nowMs, dur: 360 });
      }
    }
    this.prevTick = baseFrame.tick;
    this.prevBallFlags = curFlags;
    this.prevScores = [baseFrame.score_home ?? 0, baseFrame.score_away ?? 0];
    this.prevBallZ = curZ;
    this.prevBallState = baseFrame.ball_state ?? 5;
    this.fx = this.fx.filter((f) => nowMs - f.t0 < f.dur);

    // Draw transient court/hoop impact effects behind players
    for (const f of this.fx) {
      const p = (nowMs - f.t0) / f.dur;
      const a = Math.max(0, 1 - p);
      if (f.kind === 'bounce') {
        const [fxPx, fyPx] = toPx(f.x, f.y);
        ctx.strokeStyle = `rgba(251, 191, 36, ${(0.55 * a).toFixed(2)})`;
        ctx.lineWidth = 1.8 * dpr;
        ctx.beginPath();
        ctx.arc(fxPx, fyPx, (0.20 + p * 0.45) * scale, 0, Math.PI * 2);
        ctx.stroke();
      } else if (f.kind === 'board') {
        const [bxPx, by0] = toPx(f.x, -this.court.boardWidth * 0.5);
        const [, by1] = toPx(f.x, this.court.boardWidth * 0.5);
        ctx.strokeStyle = `rgba(224, 242, 254, ${(0.88 * a).toFixed(2)})`;
        ctx.lineWidth = (4.0 * dpr) * (0.6 + 0.5 * a);
        ctx.beginPath();
        ctx.moveTo(bxPx, by0);
        ctx.lineTo(bxPx, by1);
        ctx.stroke();
      } else if (f.kind === 'rim') {
        const [rxPx, ryPx] = toPx(f.x, f.y);
        const rBase = this.court.rimRadius * scale;
        ctx.strokeStyle = `rgba(251, 146, 60, ${(0.85 * a).toFixed(2)})`;
        ctx.lineWidth = 2.6 * dpr;
        ctx.beginPath();
        ctx.arc(rxPx, ryPx, rBase * (1 + p * 1.4), 0, Math.PI * 2);
        ctx.stroke();
      } else if (f.kind === 'swish') {
        const [rxPx, ryPx] = toPx(f.x, f.y);
        const rBase = this.court.rimRadius * scale;
        ctx.strokeStyle = `rgba(74, 222, 128, ${(0.82 * a).toFixed(2)})`;
        ctx.lineWidth = 2.6 * dpr;
        ctx.beginPath();
        ctx.arc(rxPx, ryPx, rBase * (1 + p * 2.1), 0, Math.PI * 2);
        ctx.stroke();
        if (f.pts > 0) {
          ctx.fillStyle = `rgba(250, 204, 21, ${(Math.min(1, a * 1.3)).toFixed(2)})`;
          ctx.font = `800 ${Math.round((compact ? 11 : 14) * dpr)}px -apple-system, sans-serif`;
          ctx.textAlign = 'center';
          ctx.textBaseline = 'middle';
          ctx.fillText(`+${f.pts}`, rxPx, ryPx - (10 + p * 22) * dpr);
        }
      }
    }

    // Draw players
    for (const item of interpPlayers) {
      if (!item) continue;
      const { slot: s, p, xm, ym, ang } = item;
      const isHome = p.roster_idx < 15;
      const [px, py] = toPx(xm, ym);
      const rad = Math.max(0.40 * scale, (compact ? 6 : 8) * dpr);
      const hasBall = baseFrame.ball_owner_slot === s;
      const isScreening = p.visible_action === 6 || (p.public_flags & 0x02) !== 0;
      const isAirborne = (p.public_flags & 0x04) !== 0;
      const inContact = (p.public_flags & 0x40) !== 0;
      const drawY = isAirborne ? py - 5.2 * dpr : py;
      const drawRad = isAirborne ? rad * 1.08 : rad;
      const info = this.rosterMap.get(p.roster_idx);
      const jersey = info?.jersey_number_code === 100 ? '00' : (info?.jersey_number_code ?? p.roster_idx);

      // Ground shadow when jumping / contesting / shooting
      if (isAirborne) {
        ctx.fillStyle = 'rgba(0, 0, 0, 0.36)';
        ctx.beginPath();
        ctx.ellipse(px, py + rad * 0.35, rad * 0.95, rad * 0.5, 0, 0, Math.PI * 2);
        ctx.fill();
      }

      // Subtle tactical action halos
      if (p.visible_action === 4) {
        ctx.strokeStyle = 'rgba(56, 189, 248, 0.65)';
        ctx.lineWidth = 2.2 * dpr;
        ctx.beginPath();
        ctx.arc(px, drawY, drawRad * 1.38, -ang - 0.85, -ang + 0.85);
        ctx.stroke();
      } else if (p.visible_action === 5) {
        ctx.strokeStyle = 'rgba(251, 146, 60, 0.72)';
        ctx.lineWidth = 2.4 * dpr;
        ctx.beginPath();
        ctx.arc(px, drawY, drawRad * 1.32, 0, Math.PI * 2);
        ctx.stroke();
      } else if (p.visible_action === 7) {
        ctx.strokeStyle = 'rgba(34, 211, 238, 0.65)';
        ctx.lineWidth = 1.8 * dpr;
        ctx.setLineDash([3 * dpr, 3 * dpr]);
        ctx.beginPath();
        ctx.arc(px, drawY, drawRad * 1.32, 0, Math.PI * 2);
        ctx.stroke();
        ctx.setLineDash([]);
      } else if (p.visible_action === 14 || p.visible_action === 15) {
        ctx.strokeStyle = 'rgba(248, 113, 113, 0.65)';
        ctx.lineWidth = 2.0 * dpr;
        ctx.beginPath();
        ctx.arc(px, drawY, drawRad * 1.34, -ang - 1.0, -ang + 1.0);
        ctx.stroke();
      }

      // Facing vector
      ctx.strokeStyle = hasBall ? '#fde047' : 'rgba(255,255,255,0.68)';
      ctx.lineWidth = (hasBall ? 2.8 : 1.4) * dpr;
      ctx.beginPath();
      ctx.moveTo(px, drawY);
      ctx.lineTo(px + Math.cos(ang) * drawRad * 1.42, drawY - Math.sin(ang) * drawRad * 1.42);
      ctx.stroke();

      // Screen Wall T-Bar visual cue (#facc15)
      if (isScreening) {
        const fx = Math.cos(ang);
        const fy = -Math.sin(ang);
        const bx = -fy;
        const by = fx;
        const barX = px + fx * drawRad * 1.38;
        const barY = drawY + fy * drawRad * 1.38;
        const halfBar = drawRad * 1.2;
        ctx.strokeStyle = '#facc15';
        ctx.lineWidth = 2.6 * dpr;
        ctx.lineCap = 'round';
        ctx.beginPath();
        ctx.moveTo(px + fx * drawRad * 0.95, drawY + fy * drawRad * 0.95);
        ctx.lineTo(barX, barY);
        ctx.moveTo(barX - bx * halfBar, barY - by * halfBar);
        ctx.lineTo(barX + bx * halfBar, barY + by * halfBar);
        ctx.stroke();
        ctx.lineCap = 'butt';
      }

      // Player halo when holding ball
      if (hasBall) {
        ctx.strokeStyle = 'rgba(250, 204, 21, 0.42)';
        ctx.lineWidth = 3 * dpr;
        ctx.beginPath();
        ctx.arc(px, drawY, drawRad * 1.3, 0, Math.PI * 2);
        ctx.stroke();
      }

      // Player circle
      ctx.fillStyle = isHome ? '#1d4ed8' : '#b91c1c';
      ctx.beginPath();
      ctx.arc(px, drawY, drawRad, 0, Math.PI * 2);
      ctx.fill();
      ctx.strokeStyle = hasBall ? '#facc15' : (isScreening ? '#fde047' : (inContact ? '#fb923c' : '#f8fafc'));
      ctx.lineWidth = (hasBall || inContact ? 2.4 : 1.6) * dpr;
      ctx.stroke();

      // Jersey number
      ctx.fillStyle = '#ffffff';
      ctx.font = `700 ${Math.max(8, Math.round(drawRad * 0.92))}px -apple-system, sans-serif`;
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.fillText(String(jersey), px, drawY + 0.5 * dpr);

      if (!compact || hasBall || isScreening) {
        const shortName = info ? info.display_name.split(' ').pop() : `#${jersey}`;
        const act = ACTION_LABELS[p.visible_action] || '';
        const labelTxt = compact ? act : `${shortName} · ${act}`;
        const labelFont = Math.max((compact ? 9 : 10) * dpr, Math.round(rad * 0.72));
        ctx.font = `600 ${labelFont}px -apple-system, BlinkMacSystemFont, "PingFang SC", sans-serif`;
        const pillW = ctx.measureText(labelTxt).width + 6 * dpr;
        const pillY = py + rad + 2 * dpr;
        ctx.fillStyle = 'rgba(11, 16, 23, 0.72)';
        ctx.fillRect(px - pillW / 2, pillY, pillW, labelFont + 4 * dpr);
        ctx.fillStyle = isScreening ? '#fde047' : '#f1f5f9';
        ctx.textBaseline = 'top';
        ctx.fillText(labelTxt, px, pillY + 2 * dpr);
      }

      if (this.showDebug && debugFrame && debugFrame.player_debug[s]) {
        const ds = debugFrame.player_debug[s];
        const barW = rad * 2;
        const bx0 = px - rad;
        const by0 = drawY - drawRad - 7 * dpr;
        ctx.fillStyle = 'rgba(15,23,42,0.85)';
        ctx.fillRect(bx0, by0, barW, 4 * dpr);
        ctx.fillStyle = '#22c55e';
        ctx.fillRect(bx0, by0, barW * (ds.cum_stamina / 100), 4 * dpr);
      }
    }

    // Physical body-contact clash indicators `)(` between colliding opposing players
    for (let i = 0; i < 5; i++) {
      const a = interpPlayers[i];
      if (!a) continue;
      for (let j = 5; j < 10; j++) {
        const b = interpPlayers[j];
        if (!b) continue;
        if (((a.p.public_flags & 0x40) === 0 && (b.p.public_flags & 0x40) === 0)) continue;
        const distM = Math.hypot(b.xm - a.xm, b.ym - a.ym);
        if (distM > 1.25) continue;
        const [axPx, ayPx] = toPx(a.xm, a.ym);
        const [bxPx, byPx] = toPx(b.xm, b.ym);
        const mxPx = (axPx + bxPx) * 0.5;
        const myPx = (ayPx + byPx) * 0.5;
        const dx = bxPx - axPx;
        const dy = byPx - ayPx;
        const angSep = Math.atan2(dy, dx);
        const clashR = Math.max(5 * dpr, 0.22 * scale);
        const hardClash =
          a.p.visible_action === 6 || b.p.visible_action === 6 ||
          a.p.visible_action === 5 || b.p.visible_action === 5 ||
          a.p.visible_action === 4 || b.p.visible_action === 4 ||
          a.p.visible_action === 16 || b.p.visible_action === 16;
        ctx.strokeStyle = hardClash ? 'rgba(250, 204, 21, 0.92)' : 'rgba(251, 146, 60, 0.78)';
        ctx.lineWidth = (hardClash ? 2.4 : 1.8) * dpr;
        const sepOff = Math.max(3 * dpr, 0.14 * scale);
        const ux = Math.cos(angSep);
        const uy = Math.sin(angSep);
        ctx.beginPath();
        ctx.arc(mxPx - ux * sepOff, myPx - uy * sepOff, clashR, angSep - 0.85, angSep + 0.85);
        ctx.stroke();
        ctx.beginPath();
        ctx.arc(mxPx + ux * sepOff, myPx + uy * sepOff, clashR, angSep + Math.PI - 0.85, angSep + Math.PI + 0.85);
        ctx.stroke();
      }
    }

    const zLiftFactor = scale * 0.42;

    // Pass trajectory dotted line during live pass flight
    if (baseFrame.ball_state === 2 && trail && trail.length > 1) {
      ctx.strokeStyle = 'rgba(253, 224, 71, 0.78)';
      ctx.lineWidth = 2.0 * dpr;
      ctx.setLineDash([5 * dpr, 4 * dpr]);
      ctx.beginPath();
      for (let i = 0; i < trail.length; i++) {
        const pt = trail[i];
        const [pxPt, pyPt] = toPx(pt.x, pt.y);
        const liftPtY = pyPt - Math.max(0, pt.z) * zLiftFactor;
        if (i === 0) ctx.moveTo(pxPt, liftPtY);
        else ctx.lineTo(pxPt, liftPtY);
      }
      ctx.stroke();
      ctx.setLineDash([]);
    }

    // Ball motion trail differentiated by pass, shot, or loose rebound carom
    if (trail && trail.length > 1) {
      for (let i = 0; i < trail.length - 1; i++) {
        const pt = trail[i];
        const [txPx, tyPx] = toPx(pt.x, pt.y);
        const tLiftY = tyPx - Math.max(0, pt.z) * zLiftFactor;
        const ratio = (i + 1) / trail.length;
        const color = pt.state === 2
          ? `rgba(253, 224, 71, ${(0.14 + 0.25 * ratio).toFixed(2)})`
          : pt.state === 4
            ? `rgba(254, 215, 170, ${(0.14 + 0.24 * ratio).toFixed(2)})`
            : `rgba(249, 115, 22, ${(0.16 + 0.34 * ratio).toFixed(2)})`;
        ctx.fillStyle = color;
        ctx.beginPath();
        ctx.arc(txPx, tLiftY, Math.max(2 * dpr, 0.16 * scale * ratio), 0, Math.PI * 2);
        ctx.fill();
      }
    }

    // Interpolated basketball + 3D shadow + high-arc depth drop cue
    let bxm = baseFrame.ball_x_mm / 1000;
    let bym = baseFrame.ball_y_mm / 1000;
    let bzm = baseFrame.ball_z_mm / 1000;
    if (fA !== fB && Math.hypot(fB.ball_x_mm - fA.ball_x_mm, fB.ball_y_mm - fA.ball_y_mm) < 7500) {
      bxm = lerp(fA.ball_x_mm, fB.ball_x_mm, t) / 1000;
      bym = lerp(fA.ball_y_mm, fB.ball_y_mm, t) / 1000;
      bzm = lerp(fA.ball_z_mm, fB.ball_z_mm, t) / 1000;
    }

    const [bxPx, byPx] = toPx(bxm, bym);
    const zMeters = Math.max(0, bzm);
    const ballRad = Math.max(0.24 * scale, 3.8 * dpr) * (1 + Math.min(0.55, zMeters * 0.095));
    const liftY = byPx - zMeters * zLiftFactor;

    // Height drop guide when ball rises above rim height
    if (zMeters > 2.3) {
      ctx.strokeStyle = 'rgba(255, 255, 255, 0.16)';
      ctx.lineWidth = 1 * dpr;
      ctx.setLineDash([2 * dpr, 3 * dpr]);
      ctx.beginPath();
      ctx.moveTo(bxPx, liftY);
      ctx.lineTo(bxPx, byPx);
      ctx.stroke();
      ctx.setLineDash([]);
    }

    // Shadow on court (shrinks and softens as ball climbs higher)
    const shScale = Math.max(0.45, 1.0 - zMeters * 0.08);
    ctx.fillStyle = `rgba(0,0,0,${(0.36 * shScale).toFixed(2)})`;
    ctx.beginPath();
    ctx.ellipse(bxPx, byPx + 2 * dpr, ballRad * 0.92 * shScale, ballRad * 0.52 * shScale, 0, 0, Math.PI * 2);
    ctx.fill();

    // Elevated basketball
    ctx.fillStyle = '#f97316';
    ctx.beginPath();
    ctx.arc(bxPx, liftY, ballRad, 0, Math.PI * 2);
    ctx.fill();
    ctx.strokeStyle = '#431407';
    ctx.lineWidth = 1.5 * dpr;
    ctx.stroke();

    // Film-room canvas badge for live playbook and defensive gameplan
    if (this.showDebug && debugFrame) {
      const playKey = header?.play_dict?.find(([i]) => i === debugFrame.active_play_idx)?.[1] ?? '';
      const defKey = header?.def_dict?.find(([i]) => i === debugFrame.active_def_rule_idx)?.[1] ?? '';
      const playZh = PLAYBOOK_LABELS[playKey] || playKey || '未指定';
      const defZh = DEFENSE_LABELS[defKey] || defKey || '默认防守';
      const badgeTxt = `战术: ${playZh}  |  防守: ${defZh}`;
      const badgeFont = Math.round((compact ? 10 : 12) * dpr);
      ctx.font = `600 ${badgeFont}px -apple-system, BlinkMacSystemFont, "PingFang SC", sans-serif`;
      const bw = ctx.measureText(badgeTxt).width + 16 * dpr;
      const bh = badgeFont + 10 * dpr;
      const bx = 10 * dpr;
      const by = 10 * dpr;
      ctx.fillStyle = 'rgba(11, 16, 23, 0.80)';
      ctx.fillRect(bx, by, bw, bh);
      ctx.strokeStyle = 'rgba(250, 204, 21, 0.55)';
      ctx.lineWidth = 1 * dpr;
      ctx.strokeRect(bx, by, bw, bh);
      ctx.fillStyle = '#f8fafc';
      ctx.textAlign = 'left';
      ctx.textBaseline = 'middle';
      ctx.fillText(badgeTxt, bx + 8 * dpr, by + bh / 2);
    }
  }
}
