import { decodeHps1Header, decodeHps1PlusRawBundle, decodePublicFrameV1 } from './codec.js';
import { CourtRenderer, PHASE_NAMES } from './renderer.js';

const RULE_NAMES_ZH = { nba: 'NBA · 48 分钟', fiba: 'FIBA · 40 分钟' };
const TEAM_NAMES_ZH = {
  boston_five_out: '波士顿五外', bay_motion_split: '湾区传切',
  mile_high_hub: '丹佛高位', twin_towers_grit: '铁血双塔',
};
const PLAYBOOK_NAMES_ZH = {
  spread_pnr: '拉开空间挡掩护', horns_flare: '牛角外弹掩护', split_action: '弱侧无球传切',
  high_post_hub: '高位策应', five_out_delay: '五外手递手', spain_pnr: '西班牙挡掩护',
  post_iso: '低位单打', ato_quick_three: '暂停后三分', freelance: '自由流动进攻',
};
const DEFENSE_NAMES_ZH = {
  drop_base: '沉退保护篮筐', switch_all: '全员换防', blitz_heavy: '强侧夹击', zone_2_3: '2-3 联防',
};
const MODE_NAMES_ZH = { live: '实时直播', batch_full_trace: '完整录像', batch_fast: '极速统计' };
const LIFECYCLE_ZH = { running: '进行中', finished: '已完成', aborted: '已中止' };
const LIVE_FRAME_LIMIT = 6000;
const ARCHIVE_WINDOW = 1800;
const $ = (id) => document.getElementById(id);
const canvas = $('court-canvas');
const renderer = new CourtRenderer(canvas);
const state = {
  auth: null, catalog: null, sim: null, ws: null, ready: false, retry: 0, retryTimer: null,
  selection: null, selecting: false, launching: false, changingRole: false, submitting: false,
  refreshing: false, controllingPlayback: false, loadingArchive: false, archiveTask: null,
  archived: false, archiveEnd: 0, header: null, frames: [], previewFrame: null, debugFrames: null,
  liveFollow: true, paused: true, livePaused: false, freezeTick: null, playhead: 0,
  livePace: 1, replaySpeed: 1, lastDrawnIdx: -1, lastRafMs: 0, ballTrail: [],
  activeChallenge: null, clientReqSeq: 0, latestEvents: [], afterEventSeq: 0, pollTimer: null,
  archiveCache: null, atArchiveEnd: false, seekTimer: null,
};

function labelOf(dict, key) {
  return Object.hasOwn(dict, key) ? dict[key] : key;
}

function element(tag, children, className) {
  const el = document.createElement(tag);
  if (className) el.className = className;
  if (Array.isArray(children)) el.append(...children);
  else if (children !== undefined) el.textContent = children;
  return el;
}

function teamName(id, placeholder) {
  return id ? labelOf(TEAM_NAMES_ZH, id) : placeholder;
}

function setNotice(text, error = false) {
  const el = $('action-status');
  el.textContent = text;
  el.classList.toggle('error', error);
  el.hidden = !text;
}

function setConnStatus(text) {
  $('conn-status').textContent = text;
}

async function apiFetch(path, { body, signal, auth = state.auth } = {}) {
  const headers = new Headers();
  if (auth) headers.set('Authorization', `Bearer ${auth.token}`);
  if (body) headers.set('Content-Type', 'application/json');
  const res = await fetch(path, { headers, signal, method: body ? 'POST' : 'GET', body: body && JSON.stringify(body) });
  if (!res.ok) {
    const data = (res.headers.get('content-type') ?? '').includes('application/json') ? await res.json() : null;
    const error = new Error(data?.error?.message ?? data?.reason ?? `请求失败（HTTP ${res.status}）`);
    error.code = data?.error?.code;
    error.status = res.status;
    throw error;
  }
  return res;
}

async function apiJson(path, opts) {
  const response = await apiFetch(path, opts);
  return response.json();
}

function simPath(sim = state.sim) {
  return `/api/v1/simulations/${encodeURIComponent(sim.sim_id)}`;
}

async function acquireToken(role) {
  const data = await apiJson('/api/v1/auth/demo-token', { body: { role }, auth: null });
  return { role, sub: data.sub, token: data.token };
}

function populate(select, options, labels) {
  select.replaceChildren(...options.map(([id, name]) => new Option(labelOf(labels, id) === id ? name : labels[id], id)));
}

async function loadCatalog() {
  const cat = await apiJson('/api/v1/catalog');
  state.catalog = cat;
  populate($('cfg-rule'), cat.rules.map((rule) => [rule.rule_id, rule.name]), RULE_NAMES_ZH);
  const teams = cat.teams.map((team) => [team.team_id, team.team_id]);
  populate($('cfg-home'), teams, TEAM_NAMES_ZH);
  populate($('cfg-away'), teams, TEAM_NAMES_ZH);
  if (teams.length > 1) $('cfg-away').selectedIndex = 1;
  populate($('coach-play'), cat.playbooks.map((play) => [play.play_id, play.play_id]), PLAYBOOK_NAMES_ZH);
  populate($('coach-def'), cat.defenses.map((defense) => [defense.gameplan_id, defense.gameplan_id]), DEFENSE_NAMES_ZH);
  updateLaunchFields();
}

