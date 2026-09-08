import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const code = readFileSync(new URL('./projects.js', import.meta.url), 'utf8');
const flush = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};
const project = (machine = 'local', patch = {}) => ({
  machine, cwd: '/work/app', name: 'Приложение', saved: true, pinned: false,
  lastAt: 100, agents: ['claude'], sessions: [], ...patch,
});
const conversation = (patch = {}) => ({
  id: 'history-1', agent: 'claude', title: 'Исправить оплату', lastAt: 100, ...patch,
});
const AVATAR_PNG = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO6RnvYAAAAASUVORK5CYII=';
const OTHER_AVATAR = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLbtAAAAABJRU5ErkJggg==';
const avatarCandidate = (name = 'icon.png') => ({ path: '/work/app/' + name, name, dataUrl: AVATAR_PNG });

async function fixture({ projects = [project()], sessions = [], overrides = {}, avatars } = {}) {
  const { window, document } = parseHTML('<html><body><div id="history"></div></body></html>');
  // Linkedom exposes a read-only select.value, unlike the real browser control.
  Object.defineProperty(window.HTMLSelectElement.prototype, 'value', {
    configurable: true,
    get() { return [...this.options].find(o => o.hasAttribute('selected'))?.value ?? this.options[0]?.value ?? ''; },
    set(value) { for (const option of this.options) option.toggleAttribute('selected', option.value === value); },
  });
  window.HTMLElement.prototype.scrollIntoView = () => {};
  window.jarvisIcons = { create: () => document.createElement('svg') };
  const calls = [], avatarCalls = [], events = {}, timers = new Map();
  if (avatars) {
    const avatarMethods = {
      create(p, { className = '', onChoose } = {}) {
        const element = document.createElement(onChoose ? 'button' : 'span'); element.className = className;
        if (p.avatar?.dataUrl) { const img = document.createElement('img'); img.src = p.avatar.dataUrl; element.append(img); }
        if (onChoose) element.addEventListener('click', onChoose);
        return element;
      },
      discover: async () => ({ ok: true, candidates: [] }),
      normalize: async () => OTHER_AVATAR,
      fromFile: async () => OTHER_AVATAR,
      ...avatars,
    };
    window.JarvisProjectAvatars = Object.fromEntries(Object.entries(avatarMethods).map(([name, fn]) => [name, (...args) => {
      avatarCalls.push({ name, args }); return fn(...args);
    }]));
  } else {
    delete window.JarvisProjectAvatars;
  }
  let records = structuredClone(projects), live = structuredClone(sessions), query = '', visible = true, timerId = 0;
  const methods = {
    projectsList: async () => ({ ok: true, projects: structuredClone(records), warnings: [] }),
    machinesList: async () => [
      { id: 'local', name: 'Local', online: true },
      { id: 'build-box', name: 'Build', online: true },
      { id: 'offline-box', name: 'Offline', online: false },
    ],
    projectsSave: async input => {
      const old = records.find(p => p.machine === input.machine && p.cwd === input.cwd);
      const saved = { ...(old || {}), ...input, saved: true };
      records = [...records.filter(p => p !== old), saved];
      return { ok: true, project: saved };
    },
    projectsRemove: async (machine, cwd) => {
      records = records.flatMap(p => p.machine !== machine || p.cwd !== cwd ? [p]
        : p.sessions.length ? [{ ...p, saved: false, pinned: false }] : []);
      return { ok: true };
    },
    launchSession: async () => ({ ok: true, launchId: 'our-launch' }),
    copyText: async () => ({ ok: true }),
    bundlePlaces: async () => ({ ok: true, home: '/home/developer' }),
    bundleBrowse: async (_machine, path) => ({ ok: true, path, parent: '/', dirs: [] }),
    ...overrides,
  };
  window.jarvis = Object.fromEntries(Object.entries(methods).map(([name, fn]) => [name, (...args) => {
    calls.push({ name, args }); return fn(...args);
  }]));
  window.jarvis.onLaunchTask = callback => { events.launch = callback; };
  const api = {
    visible: () => visible, sessions: () => live, query: () => query, setQuery: value => { query = value; },
    settings: () => calls.push({ name: 'settings', args: [] }),
    newChat: async p => calls.push({ name: 'newChat', args: [p] }),
    openSession: async s => calls.push({ name: 'openSession', args: [s] }),
    back: () => { calls.push({ name: 'back', args: [] }); return window.jarvisProjects.back(); },
  };
  new Function('window', 'document', 'Event', 'setTimeout', 'clearTimeout', code)(
    window, document, window.Event,
    fn => { const id = ++timerId; timers.set(id, fn); return id; }, id => timers.delete(id),
  );
  window.initProjects(api);
  await window.jarvisProjects.show();
  const button = (label, within = document) => {
    const found = [...within.querySelectorAll('button')].find(b => b.getAttribute('aria-label') === label);
    assert.ok(found, `Missing button: ${label}`); return found;
  };
  const open = (machine = 'local', cwd = '/work/app') => {
    const key = window.JarvisProjectState.keyOf({ machine, cwd });
    const row = [...document.querySelectorAll('.pr-project-open')].find(row => row.dataset.projectKey === key);
    assert.ok(row, `Missing project: ${key}`); row.click();
  };
  return {
    window, document, calls, avatarCalls, events, timers, button, open,
    setSessions(value) { live = structuredClone(value); window.jarvisProjects.stateChanged(); },
    setVisible(value) { visible = value; },
    search(value) { query = value; window.jarvisProjects.search(); },
    query: () => query,
    rows: () => [...document.querySelectorAll('.pr-project-open')],
    input(id, value) { const input = document.getElementById(id); assert.ok(input); input.value = value; input.dispatchEvent(new window.Event('input')); },
    upload(file = { name: 'my-avatar.png', size: 1024, type: 'image/png' }) {
      const input = document.querySelector('input[type="file"][aria-label="Картинка проекта"]'); assert.ok(input);
      Object.defineProperty(input, 'files', { configurable: true, value: [file] });
      input.dispatchEvent(new window.Event('change'));
    },
    submit() { document.querySelector('form').dispatchEvent(new window.Event('submit', { cancelable: true })); },
  };
}

