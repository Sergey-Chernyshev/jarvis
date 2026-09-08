// Real Chromium rendering with a synthetic native bridge; never records audio.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const os = require('node:os');
const { spawnSync } = require('node:child_process');
let playwright;
try { playwright = require('playwright'); }
catch { playwright = require(process.env.JARVIS_PLAYWRIGHT_PATH || path.join(os.homedir(), '.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }
const root = path.resolve(__dirname, '../../ui');
const out = path.resolve(__dirname, '../../docs/qa/assets/toast-motion');
fs.mkdirSync(out, { recursive: true });
const mime = { '.html':'text/html', '.js':'text/javascript', '.css':'text/css', '.svg':'image/svg+xml', '.woff2':'font/woff2' };
const server = http.createServer((req, res) => {
  const file = path.resolve(root, '.' + new URL(req.url, 'http://localhost').pathname);
  if (!file.startsWith(root + path.sep)) { res.writeHead(403).end(); return; }
  fs.readFile(file, (error, data) => error ? res.writeHead(404).end() : res.writeHead(200, { 'Content-Type': mime[path.extname(file)] || 'application/octet-stream' }).end(data));
});
function fixture() {
  const events = {}, resizes = [], calls = [];
  const qa = window.__toastQa = { events, resizes, calls, hold: true, release: null };
  window.jarvis = { getSettings: async () => ({ theme:'dark', mode:'overlay', paint:'clover' }), onAppearance: () => {} };
  window.toast = new Proxy({
    resize: async height => { resizes.push(height); if (qa.hold && height) await new Promise(resolve => { qa.release = resolve; }); },
    audioState: async () => null, meetingStatus: async () => null,
    copy: async text => { calls.push(['copy', text]); },
    voiceAbort: () => { calls.push(['abort']); },
  }, { get(target, key) { if (key in target) return target[key]; if (key.startsWith('on')) return cb => { events[key] = cb; }; return (...args) => { calls.push([key, ...args]); return Promise.resolve(); }; } });
}
(async () => {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const browser = await playwright.chromium.launch({ channel:'chrome', headless:true });
  const page = await browser.newPage({ viewport:{ width:440, height:480 }, colorScheme:'dark' });
  const errors = [], checks = []; page.on('pageerror', e => errors.push(e.message));
  page.setDefaultTimeout(6000);
  const emit = payload => page.evaluate(p => window.__toastQa.events.onVoiceHud(p), payload);
  const settled = () => page.waitForFunction(() => !document.getAnimations().some(a => a.playState === 'running' && Number.isFinite(a.effect?.getComputedTiming().endTime)));
  const state = () => page.evaluate(() => {
    const c = document.querySelector('.card.voice');
    const r = c?.getBoundingClientRect();
    return { width:r?.width, height:r?.height, opacity:c && getComputedStyle(c).opacity,
      live:document.querySelectorAll('.card.voice').length, ghost:!!c?.querySelector('.card-ghost'),
      finite:c?.getAnimations().filter(a => Number.isFinite(a.effect.getComputedTiming().endTime)).map(a => a.effect.getKeyframes()),
      phase:c?.dataset.phase, resizes:[...window.__toastQa.resizes] };
  });
  try {
    await page.route('**/toast-bridge.js', route => route.fulfill({ contentType:'text/javascript', body:'(' + fixture.toString() + ')();' }));
    await page.goto(`http://127.0.0.1:${server.address().port}/toast.html`);
    await page.evaluate(() => document.fonts.ready);
    await emit({ id:'voice-hud', phase:'listening', title:'Слушаю' });
    await page.waitForFunction(() => !!window.__toastQa.release);
    assert.equal((await state()).opacity, '0');
    assert.equal((await state()).finite.length, 0);
    checks.push('enter waits for actual native resize completion');
    await page.evaluate(() => { window.__toastQa.hold = false; window.__toastQa.release(); });
    await page.waitForFunction(() => document.querySelector('.card').getAnimations().some(a => a.playState === 'running'));
    await settled();
    const listening = await state(); assert.equal(listening.opacity, '1');
    const wave = await page.evaluate(async () => {
      const bar = document.querySelector('.hud-wave i');
      const before = getComputedStyle(bar).transform;
      await new Promise(resolve => setTimeout(resolve, 130));
      return { before, after:getComputedStyle(bar).transform, name:getComputedStyle(bar).animationName };
    });
    assert.equal(wave.name, 'hud-listening'); assert.notEqual(wave.before, wave.after);
    checks.push('listening bars visibly animate rather than remaining a static icon');
    await page.screenshot({ path:path.join(out, 'listening.png'), omitBackground:true });
    await page.evaluate(() => { window.__toastQa.original = document.querySelector('.card.voice'); });
    await emit({ id:'voice-hud', phase:'analyzing', title:'Распознаю…' });
    await page.waitForFunction(() => !!document.querySelector('.card-ghost'));
    assert.equal(await page.evaluate(() => window.__toastQa.original === document.querySelector('.card.voice')), true);
    assert.equal((await state()).opacity, '1'); await settled();
    assert.equal(await page.locator('.hud-wave i').first().evaluate(e => getComputedStyle(e).animationName), 'hud-thinking');
    checks.push('listening to processing reuses visible shell with content crossfade');
    await emit({ id:'voice-hud', phase:'heard', title:'Текст готов', body:'Тестовый текст для проверки перехода', full:'Синтетический текст', copied:true });
    await page.waitForFunction(() => document.querySelector('.card.voice').getAnimations().some(a => a.effect.getKeyframes().some(k => k.width)));
    // Freeze the real browser's width/height animation at its temporal midpoint.
    const midpoint = await page.evaluate(() => {
      const c = document.querySelector('.card.voice');
      const a = c.getAnimations().find(a => a.effect.getKeyframes().some(k => k.width));
      for (const motion of c.getAnimations({ subtree:true })) {
        if (Number.isFinite(motion.effect.getComputedTiming().endTime)) { motion.pause(); motion.currentTime = 120; }
      }
      const keys = a.effect.getKeyframes();
      return { width:c.getBoundingClientRect().width, from:parseFloat(keys[0].width), to:parseFloat(keys.at(-1).width), opacity:getComputedStyle(c).opacity };
    });
    assert.ok(midpoint.width > midpoint.from && midpoint.width < midpoint.to, JSON.stringify(midpoint));
    assert.equal(midpoint.opacity, '1');
    await page.screenshot({ path:path.join(out, 'result-morph.png'), omitBackground:true });
    await page.evaluate(() => document.querySelector('.card.voice').getAnimations({ subtree:true }).forEach(a => { if (a.playState === 'paused') a.play(); }));
    await settled();
    const result = await state(); assert.ok(result.width > listening.width && result.height > listening.height);
    await page.locator('.card.voice .cont').click();
    assert.deepEqual(await page.evaluate(() => window.__toastQa.calls), [['copy', 'Синтетический текст']]);
    checks.push('pill grows continuously into result; shell stays opaque; recovery copy still works');
    // Force the native .hot state from the bug report. Its shadow must fit the
    // canvas just like the idle shadow, instead of restoring the 48px panel blur.
    await page.evaluate(() => window.__toastQa.events.onHover({ over:true, y:document.querySelector('.card.voice').getBoundingClientRect().top + 2 }));
    const surfaces = await page.evaluate(() => {
      const c = document.querySelector('.card.voice'), stack = getComputedStyle(document.querySelector('.stack'));
      return { backgrounds:[document.documentElement, document.body, document.querySelector('.stack')].map(e => getComputedStyle(e).backgroundColor),
        shadow:getComputedStyle(c).boxShadow, backdrop:getComputedStyle(c).backdropFilter, gutter:[stack.paddingTop, stack.paddingRight, stack.paddingBottom, stack.paddingLeft] };
    });
    assert.ok(surfaces.backgrounds.every(b => b === 'rgba(0, 0, 0, 0)'));
    assert.doesNotMatch(surfaces.shadow, /48px/); assert.equal(surfaces.backdrop, 'none');
    assert.deepEqual(surfaces.gutter, ['24px','24px','28px','24px']);
    await page.screenshot({ path:path.join(out, 'result-hover.png'), omitBackground:true });
    checks.push('hover keeps compact rounded shadow and fully transparent canvas gutters');
    await emit({ id:'voice-hud', phase:'dismiss' });
    assert.equal((await state()).live, 1);
    assert.ok(!(await state()).resizes.includes(0), 'native window hid before exit');
    await emit({ id:'voice-hud', phase:'listening', title:'Слушаю снова' });
    await emit({ id:'voice-hud', phase:'analyzing', title:'Обрабатываю' });
    await emit({ id:'voice-hud', phase:'empty', title:'Не расслышал', body:'Скажи ещё раз' });
    await settled();
    assert.equal((await state()).live, 1); assert.equal((await state()).phase, 'empty');
    assert.equal((await state()).ghost, false); assert.equal((await state()).opacity, '1');
    checks.push('rapid updates and restart during exit cannot duplicate or remove current HUD');
    await page.screenshot({ path:path.join(out, 'empty.png'), omitBackground:true });
    await emit({ id:'voice-hud', phase:'dismiss' });
    await page.waitForFunction(() => !document.querySelector('.card.voice'));
    assert.equal((await state()).resizes.at(-1), 0);
    checks.push('native hide occurs after exit animation and DOM removal');
    await page.emulateMedia({ reducedMotion:'reduce' });
    await emit({ id:'voice-hud', phase:'listening', title:'Слушаю' });
    await settled();
    assert.equal((await state()).opacity, '1');
    await emit({ id:'voice-hud', phase:'empty', title:'Не расслышал', body:'Скажи ещё раз' });
    await settled(); assert.equal((await state()).finite.length, 0);
    assert.equal(await page.evaluate(() => document.getAnimations().length), 0);
    await page.locator('.card.voice .close').click();
    assert.equal((await state()).live, 0);
    assert.ok(!(await page.evaluate(() => window.__toastQa.calls)).some(c => c[0] === 'abort'), 'terminal close restarted cancellation');
    checks.push('reduced motion settles immediately and terminal Close does not abort again');
    assert.deepEqual(errors, []);
    const pixelCheck = spawnSync('python3', ['-c', `
import json, sys
from pathlib import Path
from PIL import Image
report = []
for path in sorted(Path(sys.argv[1]).glob('*.png')):
    alpha = Image.open(path).convert('RGBA').getchannel('A')
    w, h = alpha.size
    edges = [(0,0,w,1), (0,h-1,w,h), (0,0,1,h), (w-1,0,w,h)]
    report.append({'image':path.name, 'maxCanvasEdgeAlpha':max(max(alpha.crop(edge).getdata()) for edge in edges), 'paintedBounds':alpha.getbbox()})
print(json.dumps(report))
`, out], { encoding:'utf8' });
    if (pixelCheck.status !== 0) throw new Error(pixelCheck.stderr);
    const alpha = JSON.parse(pixelCheck.stdout);
    assert.ok(alpha.every(image => image.maxCanvasEdgeAlpha === 0));
    checks.push('every outer-edge screenshot pixel has alpha zero, including hovered result');
    const report = { scope:'Real Chromium rendering and browser animations; synthetic native IPC and synthetic voice payloads. Full-screen Spaces are tested separately in AppKit.', checks, errors, midpoint, surfaces, wave, alpha };
    fs.writeFileSync(path.join(out, 'report.json'), JSON.stringify(report, null, 2) + '\n');
    console.log(JSON.stringify(report, null, 2));
  } finally { await browser.close(); await new Promise(resolve => server.close(resolve)); }
})().catch(error => { console.error(error); process.exitCode = 1; server.close(); });