async function refreshHealth() {
  const health = await apiJson('/api/v1/health');
  const mib = (bytes) => (bytes / 1048576).toFixed(1);
  const rows = [
    ['服务状态', health.ok ? '正常在线' : '异常'],
    ['监听地址', health.bind],
    ['直播场次', `${health.active_live_games} / ${health.max_live_games}`],
    ['直播连接数', `${health.active_ws_connections} / ${health.max_ws_connections}`],
    ['WAL 日志序号', health.wal_seq],
    ['实时内存池', `${mib(health.pools.live_used_bytes)} / ${mib(health.pools.live_cap_bytes)} MiB`],
  ];
  $('diag-summary').replaceChildren(...rows.map(([key, value]) => element('div', [element('span', key), element('b', value)], 'diag-kv')));
  $('diag-json').textContent = JSON.stringify(health, null, 2);
}

function updateLaunchFields() {
  const live = $('cfg-mode').value === 'live';
  $('cfg-pace-field').hidden = !live;
  $('cfg-pace').disabled = !live;
  $('cfg-max-ticks-field').hidden = live;
  $('cfg-max-ticks').disabled = live;
  for (const id of ['cfg-home', 'cfg-away']) {
    const other = $(id === 'cfg-home' ? 'cfg-away' : 'cfg-home').value;
    for (const option of $(id).options) option.disabled = option.value === other;
  }
  if (!state.sim && state.catalog) {
    renderer.setRule($('cfg-rule').value);
    $('court-title').textContent = `${renderer.court.name} 球场`;
    resetScore();
    drawAt(0);
  }
}

function formatClock(ds) {
  return `${String(Math.floor(ds / 600)).padStart(2, '0')}:${String(Math.floor(ds / 10) % 60).padStart(2, '0')}.${ds % 10}`;
}

function updateScorebug(frame) {
  $('sb-quarter').textContent = frame.quarter <= 4 ? `第 ${frame.quarter} 节` : `加时 ${frame.quarter - 4}`;
  $('sb-game-clock').textContent = formatClock(frame.game_clock_ds);
  $('sb-shot-clock').textContent = frame.shot_clock_ds < 0xfffe ? (frame.shot_clock_ds / 10).toFixed(1) : '—';
  $('sb-home-score').textContent = frame.score_home;
  $('sb-away-score').textContent = frame.score_away;
  $('sb-phase').textContent = PHASE_NAMES[frame.game_phase];
  $('sb-tick').textContent = frame.tick ?? '—';
  $('sb-poss').textContent = ['球权：主队', '球权：客队', '球权：争球中'][frame.possession_side] ?? '球权：—';
}

function resetScore() {
  const sim = state.sim;
  const rule = state.catalog?.rules.find((item) => item.rule_id === $('cfg-rule').value);
  $('sb-home-name').textContent = teamName(sim?.home_team_id, '主队');
  $('sb-away-name').textContent = teamName(sim?.away_team_id, '客队');
  updateScorebug(sim ?? { quarter: 1, game_clock_ds: (rule?.quarter_sec ?? 720) * 10, score_home: 0, score_away: 0, shot_clock_ds: 0xffff, game_phase: 0 });
  $('tick-slider').min = $('tick-slider').max = $('tick-slider').value = '0';
  $('slider-label').textContent = '暂无帧';
}

function liveRunning() {
  return state.sim?.kind === 'live' && state.sim.lifecycle === 'running' && !state.archived;
}

function isParticipant() {
  const auth = state.auth;
  if (!state.sim || !auth || auth.role === 'spectator') return false;
  const participant = state.sim[`${auth.role}_participant_id`];
  return participant === '*' || participant === auth.sub;
}

function canControlLive() {
  return isParticipant() && state.sim.owner_user_id === state.auth.sub;
}

function updatePlaybackControls(live, busy, reviewing, frozen) {
  const anyFrames = state.frames.length > 0;
  const liveControl = live && !reviewing && canControlLive();
  const play = $('btn-play-pause');
  play.disabled = busy || state.controllingPlayback || !anyFrames || (liveControl && !state.ready);
  play.title = liveControl ? '暂停或继续服务器模拟' : '控制当前页面的画面播放';
  if (liveControl) play.textContent = frozen ? '继续模拟' : '暂停模拟';
  else if (live && !reviewing && frozen) {
    play.textContent = '回看画面';
    play.disabled = busy || state.frames.length < 2;
  } else play.textContent = state.paused ? '播放画面' : '暂停画面';
  const slider = $('tick-slider');
  slider.disabled = state.selecting || state.changingRole || state.controllingPlayback || !anyFrames || slider.max === slider.min;
  $('replay-speed').disabled = busy || !anyFrames || (live && !reviewing);
  const liveTail = $('btn-live-tail');
  liveTail.classList.toggle('hidden', !live || (state.ready && state.liveFollow));
  liveTail.textContent = state.ready ? '回到实时' : '重新连接';
  liveTail.disabled = busy || state.controllingPlayback || (!state.ready && Boolean(state.ws));
  $('chk-debug').disabled = busy || !state.archived || !state.sim.published_debug_uri || state.auth.role === 'spectator';
}

