// Production renderer in Chromium. Synthetic connection transport only: this
// harness never opens SSH, writes account settings, installs a node, or runs VM.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const http = require('node:http');
const { createHash } = require('node:crypto');
const os = require('node:os');
const path = require('node:path');
let playwright;
try { playwright = require('playwright'); }
catch { playwright = require(process.env.JARVIS_PLAYWRIGHT_PATH || path.join(os.homedir(), '.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }

const ui = path.resolve(__dirname, '../../ui');
const out = path.resolve(__dirname, '../../docs/qa/assets/connections-design');
const fixtureFile = fs.readFileSync(path.join(__dirname, 'session-workspace.cjs'), 'utf8');
const fixtureSource = fixtureFile.slice(fixtureFile.indexOf('function bridgeFixture()'), fixtureFile.indexOf('\nconst records ='));
const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.woff2': 'font/woff2' };
const server = http.createServer((req, res) => {
  const file = path.resolve(ui, '.' + new URL(req.url, 'http://localhost').pathname);
  if (!file.startsWith(ui + path.sep)) return res.writeHead(403).end();
  fs.readFile(file, (error, data) => error ? res.writeHead(404).end() : res.writeHead(200, { 'Content-Type': mime[path.extname(file)] || 'application/octet-stream' }).end(data));
});

function connectionsFixture() {
  const original = window.jarvis, fixture = window.__sessionFixture;
  const sources = [
    { id: 'vm-personal', agent: 'codex', label: 'Codex Personal', providerHome: '/home/coder/.codex', enabled: true },
    { id: 'vm-claude', agent: 'claude', label: 'Claude Code', providerHome: '/home/coder/.claude', enabled: true },
  ];
  const profile = { proxy: 'teleport.example.test', cluster: 'development', username: 'developer', logins: ['developer', 'root'], authenticated: true, validUntil: '2030-01-01T00:00:00Z' };
  const state = window.__connectionsQA = {
    mode: 'authenticated', fail: {}, holdProbe: false, pendingProbe: null, holdPlugins: false, pendingPlugins: null,
    remotes: [
      { name: 'build-box', transport: 'ssh', sshHost: 'coder@build.example.test', jarvisDir: '/home/coder/.jarvis', connected: true, version: '0.3.3', sources: structuredClone(sources) },
      { name: 'ticksly-runner-2', transport: 'teleport', sshHost: 'coder@4829bdbd-43bc-43e1-b8ec-c9f4894ca785', teleportProxy: profile.proxy, teleportCluster: profile.cluster, jarvisDir: '/home/coder/.jarvis', connected: true, version: '0.3.3', sources: structuredClone(sources) },
      { name: 'staging-db', transport: 'ssh', sshHost: 'admin@staging.example.test', jarvisDir: '~/.jarvis', connected: false, error: 'SSH connection timed out', version: '0.3.0', outdated: true, sources: [] },
    ],
    vm: { ok: true, available: true, version: '0.7.0', generation: 'modern', capabilities: {}, vms: [
      { name: 'dev-linux', status: 'stopped', capabilities: { start: true, stop: true, openConfig: true }, projects: [{ name: 'Jarvis', path: '/fixture/jarvis', guestPath: '/workspace/jarvis' }], directory: '/fixture/agent-vm/dev-linux', configPath: '/fixture/agent-vm/dev-linux/config.yaml', connection: { canConnect: false } },
    ] },
    resolveProbe(result) { const resolve = state.pendingProbe; state.pendingProbe = null; resolve?.(result); },
    resolvePlugins() { const resolve = state.pendingPlugins; state.pendingPlugins = null; resolve?.([]); },
    emit(name, payload) { for (const callback of fixture.events[name] || []) callback(structuredClone(payload)); },
  };
  const probe = () => ({ ok: true, os: 'linux', arch: 'x86_64', home: '/home/developer', dir: '/home/developer/.jarvis', tmux: false, curl: true, claude: true, codex: true, systemd: true, nodeSource: 'local', providerSources: [{ agent: 'codex', providerHome: '/home/developer/.codex' }], runtimeSetup: { missing: ['tmux'], automatic: true } });
  const methods = {
    getSettings: async () => ({ ...await original.getSettings(), launchTerminal: 'terminal-app' }),
    getPlugins: () => state.holdPlugins ? new Promise(resolve => { state.pendingPlugins = resolve; state.holdPlugins = false; }) : [],
    remotesList: () => state.remotes,
    machinesList: () => [{ id: 'local', name: 'Этот компьютер', kind: 'local', online: true }, ...state.remotes.map(remote => ({ id: remote.name, name: remote.name, kind: 'remote', online: remote.connected }))],
    remotesTest: name => { const remote = state.remotes.find(item => item.name === name); if (remote) { remote.connected = true; delete remote.error; } return { ok: true, host: name, version: '0.3.3', sources }; },
    remotesRepairSource: () => ({ ok: true }),
    remotesPreflight: () => state.holdProbe ? new Promise(resolve => { state.pendingProbe = resolve; state.holdProbe = false; }) : probe(),
    remotesInstall: () => ({ ok: true }),
    remotesRemove: name => { state.remotes = state.remotes.filter(remote => remote.name !== name); return { ok: true }; },
    remotesAdd: config => { state.remotes.push({ ...config, connected: true, version: '0.3.3', sources }); return { ok: true }; },
    remotesSshKey: () => ({ ok: true, publicKey: '', path: '' }),
    remotesSshAuthorize: () => ({ ok: true }),
    teleportStatus: () => state.mode === 'authenticated' ? { ok: true, available: true, version: '18.10.0', ...profile, profiles: [profile] } : { ok: true, available: true, authenticated: false, proxy: profile.proxy, profiles: [] },
    teleportNodes: () => ({ ok: true, cluster: profile.cluster, logins: profile.logins, nodes: [{ id: 'node-uuid', target: 'node-uuid', name: 'Development VM', hostname: 'development-vm', labels: { environment: 'dev' } }, { id: 'runner-uuid', target: 'runner-uuid', name: 'Build runner', hostname: 'runner-vm', labels: {} }] }),
    teleportLogin: () => { state.mode = 'authenticated'; return { ok: true, started: true }; },
    vmStatus: () => state.vm,
    vmAction: (name, action) => { const machine = state.vm.vms.find(item => item.name === name); assertMachine(machine); if (action === 'start') machine.status = 'running'; else if (action === 'stop') machine.status = 'stopped'; return { ok: true }; },
  };
  function assertMachine(machine) { if (!machine) throw new Error('Unknown synthetic VM'); }
  window.jarvis = new Proxy({}, { get(_, name) {
    if (!(name in methods)) return original[name];
    return async (...args) => {
      fixture.calls.push({ name, args: structuredClone(args) });
      if (state.fail[name]) { const error = state.fail[name]; delete state.fail[name]; return { ok: false, error }; }
      return structuredClone(await methods[name](...args));
    };
  } });
}

const records = [];
(async () => {
  fs.mkdirSync(out, { recursive: true });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const browser = await playwright.chromium.launch({ channel: 'chrome', headless: true });
  const page = await browser.newPage({ viewport: { width: 1440, height: 960 }, colorScheme: 'dark', reducedMotion: 'reduce' });
  page.setDefaultTimeout(9000);
  const errors = []; page.on('pageerror', error => errors.push(error.message));
  const pane = page.locator('#machines2 #s2-pane-remotes');
  const wizard = page.locator('#machines2 #s2-rwiz');
  const grid = pane.locator('.connection-grid');
  const detail = pane.locator('.connection-detail');
  const editor = pane.locator('#s2-ssh-setup');
  const search = pane.getByRole('searchbox', { name: 'Найти машину', exact: true });
  const add = pane.getByRole('button', { name: 'Добавить машину', exact: true });
  const selectMachine = name => grid.locator(`[data-machine-name="${name}"]`).click();
  const calls = name => page.evaluate(name => window.__sessionFixture.calls.filter(call => call.name === name), name);
  const choose = async (label, name) => {
    await wizard.getByRole('combobox', { name: label, exact: true }).click();
    const popup = page.locator('.jselect-popover:not([aria-hidden="true"]):visible');
    await popup.getByRole('option', { name, exact: true }).click();
    await popup.waitFor({ state: 'hidden' });
  };
  const capture = async name => {
    await page.evaluate(async () => { await document.fonts.ready; await Promise.allSettled(document.getAnimations().filter(animation => Number.isFinite(animation.effect?.getComputedTiming().endTime)).map(animation => animation.finished)); });
    await page.screenshot({ path: path.join(out, `${name}.png`) });
  };
  const noOverflow = async name => {
    const size = await pane.evaluate(node => ({ client: node.clientWidth, scroll: node.scrollWidth, document: document.documentElement.scrollWidth, viewport: innerWidth }));
    assert.ok(size.scroll <= size.client + 1 && size.document <= size.viewport + 1, `${name}: ${JSON.stringify(size)}`);
    records.push(`${name}: no horizontal overflow`);
  };
  try {
    await page.route('**/bridge.js', route => route.fulfill({ contentType: 'text/javascript', body: `${fixtureSource}\nbridgeFixture();\n(${connectionsFixture.toString()})();` }));
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
    await page.locator('#tabMachines').click();
    await add.waitFor();
    assert.equal(await page.locator('html').getAttribute('data-view'), 'machines');
    assert.equal(await page.locator('#machines2.settings-surface').isVisible(), true);
    assert.equal(await page.locator('#machines2 .sidebar, #machines2 .snav').count(), 0);
    assert.equal(await page.locator('#settings2:visible').count(), 0);
    records.push('launcher opens standalone Machines module with no Settings sidebar');
    await page.keyboard.press('Escape');
    await page.waitForFunction(() => document.documentElement.dataset.view === 'home');
    await page.keyboard.press('Meta+8');
    await add.waitFor();
    assert.equal(await page.locator('html').getAttribute('data-view'), 'machines');
    await page.keyboard.press('Meta+,');
    await page.waitForFunction(() => document.documentElement.dataset.view === 'settings');
    await page.locator('#settings2 .sidebar').waitFor();
    await page.locator('#pageBack').click();
    await add.waitFor();
    assert.equal(await page.locator('html').getAttribute('data-view'), 'machines');
    assert.equal(await page.locator('#machines2 .sidebar, #machines2 .snav').count(), 0);
    assert.equal(await page.locator('#s2-pane-remotes').count(), 1);
    records.push('Cmd8 opens Machines; Escape returns home; Settings Back restores standalone module');
    await page.evaluate(() => { window.__connectionsQA.holdPlugins = true; });
    await page.keyboard.press('Meta+,');
    await page.waitForFunction(() => !!window.__connectionsQA.pendingPlugins);
    await page.keyboard.press('Meta+8');
    await add.waitFor();
    await page.evaluate(() => window.__connectionsQA.resolvePlugins());
    assert.equal(await page.locator('html').getAttribute('data-view'), 'machines');
    assert.equal(await page.locator('#machines2.settings-surface').isVisible(), true);
    assert.equal(await page.locator('#settings2:visible').count(), 0);
    assert.equal(await page.locator('#s2-pane-remotes').count(), 1);
    records.push('late Settings plugin response cannot replace the active Machines module');
    await page.keyboard.press('Meta+0');
    await page.waitForFunction(() => document.documentElement.dataset.view === 'home');
    await page.locator('#query').fill('VM');
    await page.locator('#tabMachines').waitFor();
    await page.keyboard.press('Enter');
    await add.waitFor();
    assert.equal(await page.locator('html').getAttribute('data-view'), 'machines');
    await page.keyboard.press('Meta+,');
    await page.waitForFunction(() => document.documentElement.dataset.view === 'settings');
    await page.keyboard.press('Meta+k');
    await page.locator('#commandQuery').fill('Teleport');
    assert.equal(await page.locator('#commandResults [role="option"]').count(), 1);
    assert.equal(await page.locator('#commandResults [role="option"] strong').innerText(), 'Машины');
    await page.keyboard.press('Enter');
    await page.locator('#commandDialog').waitFor({ state: 'hidden' });
    await add.waitFor();
    assert.equal(await page.locator('html').getAttribute('data-view'), 'machines');
    records.push('launcher VM search and CmdK Teleport search open Machines with Enter');
    assert.equal(await grid.locator('[data-machine-name]').count(), 3);
    assert.equal(await grid.locator('[data-vm-name]').count(), 1);
    assert.equal(await editor.isVisible(), false);
    assert.equal(await grid.getByRole('button', { name: /Переустановить|Удалить|Проверить хуки/ }).count(), 0);
    await selectMachine('ticksly-runner-2');
    await detail.getByText('ticksly-runner-2', { exact: true }).waitFor();
    assert.equal((await calls('remotesTest')).length, 0);
    assert.equal((await calls('remotesPreflight')).length, 0);
    assert.equal((await calls('remotesInstall')).length, 0);
    records.push('machine selection reveals detail without connecting, checking or installing');

    await search.fill('staging');
    assert.equal(await grid.locator('[data-machine-name]').count(), 1);
    await grid.locator('[data-machine-name="staging-db"]').focus();
    await page.keyboard.press('Enter');
    await detail.getByText('staging-db', { exact: true }).waitFor();
    await detail.getByText('SSH connection timed out', { exact: true }).waitFor();
    await search.fill('does-not-exist');
    assert.equal(await grid.locator('[data-machine-name], [data-vm-name]').count(), 0);
    await search.fill('');
    assert.equal(await grid.locator('[data-machine-name]').count(), 3);
    await pane.getByRole('button', { name: 'Локальные VM', exact: true }).click();
    assert.equal(await grid.locator('[data-machine-name]').count(), 0);
    assert.equal(await grid.locator('[data-vm-name]').count(), 1);
    await pane.getByRole('button', { name: 'Удалённые', exact: true }).click();
    assert.equal(await grid.locator('[data-machine-name]').count(), 3);
    assert.equal(await grid.locator('[data-vm-name]').count(), 0);
    await pane.getByRole('button', { name: 'Все', exact: true }).click();
    records.push('search filters inventory, supports empty results and keyboard selection');

    await page.evaluate(() => { window.__connectionsQA.fail.remotesTest = 'Synthetic SSH timeout'; });
    await detail.getByRole('button', { name: 'Проверить', exact: true }).click();
    await detail.getByText('Synthetic SSH timeout', { exact: true }).waitFor();
    assert.equal((await calls('remotesTest')).at(-1).args[0], 'staging-db');
    await detail.getByRole('button', { name: 'Проверить', exact: true }).click();
    await page.waitForFunction(() => window.__connectionsQA.remotes.find(remote => remote.name === 'staging-db').connected);
    assert.equal(await detail.getByText('Synthetic SSH timeout', { exact: true }).count(), 0);
    records.push('selected machine test uses exact name, displays failure, and recovers');

    await selectMachine('ticksly-runner-2');
    await detail.getByRole('button', { name: 'Настройки', exact: true }).click();
    await detail.getByRole('button', { name: 'Проверить хуки', exact: true }).click();
    await detail.getByRole('button', { name: 'Хуки настроены', exact: true }).waitFor();
    assert.deepEqual((await calls('remotesRepairSource')).at(-1).args, ['ticksly-runner-2', 'vm-personal']);
    assert.equal((await calls('remotesInstall')).length, 0);
    await detail.getByRole('button', { name: 'Обзор', exact: true }).click();
    assert.equal(await detail.getByRole('button', { name: 'Удалить подключение', exact: true }).isVisible(), false);
    records.push('maintenance is separate; hook repair targets selected machine and exact Codex source');
    await capture('machines-dark');
    await noOverflow('wide overview');
    await page.setViewportSize({ width: 1040, height: 820 });
    await page.emulateMedia({ colorScheme: 'light' });
    await page.evaluate(() => window.jarvisTheme.adopt({ theme: 'light', mode: 'window' }));
    await capture('machines-light');
    await noOverflow('medium light overview');
    await page.setViewportSize({ width: 720, height: 820 });
    await page.emulateMedia({ colorScheme: 'dark' });
    await page.evaluate(() => window.jarvisTheme.adopt({ theme: 'dark', mode: 'window' }));
    await capture('machines-compact');
    await noOverflow('compact overview');
    await selectMachine('build-box');
    await page.waitForFunction(() => {
      const rect = document.querySelector('.connection-detail h2')?.getBoundingClientRect();
      return rect && rect.top >= 0 && rect.bottom < innerHeight;
    });
    await capture('machines-compact-inspector');
    records.push('compact card selection scrolls its inspector into view');

    await page.setViewportSize({ width: 1280, height: 900 });
    await add.click();
    await wizard.getByRole('textbox', { name: 'SSH-хост', exact: true }).fill('developer@draft.example.test');
    await editor.getByRole('button', { name: 'Отмена', exact: true }).click();
    assert.equal(await editor.isVisible(), false);
    await add.click();
    assert.equal(await wizard.getByRole('textbox', { name: 'SSH-хост', exact: true }).inputValue(), 'developer@draft.example.test');
    await page.keyboard.press('Escape');
    assert.equal(await editor.isVisible(), false);
    assert.equal(await pane.isVisible(), true);
    assert.equal(await page.locator('html').getAttribute('data-view'), 'machines');
    records.push('add editor has cancel and Escape, retaining draft without leaving Machines');

    await add.click();
    await page.evaluate(() => { window.__connectionsQA.holdProbe = true; });
    await wizard.getByRole('button', { name: 'Проверить машину', exact: true }).click();
    await page.waitForFunction(() => !!window.__connectionsQA.pendingProbe);
    await editor.getByRole('button', { name: 'Отмена', exact: true }).click();
    await page.evaluate(() => window.__connectionsQA.resolveProbe({ ok: true, os: 'STALE CANCELLED RESULT', nodeSource: 'local', claude: true }));
    await add.click();
    assert.equal(await wizard.getByText('STALE CANCELLED RESULT', { exact: false }).count(), 0);
    assert.equal(await wizard.getByRole('button', { name: 'Настроить автоматически', exact: true }).count(), 0);
    assert.equal(await wizard.getByRole('button', { name: 'Проверить машину', exact: true }).isEnabled(), true, 'cancelled preflight must restore an actionable editor');
    records.push('cancel invalidates in-flight probe and stale response cannot install another draft');

    await capture('add-ssh-dark');
    await wizard.getByRole('button', { name: 'Teleport (tsh)', exact: true }).click();
    await choose('Пользователь SSH', 'developer');
    await choose('Машина Teleport', 'Development VM');
    await wizard.getByRole('combobox', { name: 'Машина Teleport', exact: true }).click();
    await page.keyboard.press('Escape');
    assert.equal(await editor.isVisible(), true);
    assert.equal(await pane.isVisible(), true);
    await capture('add-teleport-dark');
    await wizard.getByRole('button', { name: 'Проверить машину', exact: true }).click();
    await wizard.getByRole('button', { name: 'Настроить автоматически', exact: true }).waitFor();
    const preflight = (await calls('remotesPreflight')).at(-1);
    assert.equal(preflight.args[0], 'developer@node-uuid');
    assert.equal(preflight.args[2].transport, 'teleport');
    assert.equal(preflight.args[2].teleportProxy, 'teleport.example.test');
    assert.equal(preflight.args[2].teleportCluster, 'development');
    assert.equal((await calls('remotesInstall')).length, 0);
    records.push('Teleport designed selectors retain exact proxy, cluster, login and node; probe is read-only');

    await page.setViewportSize({ width: 720, height: 820 });
    await capture('preflight-compact');
    await noOverflow('compact preflight');
    await wizard.getByRole('button', { name: 'Настроить автоматически', exact: true }).click();
    await page.waitForFunction(() => window.__sessionFixture.calls.some(call => call.name === 'remotesInstall'));
    const installed = (await calls('remotesInstall')).at(-1).args[0];
    assert.equal(installed.transport, 'teleport');
    assert.equal(installed.teleportProxy, 'teleport.example.test');
    assert.equal(installed.teleportCluster, 'development');
    assert.equal(installed.sshHost, 'developer@node-uuid');
    await page.evaluate(() => window.__connectionsQA.emit('onRemoteInstallStep', { phase: '\u001b[32mПроверка\u001b[0m', state: 'done', msg: 'События доставлены', pct: 60 }));
    await wizard.locator('#s2-rlog .msg', { hasText: 'События доставлены' }).waitFor();
    assert.equal(await wizard.locator('#s2-rlog .install-phase').last().textContent(), 'Проверка');
    await editor.getByRole('button', { name: 'Свернуть', exact: true }).click();
    assert.equal(await editor.isVisible(), false);
    await selectMachine('build-box');
    await detail.getByRole('button', { name: 'Настройки', exact: true }).click();
    await detail.getByRole('button', { name: 'Переустановить', exact: true }).click();
    await wizard.locator('#s2-rlog .msg', { hasText: 'События доставлены' }).waitFor();
    assert.equal((await calls('remotesInstall')).length, 1, 'another machine must not replace the pending installation');
    assert.equal((await calls('remotesInstall'))[0].args[0].name, installed.name);
    records.push('minimized installer resumes without resetting progress or changing its connection');
    await page.evaluate(() => window.__connectionsQA.emit('onRemoteInstallDone', { ok: true, name: 'different-installation' }));
    assert.equal(await wizard.locator('#s2-rlog').isVisible(), true);
    await page.keyboard.press('Meta+,');
    await page.waitForFunction(() => document.documentElement.dataset.view === 'settings');
    await page.keyboard.press('Meta+8');
    await page.waitForFunction(() => document.documentElement.dataset.view === 'machines');
    await wizard.locator('#s2-rlog .msg', { hasText: 'События доставлены' }).waitFor();
    records.push('installation log sanitizes ANSI and survives module navigation; unrelated completion ignored');

    await page.evaluate(config => {
      const state = window.__connectionsQA;
      state.remotes.push({ ...config, connected: true, version: '0.3.3', sources: [] });
      state.emit('onRemoteInstallDone', { ok: true, name: config.name });
    }, installed);
    await wizard.getByRole('button', { name: 'Открыть чаты', exact: true }).waitFor();
    await grid.locator(`[data-machine-name="${installed.name}"]`).waitFor({ state: 'attached' });
    await capture('installed-compact');
    await wizard.getByRole('button', { name: 'Открыть чаты', exact: true }).click();
    await page.waitForFunction(name => document.querySelector('#newChatMachine')?.value === name, installed.name);
    records.push('install completion updates inventory and Open chats targets installed machine');

    await page.keyboard.press('Meta+0');
    await page.waitForFunction(() => document.documentElement.dataset.view === 'home');
    await page.locator('#tabMachines').click();
    if (await editor.isVisible()) await editor.getByRole('button', { name: 'Отмена', exact: true }).click();
    await grid.locator('[data-vm-name="dev-linux"]').click();
    await pane.getByRole('button', { name: 'Обновить', exact: true }).click();
    await detail.getByRole('heading', { name: 'dev-linux', exact: true }).waitFor();
    assert.equal(await grid.locator('[data-vm-name="dev-linux"]').getAttribute('aria-pressed'), 'true');
    records.push('completed connection is selected once and does not steal later VM selection on refresh');

    await page.setViewportSize({ width: 1280, height: 900 });
    await page.reload();
    await page.evaluate(() => { window.__connectionsQA.mode = 'expired'; });
    await page.locator('#tabMachines').click();
    await add.click();
    await wizard.getByRole('button', { name: 'Teleport (tsh)', exact: true }).click();
    await wizard.getByRole('button', { name: 'Войти через Teleport', exact: true }).waitFor();
    assert.equal(await wizard.locator('input[type="password"]').count(), 0);
    await capture('teleport-login-dark');
    await wizard.getByRole('button', { name: 'Войти через Teleport', exact: true }).click();
    await wizard.getByRole('combobox', { name: 'Пользователь SSH', exact: true }).waitFor();
    assert.equal((await calls('teleportLogin')).length, 1);
    assert.equal((await calls('remotesSshAuthorize')).length, 0);
    records.push('expired Teleport uses SSO flow without SSH password and polling observes renewed session');
    await editor.getByRole('button', { name: 'Отмена', exact: true }).click();

    await grid.locator('[data-vm-name="dev-linux"]').click();
    await detail.getByRole('button', { name: 'Запустить', exact: true }).click();
    await detail.getByRole('button', { name: 'Остановить', exact: true }).waitFor();
    assert.deepEqual((await calls('vmAction')).at(-1).args, ['dev-linux', 'start']);
    await capture('vm-running-dark');
    await detail.getByRole('button', { name: 'Остановить', exact: true }).click();
    await detail.getByRole('button', { name: 'Запустить', exact: true }).waitFor();
    assert.deepEqual((await calls('vmAction')).at(-1).args, ['dev-linux', 'stop']);
    records.push('local VM details preserve start/stop behavior and selection across refresh');

    await selectMachine('staging-db');
    await detail.getByRole('button', { name: 'Настройки', exact: true }).click();
    await detail.getByRole('button', { name: 'Удалить подключение', exact: true }).click();
    assert.equal((await calls('remotesRemove')).length, 0);
    await detail.getByRole('button', { name: 'Подтвердить удаление', exact: true }).click();
    await grid.locator('[data-machine-name="staging-db"]').waitFor({ state: 'detached' });
    assert.deepEqual((await calls('remotesRemove')).at(-1).args, ['staging-db']);
    assert.equal(await grid.locator('[data-machine-name][aria-pressed="true"]').count(), 1);
    assert.equal(await detail.locator('[data-machine-name="staging-db"]').count(), 0);
    records.push('remove requires explicit confirmation and restores selection to a surviving machine');

    await page.evaluate(() => { window.__connectionsQA.remotes = []; window.__connectionsQA.vm.vms = []; });
    await pane.getByRole('button', { name: 'Обновить', exact: true }).click();
    await detail.getByRole('button', { name: 'Подключить машину', exact: true }).waitFor();
    assert.equal(await grid.locator('[data-machine-name], [data-vm-name]').count(), 0);
    await capture('empty-machines-dark');
    await detail.getByRole('button', { name: 'Подключить машину', exact: true }).click();
    assert.equal(await editor.isVisible(), true);
    records.push('empty inventory offers a working Add flow without a stale selected machine');
    assert.deepEqual(errors, []);
    assert.deepEqual(await page.evaluate(() => window.__sessionFixture.unknown), []);
    const sources = Object.fromEntries(['index.html', 'renderer.js', 'workspace.js', 'navigation.js', 'settings2.js', 'connection-design.css', 'settings-layout.css', 'vm-settings.css', 'select-control.js', 'select-control.css'].map(file => [file, createHash('sha256').update(fs.readFileSync(path.join(ui, file))).digest('hex')]));
    fs.writeFileSync(path.join(out, 'checks.json'), JSON.stringify({ ok: true, generatedAt: new Date().toISOString(), runtime: 'Chromium, production UI, synthetic bridge', checks: records, errors, sources }, null, 2) + '\n');
    console.log(JSON.stringify({ ok: true, checks: records.length, records }, null, 2));
  } catch (error) {
    await page.screenshot({ path: path.join(out, 'failure.png') });
    console.error(JSON.stringify({ errors, unknown: await page.evaluate(() => window.__sessionFixture?.unknown) })); throw error;
  } finally { await browser.close(); server.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
