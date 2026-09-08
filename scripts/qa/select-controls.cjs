// Production UI in Chromium, with synthetic profiles and no native/account writes.
// Interactions use the designed trigger and popup; hidden native selects are
// inspected only to verify the existing application's authoritative state.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const vm = require('node:vm');
let playwright;
try { playwright = require('playwright'); }
catch { playwright = require(process.env.JARVIS_PLAYWRIGHT_PATH || path.join(os.homedir(), '.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }

const fixtureFile = fs.readFileSync(path.join(__dirname, 'session-workspace.cjs'), 'utf8');
const fixtureContext = {};
vm.runInNewContext(fixtureFile.slice(fixtureFile.indexOf('function bridgeFixture()'), fixtureFile.indexOf('\nconst records =')) + '\nthis.source = bridgeFixture.toString();', fixtureContext);

function selectFixture() {
  let config = { version: 1, entries: [], defaultCodexInstance: 'personal' };
  const calls = [];
  const profiles = [
    { id: 'personal', label: 'Personal', models: [{ value: 'gpt-6-astra', label: 'GPT-6 Astra' }, { value: 'gpt-5.6-sol', label: 'GPT-5.6 Sol' }] },
    { id: 'work', label: 'Work', models: [{ value: 'work-reasoner', label: 'Work Reasoner' }, ...Array.from({ length: 11 }, (_, i) => ({ value: `work-model-${i + 1}`, label: `Work Model ${i + 1}` }))] },
  ];
  const overrides = {
    agentInstancesList: async () => ({ config, defaultCodexInstance: config.defaultCodexInstance, health: [], instances: profiles.map(p => ({ ...p, home: `/fixture/${p.id}`, canonicalHome: `/fixture/${p.id}`, agent: 'codex', machine: 'local', enabled: true, exists: true, observedChats: 1 })) }),
    agentInstancesSave: async value => {
      calls.push({ name: 'save', value: structuredClone(value) });
      if (window.__selectQA.failSave) { window.__selectQA.failSave = false; throw new Error('Synthetic profile save failure'); }
      config = structuredClone(value); return { ok: true };
    },
    analyticsReport: async () => ({ schemaVersion: 1, generatedAt: '2026-09-05T12:00:00Z', period: 'week', coverage: { filesScanned: 1, sessions: 1, errors: [], limited: false }, summary: { sessions: 1, prompts: 0, toolCalls: 0, toolErrors: 0, toolUnknown: 0, activeMs: 0, wallMs: 0, harnessScore: null }, sessions: [{ id: 'qa-analytics', title: 'Select validation fixture', cwd: '/fixture/project', models: [{ model: 'gpt-6-astra' }] }], projects: [], models: [], outcomes: [], economics: { recorded: 0, complete: 0, accepted: 0, groups: [], comparisons: [] } }),
  };
  window.__selectQA = { calls, failSave: false };
  const original = window.jarvis;
  window.jarvis = new Proxy(original, { get: (target, name) => overrides[name] || target[name] });
}

const root = path.resolve(__dirname, '../../ui');
const out = path.resolve(__dirname, '../../docs/qa/assets/select-controls');
const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.woff2': 'font/woff2' };
const server = http.createServer((req, res) => {
  const file = path.resolve(root, '.' + new URL(req.url, 'http://localhost').pathname);
  if (!file.startsWith(root + path.sep)) { res.writeHead(403).end(); return; }
  fs.readFile(file, (error, data) => {
    if (error) res.writeHead(404).end();
    else res.writeHead(200, { 'Content-Type': mime[path.extname(file)] || 'application/octet-stream' }).end(data);
  });
});

(async () => {
  fs.mkdirSync(out, { recursive: true });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const browser = await playwright.chromium.launch({ channel: 'chrome', headless: true });
  const page = await browser.newPage({ viewport: { width: 1280, height: 850 }, colorScheme: 'dark', reducedMotion: 'reduce' });
  page.setDefaultTimeout(8000);
  const checks = [], errors = [];
  page.on('pageerror', error => errors.push(error.message));
  const trigger = source => page.locator(source).locator('..').locator('.jselect-trigger');
  const popup = () => page.locator('.jselect-popover:not([aria-hidden="true"]):visible');
  const choose = async (source, name) => {
    await trigger(source).click();
    await popup().getByRole('option', { name, exact: true }).click();
    await popup().waitFor({ state: 'hidden' });
  };
  const sourceValue = source => page.locator(source).inputValue();
  const home = async () => {
    if (await page.locator('#pageHome').isVisible()) await page.locator('#pageHome').click();
    else await page.locator('#pageBack').click();
    await page.waitForFunction(() => document.documentElement.dataset.view === 'home');
  };
  const capture = async name => {
    await page.evaluate(async () => {
      await document.fonts.ready;
      await Promise.allSettled(document.getAnimations().filter(a => Number.isFinite(a.effect?.getComputedTiming().endTime)).map(a => a.finished));
    });
    await page.screenshot({ path: path.join(out, `${name}.png`) });
  };
  const popupInsideViewport = async context => {
    const rect = await popup().evaluate(el => { const r = el.getBoundingClientRect(); return { left: r.left, right: r.right, top: r.top, bottom: r.bottom, width: innerWidth, height: innerHeight }; });
    assert.ok(rect.left >= 0 && rect.top >= 0 && rect.right <= rect.width + 1 && rect.bottom <= rect.height + 1, `${context}: popup clips viewport: ${JSON.stringify(rect)}`);
  };
  try {
    await page.route('**/bridge.js', route => route.fulfill({ contentType: 'text/javascript', body: `(${fixtureContext.source})();(${selectFixture.toString()})();` }));
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
    await page.locator('#tabSessions').click();
    await trigger('#newChatProvider').waitFor();
    await page.waitForFunction(() => document.querySelectorAll('#newChatMachine option').length === 3);
    assert.equal(await trigger('#newChatProvider').getAttribute('role'), 'combobox');
    assert.equal(await page.locator('#newChatProvider').getAttribute('aria-hidden'), 'true');
    assert.equal(await page.locator('#newChatProvider').getAttribute('tabindex'), '-1');
    assert.equal(await trigger('#newChatInstance').isVisible(), false);
    await trigger('#newChatProvider').click();
    assert.equal(await popup().getByRole('option', { name: 'Claude', exact: true }).getAttribute('aria-selected'), 'true');
    await capture('provider-dark');
    await popup().getByRole('option', { name: 'Codex', exact: true }).click();
    await trigger('#newChatInstance').waitFor();
    await page.waitForFunction(() => document.querySelector('#newChatInstance')?.value === 'personal');
    assert.equal(await sourceValue('#newChatProvider'), 'codex');
    assert.match(await trigger('#newChatProvider').innerText(), /Codex/);
    checks.push('designed provider popup mirrors selected state and reveals source-specific profile');

    await choose('#newChatModel', 'GPT-6 Astra');
    assert.equal(await sourceValue('#newChatModel'), 'gpt-6-astra');
    await choose('#newChatInstance', 'Work');
    assert.equal(await sourceValue('#newChatModel'), '', 'previous profile model must not leak into a different catalog');
    await trigger('#newChatModel').click();
    const modelSearch = popup().locator('input');
    await modelSearch.fill('reasoner');
    assert.equal(await popup().getByRole('option').count(), 1);
    await popup().getByRole('option', { name: 'Work Reasoner', exact: true }).click();
    assert.equal(await sourceValue('#newChatModel'), 'work-reasoner');
    await choose('#newChatProvider', 'Claude');
    assert.equal(await trigger('#newChatInstance').isVisible(), false);
    assert.equal(await sourceValue('#newChatModel'), '', 'switching provider clears an incompatible model');
    checks.push('profile catalogs update, model search works, incompatible selections reset');

    const permissions = 'select[aria-label="Разрешения новой задачи"]';
    await choose(permissions, 'Планирование');
    await choose('#newChatProvider', 'Codex');
    assert.equal(await sourceValue(permissions), 'ask');
    await trigger(permissions).click();
    const plan = popup().getByRole('option', { name: 'Планирование', exact: true });
    assert.equal(await plan.getAttribute('aria-disabled'), 'true');
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('Enter');
    assert.notEqual(await sourceValue(permissions), 'plan');
    checks.push('unsupported permission stays disabled and keyboard navigation skips it');

    const view = await page.locator('html').getAttribute('data-view');
    await trigger('#newChatProvider').focus();
    await page.keyboard.press('ArrowDown');
    await popup().waitFor();
    await page.keyboard.press('Escape');
    await popup().waitFor({ state: 'hidden' });
    assert.equal(await page.locator('html').getAttribute('data-view'), view);
    assert.equal(await trigger('#newChatProvider').evaluate(el => el === document.activeElement), true);
    assert.equal(await sourceValue('#newChatProvider'), 'codex');
    await trigger('#newChatProvider').click();
    await page.keyboard.press('Tab');
    await popup().waitFor({ state: 'hidden' });
    await page.locator('#newChatPrompt').fill('Сохранить черновик при выборе модели');
    await trigger('#newChatProvider').click();
    await page.locator('#newChatPrompt').click();
    await popup().waitFor({ state: 'hidden' });
    assert.equal(await page.locator('#newChatPrompt').inputValue(), 'Сохранить черновик при выборе модели');
    checks.push('Escape closes only popup and restores focus; Tab and outside click close without losing draft');

    await choose('#newChatInstance', 'Personal');
    await trigger('#newChatMachine').click();
    await page.keyboard.press('b');
    await page.keyboard.press('Enter');
    assert.equal(await sourceValue('#newChatMachine'), 'build-box');
    await page.waitForFunction(() => document.querySelector('#newChatInstance')?.value === 'vm-personal');
    assert.match(await trigger('#newChatInstance').innerText(), /VM Personal/);
    await trigger('#newChatMachine').click();
    const offline = popup().getByRole('option', { name: /offline-box/ });
    assert.equal(await offline.getAttribute('aria-disabled'), 'true');
    await page.keyboard.press('Escape');
    await choose('#newChatMachine', 'Этот компьютер');
    checks.push('typeahead switches machine and async profile list updates; offline machine cannot be selected');

    // Drive the real settings save error path. The component must mirror the
    // source's property-only rollback, with no synthetic change event required.
    await home();
    await page.locator('#tabSettings').click();
    await page.evaluate(() => window.jarvisOpenSettingsPane('agents'));
    const defaultProfile = 'select[aria-label="Профиль Codex по умолчанию"]';
    await trigger(defaultProfile).waitFor();
    await page.evaluate(() => { window.__selectQA.failSave = true; });
    await choose(defaultProfile, 'Work');
    await page.getByText('Synthetic profile save failure', { exact: true }).waitFor();
    await page.waitForFunction(() => document.querySelector('select[aria-label="Профиль Codex по умолчанию"]')?.value === 'personal');
    await page.waitForFunction(() => document.querySelector('select[aria-label="Профиль Codex по умолчанию"]')?.parentElement.querySelector('.jselect-trigger')?.textContent.includes('Personal'));
    assert.equal(await trigger(defaultProfile).isDisabled(), false);
    await choose(defaultProfile, 'Work');
    await page.getByText('Профиль по умолчанию сохранён', { exact: true }).waitFor();
    assert.equal(await sourceValue(defaultProfile), 'work');
    assert.match(await trigger(defaultProfile).innerText(), /Work/);
    assert.equal(await page.locator('.instance-default .jselect').count(), 1);
    const profileFocus = await trigger(defaultProfile).evaluate(el => ({ focused: el === document.activeElement, active: document.activeElement?.outerHTML.slice(0, 700), sourceDisabled: el.parentElement.querySelector('select').disabled, triggerDisabled: el.disabled }));
    if (!profileFocus.focused) console.error('Profile focus evidence:', profileFocus);
    // This assertion is kept after the independent layout/form checks so a
    // failure still leaves useful evidence for the whole component review.
    checks.push('settings failure rolls back trigger and unlocks it; successful rerender keeps one working control');

    await home();
    await page.locator('#tabHistory').click();
    await page.getByRole('button', { name: 'Добавить проект', exact: true }).first().click();
    const projectMachine = 'select[aria-label="Машина проекта"]';
    await choose(projectMachine, 'build-box');
    assert.equal(await sourceValue(projectMachine), 'build-box');
    assert.equal(await trigger(projectMachine).evaluate(el => el === document.activeElement), true, 'whole-form rerender must restore focus to replacement select');
    await trigger(projectMachine).click();
    await page.keyboard.press('Escape');
    assert.equal(await page.getByRole('form', { name: 'Добавить проект', exact: true }).isVisible(), true, 'Escape dismissed project form while selecting its machine');
    await page.getByRole('button', { name: 'Отмена', exact: true }).click();
    checks.push('project form replacement retains select focus and popup Escape preserves its form');

    await home();
    await page.locator('#tabStats').click();
    await page.getByText('Записать результат задачи', { exact: true }).click();
    await trigger('select[aria-label="Сессия"]').waitFor();
    await page.getByRole('textbox', { name: 'Тип задачи', exact: true }).fill('Select validation');
    await page.getByRole('textbox', { name: 'Модель', exact: true }).fill('gpt-6-astra');
    await page.getByRole('textbox', { name: 'Версия harness', exact: true }).fill('qa');
    await page.getByRole('button', { name: 'Сохранить результат', exact: true }).click();
    assert.equal(await trigger('select[aria-label="Сессия"]').evaluate(el => el === document.activeElement), true, 'required hidden native select must redirect invalid focus to its designed trigger');
    assert.equal(await page.locator('.ai-outcome-form').evaluate(form => form.checkValidity()), false);
    checks.push('required analytics select keeps form validation and focuses the visible trigger');

    await home();
    await page.locator('#tabSessions').click();
    await choose('#newChatProvider', 'Codex');
    await choose('#newChatInstance', 'Work');
    for (const width of [900, 650]) {
      await page.setViewportSize({ width, height: 660 });
      for (const theme of ['dark', 'light']) {
        await page.evaluate(theme => window.jarvisTheme.adopt({ theme, paint: 'clover' }), theme);
        await trigger('#newChatModel').click();
        await popupInsideViewport(`model ${width}/${theme}`);
        assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), `document overflows ${width}/${theme}`);
        await capture(`model-${theme}-${width}`);
        await page.keyboard.press('Escape');
        // Machine selector sits at the bottom edge of the composer, so its
        // popup must choose the available side and remain fully visible.
        await trigger('#newChatMachine').click();
        await popupInsideViewport(`machine ${width}/${theme}`);
        await capture(`machine-${theme}-${width}`);
        await page.keyboard.press('Escape');
        checks.push(`${theme}/${width}: model and machine popups fit viewport with no horizontal overflow`);
      }
    }
    assert.equal(profileFocus.focused, true, `settings rerender must retain keyboard focus: ${JSON.stringify(profileFocus)}`);
    assert.deepEqual(errors, []);
    fs.writeFileSync(path.join(out, 'report.json'), JSON.stringify({ checks, errors }, null, 2));
    console.log(JSON.stringify({ checks, errors }, null, 2));
  } catch (error) {
    await page.screenshot({ path: path.join(out, 'failure.png') }).catch(() => {});
    throw error;
  } finally { await browser.close(); server.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; server.close(); });