function updateCoachControls(live, busy, reviewing, frozen) {
  const participant = isParticipant();
  const allowCoach = live && state.ready && !reviewing && !busy && participant && !state.submitting && !state.controllingPlayback;
  $('coach-fields').disabled = !allowCoach;
  const challenge = state.activeChallenge;
  const waitingForRuling = challenge !== null;
  for (const id of ['btn-timeout', 'btn-set-play', 'btn-set-def', 'btn-set-pace']) {
    $(id).disabled = frozen || waitingForRuling;
  }
  const side = state.auth?.role === 'home' ? 0 : 1;
  $('btn-challenge').disabled = frozen || challenge?.window_state !== 0 || !(challenge.eligible_sides_mask & (1 << side));
  $('btn-withdraw').disabled = frozen || challenge?.window_state !== 1 || challenge.grant_requester_side !== side;
  const banner = $('challenge-banner');
  banner.classList.toggle('hidden', !live || !challenge || reviewing);
  if (challenge) {
    const action = challenge.window_state === 1 ? '等待裁判确认挑战' : '挑战窗口已开启';
    banner.textContent = `${action} · 裁决 #${challenge.ruling_id}${frozen ? ' · 模拟已暂停' : ''}`;
  }
  const status = $('coach-status');
  if (!live) status.textContent = '选择进行中的直播后可执教。';
  else if (!participant) status.textContent = '当前身份仅可观战。';
  else if (reviewing) status.textContent = '回到实时画面后可执教。';
  else if (!state.ready) status.textContent = '等待直播连接。';
  else if (frozen) status.textContent = '继续模拟后可提交战术和球队暂停指令。';
  else if (waitingForRuling) status.textContent = '等待裁决结束后可提交战术和球队暂停指令。';
  else status.textContent = '指令由服务器按比赛规则处理。';
}

function courtStatus(live, reviewing, frozen) {
  if (state.selecting) return '正在载入对局';
  const sim = state.sim;
  if (!sim) return '等待开始模拟';
  if (sim.lifecycle === 'aborted') return '对局已中止';
  if (sim.kind === 'batch_fast') return '仅提供技术统计';
  if (state.archived) return state.paused ? '录像已暂停' : '正在播放录像';
  if (!live) return sim.archive_status.status === 'archive_failed' ? '录像无法载入' : '正在准备录像';
  if (!state.ready) return '正在等待直播帧';
  if (reviewing) return '直播回看';
  return frozen ? '模拟已暂停' : '直播进行中';
}

function updateDownloadLinks(busy) {
  for (const [id, uri, endpoint] of [
    ['dl-replay', state.sim?.published_replay_uri, 'replay'],
    ['dl-frames', state.sim?.published_artifact_uri, 'frames'],
  ]) {
    const link = $(id);
    const available = !busy && uri && state.sim.lifecycle === 'finished';
    link.classList.toggle('hidden', !available);
    if (available) link.href = `${simPath()}/${endpoint}?token=${encodeURIComponent(state.auth.token)}`;
    else link.removeAttribute('href');
  }
}

function updateControls() {
  const live = liveRunning();
  const busy = state.changingRole || state.selecting || state.loadingArchive || state.launching;
  const reviewing = live && !state.liveFollow;
  const frozen = live && state.livePaused;
  $('launch-fields').disabled = !state.catalog || !state.auth || state.auth.role === 'spectator' || busy || state.launching || state.controllingPlayback || state.submitting;
  $('role-select').disabled = !state.auth || busy || state.launching || state.controllingPlayback || state.submitting;
  $('btn-refresh-sims').disabled = !state.auth || state.refreshing || state.changingRole;
  updatePlaybackControls(live, busy, reviewing, frozen);
  updateCoachControls(live, busy, reviewing, frozen);
  $('court-status').textContent = courtStatus(live, reviewing, frozen);
  updateDownloadLinks(busy);
}

function updateSliderUi(idx) {
  const frame = state.frames[idx];
  if (!frame) return;
  const slider = $('tick-slider');
  const lastTick = state.archived ? state.archiveEnd : state.frames.at(-1).tick;
  slider.min = state.archived ? '0' : String(state.frames[0].tick);
  slider.max = String(lastTick);
  if (state.seekTimer === null && !state.loadingArchive) {
    slider.value = String(frame.tick);
    $('slider-label').textContent = `${frame.tick} / ${lastTick} 帧`;
  }
  updateScorebug(frame);
}

function frameIndex(tick) {
  let lo = 0;
  let hi = state.frames.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (state.frames[mid].tick <= tick) lo = mid + 1;
    else hi = mid;
  }
  return Math.max(0, lo - 1);
}

function drawAt(idx) {
  state.playhead = Math.max(0, Math.min(idx, state.frames.length - 1));
  state.lastDrawnIdx = -1;
  state.ballTrail = [];
  drawCurrent();
}

function drawCurrent() {
  if (!state.frames.length || (state.previewFrame && !state.ready && state.liveFollow)) {
    renderer.render(state.previewFrame, state.header);
    return;
  }
  const idx = Math.floor(state.playhead);
  if (Math.abs(idx - state.lastDrawnIdx) > 2) state.ballTrail.length = 0;
  const first = state.frames[idx];
  const next = state.frames[idx + 1];
  const second = next?.tick === first.tick + 1 ? next : first;
  const alpha = state.playhead - idx;
  const moving = !state.paused && !state.loadingArchive && !(state.liveFollow && state.livePaused);
  if (moving && second !== first && (first.ball_state === 2 || first.ball_state === 3 || first.ball_state === 4)) {
    const mix = (a, b) => (a + (b - a) * alpha) / 1000;
    const bx = mix(first.ball_x_mm, second.ball_x_mm);
    const by = mix(first.ball_y_mm, second.ball_y_mm);
    const bz = mix(first.ball_z_mm, second.ball_z_mm);
    const prev = state.ballTrail.at(-1);
    if (!prev || Math.hypot(bx - prev.x, by - prev.y) < 8.5) {
      state.ballTrail.push({ x: bx, y: by, z: bz, state: first.ball_state });
      if (state.ballTrail.length > 18) state.ballTrail.shift();
    } else state.ballTrail.length = 0;
  } else state.ballTrail.length = 0;
  renderer.renderInterpolated(first, second, alpha, state.header, state.debugFrames?.[idx], state.ballTrail);
  if (state.lastDrawnIdx !== idx) {
    state.lastDrawnIdx = idx;
    updateSliderUi(idx);
  }
}

