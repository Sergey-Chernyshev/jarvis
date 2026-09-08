import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const code = readFileSync(new URL('./settings2.js', import.meta.url), 'utf8');
const flush = async () => { await new Promise(resolve => setImmediate(resolve)); };
const authenticated = {
  ok: true, available: true, version: '18.0.0', authenticated: true,
  proxy: 'teleport.example.test', cluster: 'development', username: 'developer',
  logins: ['developer', 'root'], validUntil: '2030-01-01T00:00:00Z',
  profiles: [{ proxy: 'teleport.example.test', cluster: 'development', username: 'developer', logins: ['developer', 'root'], authenticated: true, validUntil: '2030-01-01T00:00:00Z' }],
};
const inventory = { ok: true, cluster: 'development', proxy: 'teleport.example.test', logins: ['developer', 'root'], nodes: [{ id: 'node-uuid', target: 'node-uuid', name: 'Hermes', hostname: 'hermes-vm', labels: { environment: 'dev' } }] };

async function fixture(overrides = {}) {
  const { window, document } = parseHTML('<html><head></head><body><div id="root"></div></body></html>');
  const calls = [], events = {}, timers = new Map(); let nextTimer = 0;
  const api = {
    getMeta: async () => ({ version: 'test' }), getSettings: async () => ({}), remotesList: async () => [],
    vmStatus: async () => ({ ok: true, available: false, vms: [] }),
    teleportStatus: async () => structuredClone(authenticated), teleportNodes: async () => structuredClone(inventory), teleportLogin: async proxy => ({ ok: true, started: true, proxy }),
    remotesPreflight: async () => ({ ok: true, os: 'Linux', arch: 'aarch64', tmux: true, claude: true, codex: true, nodeSource: 'download' }),
    remotesInstall: async () => ({ ok: true }), remotesSshKey: async () => ({ ok: true, publicKey: 'ssh-ed25519 fixture', path: '/qa/key.pub' }),
    remotesSshAuthorize: async () => ({ ok: true }), ...overrides,
  };
  window.jarvisIcons = { create: () => document.createElement('svg') }; window.jarvisKeys = { isMac: true };
  window.jarvis = new Proxy(api, { get(target, name) {
    if (name.startsWith('on')) return callback => { events[name] = callback; return () => {}; };
    if (!(name in target)) return undefined;
    return (...args) => { calls.push({ name, args }); return target[name](...args); };
  } });
  const timeout = (fn, ms) => { const id = ++nextTimer; timers.set(id, { fn, ms }); return id; };
  window.setTimeout = timeout; window.clearTimeout = id => timers.delete(id);
  new Function('window', 'document', 'setTimeout', 'clearTimeout', code)(window, document, timeout, id => timers.delete(id));
  const open = async pane => {
    if (pane === 'remotes') window.initMachines(document.getElementById('root'));
    else { window.jarvisOpenSettingsPane(pane); window.initSettings2(document.getElementById('root')); }
    await flush();
  };
  await open('remotes');
  const addMachine = [...document.querySelectorAll('#machines2 button')].find(button => button.textContent === 'Добавить машину');
  assert.ok(addMachine, 'Connection editor has an explicit entry point');
  addMachine.click(); await flush();
  const button = label => { const match = [...document.querySelectorAll('#s2-rwiz button')].find(b => b.textContent === label || b.getAttribute('aria-label') === label); assert.ok(match, `Button ${label}`); return match; };
  const input = (label, value) => { const i = document.querySelector(`[aria-label="${label}"]`); assert.ok(i, `Input ${label}`); i.value = value; i.dispatchEvent(new window.Event('input', { bubbles: true })); return i; };
  const select = async (label, value) => { const i = document.querySelector(`select[aria-label="${label}"]`); assert.ok(i, `Select ${label}`); for (const o of i.options) o.selected = false; const option = [...i.options].find(o => o.value === value); assert.ok(option, `Option ${value}`); option.selected = true; i.dispatchEvent(new window.Event('change', { bubbles: true })); await flush(); };
  const poll = async () => { const timer = [...timers].find(([, t]) => t.ms === 2000); assert.ok(timer, 'Teleport polling timer'); timers.delete(timer[0]); timer[1].fn(); await flush(); };
  return { window, document, calls, events, timers, button, input, select, poll, open, click: async label => { button(label).click(); await flush(); } };
}

