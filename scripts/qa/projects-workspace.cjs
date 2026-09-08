// Production renderer + Projects against a strict synthetic bridge; no native actions.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const os = require('node:os');
let playwright;
try { playwright = require('playwright'); } catch { playwright = require(process.env.JARVIS_PLAYWRIGHT_PATH || path.join(os.homedir(), '.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }
const root = path.resolve(__dirname, '../../ui'), out = path.resolve(__dirname, '../../docs/qa/assets/projects-workspace');
// Reuse the strict chat fixture, including real launch-event delivery behavior.
const base = fs.readFileSync(path.join(__dirname, 'session-workspace.cjs'), 'utf8');
const baseFixture = base.slice(base.indexOf('function bridgeFixture()'), base.indexOf('\nconst records = []'));
const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.woff2': 'font/woff2' };
const server = http.createServer((req, res) => {
  const file = path.resolve(root, '.' + new URL(req.url, 'http://localhost').pathname);
  if (!file.startsWith(root + path.sep)) return res.writeHead(403).end();
  fs.readFile(file, (error, data) => error ? res.writeHead(404).end() : res.writeHead(200, { 'Content-Type': mime[path.extname(file)] || 'application/octet-stream' }).end(data));
});
function projectsFixture() {
  const original = window.jarvis, f = window.__sessionFixture, now = Date.now();
  let projects = [
    { machine: 'local', cwd: '/work/jarvis', name: 'Jarvis', saved: true, pinned: true, count: 2, agents: ['claude', 'codex'], lastAt: now, sessions: [{ id: 'local-chat', agent: 'claude', title: 'Собрать интерфейс проектов', lastAt: now }, { id: 'old-local', agent: 'codex', title: 'Подготовить основу каталога', lastAt: now - 600000 }] },
    { machine: 'build-box', cwd: '/work/jarvis', name: 'Jarvis на сервере', saved: true, count: 2, agents: ['codex'], lastAt: now - 30000, sessions: [{ id: 'build-box:remote-chat', agent: 'codex', title: 'Проверить подключение по SSH', lastAt: now }, { id: 'build-box:old', agent: 'codex', title: 'Проверить проект перед сборкой', lastAt: now - 3600000 }] },
    { machine: 'offline-box', cwd: '/srv/design-system', name: 'Дизайн-система', saved: true, count: 1, agents: ['claude'], lastAt: now - 86400000, sessions: [{ id: 'offline-box:old', agent: 'claude', title: 'Обновить компоненты', lastAt: now - 86400000 }] },
    { machine: 'local', cwd: '/work/website', name: 'Личный сайт', saved: false, count: 1, agents: ['claude'], lastAt: now - 7200000, sessions: [{ id: 'website-old', agent: 'claude', title: 'Собрать первую страницу', lastAt: now - 7200000 }] },
  ];
  const artwork = (path, color, mark) => ({ path, name: path.split('/').pop(), dataUrl: 'data:image/svg+xml;base64,' + btoa(`<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64"><rect width="64" height="64" rx="15" fill="${color}"/><path d="${mark}" fill="#fff"/></svg>`) });
  const methods = {
    getSettings: async () => ({ ...await original.getSettings(), projects: projects.filter(p => p.saved) }),
    projectsIconCandidates: (machine, cwd) => ({ ok: true, candidates: cwd === '/work/jarvis' ? machine === 'local' ? [artwork('public/favicon.svg', '#587668', 'M18 17h28v9H28v19H18z')] : [artwork('public/favicon.svg', '#4b6b82', 'M18 17h28v9H28v19H18z'), artwork('public/brand-icon.svg', '#ad7552', 'M16 32l16-18 16 18-16 18z')] : [] }),
    projectsList: machine => ({ ok: true, projects: projects.filter(p => !machine || p.machine === machine), warnings: [{ machine: 'offline-box', error: 'SSH connection unavailable' }] }),
    projectsSave: p => { const found = projects.find(v => v.machine === p.machine && v.cwd === p.cwd); if (found) Object.assign(found, p, { saved: true }); else projects.push({ ...p, saved: true, count: 0, sessions: [], agents: [] }); return { ok: true, project: { ...p, saved: true } }; },
    projectsRemove: (machine, cwd) => { projects = projects.flatMap(p => p.machine !== machine || p.cwd !== cwd ? [p] : p.count ? [{ ...p, saved: false, pinned: false, name: p.cwd.split('/').pop() }] : []); return { ok: true }; },
    bundlePlaces: () => ({ ok: true, home: '/srv', known: [] }),
    bundleBrowse: (machine, cwd) => ({ ok: true, path: cwd, parent: cwd === '/' ? null : '/', dirs: cwd === '/srv' ? ['new-product', 'archive'] : [] }),
    copyText: () => ({ ok: true }),
    remotesList: () => [], vmStatus: () => ({ ok: true, available: false, generation: 'unknown', vms: [], capabilities: {} }),
  };
  window.jarvis = new Proxy({}, { get(_, name) {
    if (!(name in methods)) return original[name];
    return async (...args) => {
      f.calls.push({ name, args });
      if (f.fail[name]) { const error = f.fail[name]; delete f.fail[name]; throw new Error(error); }
      return structuredClone(await methods[name](...args));
    };
  } });
}
const records = [];
(async () => {
  fs.mkdirSync(out, { recursive: true });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const browser = await playwright.chromium.launch({ channel: 'chrome', headless: true });
  const page = await browser.newPage({ viewport: { width: 1280, height: 850 }, colorScheme: 'dark', reducedMotion: 'reduce' });
  const errors = []; page.on('pageerror', e => errors.push(e.message)); page.setDefaultTimeout(9000);
  const settle = () => page.waitForFunction(() => !document.getAnimations().some(a => a.playState === 'running' && Number.isFinite(a.effect?.getComputedTiming().endTime)));
  const capture = async name => { await settle(); await page.screenshot({ path: path.join(out, name + '.png') }); records.push(name); };
  const calls = name => page.evaluate(name => window.__sessionFixture.calls.filter(c => c.name === name), name);
  const catalog = () => page.locator('#history');
  const project = name => catalog().getByRole('button', { name, exact: true });
  try {
    await page.route('**/bridge.js', route => route.fulfill({ contentType: 'text/javascript', body: baseFixture + '\nbridgeFixture();\n(' + projectsFixture.toString() + ')();' }));
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
    await page.locator('#tabHistory').click();
    await page.locator('.pr-project-open').first().waitFor();
    assert.equal(await page.locator('.pr-project-open').count(), 4);
    assert.equal(await page.locator('.pr-warnings').evaluate(n => n.open), false);
    await page.waitForFunction(() => document.querySelector('.pr-project-row .project-avatar-image')?.naturalWidth > 0);
    await capture('projects-dark-desktop');
    assert.equal((await calls('projectsSave')).length, 0, 'Automatic favicon discovery must not save metadata');
    const remoteAvatar = page.locator('.pr-project-row').filter({ hasText: 'Jarvis на сервере' }).locator('.project-avatar');
    await remoteAvatar.locator('.project-avatar-count').waitFor();
    assert.equal(await remoteAvatar.locator('.project-avatar-count').innerText(), '2');
    await remoteAvatar.click();
    await page.locator('.pr-avatar-candidate').first().waitFor();
    assert.equal(await page.locator('.pr-avatar-candidate').count(), 2);
    await page.waitForFunction(() => [...document.querySelectorAll('.pr-avatar-candidate img')].every(n => n.naturalWidth > 0));
    await capture('avatar-candidates-dark');
    await project('public/brand-icon.svg').click();
    await page.locator('.pr-avatar-status').filter({ hasText: 'Картинка выбрана' }).waitFor();
    await project('Сохранить проект').click();
    await page.locator('.pr-heading h1').filter({ hasText: 'Jarvis на сервере' }).waitFor();
    const selectedAvatar = (await calls('projectsSave')).at(-1).args[0].avatar;
    assert.equal(selectedAvatar.source, 'project'); assert.equal(selectedAvatar.path, 'public/brand-icon.svg');
    assert.match(selectedAvatar.dataUrl, /^data:image\/png;base64,/);
    await page.waitForFunction(src => [...document.querySelectorAll('.sw-project')].some(n => n.textContent.includes('Jarvis на сервере') && n.querySelector('img')?.src === src), selectedAvatar.dataUrl);
    await project('Настроить проект').click();
    await page.locator('input[type=file][aria-label="Картинка проекта"]').setInputFiles({ name: 'broken.png', mimeType: 'image/png', buffer: Buffer.from('not a png') });
    await page.locator('.pr-avatar-status.error').waitFor();
    assert.equal(await page.locator('.pr-avatar-preview img').getAttribute('src'), selectedAvatar.dataUrl);
    await page.locator('input[type=file][aria-label="Картинка проекта"]').setInputFiles({ name: 'custom-avatar.svg', mimeType: 'image/svg+xml', buffer: Buffer.from('<svg xmlns="http://www.w3.org/2000/svg" width="400" height="200"><rect width="400" height="200" rx="40" fill="#527763"/><circle cx="200" cy="100" r="65" fill="#f3dca1"/></svg>') });
    await page.locator('.pr-avatar-status').filter({ hasText: 'Картинка загружена' }).waitFor();
    await page.evaluate(() => window.__sessionFixture.fail.projectsSave = 'Avatar save failed');
    await project('Сохранить проект').click();
    await page.getByRole('alert').filter({ hasText: 'Avatar save failed' }).waitFor();
    assert.match(await page.locator('.pr-avatar-edit-copy').innerText(), /custom-avatar.svg/);
    await project('Сохранить проект').click();
    await page.locator('.pr-heading h1').filter({ hasText: 'Jarvis на сервере' }).waitFor();
    const uploadedAvatar = (await calls('projectsSave')).at(-1).args[0].avatar;
    assert.equal(uploadedAvatar.source, 'upload'); assert.match(uploadedAvatar.dataUrl, /^data:image\/png;base64,/);
    const uploadedSize = await page.evaluate(src => new Promise(resolve => { const image = new Image(); image.onload = () => resolve([image.naturalWidth,image.naturalHeight]); image.src=src; }), uploadedAvatar.dataUrl);
    assert.deepEqual(uploadedSize, [128,64]);
    await capture('avatar-uploaded-dark');
    await project('Настроить проект').click();
    await project('Убрать картинку').click();
    await project('Сохранить проект').click();
    await page.locator('.pr-heading h1').filter({ hasText: 'Jarvis на сервере' }).waitFor();
    assert.equal((await calls('projectsSave')).at(-1).args[0].avatar, null);
    await project('Все проекты').click();
    records.push('avatars-auto-multiple-choice-upload-resize-save-failure-reset-sidebar');
    assert.equal(await catalog().locator('[data-icon="circle-dashed"]').count(), 0);
    await page.locator('#primaryHint').click();
    await page.locator('.pr-heading h1').filter({ hasText: 'Jarvis' }).waitFor();
    assert.equal(await page.locator('#primaryLabel').innerText(), 'Назад');
    await page.locator('#primaryHint').click();
    assert.equal(await page.locator('.pr-heading h1').innerText(), 'Проекты');
    records.push('footer-action-matches-label');
    await catalog().getByRole('button', { name: 'В работе', exact: true }).click();
    assert.equal(await page.locator('.pr-project-open').count(), 1);
    await catalog().getByRole('button', { name: 'Все', exact: true }).click();
    await page.locator('#query').fill('Jarvis');
    assert.equal(await page.locator('.pr-project-open').count(), 2);
    await project('Jarvis на сервере, build-box').click();
    assert.equal(await page.locator('.pr-conversation').count(), 2);
    assert.equal(await page.locator('.pr-environment').evaluate(n => n.open), false);
    await capture('project-remote-dark');
    await project('Новый чат').click();
    await page.waitForFunction(() => document.querySelector('#newChatMachine').value === 'build-box' && document.querySelector('#newChatDirectory').value === '/work/jarvis');
    assert.equal((await calls('launchSession')).length, 0);
    await page.locator('#pageBack').click();
    await page.locator('.pr-heading h1').filter({ hasText: 'Jarvis на сервере' }).waitFor();
    await project('Все проекты').click();
    assert.equal(await page.locator('#query').inputValue(), 'Jarvis');
    records.push('machine-and-path-new-chat-handoff-and-route-back');
    await page.locator('#query').fill('');
    const row = page.locator('.pr-project-row').filter({ hasText: 'Личный сайт' });
    await row.getByRole('button', { name: 'Закрепить проект' }).click();
    await page.locator('.pr-message').filter({ hasText: 'Проект сохранён' }).waitFor();
    assert.equal(await page.locator('.pr-heading h1').innerText(), 'Проекты');
    await catalog().getByRole('button', { name: 'Сохранённые', exact: true }).click();
    assert.equal(await page.locator('.pr-project-open').count(), 4);
    await project('Дизайн-система, offline-box').click();
    assert.equal(await project('Новый чат').isDisabled(), true);
    assert.equal(await project('Продолжить').isDisabled(), true);
    await capture('project-offline-dark');
    await project('Все проекты').click();
    await project('Добавить проект').click();
    await page.locator('#projectName').fill('Новый продукт');
    await page.getByRole('combobox', { name: 'Машина проекта', exact: true }).selectOption('build-box');
    await project('Выбрать папку').click();
    await project('new-product').click();
    await project('Выбрать эту папку').click();
    assert.equal(await page.locator('#projectPath').inputValue(), '/srv/new-product');
    assert.deepEqual((await calls('bundleBrowse')).map(c => c.args), [['build-box', '/srv'], ['build-box', '/srv/new-product']]);
    await page.evaluate(() => window.__sessionFixture.fail.projectsSave = 'Synthetic disk full');
    await project('Сохранить проект').click();
    await page.getByRole('alert').filter({ hasText: 'Synthetic disk full' }).waitFor();
    assert.equal(await page.locator('#projectName').inputValue(), 'Новый продукт');
    assert.equal(await page.locator('#projectPath').inputValue(), '/srv/new-product');
    await capture('project-save-error-draft');
    await project('Сохранить проект').click();
    await page.locator('.pr-heading h1').filter({ hasText: 'Новый продукт' }).waitFor();
    await project('Настроить проект').click();
    await project('Убрать из сохранённых').click();
    await page.waitForFunction(() => !document.querySelector('.pr-form') && document.querySelector('.pr-heading h1').textContent === 'Проекты');
    assert.deepEqual((await calls('projectsRemove')).map(c => c.args), [['build-box', '/srv/new-product']]);
    records.push('remote-folder-browser-save-failure-retry-metadata-removal');
    await catalog().getByRole('button', { name: 'Все', exact: true }).click();
    await project('Jarvis на сервере, build-box').click();
    await page.evaluate(() => window.__sessionFixture.nextLaunch = { hold: true, result: { ok: true, launchId: 'resume-qa', cwd: '/work/jarvis' } });
    await page.locator('.pr-conversation').filter({ hasText: 'Проверить проект перед сборкой' }).getByRole('button').click();
    await page.evaluate(() => {
      const f = window.__sessionFixture;
      f.emitState([...f.sessions(), { id: 'build-box:old', remote: 'build-box', cwd: '/work/jarvis', agent: 'codex', status: 'idle', title: 'Возобновлённый чат', updatedAt: Date.now() }]);
      f.emitLaunch({ launchId: 'somebody-else', machine: 'build-box', sessionId: 'build-box:old', status: 'ready' });
    });
    assert.equal(await page.locator('html').getAttribute('data-view'), 'history');
    await page.evaluate(() => window.__sessionFixture.emitLaunch({ launchId: 'resume-qa', machine: 'build-box', sessionId: 'build-box:old', status: 'ready' }));
    assert.equal(await page.locator('html').getAttribute('data-view'), 'history');
    await page.evaluate(() => window.__sessionFixture.resolveLaunchReplies());
    await page.waitForFunction(() => document.documentElement.dataset.view === 'chat');
    assert.deepEqual((await calls('launchSession')).at(-1).args, ['/work/jarvis', 'codex', 'build-box:old', 'build-box', { mode: 'ask' }]);
    records.push('resume-exact-launch-correlation-and-early-event');
    await page.locator('#pageBack').click();
    await project('Все проекты').click();
    await page.setViewportSize({ width: 740, height: 600 });
    await page.evaluate(() => window.jarvisTheme.adopt({ theme: 'light', mode: 'overlay' }));
    await capture('projects-light-compact');
    for (const selector of ['#panel', '#history', '.pr-page']) {
      const d = await page.locator(selector).evaluate(n => ({ width: n.clientWidth, scroll: n.scrollWidth }));
      assert.ok(d.scroll <= d.width + 1, `${selector} overflows: ${JSON.stringify(d)}`);
    }
    await project('Jarvis на сервере, build-box').click();
    await capture('project-light-compact');
    const titleTop = await page.locator('.pr-heading h1').boundingBox();
    const contentTop = await page.locator('.content').boundingBox();
    assert.ok(titleTop.y >= contentTop.y, 'Project title must be visible after opening from a scrolled catalog');
    await project('Настроить проект').click();
    await capture('project-form-light-compact');
    await project('Отмена').click();
    await page.locator('.pr-environment > summary').click();
    await project('Настроить машины и VM').click();
    await page.locator('#s2-pane-remotes .dtitle').waitFor();
    assert.equal(await page.locator('.snav [data-pane="remotes"]').evaluate(n => n.classList.contains('sel')), true);
    records.push('project-settings-opens-machines-pane');
    assert.deepEqual(errors, []);
    assert.deepEqual(await page.evaluate(() => window.__sessionFixture.unknown), []);
    records.push('no-browser-errors-or-unexpected-native-calls');
    fs.writeFileSync(path.join(out, 'checks.json'), JSON.stringify({ checks: records, errors }, null, 2) + '\n');
    console.log(JSON.stringify({ ok: true, checks: records.length, records }, null, 2));
  } finally { await browser.close(); await new Promise(r => server.close(r)); }
})().catch(error => { console.error(error); process.exitCode = 1; });