function stepRaf(now) {
  const elapsed = state.lastRafMs ? Math.min(0.10, (now - state.lastRafMs) / 1000) : 0;
  state.lastRafMs = now;
  const catchingUp = liveRunning() && !state.ready && state.liveFollow;
  if (state.frames.length && !catchingUp && !state.paused && !state.loadingArchive && !(state.liveFollow && state.livePaused)) {
    const last = state.frames.length - 1;
    const prev = Math.floor(state.playhead);
    if (liveRunning() && state.liveFollow) {
      const lag = last - state.playhead;
      const lead = Math.max(1, state.livePace * 0.45);
      if (lag > 28) drawAt(Math.max(0, last - Math.ceil(lead)));
      else state.playhead = Math.min(last, state.playhead + elapsed * state.livePace * 10 * Math.max(0.65, Math.min(1.45, 1 + (lag - lead) * 0.12)));
    } else {
      state.playhead = Math.min(last, state.playhead + elapsed * state.replaySpeed * 10);
      if (state.playhead >= last) {
        const tick = state.frames.at(-1).tick;
        if (state.archived && tick < state.archiveEnd) {
          void seekArchive(tick + 1, false);
        } else {
          state.paused = true;
          state.atArchiveEnd = state.archived;
          updateControls();
        }
      }
    }
    if (prev !== Math.floor(state.playhead) || state.playhead < last) drawCurrent();
  }
  requestAnimationFrame(stepRaf);
}

function emptyHint(id, text) {
  const el = document.createElement('div');
  el.className = 'empty-hint';
  el.textContent = text;
  $(id).replaceChildren(el);
}

function renderPbpList(items) {
  if (!items.length) {
    emptyHint('pbp-list', '等待比赛事件。');
    return;
  }
  const entries = items.toReversed().map((item) => element('div', [
    element('span', `#${item.event_seq} · 帧 ${item.tick}`, 'pbp-meta'), item.pbp_zh,
  ], 'pbp-item'));
  $('pbp-list').replaceChildren(...entries);
}

function renderBoxScore(box) {
  const rows = box.players.filter((p) => p.player_id && (p.court_time_ds > 0 || p.fga > 0 || p.fta > 0 || p.pf > 0)).map((p) => {
    const side = p.roster_idx < 15 ? '主' : '客';
    const jersey = p.jersey_number_code === 100 ? '00' : p.jersey_number_code;
    const columns = [`${side} #${jersey} ${p.display_name}`, (p.court_time_ds / 600).toFixed(1), p.pts,
      `${p.fgm}/${p.fga}`, `${p.tpm}/${p.tpa}`, `${p.ftm}/${p.fta}`, p.oreb + p.dreb, p.ast, p.stl, p.blk, p.tov, p.pf];
    return element('tr', columns.map((value) => element('td', value)));
  });
  const summary = [box.home, box.away].map((team) => element('span', [element('b', teamName(team.team_id)),
    `：${team.score_official} 分 · 投篮 ${team.fgm}/${team.fga} · 三分 ${team.tpm}/${team.tpa} · 篮板 ${team.oreb + team.dreb} · 助攻 ${team.ast} · 失误 ${team.tov}`,
  ]));
  const wrapper = element('div', summary, 'box-summary');
  const boxContainer = $('box-container');
  let scroll = boxContainer.querySelector('.box-table-scroll');
  if (!scroll) {
    const columns = ['球员', '分钟', '得分', '投篮', '三分', '罚球', '篮板', '助攻', '抢断', '盖帽', '失误', '犯规'];
    const heading = element('thead', [element('tr', columns.map((name) => element('th', name)))]);
    scroll = element('div', [element('table', [heading, element('tbody')], 'box-table')], 'box-table-scroll');
    scroll.tabIndex = 0;
    scroll.setAttribute('aria-label', '球员技术统计，可横向滚动');
    boxContainer.replaceChildren(wrapper, scroll);
  } else boxContainer.firstElementChild.replaceWith(wrapper);
  if (!rows.length) {
    const cell = element('td', '等待球员统计。');
    cell.colSpan = 12;
    rows.push(element('tr', [cell]));
  }
  scroll.querySelector('tbody').replaceChildren(...rows);
}

