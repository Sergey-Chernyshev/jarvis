// Production settings in Chrome, synthetic account registry. Never changes real profiles.
const fs = require('node:fs'), path = require('node:path'), http = require('node:http');
const os = require('node:os'), vm = require('node:vm'), assert = require('node:assert/strict');
let playwright;
try { playwright = require('playwright'); }
catch { playwright = require(path.join(os.homedir(), '.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }
const workspaceFixture = fs.readFileSync(path.join(__dirname, 'session-workspace.cjs'), 'utf8'), context = {};
vm.runInNewContext(workspaceFixture.slice(workspaceFixture.indexOf('function bridgeFixture()'), workspaceFixture.indexOf('\nconst records =')) + '\nthis.fixtureSource=bridgeFixture.toString();', context);
function profileFixture() {
  let config = { version: 1, entries: [], defaultCodexInstance: 'personal' };
  const qa = { calls: [], failSave: '', failList: '', holdSave: false, pendingSave: null };
  const profiles = [
    { id: 'personal', label: 'codex-personal', home: '/Users/example/work/sup/codex-dual-account/personal/codex-home', observedChats: 252, lastHookAt: '2026-09-05T12:24:45Z' },
    { id: 'work', label: 'codex-work', home: '/Users/example/.codex', observedChats: 4, lastHookAt: null },
  ];
  const health = [
    { instanceId: 'personal', rulesInstalled: true, trustStatus: 'trusted', errors: [] },
    { instanceId: 'work', rulesInstalled: true, trustStatus: 'unknown', errors: [] },
  ];
  const overrides = {
    agentInstancesList: async () => {
      if (qa.failList) { const error = qa.failList; qa.failList = ''; throw new Error(error); }
      const rows = profiles.map(p => ({ ...p, canonicalHome: p.home, agent: 'codex', machine: 'local', enabled: true, exists: true, cli: '/Users/example/.nvm/versions/node/v22.17.0/bin/codex', ...config.entries.find(e => e.home === p.home) }));
      for (const entry of config.entries.filter(e => !profiles.some(p => p.home === e.home))) rows.push({ ...entry, id: `profile-${rows.length}`, canonicalHome: entry.home, agent: 'codex', observedChats: 0, exists: true });
      return structuredClone({ config, defaultCodexInstance: config.defaultCodexInstance, health, instances: rows });
    },
    agentInstancesSave: async value => {
      qa.calls.push({ name: 'save', value: structuredClone(value) });
      if (qa.holdSave) { qa.holdSave = false; await new Promise(resolve => { qa.pendingSave = resolve; }); }
      if (qa.failSave) { const error = qa.failSave; qa.failSave = ''; throw new Error(error); }
      config = structuredClone(value); return { ok: true };
    },
    agentInstancesRepair: async ids => {
      qa.calls.push({ name: 'repair', ids });
      const h = health.find(h => h.instanceId === ids[0]);
      if (h) Object.assign(h, { rulesInstalled: true, trustStatus: 'trusted', errors: [] });
      return structuredClone(health);
    },
  };
  window.jarvis = new Proxy(window.jarvis, { get: (target, name) => overrides[name] || target[name] });
  window.__profileSettingsQA = qa;
}
const uiRoot = path.resolve(__dirname, '../../ui');
const out = path.resolve(__dirname, '../../docs/qa/assets/settings-redesign');
const server = http.createServer((req, res) => {
  const file = path.resolve(uiRoot, '.' + new URL(req.url, 'http://localhost').pathname);
  if (!file.startsWith(uiRoot + path.sep)) { res.writeHead(403).end(); return; }
  fs.readFile(file, (error, data) => {
    if (error) res.writeHead(404).end();
    else res.writeHead(200, { 'Content-Type': ({ '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml' })[path.extname(file)] || 'application/octet-stream' }).end(data);
  });
});
(async () => {
  fs.mkdirSync(out, { recursive: true });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const browser = await playwright.chromium.launch({ channel: 'chrome', headless: true });
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 }, colorScheme: 'dark' });
  const errors = [], checks = []; page.setDefaultTimeout(8000); page.on('pageerror', e => errors.push(e.message));
  const expectIdle = async () => page.waitForFunction(() => document.querySelector('.instance-settings')?.getAttribute('aria-busy') === 'false');
  const snapshot = async name => {
    await page.evaluate(async () => {
      await document.fonts.ready;
      await Promise.allSettled(document.getAnimations().filter(a => Number.isFinite(a.effect?.getComputedTiming().endTime)).map(a => a.finished));
    });
    await page.screenshot({ path: path.join(out, name) });
  };
  try {
    await page.route('**/bridge.js', route => route.fulfill({ contentType: 'text/javascript', body: `(${context.fixtureSource})();(${profileFixture.toString()})();` }));
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
    await page.locator('#tabSettings').click();
    await page.evaluate(() => window.jarvisOpenSettingsPane('agents'));
    const pane = page.getByRole('region', { name: 'Профили Codex' });
    const personal = pane.locator('.instance-profile[data-instance-id="personal"]');
    const work = pane.locator('.instance-profile[data-instance-id="work"]');
    await personal.getByRole('heading', { name: 'codex-personal', exact: true }).waitFor();
    assert.equal(await pane.locator('.instance-input:visible').count(), 1);
    assert.equal(await personal.getByText('События получены', { exact: true }).isVisible(), true);
    assert.equal(await work.getByText('Ждём первое событие', { exact: true }).isVisible(), true);
    assert.equal(await pane.locator('.instance-path:visible').count(), 0);
    checks.push('compact profiles expose identity and truthful event state; technical fields stay closed');

    for (const width of [1280, 900]) {
      await page.setViewportSize({ width, height: 900 });
      await snapshot(`profiles-${width}.png`);
      const geometry = await pane.evaluate(host => {
        const panel = host.getBoundingClientRect();
        const controls = [...host.querySelectorAll('button,input,select')].filter(n => n.getClientRects().length).map(n => ({ label: n.getAttribute('aria-label') || n.textContent, left: n.getBoundingClientRect().left, right: n.getBoundingClientRect().right }));
        return { width: innerWidth, scroll: document.documentElement.scrollWidth, left: panel.left, right: panel.right, controls };
      });
      assert.ok(geometry.scroll <= geometry.width + 1, `Page overflows at ${width}`);
      assert.ok(geometry.controls.every(c => c.left >= geometry.left - 1 && c.right <= geometry.right + 1), `Profile controls overflow at ${width}`);
      checks.push(`profile overview controls fit at ${width}px`);
    }
    await page.setViewportSize({ width: 1280, height: 900 });
    const defaultSelect = pane.getByLabel('Профиль Codex по умолчанию');
    await page.evaluate(() => { window.__profileSettingsQA.failSave = 'Профиль временно недоступен'; });
    await defaultSelect.selectOption('work'); await expectIdle();
    assert.equal(await defaultSelect.inputValue(), 'personal');
    assert.equal(await pane.getByRole('status').innerText(), 'Профиль временно недоступен');
    checks.push('rejected default change restores persisted selection with a visible error');
    await defaultSelect.selectOption('work'); await expectIdle();
    assert.equal(await defaultSelect.inputValue(), 'work');
    checks.push('default profile persists after retry');

    const configure = personal.getByRole('button', { name: 'Настроить codex-personal', exact: true });
    await configure.focus(); await page.keyboard.press('Enter');
    assert.equal(await configure.getAttribute('aria-expanded'), 'true');
    await configure.press('Escape');
    assert.equal(await configure.getAttribute('aria-expanded'), 'false');
    assert.equal(await pane.isVisible(), true);
    await configure.press('Enter');
    checks.push('Escape after keyboard opening a profile closes only that disclosure');
    assert.equal(await personal.getByText('/Users/example/work/sup/codex-dual-account/personal/codex-home', { exact: true }).isVisible(), true);
    const name = personal.getByLabel('Название профиля');
    await name.fill('Личный Codex');
    await page.evaluate(() => { window.__profileSettingsQA.failSave = 'Не удалось сохранить профиль'; });
    await name.press('Enter'); await expectIdle();
    assert.equal(await name.inputValue(), 'Личный Codex');
    assert.equal(await personal.getByRole('heading').innerText(), 'codex-personal');
    assert.equal(await pane.getByRole('status').innerText(), 'Не удалось сохранить профиль');
    assert.equal(await personal.getByRole('button', { name: 'Сохранить', exact: true }).isEnabled(), true);
    checks.push('keyboard disclosure and rename failure preserve the user draft');
    await personal.getByRole('button', { name: 'Сохранить', exact: true }).click(); await expectIdle();
    assert.equal(await personal.getByRole('heading').innerText(), 'Личный Codex');
    const lastSaved = await page.evaluate(() => window.__profileSettingsQA.calls.filter(c => c.name === 'save').at(-1).value);
    assert.equal(lastSaved.entries.find(e => e.label === 'Личный Codex').home, '/Users/example/work/sup/codex-dual-account/personal/codex-home');
    assert.equal(lastSaved.defaultCodexInstance, 'work');
    assert.equal(await personal.getByRole('button', { name: 'Настроить Личный Codex', exact: true }).getAttribute('aria-expanded'), 'true');
    checks.push('renaming preserves account identity/default selection and disclosure');

    await personal.getByRole('button', { name: 'Проверить хуки', exact: true }).click(); await expectIdle();
    assert.deepEqual(await page.evaluate(() => window.__profileSettingsQA.calls.find(c => c.name === 'repair').ids), ['personal']);
    checks.push('hook repair targets only the selected profile');
    await page.setViewportSize({ width: 900, height: 900 });
    await snapshot('profile-details-900.png');
    assert.ok(await personal.evaluate(n => n.scrollWidth <= n.clientWidth + 1));
    const facts = await personal.locator('.instance-facts').evaluate(n => n.scrollWidth <= n.clientWidth + 1);
    assert.ok(facts, 'Long paths overflow the disclosure');
    checks.push('long profile and CLI paths wrap within details at 900px');
    await name.focus(); await name.press('Escape');
    assert.equal(await personal.locator('.instance-details').isHidden(), true);
    assert.equal(await pane.isVisible(), true);
    assert.equal(await personal.getByRole('button', { name: 'Настроить Личный Codex', exact: true }).evaluate(n => n === document.activeElement), true);
    checks.push('Escape closes profile details and restores focus without leaving settings');

    await page.evaluate(() => { window.__profileSettingsQA.failSave = 'Наблюдение не изменено'; });
    await personal.getByLabel('Отслеживать Личный Codex').click(); await expectIdle();
    assert.equal(await personal.getByLabel('Отслеживать Личный Codex').isChecked(), true);
    checks.push('rejected monitoring change restores its switch');
    await personal.getByLabel('Отслеживать Личный Codex').uncheck(); await expectIdle();
    assert.equal(await personal.getByText('Наблюдение выключено', { exact: true }).isVisible(), true);
    assert.equal(await defaultSelect.locator('option[value="personal"]').count(), 0);
    await personal.getByRole('button', { name: 'Настроить Личный Codex', exact: true }).click();
    assert.equal(await personal.getByRole('button', { name: 'Проверить хуки', exact: true }).isDisabled(), true);
    await personal.getByLabel('Отслеживать Личный Codex').check(); await expectIdle();
    checks.push('disabled monitoring is explicit, excluded from defaults, and cannot repair hooks');

    await pane.getByRole('button', { name: 'Добавить профиль', exact: true }).click();
    const addForm = pane.locator('.instance-add-form');
    assert.equal(await addForm.getByLabel('Каталог CODEX_HOME').evaluate(n => n === document.activeElement), true);
    await addForm.getByLabel('Название профиля').fill('Отмена');
    await page.keyboard.press('Escape');
    assert.equal(await addForm.isHidden(), true);
    assert.equal(await pane.isVisible(), true);
    assert.equal(await pane.getByRole('button', { name: 'Добавить профиль', exact: true }).evaluate(n => n === document.activeElement), true);
    checks.push('Add reveals labeled fields; Escape cancels locally and restores focus');
    await pane.getByRole('button', { name: 'Добавить профиль', exact: true }).click();
    await addForm.getByLabel('Каталог CODEX_HOME').fill('/fixture/team/codex-home');
    await addForm.getByLabel('Название профиля').fill('Команда');
    await page.evaluate(() => { window.__profileSettingsQA.failSave = 'Каталог не найден'; });
    await addForm.getByRole('button', { name: 'Добавить', exact: true }).click(); await expectIdle();
    assert.equal(await addForm.getByLabel('Каталог CODEX_HOME').inputValue(), '/fixture/team/codex-home');
    assert.equal(await addForm.getByLabel('Название профиля').inputValue(), 'Команда');
    assert.equal(await pane.getByRole('status').innerText(), 'Каталог не найден');
    checks.push('invalid new profile retains both fields for correction');
    const before = await page.evaluate(() => window.__profileSettingsQA.calls.filter(c => c.name === 'save').length);
    await page.evaluate(() => { window.__profileSettingsQA.holdSave = true; });
    await addForm.getByRole('button', { name: 'Добавить', exact: true }).click();
    assert.equal(await addForm.getByRole('button', { name: 'Добавить', exact: true }).isDisabled(), true);
    await addForm.getByRole('button', { name: 'Добавить', exact: true }).evaluate(n => n.click());
    assert.equal(await page.evaluate(() => window.__profileSettingsQA.calls.filter(c => c.name === 'save').length), before + 1);
    await page.evaluate(() => window.__profileSettingsQA.pendingSave()); await expectIdle();
    assert.equal(await pane.getByRole('heading', { name: 'Команда', exact: true }).isVisible(), true);
    assert.equal(await addForm.isHidden(), true);
    checks.push('busy submit prevents duplicate saves and successful Add closes its form');

    await page.evaluate(() => { window.__profileSettingsQA.failList = 'Нет связи'; });
    await defaultSelect.selectOption('personal'); await expectIdle();
    assert.equal(await defaultSelect.isDisabled(), true);
    assert.match(await pane.getByRole('status').innerText(), /Изменение применено.*Нет связи/);
    await pane.getByRole('button', { name: 'Обновить', exact: true }).click(); await expectIdle();
    assert.equal(await defaultSelect.isEnabled(), true);
    assert.equal(await defaultSelect.inputValue(), 'personal');
    checks.push('post-save discovery failure blocks stale writes; refresh restores saved state');
    await page.setViewportSize({ width: 1280, height: 900 });
    await snapshot('profiles-final-1280.png');
    assert.equal(await page.locator('#s2-pane-agents').getByText(/Не удалось загрузить раздел/).count(), 0, 'Agents section failed to render');
    assert.deepEqual(errors, []);
    fs.writeFileSync(path.join(out, 'profile-settings-report.json'), JSON.stringify({ checks, errors }, null, 2));
    console.log(JSON.stringify({ checks, errors }, null, 2));
  } finally { await browser.close(); server.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; server.close(); });