test('SSH is the visible default and does not discover Teleport until selected', async () => {
  const f = await fixture();
  assert.ok(f.button('SSH')); assert.ok(f.button('Teleport (tsh)'));
  assert.ok(f.document.querySelector('input[placeholder="ssh-хост · user@адрес"]'));
  assert.equal(f.calls.some(c => c.name.startsWith('teleport')), false);
  assert.match(f.document.getElementById('s2-rwiz').textContent, /Доступ/);
  assert.match(f.document.getElementById('s2-rwiz').textContent, /Машина и агенты/);
});

test('authenticated Teleport selects its cluster, OS login and exact VM before preflight', async () => {
  const f = await fixture(); await f.click('Teleport (tsh)');
  await f.select('Кластер Teleport', 'development');
  await f.select('Пользователь SSH', 'developer');
  await f.select('Машина Teleport', 'node-uuid');
  await f.click('Проверить машину');
  const probe = f.calls.find(c => c.name === 'remotesPreflight'); assert.ok(probe);
  assert.equal(probe.args[0], 'developer@node-uuid');
  assert.equal(probe.args[2].transport, 'teleport');
  assert.equal(probe.args[2].teleportProxy, 'teleport.example.test');
  assert.equal(probe.args[2].teleportCluster, 'development');
  assert.equal(f.calls.some(c => c.name === 'remotesInstall'), false);
});

test('Teleport access errors never offer or invoke the SSH password/key path', async () => {
  const f = await fixture({ remotesPreflight: async () => ({ ok: false, error: 'Teleport certificate expired' }) });
  await f.click('Teleport (tsh)'); await f.select('Пользователь SSH', 'developer'); await f.select('Машина Teleport', 'node-uuid'); await f.click('Проверить машину');
  assert.match(f.document.getElementById('s2-rwiz').textContent, /Teleport certificate expired/);
  assert.equal(f.document.querySelector('input[type="password"]'), null);
  assert.equal(f.calls.some(c => c.name === 'remotesSshKey' || c.name === 'remotesSshAuthorize'), false);
});

test('Teleport proxy matching accepts case and the default HTTPS port', async () => {
  const status = { ...authenticated, proxy: 'teleport.example.test:443', profiles: authenticated.profiles.map(p => ({ ...p, proxy: 'teleport.example.test:443' })) };
  const f = await fixture({ teleportStatus: async () => structuredClone(status) });
  await f.click('Teleport (tsh)'); f.input('Teleport proxy', 'TELEPORT.EXAMPLE.TEST'); await f.click('Обновить доступ');
  assert.ok(f.document.querySelector('select[aria-label="Кластер Teleport"]'));
  assert.match(f.document.getElementById('s2-rwiz').textContent, /вход активен/);
});

test('missing tsh presents an install action without any SSH authorization request', async () => {
  const f = await fixture({ teleportStatus: async () => ({ ok: true, available: false, authenticated: false, profiles: [] }) });
  await f.click('Teleport (tsh)'); assert.ok(f.button('Установить tsh'));
  assert.equal(f.document.querySelector('input[type="password"]'), null);
  assert.equal(f.calls.some(c => c.name === 'teleportLogin' || c.name === 'remotesSshKey'), false);
});

test('normal Teleport login polls only status and resumes after returning to its pane', async () => {
  let loggedIn = false;
  const f = await fixture({ teleportStatus: async () => loggedIn ? structuredClone(authenticated) : { ...authenticated, authenticated: false, profiles: [] } });
  await f.click('Teleport (tsh)'); await f.click('Войти через Teleport');
  assert.deepEqual(f.calls.find(c => c.name === 'teleportLogin').args, ['teleport.example.test', false]);
  await f.open('about');
  assert.equal([...f.timers.values()].some(timer => timer.ms === 2000), false, 'Leaving Machines stops Teleport polling');
  loggedIn = true; await f.open('remotes');
  assert.match(f.document.getElementById('s2-rwiz').textContent, /вход активен/);
  assert.equal(f.calls.some(c => c.name === 'remotesInstall' || c.name === 'remotesPreflight'), false);
});