async function refreshSimList() {
  if (state.refreshing) return;
  state.refreshing = true;
  updateControls();
  try {
    const list = await apiJson('/api/v1/simulations');
    if (!list.length) return emptyHint('sims-container', '暂无对局记录。');
    const items = list.sort((a, b) => b.created_wall_ms - a.created_wall_ms).map((sim) => {
      const button = document.createElement('button');
      button.className = 'sim-item';
      button.classList.toggle('active', sim.sim_id === state.sim?.sim_id);
      button.setAttribute('aria-pressed', String(sim.sim_id === state.sim?.sim_id));
      const phase = sim.playback_paused ? '已暂停' : labelOf(LIFECYCLE_ZH, sim.lifecycle);
      button.append(element('span', [
        element('b', `${teamName(sim.home_team_id)} 对阵 ${teamName(sim.away_team_id)}`), element('br'),
        `${labelOf(MODE_NAMES_ZH, sim.kind)} · ${sim.rule_id.split('_')[0].toUpperCase()} · ${phase}`,
      ]), element('strong', `${sim.score_home} – ${sim.score_away}`));
      button.onclick = () => void selectSimulation(sim);
      return button;
    });
    $('sims-container').replaceChildren(...items);
  } catch (error) {
    setNotice(`读取对局列表失败：${error.message}`, true);
  } finally {
    state.refreshing = false;
    updateControls();
  }
}

function openMobilePanel(panelKey) {
  for (const button of document.querySelectorAll('.mnav-btn')) {
    const active = button.dataset.panel === panelKey;
    button.classList.toggle('active', active);
    button.setAttribute('aria-pressed', String(active));
  }
  for (const card of document.querySelectorAll('[data-mobile-card]')) card.classList.toggle('mobile-active', card.dataset.mobileCard === panelKey);
  const dataTab = ['pbp', 'box', 'diag'].includes(panelKey);
  document.querySelector('.data-tabs').classList.toggle('mobile-active', dataTab);
  if (!dataTab) return;
  const id = `tab-${panelKey}`;
  for (const button of document.querySelectorAll('.tab')) {
    const active = button.dataset.tab === id;
    button.classList.toggle('active', active);
    button.setAttribute('aria-pressed', String(active));
  }
  for (const pane of document.querySelectorAll('.tab-pane')) pane.classList.toggle('active', pane.id === id);
}

function closeWs() {
  const ws = state.ws;
  state.ws = null;
  state.ready = false;
  ws?.close();
  clearTimeout(state.retryTimer);
  state.retryTimer = null;
}

function stopSelection() {
  closeWs();
  state.selection?.abort();
  state.archiveTask?.abort();
  state.archiveTask = null;
  clearTimeout(state.pollTimer);
  clearTimeout(state.seekTimer);
  state.seekTimer = null;
}

function applyLivePlayback(paused, tick = null) {
  state.livePaused = paused;
  state.freezeTick = paused ? tick : null;
  if (state.liveFollow) {
    state.paused = paused;
    if (paused && state.frames.length && state.ready) drawAt(tick === null ? state.frames.length - 1 : frameIndex(tick));
  }
  updateControls();
}

async function selectSimulation(sim, launched = false) {
  if (!state.auth || state.changingRole || state.controllingPlayback || state.submitting || (state.launching && !launched)) return;
  stopSelection();
  const selection = new AbortController();
  state.selection = selection;
  Object.assign(state, {
    sim, selecting: true, archived: false, archiveEnd: 0, loadingArchive: false,
    header: null, frames: [], previewFrame: null, debugFrames: null, liveFollow: true, paused: true,
    livePaused: Boolean(sim.playback_paused), freezeTick: null, playhead: 0, retry: 0,
    livePace: sim.pace_multiplier, latestEvents: [], afterEventSeq: 0, activeChallenge: null,
    atArchiveEnd: false, archiveCache: null,
  });
  $('chk-debug').checked = false;
  renderer.showDebug = false;
  renderer.setRule(sim.rule_id);
  $('court-title').textContent = `${renderer.court.name} 球场`;
  resetScore();
  drawAt(0);
  emptyHint('pbp-list', '正在载入文字战报。');
  emptyHint('box-container', '正在载入技术统计。');
  openMobilePanel(sim.kind === 'batch_fast' ? 'box' : 'pbp');
  updateControls();
  setNotice('');
  if (matchMedia('(max-width: 1024px)').matches && sim.kind !== 'batch_fast') {
    $('court-panel').scrollIntoView({ block: 'nearest' });
  }

  try {
    if (sim.kind === 'batch_fast') emptyHint('pbp-list', '极速统计模式仅提供技术统计。');
    if (sim.lifecycle === 'aborted') {
      setConnStatus('对局已中止');
      setNotice(sim.abort_reason ?? '服务器已中止该对局。', true);
      return;
    }
    if (liveRunning()) {
      const tick = sim.last_tick ?? 0;
      connectLiveWs(selection, Math.max(0, tick - LIVE_FRAME_LIMIT));
      void loadLivePreview(tick, selection);
    }
    await refreshSimDetails(selection, true);
  } catch (error) {
    if (!selection.signal.aborted) setNotice(`载入对局失败：${error.message}`, true);
  } finally {
    if (state.selection === selection) {
      state.selecting = false;
      updateControls();
      void refreshSimList();
    }
  }
}

async function loadLivePreview(tick, selection) {
  const { signal } = selection;
  try {
    const response = await apiFetch(`${simPath()}/frames?format=raw&from_tick=${tick}&to_tick=${tick}`, { signal });
    const bytes = await response.arrayBuffer();
    if (signal.aborted || state.ready || !liveRunning()) return;
    const snapshot = decodeHps1PlusRawBundle(new Uint8Array(bytes));
    state.header ??= snapshot.header;
    state.previewFrame = snapshot.frames[0];
    drawCurrent();
    updateScorebug(state.previewFrame);
  } catch (error) {
    if (!signal.aborted && !state.ready) setNotice(`读取直播画面失败：${error.message}`, true);
  }
}

