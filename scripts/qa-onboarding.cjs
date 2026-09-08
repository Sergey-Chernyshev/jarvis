/* Real browser rendering + strict, isolated native-bridge fixtures.
 * Does not start Jarvis or invoke native capture/auth/install/settings commands. */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const os = require('node:os');
let playwright;
try { playwright = require('playwright'); }
catch { playwright = require(process.env.JARVIS_PLAYWRIGHT_PATH || path.join(os.homedir(), '.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }
const root = path.resolve(__dirname, '../ui');
const out = path.resolve(__dirname, '../docs/qa/assets/onboarding');
const records = [], errors = [];
fs.mkdirSync(out, { recursive: true });
const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.woff2': 'font/woff2', '.woff': 'font/woff' };
const server = http.createServer((request, response) => {
  let file;
  try { file = path.resolve(root, '.' + decodeURIComponent(new URL(request.url, 'http://localhost').pathname)); }
  catch { response.writeHead(400).end(); return; }
  if (!file.startsWith(root + path.sep)) { response.writeHead(403).end(); return; }
  fs.readFile(file, (error, data) => {
    if (error) { response.writeHead(404).end(); return; }
    response.writeHead(200, { 'Content-Type': mime[path.extname(file)] || 'application/octet-stream' }).end(data);
  });
});
function snapshot(ready = false, extra = {}) {
  return {
    coreReady: ready,
    agents: ['claude', 'codex'].map(id => ({ id, label: id, available: true, ready })),
    transport: ['hook', 'socket', 'tmux'].map(id => ({ id, available: true, ready: ready && id !== 'tmux' })),
    capabilities: [
      { id: 'whisper-turbo', available: true, ready: false },
      { id: 'qwen3-runtime', available: true, ready: false },
      { id: 'hey_jarvis', available: false, ready: false },
      { id: 'silero', available: true, ready: false },
    ],
    warnings: [], proxyConfigured: false,
    job: { id: 0, state: 'idle', kind: '', tasks: [], steps: [], failures: [] },
    ...extra,
  };
}
function bridgeFixture({ initial, theme, firstError, subscriptionError }) {
  const clone = data => JSON.parse(JSON.stringify(data));
  let state = clone(initial), nextId = state.job.id + 1;
  const handlers = {}, calls = [], unknown = [], failures = firstError ? { onboarding_get: firstError } : {};
  let deferredRead = null;
  const allowed = new Set(['settings_get', 'onboarding_get', 'onboarding_run', 'models_install', 'service_set_proxy', 'onboarding_open_panel', 'onboarding_open_settings', 'onboarding_close']);
  window.__onboardingFixture = {
    calls, unknown,
    replace(next) { state = clone(next); },
    fail(command, message) { failures[command] = message; },
    deferRead() { deferredRead = {}; deferredRead.promise = new Promise(resolve => { deferredRead.resolve = resolve; }); },
    releaseRead(data) { deferredRead.resolve(clone(data)); deferredRead = null; },
    async emit(next) { state = next.coreReady !== undefined ? clone(next) : { ...state, job: clone(next) }; await Promise.all((handlers.install_job_changed || []).map(handler => handler({ payload: clone(next) }))); },
  };
  window.__TAURI__ = {
    core: { async invoke(command, args = {}) {
      calls.push({ command, args: clone(args) });
      if (!allowed.has(command)) { unknown.push(command); throw new Error('Unknown fixture command: ' + command); }
      if (failures[command]) { const message = failures[command]; delete failures[command]; throw new Error(message); }
      switch (command) {
        case 'settings_get': return { theme, scale: 1, paint: 'coal', mode: 'window' };
        case 'onboarding_get': return deferredRead ? deferredRead.promise : clone(state);
        case 'service_set_proxy': state.proxyConfigured = !!args.proxy; return { ok: true };
        case 'onboarding_run':
        case 'models_install':
          state.job = { id: nextId++, kind: command === 'models_install' ? 'models' : 'core', state: 'running', tasks: args.ids || [], steps: [], failures: [] };
          return clone(state.job);
        default: return null;
      }
    } },
    event: { async listen(name, handler) {
      if (!['appearance', 'install_job_changed', 'onboarding:done', 'models_install_all_done'].includes(name)) throw new Error('Unknown fixture event: ' + name);
      if (subscriptionError && name !== 'appearance') throw new Error('Events unavailable');
      (handlers[name] ||= []).push(handler);
      return () => { handlers[name] = handlers[name].filter(item => item !== handler); };
    } },
  };
}
(async () => {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const browser = await playwright.chromium.launch({ channel: 'chrome', headless: true });
  const pages = [];
  async function open({ initial = snapshot(), theme = 'dark', firstError, subscriptionError, viewport = { width: 560, height: 660 }, reducedMotion = 'no-preference' } = {}) {
    const context = await browser.newContext({ viewport, colorScheme: theme, reducedMotion });
    await context.addInitScript(bridgeFixture, { initial, theme, firstError, subscriptionError });
    const page = await context.newPage(); pages.push(page);
    page.setDefaultTimeout(6000);
    page.on('pageerror', error => errors.push(error.message));
    await page.route('**/*', route => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
    await page.goto(origin + '/onboarding.html');
    await page.waitForFunction(() => document.querySelector('#content')?.dataset.screen !== 'checking');
    return page;
  }
  async function screen(page, name) { await page.waitForFunction(value => document.querySelector('#content').dataset.screen === value, name); }
  async function commands(page, name) { return page.evaluate(name => window.__onboardingFixture.calls.filter(call => call.command === name), name); }
  async function checkLayout(page, name) {
    const box = await page.evaluate(() => {
      const footer = document.querySelector('.footer').getBoundingClientRect();
      const primary = document.querySelector('#primary').getBoundingClientRect();
      const main = document.querySelector('#content');
      return { width: innerWidth, height: innerHeight, docWidth: document.documentElement.scrollWidth, contentWidth: main.clientWidth, contentScrollWidth: main.scrollWidth, footerBottom: footer.bottom, primaryRight: primary.right, primaryLeft: primary.left };
    });
    assert.ok(box.docWidth <= box.width + 1, `${name}: document horizontal overflow ${JSON.stringify(box)}`);
    assert.ok(box.contentScrollWidth <= box.contentWidth + 1, `${name}: content horizontal overflow`);
    assert.ok(box.footerBottom <= box.height + 1 && box.primaryRight <= box.width && box.primaryLeft >= 0, `${name}: footer inaccessible`);
    records.push({ name, ...box });
  }
  async function capture(page, name) {
    await page.waitForTimeout(350);
    await checkLayout(page, name);
    await page.screenshot({ path: path.join(out, `${name}.png`) });
  }
  try {
    const happy = await open();
    await screen(happy, 'welcome');
    assert.equal(await happy.locator('[data-screen="capabilities"].rail-step').isDisabled(), true);
    await capture(happy, 'welcome-dark');
    await happy.keyboard.press('Enter'); await screen(happy, 'agents');
    await capture(happy, 'agents-dark');
    await happy.locator('.proxy summary').click();
    await happy.getByLabel('Адрес прокси').fill('socks5://proxy.example:1080');
    await happy.getByLabel('Адрес прокси').press('Enter');
    assert.equal((await commands(happy, 'onboarding_run')).length, 0, 'Enter in proxy must not start setup');
    await happy.locator('#primary').click(); await screen(happy, 'installing');
    assert.equal((await commands(happy, 'onboarding_run')).length, 1);
    assert.equal(await happy.getByRole('progressbar').getAttribute('aria-valuenow'), null, 'No fabricated percentage');
    const coreRunning = snapshot(false, { job: { id: 1, kind: 'core', state: 'running', tasks: [], failures: [], steps: [{ scope: 'core', phase: 'Хуки', state: 'info', msg: 'Подключение событий агентов', pct: 53 }] } });
    await happy.evaluate(next => window.__onboardingFixture.emit(next), coreRunning);
    assert.equal(await happy.getByRole('progressbar').getAttribute('aria-valuenow'), '53');
    await capture(happy, 'preparing-dark');
    const coreDone = snapshot(true, { job: { ...coreRunning.job, state: 'done', steps: [{ phase: 'Хуки', state: 'done', msg: 'Интеграции проверены' }] } });
    await happy.evaluate(next => window.__onboardingFixture.emit(next), coreDone); await screen(happy, 'capabilities');
    assert.equal(await happy.getByLabel('Активация голосом', { exact: true }).isDisabled(), true);
    assert.equal(await happy.locator('input[type="checkbox"]:checked').count(), 0, 'Models are opt in');
    await capture(happy, 'capabilities-dark');
    await happy.getByLabel('Диктовка и расшифровки', { exact: true }).check();
    await happy.locator('#primary').click(); await screen(happy, 'installing');
    assert.deepEqual((await commands(happy, 'models_install')).map(call => call.args.ids), [['whisper-turbo']]);
    assert.deepEqual((await commands(happy, 'service_set_proxy')).map(call => call.args.proxy), ['socks5://proxy.example:1080']);
    const failed = snapshot(true, { job: { id: 2, kind: 'models', state: 'failed', tasks: ['whisper-turbo'], failures: ['Network timeout: https://user:secret@proxy.example/download'], steps: [] } });
    await happy.evaluate(next => window.__onboardingFixture.emit(next), failed); await screen(happy, 'degraded');
    assert.ok(!(await happy.locator('#content').innerText()).includes('user:secret'), 'Error credentials masked');
    await capture(happy, 'failure-dark');
    await happy.locator('#secondary').click(); await screen(happy, 'capabilities');
    assert.equal(await happy.getByLabel('Диктовка и расшифровки', { exact: true }).isChecked(), true, 'Back retains model choice');
    await happy.locator('#primary').click(); await screen(happy, 'installing');
    const allDone = snapshot(true, { job: { id: 3, kind: 'models', state: 'done', tasks: ['whisper-turbo'], failures: [], steps: [] } });
    allDone.capabilities[0].ready = true;
    await happy.evaluate(next => window.__onboardingFixture.emit(next), allDone); await screen(happy, 'ready');
    await capture(happy, 'ready-dark');
    await happy.locator('#primary').click();
    await happy.waitForFunction(() => window.__onboardingFixture.calls.some(call => call.command === 'onboarding_close'));
    assert.equal((await commands(happy, 'onboarding_open_panel')).length, 1);
    assert.equal((await commands(happy, 'onboarding_close')).length, 1);

    const errorPage = await open({ firstError: 'Connection unavailable' });
    await screen(errorPage, 'unavailable');
    assert.equal(await errorPage.getByRole('alert').count(), 1);
    await capture(errorPage, 'unavailable-dark');
    await errorPage.locator('#primary').click(); await screen(errorPage, 'welcome');
    await errorPage.locator('#primary').click(); await screen(errorPage, 'agents');
    await errorPage.evaluate(() => window.__onboardingFixture.fail('onboarding_run', 'Backend rejected setup'));
    await errorPage.locator('#primary').click();
    await errorPage.getByRole('alert').waitFor();
    assert.equal(await errorPage.locator('#content').getAttribute('data-screen'), 'agents');
    await errorPage.locator('#primary').click();
    await errorPage.waitForFunction(() => !document.querySelector('[role="alert"]'));
    assert.equal((await commands(errorPage, 'onboarding_run')).length, 1, 'Recheck does not blindly repeat mutation');
    await errorPage.keyboard.press('Escape'); await errorPage.keyboard.press('Escape');
    assert.equal((await commands(errorPage, 'onboarding_close')).length, 1, 'Close is idempotent');

    // Real time polling recovers completion even if every native subscription failed.
    const polling = await open({ initial: coreRunning, subscriptionError: true });
    await screen(polling, 'installing');
    await polling.evaluate(next => window.__onboardingFixture.replace(next), coreDone);
    await screen(polling, 'capabilities');

    // Re-rendering state events preserves editable fields and caret.
    const focus = await open({ initial: snapshot(true) });
    await focus.locator('.rail-step[data-screen="agents"]').click();
    await focus.locator('.proxy summary').click();
    await focus.getByLabel('Адрес прокси').fill('http://proxy.example:8080');
    await focus.getByLabel('Адрес прокси').evaluate(node => node.setSelectionRange(7, 12));
    await focus.evaluate(next => window.__onboardingFixture.emit(next), snapshot(true));
    assert.deepEqual(await focus.evaluate(() => ({ field: document.activeElement.dataset.focus, start: document.activeElement.selectionStart, end: document.activeElement.selectionEnd, value: document.activeElement.value })), { field: 'proxy', start: 7, end: 12, value: 'http://proxy.example:8080' });

    for (const theme of ['dark', 'light']) {
      for (const viewport of [{ width: 560, height: 660 }, { width: 360, height: 520 }, { width: 480, height: 600 }]) {
        const page = await open({ initial: snapshot(true), theme, viewport, reducedMotion: 'reduce' });
        const size = `${viewport.width}x${viewport.height}`;
        await capture(page, `ready-${theme}-${size}`);
        await page.locator('.rail-step[data-screen="welcome"]').click();
        await capture(page, `welcome-${theme}-${size}`);
        await page.locator('.rail-step[data-screen="capabilities"]').click();
        await capture(page, `capabilities-${theme}-${size}`);
        assert.equal(await page.locator('#content').evaluate(node => getComputedStyle(node).animationName), 'none');
      }
    }
    const noAgents = snapshot(false, { agents: [{ id: 'claude', available: false, ready: false }, { id: 'codex', available: false, ready: false }] });
    const missing = await open({ initial: noAgents, theme: 'light' });
    await missing.locator('#primary').click();
    assert.equal(await missing.locator('#primary').innerText(), 'Настроить голос');
    await missing.getByRole('button', { name: 'Проверить агентов снова' }).click();
    assert.equal((await commands(missing, 'onboarding_run')).length, 0);
    await capture(missing, 'missing-agents-light');
    await missing.locator('#primary').click(); await screen(missing, 'capabilities');
    assert.equal(await missing.locator('.rail-step[data-screen="agents"]').getAttribute('data-state'), 'skipped');
    await missing.getByLabel('Диктовка и расшифровки', { exact: true }).check();
    await missing.locator('#primary').click(); await screen(missing, 'installing');
    assert.deepEqual((await commands(missing, 'models_install')).map(call => call.args.ids), [['whisper-turbo']]);
    const voiceDone = { ...noAgents, capabilities: noAgents.capabilities.map(item => ({ ...item, ready: item.id === 'whisper-turbo' })), job: { id: 1, kind: 'models', state: 'done', tasks: ['whisper-turbo'], failures: [], steps: [] } };
    await missing.evaluate(next => window.__onboardingFixture.emit(next), voiceDone); await screen(missing, 'ready');
    assert.ok((await missing.locator('#content').innerText()).includes('Агенты · позже'));
    assert.ok(!(await missing.locator('#content').innerText()).includes('Агенты подключены'));
    await capture(missing, 'voice-only-ready-light');
    const reopenedVoice = await open({ initial: voiceDone });
    await screen(reopenedVoice, 'ready');
    const deferInstalled = await open();
    await deferInstalled.locator('#primary').click();
    await deferInstalled.getByRole('button', { name: 'Пока без агентов' }).click(); await screen(deferInstalled, 'capabilities');
    await deferInstalled.locator('#primary').click(); await screen(deferInstalled, 'ready');
    assert.equal((await commands(deferInstalled, 'onboarding_run')).length, 0);
    assert.equal((await commands(deferInstalled, 'models_install')).length, 0);
    for (const page of pages) assert.deepEqual(await page.evaluate(() => window.__onboardingFixture.unknown), []);
    assert.deepEqual(errors, [], 'No browser exceptions');
    const report = { passed: true, browser: 'Chrome headless', native: false, screenshots: records.length, checks: ['core + model flow', 'strict bridge', 'real stage progress', 'failure/back/retry', 'masked credentials', 'opt-in models', 'unavailable model disabled', 'proxy saved before models', 'lost event polling', 'no fake auth', 'missing CLI recheck', 'voice-only install + reopen', 'defer available agents + skip models', 'focus/caret preservation', 'Escape idempotency', 'Enter input isolation', 'initial IPC error', 'mutation IPC error', 'responsive light/dark', 'reduced motion'], layouts: records };
    fs.writeFileSync(path.join(out, 'report.json'), JSON.stringify(report, null, 2) + '\n');
    console.log(JSON.stringify({ passed: true, screenshots: records.length, checks: report.checks, errors }, null, 2));
  } finally { await browser.close(); server.close(); }
})().catch(error => { console.error(error); server.close(); process.exitCode = 1; });