test('explicit renewal cannot succeed merely because the old local certificate is still valid', async () => {
  let status = structuredClone(authenticated);
  const f = await fixture({ teleportStatus: async () => structuredClone(status), remotesPreflight: async () => ({ ok: false, error: 'certificate revoked' }) });
  await f.click('Teleport (tsh)'); await f.select('Пользователь SSH', 'developer'); await f.select('Машина Teleport', 'node-uuid'); await f.click('Проверить машину');
  await f.click('Выйти и войти заново');
  assert.deepEqual(f.calls.find(c => c.name === 'teleportLogin').args, ['teleport.example.test', true]);
  await f.poll(); assert.match(f.document.getElementById('s2-rwiz').textContent, /Ждём новый вход/);
  status.validUntil = '2031-01-01T00:00:00Z'; status.profiles[0].validUntil = status.validUntil;
  await f.poll(); assert.doesNotMatch(f.document.getElementById('s2-rwiz').textContent, /Ждём новый вход/);
});

test('a changed profile login clears the old target and its actionable preflight', async () => {
  let status = structuredClone(authenticated);
  const f = await fixture({ teleportStatus: async () => structuredClone(status) });
  await f.click('Teleport (tsh)'); await f.select('Пользователь SSH', 'root'); await f.select('Машина Teleport', 'node-uuid'); await f.click('Проверить машину');
  assert.ok(f.button('Настроить автоматически'));
  status.logins = ['developer']; status.profiles[0].logins = ['developer']; await f.click('Обновить доступ');
  assert.equal(f.document.querySelector('select[aria-label="Пользователь SSH"]').value, 'developer');
  assert.equal([...f.document.querySelectorAll('button')].some(b => b.textContent === 'Настроить автоматически'), false);
  assert.equal(f.button('Проверить машину').disabled, true);
});

test('late status from a previous proxy cannot replace the current authenticated profile', async () => {
  let release, reads = 0;
  const next = { ...authenticated, proxy: 'new.example.test', profiles: authenticated.profiles.map(p => ({ ...p, proxy: 'new.example.test' })) };
  const f = await fixture({ teleportStatus: () => ++reads === 1 ? new Promise(resolve => { release = resolve; }) : Promise.resolve(structuredClone(next)) });
  await f.click('Teleport (tsh)'); f.input('Teleport proxy', 'new.example.test'); await f.click('Обновить доступ');
  release(structuredClone(authenticated)); await flush();
  assert.equal(f.document.querySelector('input[aria-label="Teleport proxy"]').value, 'new.example.test');
  assert.ok(f.document.querySelector('select[aria-label="Кластер Teleport"]'));
});

test('late nodes from a previous cluster cannot become the current machine choices', async () => {
  let release;
  const status = structuredClone(authenticated); status.profiles.push({ ...status.profiles[0], cluster: 'production' });
  const f = await fixture({ teleportStatus: async () => status, teleportNodes: (_, cluster) => cluster === 'development' ? new Promise(resolve => { release = resolve; }) : Promise.resolve({ ...inventory, cluster, nodes: [{ id: 'prod', target: 'prod', name: 'Production VM' }] }) });
  await f.click('Teleport (tsh)'); await f.select('Пользователь SSH', 'developer'); await f.select('Кластер Teleport', 'production'); await f.select('Пользователь SSH', 'developer');
  release(structuredClone(inventory)); await flush();
  const choices = f.document.querySelector('select[aria-label="Машина Teleport"]');
  assert.match(choices.textContent, /Production VM/); assert.doesNotMatch(choices.textContent, /Hermes/);
});

test('late SSH preflight cannot expose an installer after switching transport', async () => {
  let release;
  const f = await fixture({ remotesPreflight: () => new Promise(resolve => { release = resolve; }) });
  f.input('SSH-хост', 'developer@old-machine'); await f.click('Проверить машину'); await f.click('Teleport (tsh)');
  release({ ok: true, nodeSource: 'download' }); await flush();
  assert.equal([...f.document.querySelectorAll('button')].some(b => b.textContent === 'Настроить автоматически'), false);
});