function connectLiveWs(selection, fromTick = null) {
  if (selection.signal.aborted || !liveRunning()) return;
  closeWs();
  const url = new URL(`/api/v1/ws/simulations/${encodeURIComponent(state.sim.sim_id)}`, window.location.href);
  url.protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
  const last = state.frames.at(-1);
  url.searchParams.set('from_tick', String(fromTick ?? (last ? last.tick + 1 : (state.sim.last_tick ?? 0))));
  url.searchParams.set('after_event_seq', String(state.afterEventSeq));
  url.searchParams.set('token', state.auth.token);
  const ws = new WebSocket(url);
  ws.binaryType = 'arraybuffer';
  state.ws = ws;
  setConnStatus('连接直播中');
  updateControls();

  ws.onmessage = ({ data }) => {
    if (state.ws !== ws) return;
    try {
      if (typeof data === 'string') handleLiveTransport(JSON.parse(data));
      else handleLivePacket(new Uint8Array(data));
    } catch (error) {
      setNotice(`直播数据读取失败：${error.message}`, true);
      ws.close(4000, 'Invalid stream');
    }
  };
  ws.onclose = (event) => {
    if (state.ws !== ws || selection.signal.aborted) return;
    state.ws = null;
    state.ready = false;
    state.activeChallenge = null;
    updateControls();
    if (!liveRunning()) return;
    if (event.code === 1000 && event.reason === 'MatchFinished') {
      setConnStatus('直播结束，正在载入录像');
      return;
    }
    setConnStatus(`直播已断开（${event.code}）`);
    if (state.retry < 3) {
      state.retry += 1;
      state.retryTimer = setTimeout(() => connectLiveWs(selection), state.retry * 700);
    }
  };
}

function handleLiveTransport(event) {
  switch (event.type) {
    case 'session_sync':
      state.livePace = event.pace_multiplier;
      state.activeChallenge = event.active_challenge_window;
      applyLivePlayback(event.playback_paused, event.last_published_frame_tick);
      break;
    case 'catch_up_complete':
      state.ready = true;
      state.retry = 0;
      state.previewFrame = null;
      if (state.liveFollow) drawAt(state.frames.length - 1);
      else updateSliderUi(Math.floor(state.playhead));
      setConnStatus('直播已连接');
      updateControls();
      break;
    case 'playback_changed':
      applyLivePlayback(event.paused, event.tick);
      break;
    case 'challenge_window_wall_clock':
      state.activeChallenge = event;
      updateControls();
      break;
    case 'challenge_window_closed':
      state.activeChallenge = null;
      updateControls();
      break;
    case 'historical_frames_unavailable':
      setNotice('部分历史帧不可用，直播将继续。');
      break;
  }
}

function handleLivePacket(packet) {
  if (packet[0] === 0x01) {
    state.header = decodeHps1Header(packet.subarray(1));
    return;
  }
  if (packet[0] !== 0x02) return;
  const frame = decodePublicFrameV1(packet.subarray(1), state.header.roster_count);
  if (state.frames.length && frame.tick <= state.frames.at(-1).tick) return;
  state.frames.push(frame);
  if (state.frames.length > LIVE_FRAME_LIMIT + 100) {
    state.frames.splice(0, 100);
    state.playhead = Math.max(0, state.playhead - 100);
    state.lastDrawnIdx = -1;
  }
  if (!state.ready) {
    if (!state.previewFrame && state.frames.length === 1) drawAt(0);
    return;
  }
  if (state.frames.length === 1 || (state.liveFollow && state.freezeTick === frame.tick)) drawAt(state.frames.length - 1);
  else updateSliderUi(Math.floor(state.playhead));
  if (state.frames.length <= 2) updateControls();
}

async function refreshSimDetails(selection, first = false) {
  const { signal } = selection;
  const base = simPath();
  try {
    const sim = await apiJson(base, { signal });
    if (signal.aborted) return;
    state.sim = sim;
    if (!state.ready && liveRunning() && !state.ws) applyLivePlayback(Boolean(sim.playback_paused), sim.last_tick);
    if (sim.lifecycle === 'finished' && sim.kind !== 'batch_fast' && sim.published_artifact_uri && !state.archived) {
      const tick = state.frames[Math.floor(state.playhead)]?.tick ?? 0;
      const following = state.liveFollow;
      closeWs();
      state.activeChallenge = null;
      state.paused = true;
      state.archiveEnd = sim.last_tick;
      let archiveTick = 0;
      if (sim.kind === 'live' && !first) archiveTick = following ? sim.last_tick : tick;
      const loaded = await seekArchive(archiveTick, true);
      if (signal.aborted) return;
      if (loaded) setConnStatus('录像已载入');
      void refreshSimList();
    } else if (sim.kind === 'batch_fast' && sim.lifecycle === 'finished') {
      updateScorebug(sim);
      setConnStatus('批量统计已完成');
    }

    const [box, report] = await Promise.all([
      apiJson(`${base}/boxscore`, { signal }),
      sim.kind === 'batch_fast' ? null : apiJson(`${base}/events?after_event_seq=${state.afterEventSeq}&limit=90`, { signal }),
    ]);
    if (signal.aborted) return;
    renderBoxScore(box);
    if (report?.events.length) {
      state.latestEvents = state.latestEvents.concat(report.events).slice(-90);
      state.afterEventSeq = report.events.at(-1).event_seq;
      renderPbpList(state.latestEvents);
    } else if (report && first) renderPbpList([]);
    updateControls();
  } catch (error) {
    if (signal.aborted) return;
    if (error.code === 'MatchAborted') {
      state.sim.lifecycle = 'aborted';
      state.paused = true;
      closeWs();
      setConnStatus('对局已中止');
      void refreshSimList();
    }
    setNotice(`对局状态：${error.message}`, true);
    updateControls();
  } finally {
    if (!signal.aborted && (state.sim.lifecycle === 'running' || (state.sim.lifecycle === 'finished' && !state.archived && state.sim.kind !== 'batch_fast' && state.sim.archive_status.status !== 'archive_failed'))) {
      state.pollTimer = setTimeout(() => void refreshSimDetails(selection), 1000);
    }
  }
}

