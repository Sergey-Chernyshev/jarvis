// Isolated Chromium checks against the production picker source and CSS.
// No bridge, app process, user profile, or network service is used.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
let playwright;
try { playwright = require('playwright'); }
catch { playwright = require(path.join(os.homedir(), '.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }
const repo = path.resolve(__dirname, '../..');
const component = fs.readFileSync(path.join(repo, 'ui/select-control.js'), 'utf8');
const css = fs.readFileSync(path.join(repo, 'ui/select-control.css'), 'utf8');
const checks = [], failures = [];
(async () => {
  const browser = await playwright.chromium.launch({ channel: 'chrome', headless: true });
  try {
    const page = await browser.newPage({ viewport: { width: 800, height: 540 }, reducedMotion: 'reduce' });
    const errors = []; page.on('pageerror', error => errors.push(error.message));
    const fresh = async body => {
      await page.goto('about:blank');
      await page.setContent(`<html><head><style>:root { zoom: var(--ui-scale, 1); --font: sans-serif; --ink: #eee; --paper-2: #222; --line-strong: #555; --fill-2: #333; --ink-2: #ddd; --fill-3: #444; --accent-text: #8db; --accent-soft: #254; } body { margin: 0; } ${css}</style></head><body>${body}</body></html>`);
      await page.addScriptTag({ content: component });
    };
    const run = async (name, action) => {
      try { await action(); checks.push(name); }
      catch (error) { failures.push({ name, error: error.message }); }
    };
    const trigger = () => page.locator('button[role="combobox"]');
    const active = () => page.locator('.jselect-popover:not([aria-hidden="true"])');
    const base = '<select aria-label="Fixture"><option value="a">Alpha</option><option value="b">Beta</option></select>';

    await run('root CSS zoom keeps edge-positioned popups in the viewport', async () => {
      for (const scale of [.8, 1, 1.25, 1.5]) {
        for (const [width, height] of [[800, 540], [420, 360]]) {
          await page.setViewportSize({ width, height });
          for (const edge of ['top:8px;left:8px', 'top:8px;right:8px', 'bottom:8px;left:8px', 'bottom:8px;right:8px']) {
            await fresh(`<div style="position:fixed;${edge}">${base}</div>`);
            await page.evaluate(value => document.documentElement.style.setProperty('--ui-scale', value), scale);
            await trigger().click(); await active().waitFor();
            const box = await active().evaluate(node => { const r = node.getBoundingClientRect(); return { left:r.left, top:r.top, right:r.right, bottom:r.bottom }; });
            assert.ok(box.left >= 0 && box.top >= 0 && box.right <= width + 1 && box.bottom <= height + 1, JSON.stringify({ scale, width, height, edge, box }));
            await page.keyboard.press('Escape');
          }
        }
      }
    });
    await page.setViewportSize({ width: 800, height: 540 });

    await run('rapid reopen preserves unique aria-controls and option IDs', async () => {
      await fresh(base); await page.emulateMedia({ reducedMotion: 'no-preference' });
      const result = await page.evaluate(() => {
        const button = document.querySelector('button[role="combobox"]');
        button.click(); window.JarvisSelect.close(); button.click();
        const ids = [...document.querySelectorAll('[id]')].map(element => element.id);
        const controlled = document.getElementById(button.getAttribute('aria-controls'));
        return { duplicates: ids.filter((id, index) => ids.indexOf(id) !== index), staleControl: !!controlled?.closest('[aria-hidden="true"]') };
      });
      assert.deepEqual(result.duplicates, []); assert.equal(result.staleControl, false);
      await page.emulateMedia({ reducedMotion: 'reduce' });
    });

    await run('a disabled fieldset closes the popup and disables its trigger', async () => {
      await fresh(`<fieldset>${base}</fieldset>`); await trigger().click();
      await page.evaluate(() => { document.querySelector('fieldset').disabled = true; });
      await page.waitForFunction(() => document.querySelector('button[role="combobox"]').disabled && !window.JarvisSelect.isOpen());
      assert.equal(await page.locator('select').inputValue(), 'a');
    });
    await run('native form reset updates the displayed selection without emitting change', async () => {
      await fresh(`<form>${base}</form>`);
      await page.evaluate(() => { const s = document.querySelector('select'); window.changeCount = 0; s.addEventListener('change', () => window.changeCount++); s.value = 'b'; });
      await page.waitForFunction(() => document.querySelector('.jselect-value').textContent === 'Beta');
      await page.evaluate(() => document.querySelector('form').reset());
      await page.waitForFunction(() => document.querySelector('.jselect-value').textContent === 'Alpha');
      assert.equal(await page.evaluate(() => window.changeCount), 0);
    });
    await run('direct selected-property updates and disabled optgroups are honored', async () => {
      await fresh('<select aria-label="Fixture"><optgroup label="Disabled" disabled><option value="a">Alpha</option></optgroup><option value="b">Beta</option><option value="c">Gamma</option></select>');
      await page.evaluate(() => { document.querySelector('option[value="c"]').selected = true; });
      await page.waitForFunction(() => document.querySelector('.jselect-value').textContent === 'Gamma');
      await trigger().click();
      assert.equal(await active().getByRole('option', { name:'Alpha' }).getAttribute('aria-disabled'), 'true');
      await page.keyboard.press('Home'); await page.keyboard.press('Enter');
      assert.equal(await page.locator('select').inputValue(), 'b');
    });
    await run('hide or removal of the control clears an open portal', async () => {
      await fresh(`<section>${base}</section>`); await trigger().click();
      await page.evaluate(() => { document.querySelector('section').hidden = true; });
      await page.waitForFunction(() => !window.JarvisSelect.isOpen());
      await page.evaluate(() => { document.querySelector('section').hidden = false; });
      await trigger().click();
      await page.evaluate(() => { document.querySelector('section').remove(); });
      await page.waitForFunction(() => !window.JarvisSelect.isOpen());
    });
    await run('removing only the original select does not leave an actionable orphan', async () => {
      await fresh(base); await trigger().click();
      await page.evaluate(() => { document.querySelector('select').remove(); });
      await page.waitForFunction(() => !window.JarvisSelect.isOpen());
      assert.equal(await trigger().count(), 0, 'Detached source left a visible, stale combobox');
    });
    await run('invalid required selects still focus a visible control', async () => {
      await fresh('<form><label>Required<select aria-label="Fixture" required><option value="">Choose</option><option value="a">Alpha</option></select></label><button type="submit">Submit</button></form>');
      await page.getByRole('button', { name: 'Submit' }).click();
      assert.equal(await trigger().evaluate(element => element === document.activeElement), true);
      assert.equal(await page.locator('select').evaluate(element => element.validity.valueMissing), true);
    });
    await run('normal motion interpolates opacity both entering and exiting the popup', async () => {
      await fresh(base); await page.emulateMedia({ reducedMotion: 'no-preference' });
      const samples = await page.evaluate(async () => {
        const frames = async (node, duration) => {
          const start = performance.now(), values = [];
          while (performance.now() - start < duration) {
            await new Promise(requestAnimationFrame);
            values.push({ ms: performance.now() - start, connected: node.isConnected, opacity: Number(getComputedStyle(node).opacity) });
          }
          return values;
        };
        document.querySelector('button[role="combobox"]').click();
        const popup = document.querySelector('.jselect-popover');
        const entered = await frames(popup, 250);
        window.JarvisSelect.close();
        const exited = await frames(popup, 200);
        return { entered, exited };
      });
      assert.ok(samples.entered.some(sample => sample.opacity > 0 && sample.opacity < 1), JSON.stringify(samples));
      assert.equal(samples.entered.at(-1).opacity, 1);
      assert.ok(samples.exited.some(sample => sample.connected && sample.opacity > 0 && sample.opacity < 1), JSON.stringify(samples));
      assert.equal(samples.exited.at(-1).connected, false);
      await page.emulateMedia({ reducedMotion: 'reduce' });
    });
    if (errors.length) failures.push({ name:'browser errors', error:errors.join('\n') });
    process.stdout.write(JSON.stringify({ ok: !failures.length, checks, failures }, null, 2) + '\n');
    process.exitCode = failures.length ? 1 : 0;
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