test('cancelling pending preflight preserves the draft and reopens an enabled editor', async () => {
  let release;
  const f = await fixture({ remotesPreflight: () => new Promise(resolve => { release = resolve; }) });
  f.input('SSH-хост', 'developer@cancelled-machine'); await f.click('Проверить машину');
  const editor = f.document.getElementById('s2-ssh-setup');
  [...editor.querySelectorAll('button')].find(button => button.textContent === 'Отмена').click();
  assert.equal(editor.hidden, true);
  release({ ok: true, os: 'Stale machine', nodeSource: 'download' }); await flush();
  [...f.document.querySelectorAll('#machines2 button')].find(button => button.textContent === 'Добавить машину').click();
  assert.equal(editor.hidden, false);
  assert.equal(f.document.querySelector('[aria-label="SSH-хост"]').value, 'developer@cancelled-machine');
  assert.equal(f.button('Проверить машину').disabled, false);
  assert.equal([...editor.querySelectorAll('button')].some(button => button.textContent === 'Настроить автоматически'), false);
});

test('manual existing-node drafts survive asynchronous node discovery and failed saves', async () => {
  let release;
  const f = await fixture({ teleportNodes: () => new Promise(resolve => { release = resolve; }), remotesAdd: async () => ({ ok: false, error: 'store unavailable' }) });
  await f.click('Teleport (tsh)'); await f.select('Пользователь SSH', 'developer'); await f.click('Jarvis уже установлен — добавить вручную');
  f.input('Имя установленного узла', 'Hermes'); f.input('Адрес установленного узла', 'developer@node-uuid'); f.input('Каталог установленного узла', '/opt/jarvis');
  release(structuredClone(inventory)); await flush(); await f.click('Добавить');
  assert.equal(f.document.querySelector('[aria-label="Имя установленного узла"]').value, 'Hermes');
  assert.equal(f.document.querySelector('[aria-label="Адрес установленного узла"]').value, 'developer@node-uuid');
  const saved = f.calls.find(c => c.name === 'remotesAdd').args[0]; assert.equal(saved.transport, 'teleport'); assert.equal(saved.teleportCluster, 'development'); assert.equal(saved.jarvisDir, '/opt/jarvis');
  assert.match(f.document.getElementById('s2-rwiz').textContent, /store unavailable/);
});

test('automatic preparation is disclosed and installer completion advances the final stage', async () => {
  const f = await fixture({ remotesPreflight: async () => ({ ok: true, nodeSource: 'download', claude: true, runtimeSetup: { missing: ['tmux', 'curl'], automatic: true, command: 'sudo apt install tmux curl' } }) });
  let opened;
  f.window.jarvisSessionWorkspace = { newChat: async target => { opened = target; } };
  f.input('SSH-хост', 'developer@hermes'); await f.click('Проверить машину');
  assert.match(f.document.getElementById('s2-rwiz').textContent, /Установим tmux \+ curl/);
  assert.equal(f.document.querySelector('.s2rconnection-summary').hasAttribute('open'), false);
  await f.click('Настроить автоматически');
  f.events.onRemoteInstallDone({ ok: true, name: 'another-machine' }); await flush();
  assert.equal(f.document.querySelector('.s2rsteps [aria-current="step"]').textContent, '3Автонастройка');
  f.events.onRemoteInstallDone({ ok: true, name: 'hermes' }); await flush();
  assert.equal(f.document.querySelector('.s2rsteps [aria-current="step"]').textContent, '4Подключено');
  assert.ok(f.button('Подключить ещё'));
  await f.click('Открыть чаты'); assert.deepEqual(opened, { machine: 'hermes', cwd: '' });
});

test('reinstall preserves the full saved Teleport connection including its custom TCP port', async () => {
  const saved = { name: 'hermes', connected: true, sshHost: 'developer@node-uuid', jarvisDir: '/opt/jarvis', transport: 'teleport', teleportProxy: 'teleport.example.test', teleportCluster: 'development', runAsUser: 'agent', nodeTcpPort: 7777 };
  const f = await fixture({ remotesList: async () => [saved] });
  [...f.document.querySelectorAll('button')].find(b => b.textContent === 'Переустановить').click(); await flush();
  const installed = f.calls.find(c => c.name === 'remotesInstall').args[0];
  for (const key of ['name', 'sshHost', 'jarvisDir', 'transport', 'teleportProxy', 'teleportCluster', 'runAsUser', 'nodeTcpPort']) assert.equal(installed[key], saved[key]);
});