test('project keys distinguish identical directories on different machines and delimiter-like paths', async () => {
  const f = await fixture({ projects: [project(), project('build-box')] });
  const { keyOf } = f.window.JarvisProjectState;
  assert.equal(keyOf({ cwd: '/work/app' }), keyOf({ machine: 'local', cwd: '/work/app' }));
  assert.equal(keyOf({ remote: 'build-box', cwd: '/work/app' }), keyOf({ machine: 'build-box', cwd: '/work/app' }));
  assert.notEqual(keyOf(project()), keyOf(project('build-box')));
  assert.notEqual(keyOf({ machine: 'one:two', cwd: 'three' }), keyOf({ machine: 'one', cwd: 'two:three' }));
  assert.equal(new Set(f.rows().map(row => row.dataset.projectKey)).size, 2);
  f.setSessions([{ id: 'remote-live', remote: 'build-box', cwd: '/work/app', status: 'working' }]);
  assert.doesNotMatch(f.rows()[0].textContent, /1 в работе/);
  assert.match(f.rows()[1].textContent, /1 в работе/);
  f.open('build-box'); f.button('Новый чат').click(); await flush();
  assert.equal(f.calls.find(call => call.name === 'newChat').args[0].machine, 'build-box');
});

test('filters intersect machine, saved, activity and all search terms while keeping pins first', async () => {
  const projects = [
    project('local', { name: 'Ядро', pinned: true, lastAt: 1 }),
    project('build-box', { name: 'Оплата', agents: ['codex'], saved: false, lastAt: 500, liveCount: 1, sessions: [conversation({ title: 'Проверить webhook' })] }),
    project('build-box', { cwd: '/work/admin', name: 'Админка', agents: ['codex'], saved: true, lastAt: 200, liveCount: 0 }),
  ];
  const f = await fixture({ projects });
  const { visibleProjects } = f.window.JarvisProjectState;
  const original = projects.map(p => p.name);
  assert.deepEqual(visibleProjects(projects).map(p => p.name), ['Ядро', 'Оплата', 'Админка']);
  assert.deepEqual(visibleProjects(projects, { sort: 'name' }).map(p => p.name), ['Ядро', 'Админка', 'Оплата']);
  assert.deepEqual(visibleProjects(projects, { machine: 'build-box', filter: 'active', query: '  CODEX webhook оплата ' }).map(p => p.name), ['Оплата']);
  assert.deepEqual(visibleProjects(projects, { machine: 'build-box', filter: 'saved' }).map(p => p.name), ['Админка']);
  assert.deepEqual(projects.map(p => p.name), original, 'sorting must not reorder source inventory');
  f.button('Сохранённые').click(); assert.equal(f.rows().length, 2);
  f.search('админка'); assert.equal(f.rows().length, 1);
});

