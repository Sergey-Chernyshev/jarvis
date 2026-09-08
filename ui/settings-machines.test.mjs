import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const code = readFileSync(new URL('./settings2.js', import.meta.url), 'utf8');
const flush = async () => { await new Promise(resolve => setImmediate(resolve)); };
const remote = { name: 'build-box', sshHost: 'developer@build-box', jarvisDir: '~/.jarvis', connected: true, version: '0.3.3' };
const modern = {
  ok: true, available: true, version: '0.10.0', generation: 'modern', capabilities: { start: true, stop: true },
  vms: [
    { name: 'dev-linux', status: 'stopped', registryStatus: 'managed', directory: '/work/environments/dev-linux', configPath: '/work/environments/dev-linux/agent-vm.yaml',
      projects: [{ name: 'frontend', path: '/work/web', guestPath: '/projects/web' }, { name: 'backend', path: '/work/api', guestPath: '/projects/api' }], capabilities: { start: true, stop: true, openConfig: true } },
    { name: 'review-linux', status: 'running', registryStatus: 'managed', projects: [], capabilities: { start: true, stop: true, openConfig: false } },
    { name: 'unrecognized-linux', status: 'unknown', registryStatus: 'unmanaged', projects: [], capabilities: { start: false, stop: false, openConfig: false } },
  ],
};

async function fixture({ vm = modern, failAction = false, failSave = false, initialPane = 'remotes' } = {}) {
  const { window, document } = parseHTML('<html><head></head><body><div id="root"></div></body></html>');
  const calls = [], events = {}, info = structuredClone(vm);
  let settings = { launchTerminal: 'terminal-app', launchDangerous: true, launchDockerImage: 'example/agent:stable', launchProxyCmd: '' };
  window.jarvisIcons = { create: () => document.createElement('svg') };
  window.jarvisKeys = { isMac: true };
  const api = {
    getMeta: async () => ({ version: 'test' }), getSettings: async () => settings,
    remotesList: async () => [remote], remotesPreflight: async () => ({ ok: true }), remotesInstall: async () => ({ ok: true }),
    vmStatus: async () => structuredClone(info),
    vmAction: async (name, action) => {
      if (failAction) return { ok: false, error: 'VM runtime unavailable' };
      if (action === 'start') info.vms.find(vm => vm.name === name).status = 'running';
      return { ok: true };
    },
    setSettings: async patch => {
      if (failSave) return { ok: false, error: 'Settings store unavailable' };
      settings = { ...settings, ...patch }; return { ok: true };
    },
  };
  window.jarvis = new Proxy(api, { get(target, name) {
    if (name.startsWith('on')) return callback => { events[name] = callback; return () => {}; };
    if (!(name in target)) return undefined;
    return (...args) => { calls.push({ name, args }); return target[name](...args); };
  } });
  new Function('window', 'document', code)(window, document);
  const open = async pane => {
    if (pane === 'remotes') window.initMachines(document.getElementById('root'));
    else { window.jarvisOpenSettingsPane(pane); window.initSettings2(document.getElementById('root')); }
    await flush();
  };
  await open(initialPane);
  return { window, document, calls, events, settings: () => settings, open, selectVm: async name => {
    const card = document.querySelector(`.connection-grid [data-vm-name="${name}"]`);
    assert.ok(card, `VM ${name} exists in inventory`); card.click(); await flush();
    return document.querySelector(`.connection-detail [data-vm-name="${name}"]`);
  } };
}

test('machines settings keeps SSH usable when agent-vm is absent', async () => {
  const f = await fixture({ vm: { ok: true, available: false, generation: 'unknown', vms: [] } });
  const pane = f.document.getElementById('s2-pane-remotes');
  assert.equal(pane.querySelector('.dtitle').textContent, 'Машины');
  assert.ok(f.document.querySelector('#machines2.settings-surface'));
  assert.equal(f.document.querySelector('#machines2 .sidebar, #machines2 .snav'), null);
  assert.ok(pane.querySelector('.connection-workspace'));
  assert.match(pane.textContent, /build-box/);
  assert.match(pane.textContent, /agent-vm не найден/);
  assert.equal(f.document.getElementById('s2-ssh-setup').open, false);
  for (const placeholder of ['ssh-хост · user@адрес', 'имя · vps', '~/.jarvis']) {
    assert.ok(f.document.getElementById('s2-rwiz').querySelector(`input[placeholder="${placeholder}"]`), 'existing SSH setup control remains present');
  }
  assert.ok(f.document.querySelector('[aria-label="SSH config"]'));
  assert.ok(f.document.querySelector('[aria-label="Пользователь агента на узле"]'));
  assert.equal(f.document.querySelectorAll('[data-vm-name]').length, 0);
  assert.equal(f.calls.some(call => call.name === 'vmAction'), false);
});