test('raw terminal progress and download errors stay readable and retry preserves the selected connection', async () => {
  const saved = { name: 'hermes', connected: true, sshHost: 'developer@node-uuid', jarvisDir: '/opt/jarvis', transport: 'teleport', teleportProxy: 'teleport.example.test', teleportCluster: 'development', runAsUser: 'agent', nodeTcpPort: 7777 };
  const f = await fixture({ remotesList: async () => [saved] });
  [...f.document.querySelectorAll('button')].find(b => b.textContent === 'Переустановить').click(); await flush();
  const first = f.calls.find(c => c.name === 'remotesInstall').args[0];
  f.events.onRemoteInstallStep({ phase: '\x1b[32mУзел\x1b[0m', state: 'info', msg: '\x1b]0;hidden title\x07\x1b]8;;https://hidden.test\x1b\\скачиваем\x1b]8;;\x1b\\\r\nпроверяем\x07' });
  assert.equal(f.document.querySelector('#s2-rlog .install-phase').textContent, 'Узел');
  assert.equal(f.document.querySelector('#s2-rlog .msg').textContent, 'скачиваем\nпроверяем');
  f.events.onRemoteInstallDone({ ok: false, name: 'hermes', error: 'curl: (22) The requested URL returned error: 404\r\n\x1b[31mERROR: \x1b[0mProcess exited with status 22\x00' });
  await flush();
  assert.equal(f.document.querySelector('#s2-rwiz .s2rpre.bad').textContent, 'curl: (22) The requested URL returned error: 404\nERROR: Process exited with status 22');
  await f.click('Повторить');
  const installs = f.calls.filter(c => c.name === 'remotesInstall');
  assert.equal(installs.length, 2); assert.deepEqual(installs[1].args[0], first);
  assert.equal(f.document.querySelector('#s2-rwiz .s2rpre.bad'), null);
  assert.equal(f.document.querySelector('#s2-rlog').textContent, '');
  assert.equal(f.calls.some(c => ['teleportLogin', 'remotesSshAuthorize'].includes(c.name)), false);
});

test('terminal output is bounded and unterminated control strings are not displayed', async () => {
  const f = await fixture(); f.input('SSH-хост', 'developer@hermes'); await f.click('Проверить машину'); await f.click('Настроить автоматически');
  f.events.onRemoteInstallStep({ phase: 'Узел', state: 'info', msg: 'готово\x90hidden payload\x9c\x9b31m ok\x9b0m\x1b]unterminated title' });
  assert.equal(f.document.querySelector('#s2-rlog .msg').textContent, 'готово ok');
  f.events.onRemoteInstallStep({ phase: 'Узел', state: 'warn', msg: 'Ошибка 🙂 '.repeat(10000) });
  const msg = [...f.document.querySelectorAll('#s2-rlog .msg')].at(-1).textContent;
  assert.ok(msg.length <= 4096); assert.match(msg, /… \(вывод сокращён\)$/);
  f.events.onRemoteInstallDone({ ok: false, name: 'hermes', error: 'Ошибка 🙂 '.repeat(10000) }); await flush();
  const error = f.document.querySelector('#s2-rwiz .s2rpre.bad').textContent;
  assert.ok(error.length <= 4096); assert.match(error, /… \(вывод сокращён\)$/);
  assert.doesNotMatch(error, /[\ud800-\udbff](?![\udc00-\udfff])/u);
});

test('preflight and immediate installer rejection also strip raw ANSI', async () => {
  let refuseProbe = true;
  const f = await fixture({ remotesPreflight: async () => refuseProbe ? { ok: false, error: '\x1b[31mошибка проверки\x1b[0m' } : { ok: true, nodeSource: 'download' }, remotesInstall: async () => ({ ok: false, error: '\x1b[31mуже идёт установка\x1b[0m' }) });
  f.input('SSH-хост', 'developer@hermes'); await f.click('Проверить машину');
  assert.equal(f.document.querySelector('#s2-rwiz .s2rpre.bad').textContent, 'ошибка проверки');
  refuseProbe = false; await f.click('Проверить машину'); await f.click('Настроить автоматически');
  assert.equal(f.document.querySelector('#s2-rwiz .s2rpre.bad').textContent, 'уже идёт установка');
  assert.ok(f.button('Повторить'));
});