async function seekArchive(tick, pause = state.paused, debug = $('chk-debug').checked) {
  const selection = state.selection;
  const target = Math.max(0, Math.min(tick, state.archiveEnd));
  state.archiveTask?.abort();
  const task = new AbortController();
  state.archiveTask = task;
  state.loadingArchive = true;
  updateControls();
  const start = Math.floor(target / ARCHIVE_WINDOW) * ARCHIVE_WINDOW;
  const end = Math.min(start + ARCHIVE_WINDOW - 1, state.archiveEnd);
  const key = `${start}:${debug}`;
  try {
    let decoded = state.archiveCache?.key === key ? state.archiveCache.decoded : null;
    if (!decoded) {
      const q = `stream=${debug ? 'debug' : 'public'}&format=raw&from_tick=${start}&to_tick=${end}`;
      const res = await apiFetch(`${simPath()}/frames?${q}`, { signal: task.signal });
      decoded = decodeHps1PlusRawBundle(new Uint8Array(await res.arrayBuffer()));
    }
    if (task.signal.aborted || state.selection !== selection) return;
    state.archiveCache = { key, decoded };
    state.archived = true;
    state.previewFrame = null;
    state.header = decoded.header;
    state.frames = decoded.frames;
    state.debugFrames = decoded.debugFrames;
    renderer.showDebug = debug;
    $('chk-debug').checked = debug;
    state.liveFollow = false;
    state.paused = pause;
    state.atArchiveEnd = target === state.archiveEnd;
    drawAt(frameIndex(target));
    return true;
  } catch (error) {
    if (!task.signal.aborted && state.selection === selection) {
      state.paused = true;
      $('chk-debug').checked = renderer.showDebug;
      setNotice(`读取录像失败：${error.message}`, true);
    }
    return false;
  } finally {
    if (state.archiveTask === task && state.selection === selection) {
      state.archiveTask = null;
      state.loadingArchive = false;
      if (state.frames.length) updateSliderUi(Math.floor(state.playhead));
      updateControls();
    }
  }
}

function nextRequestId() {
  state.clientReqSeq = Math.max(state.clientReqSeq + 1, Math.floor((performance.timeOrigin + performance.now()) * 1000));
  return state.clientReqSeq;
}

async function sendCoachCommand(body, success, endpoint = 'commands') {
  if ($('coach-fields').disabled) return;
  const selection = state.selection;
  state.submitting = true;
  setNotice('正在提交教练指令。');
  updateControls();
  try {
    await apiJson(`${simPath()}/${endpoint}`, { body: { client_request_id: nextRequestId(), ...body }, signal: selection.signal });
    if (!selection.signal.aborted) setNotice(success);
  } catch (error) {
    if (!selection.signal.aborted) setNotice(`教练指令未接受：${error.message}`, true);
  } finally {
    state.submitting = false;
    updateControls();
  }
}

async function togglePlayback() {
  if ($('btn-play-pause').disabled) return;
  if (liveRunning() && state.liveFollow && canControlLive()) {
    const selection = state.selection;
    state.controllingPlayback = true;
    updateControls();
    try {
      const out = await apiJson(`${simPath()}/playback`, { body: { paused: !state.livePaused }, signal: selection.signal });
      if (!selection.signal.aborted) {
        applyLivePlayback(out.paused, out.tick);
        setNotice(out.paused ? '模拟已暂停。' : '模拟已继续。');
      }
    } catch (error) {
      if (!selection.signal.aborted) setNotice(`控制模拟失败：${error.message}`, true);
    } finally {
      state.controllingPlayback = false;
      updateControls();
    }
    return;
  }
  if (liveRunning() && state.liveFollow) {
    state.liveFollow = false;
    state.paused = !state.livePaused;
    if (state.livePaused) drawAt(Math.max(0, state.frames.length - 1 - 50));
  } else if (state.archived && state.paused && state.atArchiveEnd) {
    void seekArchive(0, false);
  } else {
    if (!state.paused && state.playhead % 1 !== 0) drawAt(Math.floor(state.playhead));
    state.paused = !state.paused;
  }
  updateControls();
}

