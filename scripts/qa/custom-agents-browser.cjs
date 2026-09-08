// Production settings UI and real keyboard submission; only synthetic CLI registry writes.
const fs = require('node:fs'), path = require('node:path'), http = require('node:http'), os = require('node:os'), vm = require('node:vm'), assert = require('node:assert/strict');
let playwright; try { playwright = require('playwright'); } catch { playwright = require(path.join(os.homedir(), '.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }
const source = fs.readFileSync(path.join(__dirname, 'session-workspace.cjs'), 'utf8'), context = {};
vm.runInNewContext(source.slice(source.indexOf('function bridgeFixture()'), source.indexOf('\nconst records =')) + '\nthis.fixtureSource=bridgeFixture.toString();', context);
function registryFixture() {
  let agents = [{ id: 'qwen', name: 'Qwen Code', bin: '/fixture/bin/qwen', resume: 'qwen --resume {sid}', dangerousFlag: '--yolo' }];
  const calls = [], state = { fail: false };
  const original = window.jarvis;
  const override = {
    agentInstancesList: async () => ({ config: { entries: [] }, instances: [], health: [] }),
    agentsList: async () => ({ ok: true, agents: structuredClone(agents), presets: [{ id: 'pi', name: 'Pi', bin: 'pi', resume: '' }, { id: 'opencode', name: 'OpenCode', bin: 'opencode', resume: '' }] }),
    agentsSave: async next => {
      calls.push(structuredClone(next));
      if (state.fail) return { ok: false, error: 'Cannot save registry' };
      agents = structuredClone(next); return { ok: true };
    },
  };
  window.jarvis = new Proxy(original, { get: (target, name) => override[name] || target[name] });
  window.__agentRegistryQA = { calls, state };
}
const root = path.resolve(__dirname, '../../ui'), out = path.resolve(__dirname, '../../docs/qa/assets/settings-refinement');
const server = http.createServer((req, res) => {
  const file = path.resolve(root, '.' + new URL(req.url, 'http://localhost').pathname);
  if (!file.startsWith(root + path.sep)) { res.writeHead(403).end(); return; }
  fs.readFile(file, (error, data) => { if (error) res.writeHead(404).end(); else res.writeHead(200, { 'Content-Type': ({ '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.woff2': 'font/woff2' })[path.extname(file)] || 'application/octet-stream' }).end(data); });
});
(async () => {
  fs.mkdirSync(out, { recursive: true }); await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const browser = await playwright.chromium.launch({ channel: 'chrome', headless: true });
  const page = await browser.newPage({ viewport: { width: 1120, height: 840 }, colorScheme: 'dark', reducedMotion: 'reduce' });
  const errors = [], checks = []; page.on('pageerror', error => errors.push(error.message)); page.setDefaultTimeout(8000);
  try {
    await page.route('**/bridge.js', route => route.fulfill({ contentType: 'text/javascript', body: `(${context.fixtureSource})();(${registryFixture.toString()})();` }));
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
    await page.locator('#tabSettings').click(); await page.evaluate(() => window.jarvisOpenSettingsPane('agents'));
    const section = page.locator('.s2-custom-agents'), form = section.locator('form');
    await section.waitFor(); assert.equal(await form.isVisible(), false); checks.push('registration form hidden until requested');
    await section.getByRole('button', { name: 'Добавить агента', exact: true }).click();
    assert.equal(await page.getByLabel('Название', { exact: true }).isVisible(), true);
    await section.getByRole('button', { name: 'Pi', exact: true }).click();
    await page.getByLabel('Программа', { exact: true }).press('Enter');
    await page.waitForFunction(() => window.__agentRegistryQA.calls.length === 1);
    assert.equal(await form.isVisible(), false);
    assert.equal(await section.locator('[data-agent-id="pi"]').count(), 1); checks.push('Enter submits a new preset exactly once');
    await section.getByRole('button', { name: 'Настроить Qwen Code', exact: true }).click();
    await page.getByLabel('Название', { exact: true }).fill('Qwen renamed');
    await page.evaluate(() => { window.__agentRegistryQA.state.fail = true; });
    await page.getByLabel('Название', { exact: true }).press('Enter');
    await section.getByRole('alert').filter({ hasText: 'Cannot save registry' }).waitFor();
    assert.equal(await page.getByLabel('Название', { exact: true }).inputValue(), 'Qwen renamed');
    assert.equal(await section.locator('[data-agent-id="qwen"] .dt').textContent(), 'Qwen Code');
    checks.push('failed save preserves form draft and installed agent');
    await page.evaluate(() => { window.__agentRegistryQA.state.fail = false; });
    await page.getByLabel('Название', { exact: true }).press('Enter');
    await section.locator('[data-agent-id="qwen"] .dt').filter({ hasText: 'Qwen renamed' }).waitFor();
    const saved = await page.evaluate(() => window.__agentRegistryQA.calls.at(-1)); assert.equal(saved[0].dangerousFlag, '--yolo');
    checks.push('rename preserves stable identity and launch settings');
    await section.getByRole('button', { name: 'Добавить агента', exact: true }).click();
    await section.getByRole('button', { name: 'OpenCode', exact: true }).click();
    await form.scrollIntoViewIfNeeded();
    const geometry = await form.evaluate(element => ({ width: element.clientWidth, scrollWidth: element.scrollWidth, gap: getComputedStyle(element.querySelector('.s2-form-grid')).columnGap }));
    assert.equal(geometry.scrollWidth <= geometry.width + 1, true); assert.equal(parseFloat(geometry.gap) >= 12, true);
    checks.push('main fields have visible spacing and no form overflow');
    await page.screenshot({ path: path.join(out, 'custom-agent-form.png') });
    for (const width of [1300, 900]) {
      await page.setViewportSize({ width, height: 880 }); await form.scrollIntoViewIfNeeded();
      const overflow = await form.evaluate(element => element.scrollWidth - element.clientWidth);
      assert.equal(overflow <= 1, true);
      await page.screenshot({ path: path.join(out, `custom-agent-form-${width}.png`) });
    }
    checks.push('form remains unclipped at 1300px and 900px window widths');
    await page.getByLabel('Название', { exact: true }).press('Escape');
    assert.equal(await form.isVisible(), false); assert.equal(await section.isVisible(), true);
    checks.push('Escape closes the form without leaving settings');
    assert.deepEqual(errors, []);
    fs.writeFileSync(path.join(out, 'custom-agents-browser.json'), JSON.stringify({ checks, errors, geometry }, null, 2));
    console.log(JSON.stringify({ checks, errors, geometry }, null, 2));
  } finally { await browser.close(); server.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; server.close(); });