test('pinning saves the exact project metadata and remains in the catalog', async () => {
  const f = await fixture({ projects: [project(), project('build-box')] });
  const remoteRow = f.rows().find(row => row.dataset.projectKey.includes('build-box')).parentElement;
  f.button('Закрепить проект', remoteRow).click(); await flush();
  assert.deepEqual(f.calls.find(call => call.name === 'projectsSave').args, [{ machine: 'build-box', cwd: '/work/app', name: 'Приложение', pinned: true }]);
  assert.equal(f.window.jarvisProjects.snapshot().selected, null);
  assert.ok(f.rows()[0].dataset.projectKey.includes('build-box'));
  assert.equal(f.calls.some(call => call.name === 'launchSession'), false);
});

test('save failure preserves typed name, machine, directory and pinned state for retry', async () => {
  const save = deferred();
  const f = await fixture({ projects: [], overrides: { projectsSave: () => save.promise } });
  f.button('Добавить проект').click();
  const machine = f.document.querySelector('select[aria-label="Машина проекта"]');
  machine.value = 'build-box'; machine.dispatchEvent(new f.window.Event('change'));
  f.input('projectName', 'Новый проект'); f.input('projectPath', '/srv/work in progress');
  const pin = f.document.querySelector('input[type="checkbox"]'); pin.checked = true; pin.dispatchEvent(new f.window.Event('change'));
  f.submit();
  assert.equal(f.document.getElementById('projectName').disabled, true);
  f.setSessions([{ id: 'unrelated', cwd: '/elsewhere', status: 'working' }]);
  save.resolve({ ok: false, error: 'Registry disk is full' }); await flush();
  assert.match(f.document.querySelector('[role="alert"]').textContent, /Registry disk is full/);
  assert.equal(f.document.getElementById('projectName').value, 'Новый проект');
  assert.equal(f.document.getElementById('projectPath').value, '/srv/work in progress');
  assert.equal(f.document.querySelector('select[aria-label="Машина проекта"]').value, 'build-box');
  assert.equal(f.document.querySelector('input[type="checkbox"]').checked, true);
  assert.equal(f.document.getElementById('projectName').disabled, false);
  assert.deepEqual(f.calls.find(call => call.name === 'projectsSave').args, [{ machine: 'build-box', cwd: '/srv/work in progress', name: 'Новый проект', pinned: true, avatar: null }]);
});

test('metadata removal keeps discovered chat history and never calls a file or lifecycle mutation', async () => {
  const f = await fixture({ projects: [project('build-box', { sessions: [conversation()] })] });
  let changes = 0; f.window.addEventListener('jarvis-projects-changed', () => changes++);
  f.open('build-box'); f.button('Настроить проект').click();
  assert.equal(f.document.getElementById('projectPath').disabled, true);
  f.button('Убрать из сохранённых').click(); await flush();
  assert.deepEqual(f.calls.find(call => call.name === 'projectsRemove').args, ['build-box', '/work/app']);
  assert.match(f.document.body.textContent, /Файлы и история чатов сохранены/);
  assert.match(f.document.body.textContent, /Исправить оплату/);
  assert.equal(changes, 1);
  assert.ok(f.button('Сохранить проект'));
  assert.deepEqual([...new Set(f.calls.map(call => call.name))].sort(), ['machinesList', 'projectsList', 'projectsRemove']);
});

test('failed metadata removal keeps the editor and its unsaved name', async () => {
  const f = await fixture({ overrides: { projectsRemove: async () => ({ ok: false, error: 'Registry is read-only' }) } });
  f.open(); f.button('Настроить проект').click(); f.input('projectName', 'Название в редакторе');
  f.button('Убрать из сохранённых').click(); await flush();
  assert.equal(f.document.getElementById('projectName').value, 'Название в редакторе');
  assert.match(f.document.querySelector('[role="alert"]').textContent, /Registry is read-only/);
  assert.equal(f.button('Убрать из сохранённых').disabled, false);
});