test('SSH password authorization carries the selected SSH config and rechecks the same machine', async () => {
  let probes = 0;
  const f = await fixture({ remotesPreflight: async () => ++probes === 1 ? { ok: false, error: 'Permission denied' } : { ok: true, nodeSource: 'download' } });
  f.input('SSH-хост', 'developer@ssh-machine'); f.input('SSH config', '/qa/ssh/config'); await f.click('Проверить машину');
  const password = f.document.querySelector('input[type="password"]'); assert.ok(password); password.value = 'fixture-only-secret';
  await f.click('Войти по паролю');
  const authorization = f.calls.find(c => c.name === 'remotesSshAuthorize');
  assert.equal(authorization.args[0], 'developer@ssh-machine');
  assert.equal(authorization.args[2].transport, 'ssh'); assert.equal(authorization.args[2].sshConfigFile, '/qa/ssh/config');
  assert.equal(probes, 2); assert.ok(f.button('Настроить автоматически'));
});

test('a disconnected Teleport row renews access without reinstalling its known node', async () => {
  const remote = { name: 'hermes', connected: false, transport: 'teleport', sshHost: 'developer@node-uuid', teleportProxy: 'teleport.example.test', teleportCluster: 'development', nodeTcpPort: 7777 };
  const f = await fixture({ remotesList: async () => [remote], remotesTest: async () => ({ ok: true }) });
  [...f.document.querySelectorAll('button')].find(b => b.textContent === 'Обновить вход').click(); await flush();
  assert.ok(f.button('Выйти и войти заново')); await f.click('Проверить подключение');
  assert.equal(f.calls.some(c => c.name === 'remotesInstall'), false);
  assert.deepEqual(f.calls.find(c => c.name === 'remotesTest').args, ['hermes']);
  assert.match(f.document.getElementById('s2-rwiz').textContent, /hermes.*на связи/);
});

test('inventory actions resume an active installation without replacing its target or completion', async t => {
  for (const action of ['Переустановить', 'Обновить вход', 'Удалить подключение', 'Подключить к Jarvis']) {
    await t.test(action, async () => {
      const saved = { name: 'beta', sshHost: 'developer@beta', connected: false, transport: 'teleport', teleportProxy: 'teleport.example.test', teleportCluster: 'development' };
      const vm = { name: 'local-beta', status: 'running', projects: [], capabilities: {}, connection: { canConnect: true, name: 'local-beta', sshHost: 'developer@local-beta' } };
      const f = await fixture({
        remotesList: async () => [saved],
        vmStatus: async () => ({ ok: true, available: true, vms: [vm] }),
        remotesRemove: async () => ({ ok: true }),
      });
      f.input('SSH-хост', 'developer@gamma'); f.input('Имя подключения', 'gamma');
      await f.click('Проверить машину'); await f.click('Настроить автоматически');
      assert.equal(f.document.querySelector('.connection-editor-head h2').textContent, 'Подготовка машины');
      const collapse = f.document.querySelector('[data-connection-cancel]');
      assert.equal(collapse.textContent, 'Свернуть');
      collapse.click(); await flush();
      assert.equal(f.document.getElementById('s2-ssh-setup').hidden, true);

      const card = action === 'Подключить к Jarvis'
        ? f.document.querySelector('.connection-card[data-vm-name="local-beta"]')
        : f.document.querySelector('.connection-card[data-machine-name="beta"]');
      assert.ok(card); card.click(); await flush();
      if (action === 'Переустановить' || action === 'Удалить подключение') {
        [...f.document.querySelectorAll('.connection-detail-tabs button')].find(b => b.textContent === 'Настройки').click();
      }
      const control = [...f.document.querySelectorAll('.connection-detail button')].find(b => b.textContent === action);
      assert.ok(control, action); control.click(); await flush();

      assert.equal(f.document.getElementById('s2-ssh-setup').hidden, false, 'Return to the original installation');
      assert.deepEqual(f.calls.filter(c => c.name === 'remotesInstall').map(c => c.args[0].name), ['gamma']);
      assert.equal(f.calls.filter(c => c.name === 'remotesPreflight').length, 1);
      assert.equal(f.calls.some(c => c.name === 'remotesRemove' || c.name.startsWith('teleport')), false);
      f.events.onRemoteInstallStep({ phase: 'Подготовка', state: 'info', msg: 'Продолжаем gamma' });
      assert.match(f.document.querySelector('#s2-rlog').textContent, /Продолжаем gamma/);
      f.events.onRemoteInstallDone({ ok: true, name: 'gamma' }); await flush();
      assert.match(f.document.querySelector('#s2-rwiz').textContent, /Машина «gamma» подключена/);
      assert.equal(f.document.querySelector('.connection-editor-head h2').textContent, 'Подключение готово');
      assert.equal(f.document.querySelector('[data-connection-cancel]').textContent, 'Отмена');
    });
  }
});

