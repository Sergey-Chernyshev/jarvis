/* Real voice UI in Chrome, strict native-contract fixtures. No microphone,
 * actual files in Jarvis data, model calls or account changes are used. */
const assert = require('node:assert/strict');
const fs = require('node:fs'); const path = require('node:path'); const http = require('node:http'); const os = require('node:os');
let playwright; try { playwright = require('playwright'); } catch { playwright = require(process.env.JARVIS_PLAYWRIGHT_PATH || path.join(os.homedir(), '.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }
const root = path.resolve(__dirname, '../ui'), out = path.resolve(__dirname, '../docs/qa/assets/voice'); fs.mkdirSync(out, { recursive: true });
const index = fs.readFileSync(path.join(root, 'index.html'), 'utf8'); const styles = index.match(/<style[\s>][\s\S]*?<\/style>/g)?.join('\n') || '';
const html = `<!doctype html><html lang="ru"><head><meta charset="utf-8"><link rel="stylesheet" href="theme.css">${styles}<link rel="stylesheet" href="workspace.css"><link rel="stylesheet" href="vendor/phosphor/style.css"><script src="theme.js"></script><script src="icons.js"></script><style>html,body{margin:0;width:100%;height:100%;min-width:0;overflow:hidden}body{display:block;padding:0;background:var(--panel-glass,var(--paper))}#voicehist{width:100%;height:100%;}</style></head><body><div id="voicehist"></div><script src="voice-history.js"></script></body></html>`;
const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.woff2': 'font/woff2', '.woff': 'font/woff', '.svg': 'image/svg+xml' };
const server = http.createServer((request, response) => {
  const pathname = new URL(request.url, 'http://localhost').pathname;
  if (pathname === '/voice-preview.html') { response.writeHead(200, { 'Content-Type': 'text/html' }).end(html); return; }
  const file = path.resolve(root, '.' + decodeURIComponent(pathname));
  if (!file.startsWith(root + path.sep)) { response.writeHead(403).end(); return; }
  fs.readFile(file, (error, data) => { if (error) response.writeHead(404).end(); else response.writeHead(200, { 'Content-Type': mime[path.extname(file)] || 'application/octet-stream' }).end(data); });
});
function fixture({ theme, failedReads = [] }) {
  const clone = value => JSON.parse(JSON.stringify(value));
  const now = Math.floor(Date.now() / 1000);
  const saved = { items: [
    { id: 12, ts: now - 600, text: 'Запланировать проверку интерфейса Jarvis. Сначала проверить словарь и горячие клавиши, затем подготовить описание изменений.', source: 'dictation', hasAudio: true },
    { id: 11, ts: now - 1800, text: 'Созвон с командой в пятницу: обсудить результаты, сверить требования и распределить следующие задачи.', source: 'dictation', hasAudio: false },
    { id: 10, ts: now - 86400, text: 'Нужно сохранить черновик и проверить сообщение об ошибке.', source: 'dictation', hasAudio: false },
    { id: 9, ts: now - 86400, text: 'WAKE PRIVATE EXCLUDED', source: 'wake', hasAudio: false },
  ], words: [{ word: 'джарвис', replacement: 'Jarvis' }, { word: 'тайп скрипт', replacement: 'TypeScript' }, { word: 'клауд код', replacement: 'Claude Code' }], smart: false, scratch: 'План на неделю\n\n— Проверить макет на маленьком экране\n— Обсудить доступ к удалённой машине\n— Записать итоги встречи' };
  const errors = Object.fromEntries(failedReads.map(name => [name, 'Fixture read unavailable: ' + name])); const calls = [], held = {}, release = {};
  const methods = {
    getSettings: () => ({ theme, paint: 'coal', scale: 1 }),
    transcriptsGet: () => ({ items: clone(saved.items) }),
    transcriptUpdate: (id, text) => { const item = saved.items.find(item => item.id === id); if (!item) return { ok: false, error: 'Not found' }; item.text = text.trim(); return { ok: true, text: item.text }; },
    transcriptDelete: id => { saved.items = saved.items.filter(item => item.id !== id); return { ok: true }; },
    transcriptEnhance: () => ({ ok: true, result: 'Проверить интерфейс Jarvis: словарь, горячие клавиши и сохранение черновика.' }),
    transcriptRetranscribe: id => { const item = saved.items.find(item => item.id === id); item.text = 'Повторное распознавание из сохранённого аудио'; return { ok: true, text: item.text }; },
    promptsGet: () => ({ prompts: [
      { id: 'prompt', name: 'Промпт для агента', desc: 'Чёткий промпт из надиктованного.', auto: true },
      { id: 'commit', name: 'Коммит-сообщение', desc: 'Заголовок и описание изменения.', auto: true },
      { id: 'clean', name: 'Чистовик', desc: 'Убирает повторы и исправляет пунктуацию.', auto: true },
      { id: 'translate', name: 'Перевод на English', desc: 'Перевод с сохранением смысла.', auto: false },
    ] }),
    promptsGetSettings: () => ({ smart: saved.smart }),
    promptsSetSmart: on => { saved.smart = on; return { ok: true }; },
    dictionaryGet: () => ({ words: clone(saved.words) }),
    dictionaryAdd: (word, replacement) => { if (!word || !replacement) return { ok: false, error: 'Заполни оба поля' }; const existing = saved.words.find(entry => entry.word === word); if (existing) existing.replacement = replacement; else saved.words.push({ word, replacement }); return { words: clone(saved.words) }; },
    dictionaryRemove: word => { saved.words = saved.words.filter(entry => entry.word !== word); return { words: clone(saved.words) }; },
    scratchpadGet: () => ({ text: saved.scratch }),
    scratchpadSet: text => { saved.scratch = text; return { ok: true, text }; },
    copyText: text => { saved.clipboard = text; return null; },
  };
  window.__voiceFixture = { saved, calls, fail(name, message = 'Не удалось сохранить: диск недоступен') { errors[name] = message; }, hold(name) { held[name] = new Promise(resolve => { release[name] = resolve; }); }, release(name) { release[name](); }, };
  window.jarvis = Object.fromEntries(Object.entries(methods).map(([name, fn]) => [name, async (...args) => {
    calls.push([name, ...args]); if (errors[name]) { const error = errors[name]; delete errors[name]; throw new Error(error); }
    if (held[name]) { const pending = held[name]; delete held[name]; await pending; }
    return fn(...args);
  }]));
}
(async () => {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve)); const origin = `http://127.0.0.1:${server.address().port}`;
  const browser = await playwright.chromium.launch({ channel: 'chrome', headless: true }); const exceptions = [], captures = [], checks = [];
  async function open(theme = 'dark', viewport = { width: 1040, height: 720 }, failedReads = []) {
    const context = await browser.newContext({ viewport, colorScheme: theme }); await context.addInitScript(fixture, { theme, failedReads }); const page = await context.newPage(); page.setDefaultTimeout(6000);
    page.on('pageerror', error => exceptions.push(error.message)); await page.route('**/*', route => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
    await page.goto(origin + '/voice-preview.html'); await page.evaluate(() => window.initVoiceHistory(document.getElementById('voicehist'))); return page;
  }
  async function section(page, key) { await page.locator(`nav button[data-k="${key}"]`).click(); await page.locator(`section[data-k="${key}"]`).waitFor({ state: 'visible' }); }
  async function capture(page, name) {
    await page.waitForTimeout(160); const geometry = await page.evaluate(() => ({ width: innerWidth, height: innerHeight, docWidth: document.documentElement.scrollWidth, hostWidth: document.getElementById('voicehist').scrollWidth, paneWidth: document.querySelector('.pane.on').clientWidth, paneScroll: document.querySelector('.pane.on').scrollWidth, mainBottom: document.querySelector('#voicehist .main').getBoundingClientRect().bottom }));
    assert.ok(geometry.docWidth <= geometry.width + 1 && geometry.hostWidth <= geometry.width + 1 && geometry.paneScroll <= geometry.paneWidth + 1 && geometry.mainBottom <= geometry.height + 1, `${name}: overflow ${JSON.stringify(geometry)}`);
    await page.screenshot({ path: path.join(out, name + '.png') }); captures.push({ name, ...geometry });
  }
  try {
    const page = await open(); const first = page.locator('article[data-id="12"]');
    assert.equal(await page.locator('article').count(), 3); assert.ok(!(await page.locator('#voicehist').innerText()).includes('WAKE PRIVATE'));
    assert.equal(await page.getByRole('button', { name: 'Распознать снова', exact: true }).count(), 1);
    await first.getByRole('button', { name: 'Изменить', exact: true }).click(); await page.getByLabel('Текст диктовки').fill('Новый вариант');
    await page.evaluate(() => window.__voiceFixture.fail('transcriptUpdate')); await page.getByRole('button', { name: 'Сохранить', exact: true }).click();
    await page.locator('.vh-editor [role="alert"]').waitFor(); assert.ok((await first.locator('.vh-text').innerText()).startsWith('Запланировать')); assert.equal(await page.getByLabel('Текст диктовки').inputValue(), 'Новый вариант');
    await capture(page, 'history-edit-error-dark');
    await page.keyboard.press('Escape'); assert.equal(await page.locator('.vh-editor').count(), 0);
    await first.getByRole('button', { name: 'Изменить', exact: true }).click(); await page.getByLabel('Текст диктовки').fill('Сохранённый вариант'); await page.getByRole('button', { name: 'Сохранить', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('article[data-id="12"] .vh-text').textContent === 'Сохранённый вариант');
    assert.equal(await page.evaluate(() => window.__voiceFixture.saved.items[0].text), 'Сохранённый вариант');
    await first.getByRole('button', { name: 'Преобразовать', exact: true }).click(); await page.keyboard.press('ArrowDown'); await page.keyboard.press('Escape'); assert.equal(await page.getByRole('menu').count(), 0);
    await first.getByRole('button', { name: 'Преобразовать', exact: true }).click(); await page.getByRole('menuitem', { name: 'Чистовик', exact: true }).click();
    await page.getByText('Предпросмотр · не сохранён').waitFor(); assert.equal(await first.locator('.vh-text').innerText(), 'Сохранённый вариант');
    await page.getByRole('button', { name: 'Изменить и сохранить', exact: true }).click(); await page.getByRole('button', { name: 'Сохранить', exact: true }).click();
    await page.waitForFunction(() => !document.querySelector('.vh-editor')); assert.ok((await first.locator('.vh-text').innerText()).startsWith('Проверить интерфейс'));
    await first.getByRole('button', { name: 'Копировать', exact: true }).click(); assert.ok((await page.evaluate(() => window.__voiceFixture.saved.clipboard)).startsWith('Проверить интерфейс'));
    await page.evaluate(() => window.__voiceFixture.fail('transcriptDelete')); await first.getByRole('button', { name: 'Удалить', exact: true }).click(); await first.getByRole('alert').waitFor(); assert.equal(await page.locator('article').count(), 3);
    await first.getByRole('button', { name: 'Распознать снова', exact: true }).click(); await page.waitForFunction(() => document.querySelector('article[data-id="12"] .vh-text').textContent.startsWith('Повторное'));
    await page.getByLabel('Поиск по диктовкам').fill('нет такого текста'); assert.equal(await page.locator('article').count(), 0); await page.keyboard.press('Escape'); assert.equal(await page.locator('article').count(), 3);
    checks.push('history edit failure/cancel/save', 'enhance preview vs persisted replacement', 'copy native void acknowledgement', 'delete failure retains entry', 'audio capability and retranscribe', 'menu/search keyboard');

    await section(page, 'dict'); await page.getByLabel('Распознанное слово или фраза').fill('таури'); await page.getByLabel('Правильная запись').fill('Tauri');
    await page.evaluate(() => window.__voiceFixture.fail('dictionaryAdd')); await page.getByRole('button', { name: 'Добавить', exact: true }).click(); await page.locator('.vh-dictlist [role="alert"]').waitFor(); assert.equal(await page.locator('.lrow').count(), 3); assert.equal(await page.getByLabel('Правильная запись').inputValue(), 'Tauri');
    await capture(page, 'dictionary-error-dark'); await page.getByRole('button', { name: 'Добавить', exact: true }).click(); await page.waitForFunction(() => document.querySelectorAll('.lrow').length === 4);
    const tauri = page.locator('.lrow').filter({ hasText: 'таури' }); await tauri.getByRole('button', { name: 'Изменить', exact: true }).click(); await page.getByLabel('Правильная запись').fill('Tauri.app'); await page.keyboard.press('Escape'); assert.equal(await page.getByLabel('Правильная запись').inputValue(), '');
    await tauri.getByRole('button', { name: 'Изменить', exact: true }).click(); await page.getByLabel('Правильная запись').fill('Tauri.app'); await page.getByRole('button', { name: 'Сохранить', exact: true }).click(); await page.waitForFunction(() => window.__voiceFixture.saved.words.some(item => item.replacement === 'Tauri.app'));
    await page.evaluate(() => window.__voiceFixture.fail('dictionaryRemove')); await tauri.getByRole('button', { name: 'Удалить замену таури' }).click(); await page.locator('.vh-dictlist [role="alert"]').waitFor(); assert.equal(await page.locator('.lrow').count(), 4);
    await tauri.getByRole('button', { name: 'Удалить замену таури' }).click(); await page.waitForFunction(() => document.querySelectorAll('.lrow').length === 3);
    checks.push('dictionary add/edit/delete acknowledged state', 'dictionary failure preserves input', 'dictionary Escape cancels edit');

    await section(page, 'transforms'); assert.equal(await page.getByRole('switch').count(), 1); await page.evaluate(() => window.__voiceFixture.fail('promptsSetSmart')); await page.getByRole('switch').click(); await page.locator('section[data-k="transforms"] [role="alert"]').waitFor(); assert.equal(await page.getByRole('switch').getAttribute('aria-checked'), 'false');
    await capture(page, 'transforms-error-dark'); await page.getByRole('switch').click(); await page.waitForFunction(() => document.querySelector('[role="switch"]').getAttribute('aria-checked') === 'true');
    checks.push('smart mode native save and failure rollback', 'fixed styles have no unsupported mutable controls');

    await section(page, 'scratch'); const draft = page.locator('.scratch textarea'); await page.evaluate(() => window.__voiceFixture.fail('scratchpadSet')); await draft.fill('Несохранённый текст'); await page.locator('.scratch [role="alert"]').waitFor(); assert.ok((await page.locator('.vh-save-state').innerText()).includes('Не сохранено')); await capture(page, 'scratch-error-dark');
    await page.locator('.scratch').getByRole('button', { name: 'Повторить', exact: true }).click(); await page.waitForFunction(() => document.querySelector('.vh-save-state').textContent.startsWith('Сохранено'));
    await page.evaluate(() => window.__voiceFixture.hold('scratchpadSet')); await draft.fill('Запрос один'); await draft.fill('Последняя правка'); await page.evaluate(() => window.__voiceFixture.release('scratchpadSet')); await page.waitForFunction(() => window.__voiceFixture.saved.scratch === 'Последняя правка' && document.querySelector('.vh-save-state').textContent.startsWith('Сохранено'));
    await draft.focus(); await page.keyboard.press('Escape'); assert.equal(await page.locator('section[data-k="scratch"]').isVisible(), true); await page.keyboard.press('Escape'); assert.equal(await page.locator('section[data-k="history"]').isVisible(), true);
    await page.evaluate(async () => { document.getElementById('voicehist').replaceChildren(); await window.initVoiceHistory(document.getElementById('voicehist')); }); await section(page, 'scratch'); assert.equal(await page.locator('.scratch textarea').inputValue(), 'Последняя правка');
    checks.push('scratch failure explicit/retry', 'serialized writes preserve newest edit', 'Escape preserves draft and returns within voice', 'reinit reads persisted draft');

    for (const theme of ['dark', 'light']) for (const viewport of [{ width: 1040, height: 720 }, { width: 680, height: 560 }, { width: 480, height: 620 }]) {
      const visual = await open(theme, viewport);
      for (const key of ['history', 'insights', 'dict', 'transforms', 'scratch']) { await section(visual, key); await capture(visual, `${key}-${theme}-${viewport.width}x${viewport.height}`); }
    }
    const failure = await open('light', { width: 680, height: 560 }, ['transcriptsGet', 'dictionaryGet', 'promptsGet', 'scratchpadGet']);
    for (const key of ['history', 'insights', 'dict', 'transforms', 'scratch']) { await section(failure, key); await capture(failure, `${key}-read-error-light`); assert.ok(await failure.locator('.pane.on [role="alert"]').count()); }
    assert.equal(await failure.locator('.scratch textarea').isDisabled(), true); assert.equal(await failure.evaluate(() => window.__voiceFixture.calls.filter(call => call[0] === 'scratchpadSet').length), 0);
    checks.push('all five read errors explicit', 'missing read never writes empty draft', 'populated responsive light/dark');
    assert.deepEqual(exceptions, []); const report = { passed: true, native: false, fixtures: true, checks, screenshots: captures.length, captures }; fs.writeFileSync(path.join(out, 'report.json'), JSON.stringify(report, null, 2)); console.log(JSON.stringify({ passed: true, checks, screenshots: captures.length, exceptions }, null, 2));
  } finally { await browser.close(); server.close(); }
})().catch(error => { console.error(error); server.close(); process.exitCode = 1; });