test('resume retains a matching launch event received before its IPC response and opens once', async () => {
  const launched = deferred();
  const f = await fixture({ projects: [project('build-box', { sessions: [conversation()] })], overrides: { launchSession: () => launched.promise } });
  f.open('build-box'); f.button('Продолжить').click();
  f.events.launch({ launchId: 'our-launch', status: 'attached', sessionId: 'runtime-42' });
  f.setSessions([{ id: 'runtime-42', remote: 'build-box', cwd: '/work/app', status: 'idle' }]);
  assert.equal(f.calls.filter(call => call.name === 'openSession').length, 0);
  launched.resolve({ ok: true, launchId: 'our-launch' }); await flush();
  assert.deepEqual(f.calls.find(call => call.name === 'launchSession').args, ['/work/app', 'claude', 'history-1', 'build-box', { mode: 'ask' }]);
  assert.equal(f.calls.filter(call => call.name === 'openSession').length, 1);
  assert.equal(f.calls.find(call => call.name === 'openSession').args[0].id, 'runtime-42');
  f.events.launch({ launchId: 'our-launch', status: 'attached', sessionId: 'runtime-42' });
  f.window.jarvisProjects.stateChanged();
  assert.equal(f.calls.filter(call => call.name === 'openSession').length, 1);
  assert.equal(f.timers.size, 0);
});

test('a foreign launch cannot claim a same-id session while the resume IPC response is pending', async () => {
  const launched = deferred();
  const f = await fixture({ projects: [project('build-box', { sessions: [conversation()] })], overrides: { launchSession: () => launched.promise } });
  f.open('build-box'); f.button('Продолжить').click();
  f.events.launch({ launchId: 'foreign-launch', status: 'attached', sessionId: 'history-1' });
  f.setSessions([{ id: 'history-1', remote: 'build-box', cwd: '/work/app', status: 'idle' }]);
  assert.equal(f.calls.filter(call => call.name === 'openSession').length, 0, 'no fallback before launch correlation is known');
  launched.resolve({ ok: true, launchId: 'our-launch' }); await flush();
  f.events.launch({ launchId: 'foreign-launch', status: 'attached', sessionId: 'history-1' });
  assert.equal(f.calls.filter(call => call.name === 'openSession').length, 0);
  f.events.launch({ launchId: 'our-launch', status: 'attached', sessionId: 'our-session' });
  f.setSessions([{ id: 'our-session', remote: 'build-box', cwd: '/work/app', status: 'idle' }]);
  assert.equal(f.calls.filter(call => call.name === 'openSession').length, 1);
  assert.equal(f.calls.find(call => call.name === 'openSession').args[0].id, 'our-session');
});

test('matching resumed session id on a different machine does not open the wrong chat', async () => {
  const f = await fixture({ projects: [project('build-box', { sessions: [conversation()] })] });
  f.open('build-box'); f.button('Продолжить').click(); await flush();
  f.events.launch({ launchId: 'our-launch', status: 'attached', sessionId: 'runtime-42' });
  f.setSessions([{ id: 'runtime-42', cwd: '/work/app', status: 'idle' }]);
  assert.equal(f.calls.filter(call => call.name === 'openSession').length, 0);
  f.setSessions([{ id: 'runtime-42', remote: 'build-box', cwd: '/work/app', status: 'idle' }]);
  assert.equal(f.calls.filter(call => call.name === 'openSession').length, 1);
});

test('leaving the project before resume attaches does not navigate the user back', async () => {
  const f = await fixture({ projects: [project('build-box', { sessions: [conversation()] })] });
  f.search('приложение'); f.open('build-box');
  assert.equal(f.query(), '');
  f.button('Продолжить').click(); await flush();
  f.window.jarvisProjects.back(); assert.equal(f.query(), 'приложение');
  f.events.launch({ launchId: 'our-launch', status: 'attached', sessionId: 'runtime-42' });
  f.setSessions([{ id: 'runtime-42', remote: 'build-box', cwd: '/work/app', status: 'idle' }]);
  assert.equal(f.calls.filter(call => call.name === 'openSession').length, 0);
  assert.equal(f.window.jarvisProjects.snapshot().selected, null);
});