test('VM selection shows capability-based controls and multiple mounts in its inspector', async () => {
  const f = await fixture();
  let row = await f.selectVm('dev-linux');
  const environment = f.document.getElementById('s2-vm-environment');
  assert.equal(environment.open, false, 'Runtime explanations start collapsed');
  assert.match(environment.textContent, /0\.10\.0/);
  assert.match(environment.textContent, /несколько подключённых проектов/);
  assert.equal(row.querySelector('.s2-vm-status').dataset.state, 'stopped');
  assert.match(row.textContent, /Проектов: 2/);
  assert.equal(row.querySelector('details').open, false);
  assert.match(row.textContent, /frontend/); assert.match(row.textContent, /backend/);
  assert.match(row.textContent, /\/projects\/api/);
  assert.equal(f.document.querySelectorAll('.connection-grid button button').length, 0, 'inventory has no nested maintenance controls');
  const start = [...row.querySelectorAll('button')].find(button => button.textContent === 'Запустить');
  start.click(); await flush();
  assert.deepEqual(f.calls.find(call => call.name === 'vmAction').args, ['dev-linux', 'start']);
  assert.equal(f.document.querySelectorAll('.connection-grid [data-vm-name="dev-linux"]').length, 1);
  row = f.document.querySelector('.connection-detail [data-vm-name="dev-linux"]');
  assert.match(row.textContent, /Работает/);
  assert.equal(row.querySelector('.s2-vm-status').dataset.state, 'running');
  assert.ok([...row.querySelectorAll('button')].some(button => button.textContent === 'Остановить'));
  [...row.querySelectorAll('button')].find(button => button.textContent === 'Открыть конфигурацию').click(); await flush();
  assert.deepEqual(f.calls.filter(call => call.name === 'vmAction').at(-1).args, ['dev-linux', 'open-config']);
  const unknown = await f.selectVm('unrecognized-linux');
  assert.deepEqual([...unknown.querySelectorAll('button')].map(button => button.textContent), []);
});

test('failed VM action keeps its real stopped state and offers retry', async () => {
  const f = await fixture({ failAction: true });
  const row = await f.selectVm('dev-linux');
  const start = [...row.querySelectorAll('button')].find(button => button.textContent === 'Запустить');
  start.click(); await flush();
  assert.match(f.document.getElementById('settings-save-error').textContent, /VM runtime unavailable/);
  assert.match(row.textContent, /Остановлена/);
  assert.equal(start.disabled, false);
  assert.equal(start.textContent, 'Запустить');
});

test('partial VM inventory shows its warning without hiding usable rows or SSH', async () => {
  const f = await fixture({ vm: { ...modern, partial: true, error: 'VM runtime not responding' } });
  const pane = f.document.getElementById('s2-pane-remotes');
  assert.match(pane.textContent, /VM runtime not responding/);
  assert.match(pane.textContent, /build-box/);
  assert.equal(pane.querySelectorAll('.connection-grid [data-vm-name]').length, 3);
});

test('Docker image has one saved control and task permissions stay in advanced local settings', async () => {
  const f = await fixture();
  const image = f.document.querySelector('input[aria-label="Образ Docker"]');
  image.value = 'example/agent:next';
  [...f.document.querySelectorAll('button')].find(button => button.textContent === 'Сохранить образ').click(); await flush();
  assert.equal(f.settings().launchDockerImage, 'example/agent:next');
  assert.deepEqual(f.calls.find(call => call.name === 'setSettings').args, [{ launchDockerImage: 'example/agent:next' }]);
  await f.open('launch');
  assert.equal(f.document.querySelectorAll('input[aria-label="Образ Docker"]').length, 0, 'Switching modules unmounts the Machines editor');
  assert.equal(f.document.getElementById('s2-launch-advanced').open, false);
  assert.match(f.document.getElementById('s2-launch-advanced').textContent, /Выбор разрешений в новом чате или проекте всегда важнее/);
});

test('failed Docker image save preserves the editable value', async () => {
  const f = await fixture({ failSave: true });
  const image = f.document.querySelector('input[aria-label="Образ Docker"]');
  image.value = 'example/agent:unsaved';
  [...f.document.querySelectorAll('button')].find(button => button.textContent === 'Сохранить образ').click(); await flush();
  assert.equal(image.value, 'example/agent:unsaved');
  assert.equal(f.settings().launchDockerImage, 'example/agent:stable');
  assert.match(f.document.getElementById('settings-save-error').textContent, /Settings store unavailable/);
});

test('search opens advanced containers around a matching setting', async () => {
  const f = await fixture({ initialPane: 'general' });
  const search = f.document.getElementById('settingsSearch');
  search.value = 'команда прокси'; search.dispatchEvent(new f.window.Event('input'));
  const result = [...f.document.querySelectorAll('.settings-result')].find(button => button.querySelector('strong').textContent === 'Команда прокси');
  assert.ok(result); result.click(); await flush();
  assert.equal(f.document.getElementById('s2-launch-advanced').open, true);
});

test('a profile settings deep link survives opening before the settings UI initializes', async () => {
  const f = await fixture({ initialPane: 'agents' });
  assert.equal(f.document.querySelector('.snav .item.sel').dataset.pane, 'agents');
  assert.ok(f.document.getElementById('s2-pane-agents').classList.contains('on'));
});
