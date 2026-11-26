import assert from 'node:assert/strict';
import { chromium } from 'playwright';
import { decodeHps1Header, decodePublicFrameV1 } from '../viewer/codec.js';

const url = process.argv[2] ?? 'http://127.0.0.1:38110/';
const browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE });

try {
  const context = await browser.newContext({ viewport: { width: 320, height: 760 }, isMobile: true, hasTouch: true, deviceScaleFactor: 2 });
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  const loc = (selector) => page.locator(selector);
  const panel = (name) => loc(`[data-panel="${name}"]`).click();
  const waitEnabled = (id) => page.waitForFunction((id) => !document.querySelector(id).matches(':disabled'), id);
  const waitText = async (id, text) => {
    try {
      await page.waitForFunction(([id, text]) => document.querySelector(id).textContent.includes(text), [id, text], { timeout: 12_000 });
    } catch (error) {
      throw new Error(`${id}: ${await loc(id).textContent()}；页面提示：${await loc('#action-status').textContent()}；浏览器错误：${errors.join(', ')}`, { cause: error });
    }
  };
  const waitTick = (condition, tick) => page.waitForFunction(([condition, tick]) => {
    const value = Number(document.querySelector('#sb-tick').textContent);
    return condition === 'after' ? value > tick : value === tick;
  }, [condition, tick]);
  const viewportFits = () => page.evaluate(() => {
    const width = document.documentElement.clientWidth;
    return window.visualViewport.scale === 1 && document.documentElement.scrollWidth === width
      && [...document.querySelectorAll('.card')].filter((card) => card.checkVisibility()).every((card) => {
        const rect = card.getBoundingClientRect();
        return rect.left >= 0 && rect.right <= width && card.scrollWidth <= card.clientWidth;
      });
  });

  let header;
  let frame;
  let newestTick = -1;
  page.on('websocket', (socket) => socket.on('framereceived', ({ payload }) => {
    if (typeof payload === 'string') return;
    if (payload[0] === 1) {
      header = decodeHps1Header(payload.subarray(1));
      newestTick = -1;
    }
    if (payload[0] === 2) {
      frame = decodePublicFrameV1(payload.subarray(1), header.roster_count);
      newestTick = Math.max(frame.tick, newestTick);
    }
  }));

  await page.goto(url);
  await waitEnabled('#btn-launch');
  for (const width of [320, 360, 390, 430, 768, 1024]) {
    await page.setViewportSize({ width, height: 760 });
    for (const name of ['launch', 'coach', 'sims', 'box', 'pbp', 'diag']) {
      await panel(name);
      assert(await viewportFits(), `${width}px: ${name}`);
    }
  }
  await page.setViewportSize({ width: 390, height: 844 });
  await panel('launch');
  await loc('#cfg-pace').selectOption('5');
  await loc('#btn-launch').click();
  await waitText('#conn-status', '直播已连接');
  await waitTick('after', 5);
  assert.equal(frame.players.filter((player) => player.roster_idx !== 255).length, 10);
  const playerColors = await page.evaluate(() => {
    const canvas = document.querySelector('canvas');
    const pixels = canvas.getContext('2d').getImageData(0, 0, canvas.width, canvas.height).data;
    let home = 0;
    let away = 0;
    for (let i = 0; i < pixels.length; i += 4) {
      const [r, g, b] = [pixels[i], pixels[i + 1], pixels[i + 2]];
      if (r === 29 && g === 78 && b === 216) home += 1;
      if (r === 185 && g === 28 && b === 28) away += 1;
    }
    return { home, away };
  });
  assert(playerColors.home > 100 && playerColors.away > 100, '比赛帧中的两队球员应绘制到 Canvas');
  assert(await viewportFits());

  const playback = page.waitForResponse((response) => response.url().endsWith('/playback') && response.ok());
  await loc('#btn-play-pause').click();
  const stopped = await (await playback).json();
  assert.equal(stopped.paused, true);
  await waitTick('at', stopped.tick);
  await new Promise((resolve) => setTimeout(resolve, 400));
  assert.equal(newestTick, stopped.tick, '服务端暂停后不得继续发送新帧');
  await waitText('#btn-play-pause', '继续模拟');
  await panel('coach');
  assert(await loc('#btn-set-play').isDisabled());
  await loc('#btn-play-pause').click();
  await waitTick('after', stopped.tick);
  await waitEnabled('#btn-set-play');
  for (const [button, cmd] of [['#btn-set-play', 'set_offensive_play'], ['#btn-set-def', 'set_defensive_gameplan'], ['#btn-set-pace', 'set_pace_and_pressure']]) {
    const response = page.waitForResponse((response) => response.url().endsWith('/commands') && response.request().postDataJSON().cmd === cmd);
    await loc(button).click();
    const reply = await response;
    assert.equal((await reply.json()).type, 'command_accepted');
  }
  const timeoutResponse = page.waitForResponse((response) => response.url().endsWith('/commands') && response.request().postDataJSON().cmd === 'call_timeout');
  await loc('#btn-timeout').click();
  assert.equal((await (await timeoutResponse).json()).type, 'command_accepted');
  await loc('#tick-slider').press('Home');
  await waitTick('at', 0);
  await waitText('#court-status', '直播回看');
  await loc('#replay-speed').selectOption('2');
  await loc('#btn-play-pause').click();
  await waitTick('after', 0);
  await loc('#btn-live-tail').click();
  await waitText('#court-status', '直播进行中');
  await loc('#role-select').selectOption('spectator');
  await waitText('#coach-status', '当前身份仅可观战');
  await waitText('#conn-status', '直播已连接');
  assert(await loc('#btn-forfeit').isDisabled());
  await loc('#btn-play-pause').click();
  await waitText('#court-status', '直播回看');
  await loc('#role-select').selectOption('home');
  await waitText('#btn-live-tail', '回到实时');
  await loc('#btn-live-tail').click();
  page.once('dialog', (dialog) => void dialog.accept());
  await loc('#btn-forfeit').click();
  await waitText('#conn-status', '录像已载入');
  assert(await loc('#dl-replay').isVisible());
  assert(await loc('#dl-frames').isVisible());
  for (const id of ['#dl-replay', '#dl-frames']) {
    const href = await loc(id).getAttribute('href');
    const download = await context.request.get(new URL(href, url).href);
    assert(download.ok());
    assert((await download.body()).length > 100);
  }
  await panel('box');
  assert(await loc('.box-table').isVisible());
  assert(await viewportFits());
  await panel('sims');
  await loc('#btn-refresh-sims').click();
  await waitEnabled('#btn-refresh-sims');
  await loc('.sim-item.active').click();
  await waitText('#conn-status', '录像已载入');
  await waitTick('at', 0);

  await panel('launch');
  await loc('#cfg-rule').selectOption('fiba');
  await loc('#cfg-away').selectOption('mile_high_hub');
  await loc('#cfg-home').selectOption('boston_five_out');
  await loc('#cfg-away').selectOption('bay_motion_split');
  await loc('#btn-launch').click();
  await waitText('#court-title', 'FIBA');
  await waitText('#conn-status', '直播已连接');
  await panel('coach');
  await waitEnabled('#btn-challenge');
  const stoppedReview = page.waitForResponse((response) => response.url().endsWith('/playback') && response.ok());
  await loc('#btn-play-pause').click();
  const reviewTick = (await (await stoppedReview).json()).tick;
  await waitTick('at', reviewTick);
  await new Promise((resolve) => setTimeout(resolve, 350));
  assert.equal(newestTick, reviewTick);
  assert(await loc('#btn-challenge').isDisabled());
  await loc('#btn-play-pause').click();
  await waitEnabled('#btn-challenge');
  const challenge = page.waitForResponse((response) => response.url().endsWith('/commands') && response.request().postDataJSON().cmd === 'challenge_call');
  await loc('#btn-challenge').click();
  assert.equal((await (await challenge).json()).type, 'command_accepted');
  await waitEnabled('#btn-withdraw');
  const withdraw = page.waitForResponse((response) => response.url().endsWith('/commands') && response.request().postDataJSON().cmd === 'withdraw_challenge');
  await loc('#btn-withdraw').click();
  assert.equal((await (await withdraw).json()).type, 'command_accepted');
  assert(await loc('#btn-withdraw').isDisabled());
  page.once('dialog', (dialog) => void dialog.accept());
  await loc('#btn-forfeit').click();
  await waitText('#conn-status', '录像已载入');

  await panel('launch');
  await loc('#cfg-mode').selectOption('full_trace');
  await loc('#cfg-home').selectOption('mile_high_hub');
  await loc('#cfg-max-ticks').fill('1950');
  await loc('#btn-launch').click();
  await waitText('#court-title', 'FIBA');
  await waitText('#conn-status', '录像已载入');
  await waitEnabled('#chk-debug');
  await loc('#tick-slider').press('End');
  const end = Number(await loc('#tick-slider').getAttribute('max'));
  assert(end > 1800);
  await waitTick('at', end);
  await loc('#chk-debug').check();
  await waitEnabled('#chk-debug');
  assert(await loc('#chk-debug').isChecked(), await loc('#action-status').textContent());
  await waitTick('at', end);
  await loc('#role-select').selectOption('spectator');
  await waitText('#action-status', '已切换为观战视角');
  assert(!(await loc('#chk-debug').isChecked()));
  assert(await loc('#chk-debug').isDisabled());
  await loc('#btn-play-pause').click();
  await waitTick('after', 0);
  await page.waitForFunction(() => Number(document.querySelector('#sb-tick').textContent) < 500);
  await loc('#role-select').selectOption('home');
  await waitText('#action-status', '已切换为主队教练');

  await panel('launch');
  await loc('#cfg-mode').selectOption('fast_batch');
  await loc('#cfg-max-ticks').fill('100');
  await loc('#btn-launch').click();
  await waitText('#conn-status', '批量统计已完成');
  await waitText('#court-status', '仅提供技术统计');
  assert(await loc('#btn-play-pause').isDisabled());
  assert(!(await loc('#dl-replay').isVisible()));
  assert(await loc('.box-table').isVisible());
  assert(await viewportFits());
  await context.close();

  const desktop = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  await desktop.goto(url);
  await desktop.waitForFunction(() => !document.querySelector('#btn-launch').matches(':disabled'));
  assert(await desktop.locator('.launcher').isVisible());
  assert(await desktop.locator('.court-card').isVisible());
  assert(!(await desktop.locator('.mobile-nav').isVisible()));
  await desktop.locator('[data-tab="tab-diag"]').click();
  await desktop.getByText('正常在线').waitFor();
  assert.deepEqual(errors, []);
  console.log('Viewer: mobile panels, canvas players, live pause/resume, coaching, challenge/withdraw, role controls, archives, and batch stats passed.');
} finally {
  await browser.close();
}