test('offline project retains history and disables creating or resuming remote work', async () => {
  const f = await fixture({ projects: [project('offline-box', { sessions: [conversation()] })] });
  f.open('offline-box');
  assert.equal(f.button('Новый чат').disabled, true);
  assert.equal(f.button('Продолжить').disabled, true);
  assert.match(f.document.body.textContent, /Исправить оплату/);
  assert.match(f.document.body.textContent, /Нет связи/);
});

test('primary opens the keyboard-selected catalog project and delegates detail navigation to api.back', async () => {
  const f = await fixture({ projects: [project(), project('build-box')] });
  f.window.jarvisProjects.key({ key: 'ArrowDown', preventDefault() {} });
  f.window.jarvisProjects.primary();
  assert.equal(f.window.jarvisProjects.snapshot().selected, f.window.JarvisProjectState.keyOf(project('build-box')));
  assert.equal(f.calls.filter(call => call.name === 'back').length, 0);
  f.window.jarvisProjects.primary();
  assert.equal(f.calls.filter(call => call.name === 'back').length, 1);
  assert.equal(f.window.jarvisProjects.snapshot().selected, null);
  assert.equal(f.rows().length, 2);
  assert.equal(f.calls.some(call => call.name === 'launchSession' || call.name === 'newChat'), false);
});

for (const outcome of ['success', 'error']) {
  test(`stale folder ${outcome} after closing its browser cannot reopen it or change the form`, async () => {
    const browse = deferred();
    const f = await fixture({ projects: [], overrides: { bundleBrowse: () => browse.promise } });
    f.button('Добавить проект').click();
    f.input('projectPath', '/old-request'); f.button('Выбрать папку').click();
    assert.ok(f.document.querySelector('.pr-browser'));
    f.button('Закрыть выбор папки').click();
    f.input('projectName', 'Продолжаю ввод'); f.input('projectPath', '/new-typed-path');
    if (outcome === 'success') browse.resolve({ ok: true, path: '/stale-success', parent: '/', dirs: ['stale-child'] });
    else browse.reject(new Error('Stale folder failure'));
    await flush();
    assert.equal(f.document.querySelector('.pr-browser'), null);
    assert.equal(f.document.getElementById('projectName').value, 'Продолжаю ввод');
    assert.equal(f.document.getElementById('projectPath').value, '/new-typed-path');
    assert.doesNotMatch(f.document.body.textContent, /stale-success|stale-child|Stale folder failure/);
  });

  test(`stale folder ${outcome} from the previous machine cannot overwrite the current machine browser`, async () => {
    const oldBrowse = deferred();
    const f = await fixture({ projects: [], overrides: {
      bundleBrowse: (machine, path) => machine === 'local' ? oldBrowse.promise
        : Promise.resolve({ ok: true, path, parent: '/srv', dirs: ['current-child'] }),
    } });
    f.button('Добавить проект').click();
    f.input('projectPath', '/old-request'); f.button('Выбрать папку').click();
    const machine = f.document.querySelector('select[aria-label="Машина проекта"]');
    machine.value = 'build-box'; machine.dispatchEvent(new f.window.Event('change'));
    f.input('projectName', 'Удалённый проект'); f.input('projectPath', '/srv/current-project');
    f.button('Выбрать папку').click(); await flush();
    assert.match(f.document.querySelector('.pr-browser').textContent, /current-child/);
    if (outcome === 'success') oldBrowse.resolve({ ok: true, path: '/stale-local-path', parent: '/', dirs: ['stale-local-child'] });
    else oldBrowse.reject(new Error('Stale local failure'));
    await flush();
    assert.equal(f.document.querySelector('select[aria-label="Машина проекта"]').value, 'build-box');
    assert.equal(f.document.getElementById('projectName').value, 'Удалённый проект');
    assert.equal(f.document.getElementById('projectPath').value, '/srv/current-project');
    assert.match(f.document.querySelector('.pr-browser').textContent, /current-child/);
    assert.doesNotMatch(f.document.body.textContent, /stale-local|Stale local failure/);
  });
}