test('completed connection is selected once and refresh preserves a later explicit selection', async () => {
  const remotes = [{ name: 'alpha', sshHost: 'developer@alpha', connected: true }];
  const f = await fixture({ remotesList: async () => remotes });
  let opened;
  f.window.jarvisSessionWorkspace = { newChat: target => { opened = target; } };
  f.input('SSH-хост', 'developer@gamma'); f.input('Имя подключения', 'gamma');
  await f.click('Проверить машину'); await f.click('Настроить автоматически');
  remotes.push({ name: 'gamma', sshHost: 'developer@gamma', connected: true });
  f.events.onRemoteInstallDone({ ok: true, name: 'gamma' }); await flush();
  assert.equal(f.document.querySelector('.connection-card[aria-pressed="true"]').dataset.machineName, 'gamma');
  await f.click('Открыть чаты'); assert.deepEqual(opened, { machine: 'gamma', cwd: '' });
  f.document.querySelector('[data-connection-cancel]').click(); await flush();
  f.document.querySelector('.connection-card[data-machine-name="alpha"]').click(); await flush();
  f.document.querySelector('.connection-toolbar button').click(); await flush();
  assert.equal(f.document.querySelector('.connection-card[aria-pressed="true"]').dataset.machineName, 'alpha');
  assert.equal(f.document.querySelector('.connection-host').dataset.machineName, 'alpha');
});

test('a pending manual save keeps its draft and completion when cancellation or a stale inventory action arrives', async () => {
  let release;
  const f = await fixture({
    remotesList: async () => [{ name: 'beta', sshHost: 'developer@beta', connected: true }],
    remotesAdd: () => new Promise(resolve => { release = resolve; }),
  });
  // Keep an already-rendered handler to exercise the action's state guard,
  // independently of the editor hiding its inventory while the save is pending.
  const reinstall = [...f.document.querySelectorAll('.connection-detail button')].find(b => b.textContent === 'Переустановить');
  await f.click('Jarvis уже установлен — добавить вручную');
  f.input('Имя установленного узла', 'gamma'); f.input('Адрес установленного узла', 'developer@gamma');
  f.input('Каталог установленного узла', '/srv/gamma'); await f.click('Добавить');
  assert.equal(f.document.querySelector('[data-connection-cancel]').disabled, true);
  f.document.querySelector('[data-connection-cancel]').click(); await flush();
  assert.equal(f.document.getElementById('s2-ssh-setup').hidden, false);
  assert.ok(reinstall); reinstall.click(); await flush();
  assert.equal(f.calls.some(c => c.name === 'remotesInstall'), false);
  assert.equal(f.document.querySelector('[aria-label="Имя установленного узла"]').value, 'gamma');
  assert.equal(f.document.querySelector('[aria-label="Каталог установленного узла"]').value, '/srv/gamma');
  assert.equal(f.calls.filter(c => c.name === 'remotesAdd').length, 1);
  release({ ok: true }); await flush();
  assert.match(f.document.querySelector('#s2-rwiz').textContent, /Машина «gamma» добавлена/);
  assert.equal(f.document.querySelector('[data-connection-cancel]').disabled, false);
});
