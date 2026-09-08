// Production DOM/CSS in Chromium. Synthetic bridge: no account or VM changes.
const fs = require('node:fs'), path = require('node:path'), http = require('node:http'), os = require('node:os'), vm = require('node:vm'), assert = require('node:assert/strict');
let playwright; try { playwright = require('playwright'); } catch { playwright = require(path.join(os.homedir(), '.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }
const fixture = fs.readFileSync(path.join(__dirname, 'session-workspace.cjs'), 'utf8'), context = {};
vm.runInNewContext(fixture.slice(fixture.indexOf('function bridgeFixture()'), fixture.indexOf('\nconst records =')) + '\nthis.fixtureSource=bridgeFixture.toString();', context);
const root = path.resolve(__dirname, '../../ui'), out = path.resolve(__dirname, '../../docs/qa/assets/settings-clarity');
const server = http.createServer((req, res) => {
  const file = path.resolve(root, '.' + new URL(req.url, 'http://localhost').pathname);
  if (!file.startsWith(root + path.sep)) return res.writeHead(403).end();
  fs.readFile(file, (e, b) => e ? res.writeHead(404).end() : res.writeHead(200, { 'Content-Type': ({ '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml' })[path.extname(file)] || 'application/octet-stream' }).end(b));
});
const checks = [], errors = [];
function contrast(foreground, background) {
  const luminance = rgb => rgb.map(v => { v /= 255; return v <= .04045 ? v / 12.92 : ((v + .055) / 1.055) ** 2.4; }).reduce((sum, v, i) => sum + v * [.2126, .7152, .0722][i], 0);
  const rgb = value => value.match(/[\d.]+/g).slice(0, 3).map(Number);
  const a = luminance(rgb(foreground)), b = luminance(rgb(background));
  return (Math.max(a, b) + .05) / (Math.min(a, b) + .05);
}
(async () => {
  fs.mkdirSync(out, { recursive: true }); await new Promise(r => server.listen(0, '127.0.0.1', r));
  const browser = await playwright.chromium.launch({ channel: 'chrome', headless: true });
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 }, colorScheme: 'dark' });
  page.on('pageerror', e => errors.push(e.message)); page.setDefaultTimeout(10000);
  try {
    await page.route('**/bridge.js', r => r.fulfill({ contentType: 'text/javascript', body: `(${context.fixtureSource})();` }));
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
    await page.locator('#tabSettings').waitFor();
    for (const theme of ['dark', 'light']) {
      await page.evaluate(theme => window.jarvisTheme.set({ theme, paint: 'clover' }), theme);
      await page.screenshot({ path: path.join(out, `launcher-${theme}.png`) });
      const colors = await page.locator('.module-icon').evaluateAll(nodes => [...new Set(nodes.map(n => getComputedStyle(n).color))]);
      assert.ok(colors.length >= 4, 'Module identities lost their semantic colors');
      await page.locator('#tabSettings').click();
      await page.evaluate(() => window.jarvisOpenSettingsPane('look'));
      await page.locator('#s2-pane-look .paintdot').first().waitFor();
      for (const width of [1280, 900, 650]) {
        await page.setViewportSize({ width, height: 900 });
        await page.waitForTimeout(220);
        const layout = await page.locator('#settings2').evaluate(host => ({
          overflow: host.scrollWidth > host.clientWidth + 1,
          controls: [...host.querySelectorAll('.dpane.on button, .dpane.on input, .dpane.on select')].filter(el => el.getBoundingClientRect().width > 0).map(el => ({ text: el.textContent, right: el.getBoundingClientRect().right })),
          heading: getComputedStyle(host.querySelector('.dpane.on .dtitle')).color,
          description: getComputedStyle(host.querySelector('.dpane.on .dd')).color,
          background: getComputedStyle(document.documentElement).getPropertyValue('--paper').trim(),
        }));
        assert.equal(layout.overflow, false, `Settings overflow ${width}/${theme}`);
        await page.screenshot({ path: path.join(out, `appearance-${theme}-${width}.png`) });
        assert.ok(layout.controls.every(c => c.right <= width + 1), `Controls escape viewport ${width}/${theme}: ${JSON.stringify(layout.controls.filter(c => c.right > width + 1))}`);
        // Resolve CSS hex to computed RGB for the contrast calculation.
        const bg = await page.evaluate(value => { const n = document.createElement('div'); n.style.color = value; document.body.append(n); const c = getComputedStyle(n).color; n.remove(); return c; }, layout.background);
        assert.ok(contrast(layout.description, bg) >= 4.5, `Description contrast below 4.5 ${width}/${theme}`);
        await page.screenshot({ path: path.join(out, `appearance-${theme}-${width}.png`) });
        checks.push(`${theme}/${width}: visible controls, no horizontal overflow, readable descriptions`);
      }
      await page.setViewportSize({ width: 1280, height: 900 });
      await page.getByLabel('Поиск настроек').fill('микрофон');
      await page.locator('.settings-result').first().waitFor();
      assert.equal(await page.locator('.snav .grp:visible').count(), 0, 'Search shows unrelated category headings');
      await page.getByLabel('Поиск настроек').fill('');
      assert.equal(await page.locator('.snav .grp:visible').count(), 4);
      await page.locator('#pageHome').click();
    }
    checks.push('settings search hides/restores category headings');
    assert.deepEqual(errors, []);
    fs.writeFileSync(path.join(out, 'report.json'), JSON.stringify({ checks, errors }, null, 2));
    console.log(JSON.stringify({ checks, errors }));
  } finally { await browser.close(); server.close(); }
})().catch(e => { console.error(e); process.exitCode = 1; server.close(); });