test('uploaded avatar draft survives rerender, navigation snapshot and a failed metadata save', async () => {
  const f = await fixture({ projects: [], avatars: {}, overrides: { projectsSave: async () => ({ ok: false, error: 'Cannot persist avatar metadata' }) } });
  f.button('Добавить проект').click(); f.input('projectPath', '/work/app'); f.input('projectName', 'Новый проект');
  f.upload(); await flush();
  const expected = { dataUrl: OTHER_AVATAR, source: 'upload', label: 'my-avatar.png' };
  assert.deepEqual(f.window.jarvisProjects.snapshot().form.avatar, expected);
  f.search(''); assert.equal(f.document.querySelector('.pr-avatar-preview img').src, OTHER_AVATAR);
  const saved = f.window.jarvisProjects.snapshot();
  f.window.jarvisProjects.enter(saved, true); await f.window.jarvisProjects.show();
  assert.deepEqual(f.window.jarvisProjects.snapshot().form.avatar, expected);
  assert.equal(f.document.getElementById('projectName').value, 'Новый проект');
  assert.equal(f.calls.some(call => call.name === 'projectsSave'), false);
  f.submit(); await flush();
  assert.match(f.document.querySelector('[role="alert"]').textContent, /Cannot persist avatar metadata/);
  assert.deepEqual(f.window.jarvisProjects.snapshot().form.avatar, expected);
  assert.deepEqual(f.calls.find(call => call.name === 'projectsSave').args[0].avatar, expected);
  assert.equal(f.document.querySelector('button[type="submit"]').disabled, false);
});

test('failed upload retains the previous picture and typed project name', async () => {
  const original = { dataUrl: AVATAR_PNG, source: 'upload', label: 'existing.png' };
  const f = await fixture({ projects: [project('local', { avatar: original })], avatars: { fromFile: async () => { throw new Error('Upload decode failed'); } } });
  f.open(); f.button('Настроить проект').click(); f.input('projectName', 'Несохранённое имя');
  f.upload(); await flush();
  assert.match(f.document.querySelector('.pr-avatar-status').textContent, /Upload decode failed/);
  assert.deepEqual(f.window.jarvisProjects.snapshot().form.avatar, original);
  assert.equal(f.document.getElementById('projectName').value, 'Несохранённое имя');
  assert.equal(f.button('Загрузить картинку').disabled, false);
  assert.equal(f.calls.some(call => call.name === 'projectsSave'), false);
});

test('one discovered candidate becomes a normalized draft, while an existing custom avatar wins', async () => {
  for (const original of [null, { dataUrl: AVATAR_PNG, source: 'upload', label: 'existing.png' }]) {
    const f = await fixture({ projects: [project('local', { avatar: original })], avatars: {
      discover: async () => ({ ok: true, candidates: [avatarCandidate()] }),
    } });
    f.open(); f.button('Настроить проект').click(); f.button('Найти в проекте').click(); await flush();
    assert.deepEqual(f.avatarCalls.find(call => call.name === 'discover').args, ['local', '/work/app', { refresh: true }]);
    const selected = f.window.jarvisProjects.snapshot().form.avatar;
    if (original) {
      assert.deepEqual(selected, original);
      assert.equal(f.avatarCalls.some(call => call.name === 'normalize'), false);
    } else {
      assert.deepEqual(selected, { dataUrl: OTHER_AVATAR, source: 'project', path: '/work/app/icon.png', label: 'icon.png' });
      assert.deepEqual(f.avatarCalls.find(call => call.name === 'normalize').args, [AVATAR_PNG]);
    }
    assert.equal(f.calls.some(call => call.name === 'projectsSave'), false);
  }
});

test('multiple discovered candidates require an explicit choice and normalize the chosen source', async () => {
  const candidates = [avatarCandidate('first.png'), avatarCandidate('second.png')];
  const f = await fixture({ avatars: { discover: async () => ({ ok: true, candidates, truncated: true }) } });
  f.open(); f.button('Настроить проект').click(); f.button('Найти в проекте').click(); await flush();
  assert.equal(f.window.jarvisProjects.snapshot().form.avatar, null);
  assert.equal(f.document.querySelectorAll('.pr-avatar-candidates button').length, 2);
  assert.equal(f.avatarCalls.some(call => call.name === 'normalize'), false);
  f.button('second.png', f.document.querySelector('.pr-avatar-candidates')).click(); await flush();
  assert.deepEqual(f.window.jarvisProjects.snapshot().form.avatar, { dataUrl: OTHER_AVATAR, source: 'project', path: '/work/app/second.png', label: 'second.png' });
  assert.deepEqual(f.avatarCalls.find(call => call.name === 'normalize').args, [AVATAR_PNG]);
  assert.equal(f.calls.some(call => call.name === 'projectsSave'), false);
});