$('launch-form').onsubmit = async (event) => {
  event.preventDefault();
  if ($('launch-fields').disabled) return;
  const mode = $('cfg-mode').value;
  const body = {
    mode, rule_id: $('cfg-rule').value, home_team_id: $('cfg-home').value,
    away_team_id: $('cfg-away').value, seed: $('cfg-seed').valueAsNumber,
    ...(mode === 'live' ? { pace_multiplier: Number($('cfg-pace').value) } : { max_ticks: $('cfg-max-ticks').valueAsNumber }),
  };
  state.launching = true;
  setNotice(mode === 'live' ? '正在启动直播。' : '正在运行批量模拟。');
  updateControls();
  try {
    const out = await apiJson('/api/v1/simulations', { body });
    await selectSimulation(out.directory, true);
  } catch (error) {
    setNotice(`启动模拟失败：${error.message}`, true);
  } finally {
    state.launching = false;
    updateControls();
  }
};

for (const id of ['cfg-mode', 'cfg-rule', 'cfg-home', 'cfg-away']) $(id).onchange = updateLaunchFields;
$('btn-refresh-sims').onclick = refreshSimList;
$('btn-play-pause').onclick = togglePlayback;
$('btn-live-tail').onclick = () => {
  if (!liveRunning()) return;
  state.liveFollow = true;
  state.paused = state.livePaused;
  if (state.frames.length) drawAt(state.frames.length - 1);
  if (!state.ready) {
    state.retry = 0;
    connectLiveWs(state.selection);
  }
  updateControls();
};
$('replay-speed').onchange = (event) => { state.replaySpeed = Number(event.target.value); };
$('tick-slider').oninput = (event) => {
  const tick = Number(event.target.value);
  state.liveFollow = false;
  state.paused = true;
  if (state.archived) {
    clearTimeout(state.seekTimer);
    $('slider-label').textContent = `${tick} / ${state.archiveEnd} 帧`;
    state.seekTimer = setTimeout(() => {
      state.seekTimer = null;
      void seekArchive(tick, true);
    }, 80);
  } else drawAt(frameIndex(tick));
  updateControls();
};
$('chk-debug').onchange = (event) => {
  void seekArchive(state.frames[Math.floor(state.playhead)].tick, state.paused, event.target.checked);
};

$('role-select').onchange = async (event) => {
  const role = event.target.value;
  state.changingRole = true;
  updateControls();
  try {
    if (role === 'spectator' && renderer.showDebug) {
      const loaded = await seekArchive(state.frames[Math.floor(state.playhead)].tick, state.paused, false);
      if (!loaded) {
        event.target.value = state.auth.role;
        return;
      }
    }
    state.auth = await acquireToken(role);
    if (liveRunning()) connectLiveWs(state.selection);
    const roleLabel = event.target.selectedOptions[0].textContent;
    setNotice(`已切换为${roleLabel}。`);
  } catch (error) {
    event.target.value = state.auth.role;
    setNotice(`切换身份失败：${error.message}`, true);
  } finally {
    state.changingRole = false;
    updateControls();
  }
};

$('btn-timeout').onclick = () => sendCoachCommand({ cmd: 'call_timeout', queue_if_live_disallowed: true, advance: 0 }, '球队暂停请求已接受，将按规则执行。');
$('btn-challenge').onclick = () => {
  const { ruling_id, window_epoch } = state.activeChallenge;
  return sendCoachCommand({ cmd: 'challenge_call', phase: 1, ruling_id, expected_window_epoch: window_epoch }, '挑战请求已接受。');
};
$('btn-withdraw').onclick = () => {
  const { ruling_id, window_epoch, request_seq } = state.activeChallenge;
  return sendCoachCommand({ cmd: 'withdraw_challenge', phase: 1, ruling_id, expected_window_epoch: window_epoch, expected_request_seq: request_seq }, '挑战已撤回。');
};
$('btn-set-play').onclick = () => sendCoachCommand({ cmd: 'set_offensive_play', play_id: $('coach-play').value, force_immediate: true }, '进攻战术指令已接受。');
$('btn-set-def').onclick = () => sendCoachCommand({ cmd: 'set_defensive_gameplan', gameplan_id: $('coach-def').value }, '防守方案指令已接受。');
$('coach-pace').oninput = (event) => { $('coach-pace-val').textContent = event.target.value; };
$('btn-set-pace').onclick = () => sendCoachCommand({ cmd: 'set_pace_and_pressure', pace: Number($('coach-pace').value), full_court_press: $('coach-press').checked }, '节奏和防守压力指令已接受。');
$('btn-forfeit').onclick = () => {
  const side = state.auth.role === 'home' ? '主队' : '客队';
  if (window.confirm(`确认代表${side}认输并结束比赛？`)) return sendCoachCommand({}, '弃权指令已接受，正在归档。', 'forfeit');
};

function showPanel(panel) {
  openMobilePanel(panel);
  if (panel === 'diag') void refreshHealth().catch((error) => setNotice(`读取系统状态失败：${error.message}`, true));
  if (panel === 'sims') void refreshSimList();
}
for (const button of document.querySelectorAll('.mnav-btn')) button.onclick = () => showPanel(button.dataset.panel);
for (const button of document.querySelectorAll('.tab')) button.onclick = () => showPanel(button.dataset.tab.slice(4));

new ResizeObserver(drawCurrent).observe(canvas);
openMobilePanel('launch');
updateControls();
requestAnimationFrame(stepRaf);
try {
  state.auth = await acquireToken('home');
  await loadCatalog();
  setConnStatus('就绪');
  void refreshSimList();
} catch (error) {
  setConnStatus('初始化失败');
  setNotice(`页面初始化失败：${error.message}`, true);
}
updateControls();