test('removing an avatar saves explicit null, while pinning leaves avatar metadata untouched', async () => {
  const original = { dataUrl: AVATAR_PNG, source: 'upload', label: 'existing.png' };
  const f = await fixture({ projects: [project('local', { avatar: original })], avatars: {} });
  f.button('Закрепить проект').click(); await flush();
  assert.equal(Object.hasOwn(f.calls.find(call => call.name === 'projectsSave').args[0], 'avatar'), false);
  f.open(); f.button('Настроить проект').click();
  assert.deepEqual(f.window.jarvisProjects.snapshot().form.avatar, original);
  f.button('Убрать картинку').click(); f.submit(); await flush();
  assert.equal(f.calls.filter(call => call.name === 'projectsSave').at(-1).args[0].avatar, null);
});

test('clicking project artwork opens its editor and uses cached discovery without starting a chat', async () => {
  const f = await fixture({ avatars: { discover: async () => ({ ok: true, candidates: [avatarCandidate()] }) } });
  const artwork = f.document.querySelector('.pr-project-row .pr-folder'); assert.ok(artwork);
  artwork.click(); await flush();
  assert.equal(f.document.getElementById('projectName').value, 'Приложение');
  assert.deepEqual(f.avatarCalls.find(call => call.name === 'discover').args, ['local', '/work/app', { refresh: false }]);
  assert.equal(f.window.jarvisProjects.snapshot().form.avatar.dataUrl, OTHER_AVATAR);
  assert.equal(f.calls.some(call => ['newChat', 'launchSession', 'projectsSave'].includes(call.name)), false);
});

for (const [operation, transition] of [['upload', 'path'], ['upload', 'back'], ['discover', 'machine'], ['discover', 'enter']]) {
  for (const outcome of ['success', 'error']) {
    test(`stale avatar ${operation} ${outcome} after ${transition} cannot alter the current form`, async () => {
      const pending = deferred();
      const f = await fixture({ projects: [], avatars: {
        fromFile: () => pending.promise,
        discover: () => pending.promise,
      } });
      f.button('Добавить проект').click(); f.input('projectName', 'Текущая форма'); f.input('projectPath', '/work/app');
      if (operation === 'upload') f.upload(); else f.button('Найти в проекте').click();
      assert.equal(f.document.querySelector('button[type="submit"]').disabled, true);
      if (transition === 'path') f.input('projectPath', '/new/project');
      else if (transition === 'machine') {
        const machine = f.document.querySelector('select[aria-label="Машина проекта"]');
        machine.value = 'build-box'; machine.dispatchEvent(new f.window.Event('change')); f.input('projectPath', '/remote/project');
      } else if (transition === 'back') {
        f.window.jarvisProjects.back(); f.button('Добавить проект').click(); f.input('projectName', 'Новая форма'); f.input('projectPath', '/work/app');
      } else {
        const saved = f.window.jarvisProjects.snapshot(); f.window.jarvisProjects.enter(saved, true); await f.window.jarvisProjects.show();
      }
      const before = f.window.jarvisProjects.snapshot().form;
      if (outcome === 'success') pending.resolve(operation === 'upload' ? OTHER_AVATAR : { ok: true, candidates: [avatarCandidate('stale.png')] });
      else pending.reject(new Error('Stale avatar operation failed'));
      await flush();
      const after = f.window.jarvisProjects.snapshot().form;
      assert.equal(after.machine, before.machine); assert.equal(after.cwd, before.cwd); assert.equal(after.name, before.name);
      assert.equal(after.avatar, null);
      assert.deepEqual(after.avatarCandidates, []);
      assert.equal(f.document.querySelector('button[type="submit"]').disabled, false);
      assert.doesNotMatch(f.document.body.textContent, /Stale avatar operation failed|stale\.png/);
    });
  }
}
