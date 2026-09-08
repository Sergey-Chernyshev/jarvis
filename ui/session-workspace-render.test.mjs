import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const read = name => readFileSync(new URL(name, import.meta.url), 'utf8');
const base = { id: 's1', title: 'Исправить меню', project: 'Jarvis', cwd: '/work/jarvis', status: 'idle', agent: 'codex', model: 'gpt-5.5', tmuxPane: '%1' };
const tick = () => new Promise(resolve => setTimeout(resolve, 0));

test('synthetic error models show an unknown placeholder without selecting another model', async () => {
  const session = { ...base, model: '<synthetic>' };
  const ui = await boot({ sessions: [session] });
  ui.open(session.id);
  const picker = ui.document.querySelector('select[aria-label="Модель текущего чата"]');
  assert.ok(picker);
  assert.equal(picker.value, '');
  assert.equal(picker.options[0].textContent, 'Модель не определена');
  assert.equal(picker.options[0].disabled, true);
  assert.ok(![...picker.options].some(o => /synthetic/i.test(o.textContent)));
  ui.refresh([{ ...session, status: 'waiting' }]);
  assert.ok(!ui.document.querySelector('.chatinput').textContent.includes('<synthetic>'));
});

async function boot(options = {}) {
  const { window, document } = parseHTML(read('./index.html'));
  Object.defineProperty(window.HTMLSelectElement.prototype, 'value', {
    configurable: true,
    get() { return [...this.options].find(o => o.hasAttribute('selected'))?.value || this.options[0]?.value || ''; },
    set(value) { for (const o of this.options) o.toggleAttribute('selected', o.value === value); },
  });
  const memory = options.memory || new Map();
  const storage = { getItem: k => memory.get(k), setItem: (k, v) => memory.set(k, v) };
  const calls = [];
  const timers = new Map(); let timerId = 0, launchListener;
  let sessions = options.sessions || [base], activeId = null;
  const bridge = {
    getSettings: async () => ({ projects: [] }),
    machinesList: async () => options.machines || [{ id: 'local', online: true, kind: 'local' }, { id: 'remote', online: true, kind: 'remote', name: 'remote' }],
    projectsList: async () => ({ ok: true, projects: [] }), agentsList: async () => ({ ok: true, agents: [] }),
    getSessionUsage: async id => { calls.push(['usage', id]); return options.usage ? options.usage(id) : { tok: 100 }; },
    openWorkspace: async route => { calls.push(['window', route]); return { ok: true }; },
    copyText: async text => { calls.push(['copy', text]); return { ok: true }; },
    setPin: async (id, pinned) => { calls.push(['pin', id, pinned]); return { ok: true }; },
    terminalSnapshot: async id => { calls.push(['terminal', id]); return { ok: true, connection: {}, text: '' }; },
    continueSession: async id => { calls.push(['continue', id]); return options.continueSession?.(id) || { ok: true, launchId: 'launch-fixture', terminalId: 'launch-fixture' }; },
    onLaunchTask: callback => { launchListener = callback; },
  };
  window.JarvisTerminal = { create: () => { const root = document.createElement('section'); root.className = 'sw-terminal'; return { root, setSession: (...args) => calls.push(['terminal-session', ...args]) }; } };
  Object.assign(bridge, options.bridge);
  window.jarvis = bridge;
  window.__JARVIS_WORKSPACE__ = options.workspace ? {} : null;
  document.hasFocus = () => true;
  for (const file of ['icons.js', 'async-state.js', 'attachments.js', 'session-workspace.js']) {
    new Function('window', 'document', 'localStorage', 'setInterval', 'setTimeout', 'clearTimeout', read('./' + file))(window, document, storage, () => 0,
      (callback, delay) => { timers.set(++timerId, { callback, delay }); return timerId; }, id => timers.delete(id));
  }
  const api = {
    navigate: next => window.jarvisSessionWorkspace.changed(next), sessions: () => sessions, currentId: () => activeId,
    openSession: s => { activeId = s.id; window.jarvisSessionWorkspace.changed('chat'); },
    trackLaunchMessage: (id, message) => calls.push(['outgoing', id, message]),
    toast: text => calls.push(['toast', text]), settingsPane() {}, models: () => [['gpt-5.5', 'GPT-5.5']], efforts: () => [['auto', 'auto']],
    sessionLoadState: options.loadState, sessionLoadError: () => 'Нет связи', retrySessions: options.retrySessions,
    terminal: s => calls.push(['focus', s.id]), send() {}, attach() {}, hasAttachments: () => false,
  };
  window.initSessionWorkspace(api); window.jarvisSessionWorkspace.changed('list'); await tick();
  return { window, document, calls, memory, open: id => api.openSession(sessions.find(s => s.id === id)),
    currentId: () => activeId, emitLaunch: event => launchListener?.(event),
    runTimer: delay => { for (const [id, timer] of [...timers]) if (timer.delay === delay) { timers.delete(id); timer.callback(); } },
    refresh: next => { sessions = next; window.jarvisSessionWorkspace.refresh(sessions); },
    click: el => el.dispatchEvent(new window.Event('click', { bubbles: true })),
  };
}

test('sidebar limits rows, preserves a collapsed project on live refresh and after restart', async () => {
  const sessions = Array.from({ length: 8 }, (_, i) => ({ ...base, id: 's' + i, title: 'Задача ' + i, createdAt: i }));
  const ui = await boot({ sessions });
  const rows = () => ui.document.querySelectorAll('#sessionSidebar .sw-session');
  assert.equal(rows().length, 5);
  assert.equal(rows()[0].querySelector('small'), null, 'one title line in sidebar');
  ui.click(ui.document.querySelector('.sw-show-more')); assert.equal(rows().length, 8);
  ui.click(ui.document.querySelector('.sw-project-toggle')); assert.equal(rows().length, 0);
  ui.refresh(sessions.map(s => ({ ...s, updatedAt: 1000, status: 'working' })));
  assert.equal(rows().length, 0);
  assert.equal(ui.document.querySelector('.sw-project-toggle').getAttribute('aria-expanded'), 'false');
  const restarted = await boot({ sessions, memory: ui.memory });
  assert.equal(restarted.document.querySelectorAll('#sessionSidebar .sw-session').length, 0);
  restarted.click(restarted.document.querySelector('.sw-project-toggle'));
  assert.equal(restarted.document.querySelectorAll('#sessionSidebar .sw-session').length, 8);
});

test('chat and project actions dispatch exact supported routes without opening the chat', async () => {
  const ui = await boot({ sessions: [{ ...base, remote: 'remote' }] });
  const find = (root, text) => [...root.querySelectorAll('button')].find(b => b.textContent === text);
  const chatMenu = ui.document.querySelector('.sw-session-row .sw-actions');
  ui.click(find(chatMenu, 'В отдельном окне'));
  assert.deepEqual(ui.calls.find(c => c[0] === 'window')[1], { sessionId: 's1', project: '/work/jarvis', remote: 'remote', detached: true });
  ui.click(find(chatMenu, 'Закрепить')); assert.deepEqual(ui.calls.find(c => c[0] === 'pin'), ['pin', 's1', true]);
  ui.click(find(chatMenu, 'Копировать название')); assert.deepEqual(ui.calls.find(c => c[0] === 'copy'), ['copy', 'Исправить меню']);
  assert.equal(ui.document.querySelector('[aria-current="page"]'), null);
  ui.click(find(ui.document.querySelector('.sw-project-head .sw-actions'), 'В отдельном окне'));
  assert.deepEqual(ui.calls.filter(c => c[0] === 'window').at(-1)[1], { project: '/work/jarvis', remote: 'remote', detached: true });
});

test('external Codex hides unusable composer and terminal, rejects stale usage, and can continue in Codex', async () => {
  const pendingUsage = new Map();
  const external = { ...base, id: 'external', title: '<environment_context> generated', lastPrompt: 'Проверить изменения', tmuxPane: null, controlMode: 'external', instanceLabel: 'Personal' };
  const ui = await boot({ sessions: [base, external], usage: id => new Promise(resolve => { pendingUsage.set(id, resolve); }) });
  ui.open(base.id); await tick();
  assert.equal(ui.document.querySelector('.chatinput').hidden, false);
  ui.open(external.id); await tick();
  assert.equal(ui.document.querySelector('.chatinput').hidden, true);
  assert.equal(ui.document.getElementById('tmuxHint').hidden, true);
  assert.equal(ui.document.querySelector('.sw-terminal').hidden, true);
  assert.equal(ui.document.getElementById('chatTitle').textContent, 'Проверить изменения');
  assert.deepEqual(ui.calls.filter(c => c[0] === 'usage').map(c => c[1]), ['s1', 'external'], 'read-only access still supports session-scoped statistics');
  pendingUsage.get('s1')({ tok: 900 }); await tick();
  assert.equal(ui.document.querySelector('.sw-chat-usage').getAttribute('aria-busy'), 'true', 'late previous usage cannot replace the current skeleton');
  assert.ok(ui.document.querySelector('.sw-chat-usage .ui-skeleton'));
  assert.doesNotMatch(ui.document.querySelector('.sw-chat-usage').textContent, /900/);
  pendingUsage.get('external')({ tok: 250, instanceLabel: 'Personal' }); await tick();
  assert.equal(ui.document.querySelector('.sw-chat-usage').hidden, false);
  assert.match(ui.document.querySelector('.sw-chat-usage summary').textContent, /250/);
  assert.match(ui.document.querySelector('.sw-usage-details').textContent, /Personal/);
  const usageMenu = ui.document.querySelector('.sw-chat-usage').closest('.sw-chat-actions');
  assert.ok(usageMenu, 'readonly usage stays inside the details menu');
  assert.equal(!!usageMenu.open, false, 'usage does not open a panel on its own');
  ui.click([...ui.document.querySelectorAll('.sw-chat-context button')].find(b => b.textContent === 'Открыть Codex'));
  assert.deepEqual(ui.calls.at(-1), ['focus', 'external']);
  ui.open(base.id); assert.equal(ui.document.querySelector('.chatinput').hidden, false);
});

test('workspace hides quick-open action, keeps tools in disclosure, and can focus a project', async () => {
  const ui = await boot({ workspace: true, sessions: [base, { ...base, id: 'other', cwd: '/work/other', project: 'Other' }] });
  ui.open(base.id);
  assert.equal(ui.document.querySelector('.sw-workspace-open').hidden, true);
  assert.ok(ui.document.querySelector('.sw-chat-actions #changesBtn'));
  assert.ok(ui.document.querySelector('.sw-chat-actions #chatModel'));
  await ui.window.jarvisSessionWorkspace.focusProject('/work/other');
  assert.equal(ui.document.getElementById('newChatDirectory').value, '/work/other');
  assert.equal(ui.document.querySelectorAll('#sessionSidebar .sw-project').length, 1);
  ui.click(ui.document.querySelector('.sw-project-filter button'));
  assert.equal(ui.document.querySelectorAll('#sessionSidebar .sw-project').length, 2);
});

const imported = { ...base, id: 'imported', title: 'История исходного чата', tmuxPane: null, controlMode: 'external', remote: 'remote', instanceId: 'personal' };
const child = { ...imported, id: 'continued', title: 'Продолжение', tmuxPane: '%9', controlMode: 'managed' };

test('continuation deduplicates clicks, exposes startup terminal and binds an early event after state refresh', async () => {
  let resolve;
  const ui = await boot({ sessions: [imported], continueSession: () => new Promise(r => { resolve = r; }) });
  ui.open(imported.id);
  const button = ui.document.querySelector('.sw-continue');
  assert.equal(button.hidden, false);
  ui.click(button); ui.click(button);
  assert.equal(button.disabled, true);
  assert.deepEqual(ui.calls.filter(c => c[0] === 'continue'), [['continue', imported.id]]);
  resolve({ ok: true, launchId: 'l1', terminalId: 'l1' }); await tick();
  const toggle = ui.document.querySelector('.sw-chat-context button[aria-label="Терминал"]');
  assert.equal(toggle.hidden, false); assert.equal(toggle.disabled, false);
  ui.click(toggle);
  assert.deepEqual(ui.calls.filter(c => c[0] === 'terminal-session').at(-1).slice(0, 3), ['terminal-session', 'l1', true]);
  assert.equal(ui.document.querySelector('.chatinput').hidden, true);
  ui.emitLaunch({ launchId: 'l1', status: 'ready', sessionId: child.id });
  assert.equal(ui.currentId(), imported.id, 'wait for actual provider session');
  ui.refresh([imported, child]);
  assert.equal(ui.currentId(), child.id);
  assert.equal(ui.document.querySelector('.chatinput').hidden, false);
  assert.equal(ui.document.querySelector('.sw-continue').hidden, true);
  assert.equal(imported.controlMode, 'external'); assert.equal(imported.tmuxPane, null);
  ui.open(imported.id);
  assert.equal(ui.currentId(), imported.id, 'original history remains readable after continuation opens');
  assert.equal(ui.document.querySelector('.chatinput').hidden, true);
  ui.click(ui.document.querySelector('.sw-continue'));
  assert.equal(ui.currentId(), child.id);
  assert.equal(ui.calls.filter(c => c[0] === 'continue').length, 1);
});

test('continuation consumes hook arriving before response and does not hijack another active chat', async () => {
  let resolve;
  const ui = await boot({ sessions: [imported, base], continueSession: () => new Promise(r => { resolve = r; }) });
  ui.open(imported.id); ui.click(ui.document.querySelector('.sw-continue'));
  ui.emitLaunch({ launchId: 'early', status: 'ready', sessionId: child.id });
  ui.open(base.id); ui.refresh([imported, base, child]);
  resolve({ ok: true, launchId: 'early', terminalId: 'early' }); await tick();
  assert.equal(ui.currentId(), base.id);
  ui.open(imported.id);
  assert.equal(ui.currentId(), child.id);
  assert.equal(ui.calls.filter(c => c[0] === 'continue').length, 1);
});

test('continuation failure preserves startup terminal and never launches a duplicate on recovery', async () => {
  const ui = await boot({ sessions: [imported] }); ui.open(imported.id);
  ui.click(ui.document.querySelector('.sw-continue')); await tick();
  ui.emitLaunch({ launchId: 'launch-fixture', status: 'failed', error: 'Нужен вход в агент' });
  assert.match(ui.document.querySelector('.sw-continue-status').textContent, /Нужен вход/);
  assert.equal(ui.document.querySelector('.chatinput').hidden, true);
  ui.click(ui.document.querySelector('.sw-continue')); await tick();
  assert.equal(ui.calls.filter(c => c[0] === 'continue').length, 1);
  assert.equal(ui.calls.filter(c => c[0] === 'terminal-session').at(-1)[2], true);
});

test('continuation rejects offline launch and exposes recoverable errors and a bounded wait', async () => {
  const offline = await boot({ sessions: [imported], machines: [{ id: 'remote', online: false }] });
  offline.open(imported.id); offline.click(offline.document.querySelector('.sw-continue')); await tick();
  assert.equal(offline.document.querySelector('.sw-continue').disabled, true);
  assert.equal(offline.calls.filter(c => c[0] === 'continue').length, 0);
  const failed = await boot({ sessions: [imported], continueSession: () => ({ ok: false, error: 'Профиль недоступен' }) });
  failed.open(imported.id); failed.click(failed.document.querySelector('.sw-continue')); await tick();
  assert.match(failed.document.querySelector('.sw-continue-status').textContent, /Профиль недоступен/);
  assert.equal(failed.document.querySelector('.sw-continue').disabled, false);
  const waiting = await boot({ sessions: [imported] });
  waiting.open(imported.id); waiting.click(waiting.document.querySelector('.sw-continue')); await tick(); waiting.runTimer(100000);
  assert.match(waiting.document.querySelector('.sw-continue-status').textContent, /терминал запуска/);
  assert.equal(waiting.document.querySelector('.sw-continue').disabled, false);
});

test('an already managed response does not recursively reopen its original session', async () => {
  const ui = await boot({ sessions: [imported], continueSession: () => ({ ok: true, sessionId: imported.id }) });
  ui.open(imported.id); ui.click(ui.document.querySelector('.sw-continue')); await tick();
  assert.equal(ui.currentId(), imported.id);
  assert.equal(ui.calls.filter(c => c[0] === 'continue').length, 1);
  ui.refresh([{ ...imported, tmuxPane: '%10', controlMode: 'managed' }]);
  assert.equal(ui.document.querySelector('.chatinput').hidden, false);
});

test('polling and status changes keep recent buttons and sidebar scroll stable', async () => {
  const sessions = Array.from({ length: 8 }, (_, i) => ({ ...base, id: `stable-${i}`, createdAt: i, updatedAt: i }));
  const ui = await boot({ sessions });
  const recent = [...ui.document.querySelectorAll('.sw-recent [data-session-id]')];
  const sidebar = ui.document.querySelector('.sw-projects'); sidebar.scrollTop = 300;
  const first = sidebar.querySelector('[data-session-id]');
  ui.refresh([...sessions].reverse().map((s, i) => ({ ...s, updatedAt: 9000 + i, detail: 'stream ' + i, status: 'working' })));
  assert.deepEqual([...ui.document.querySelectorAll('.sw-recent [data-session-id]')], recent);
  assert.equal(sidebar.querySelector('[data-session-id]'), first);
  assert.equal(sidebar.scrollTop, 300);
  ui.click(recent[2]);
  assert.equal(ui.currentId(), recent[2].dataset.sessionId);
});

test('new launch immediately shows task, freezes selected model and retains delivery errors', async () => {
  let resolve, args;
  const ui = await boot({ sessions: [], bridge: { launchSession: (...values) => { args = values; return new Promise(r => { resolve = r; }); } } });
  const prompt = ui.document.querySelector('#newChatPrompt'), cwd = ui.document.querySelector('#newChatDirectory'), model = ui.document.querySelector('#newChatModel');
  cwd.value = '/work/jarvis'; prompt.value = 'Почини отправку'; model.value = 'gpt-5.5';
  model.dispatchEvent(new ui.window.Event('change')); prompt.dispatchEvent(new ui.window.Event('input'));
  ui.document.querySelector('.sw-composer').dispatchEvent(new ui.window.Event('submit', { cancelable: true }));
  assert.equal(ui.document.querySelector('.sw-startup').hidden, false);
  assert.match(ui.document.querySelector('.sw-startup').textContent, /Почини отправку/);
  await tick(); assert.equal(args[4].model, 'gpt-5.5');
  resolve({ ok: true, launchId: 'new-launch', pane: '%42' }); await tick();
  const child = { ...base, id: 'new-chat', agent: 'claude', tmuxPane: '%42' };
  ui.refresh([child]); ui.emitLaunch({ launchId: 'new-launch', status: 'connected', sessionId: child.id });
  assert.equal(ui.currentId(), child.id, 'open before delivery finishes');
  assert.deepEqual(ui.calls.find(c => c[0] === 'outgoing').slice(1), [child.id, {key:'new-launch',text:'Почини отправку',files:[],displayText:'Почини отправку',status:'Отправляем…',kind:'loading'}]);
  ui.emitLaunch({ launchId: 'new-launch', status: 'failed', error: 'Доставка не удалась' });
  assert.equal(prompt.value, 'Почини отправку');
  assert.ok(ui.calls.some(c => c[0] === 'toast' && c[1] === 'Доставка не удалась'));
  const restarted = await boot({ memory: ui.memory });
  assert.equal(restarted.document.querySelector('#newChatModel').value, 'gpt-5.5');
});

test('initial chat loading distinguishes skeletons, retryable error and confirmed empty list', async () => {
  let state = 'loading', retries = 0;
  const ui = await boot({ sessions: [], loadState: () => state, retrySessions: () => { retries++; state = 'loading'; ui.refresh([]); } });
  assert.ok(ui.document.querySelector('.sw-projects .ui-skeleton'));
  assert.ok(ui.document.querySelector('.sw-recent .ui-skeleton'));
  assert.doesNotMatch(ui.document.querySelector('.sw-recent').textContent, /Запусти задачу/);
  state = 'error'; ui.refresh([]);
  assert.equal(ui.document.querySelector('.sw-recent .ui-state').getAttribute('role'), 'alert');
  ui.click(ui.document.querySelector('.sw-recent .ui-state-action'));
  assert.equal(retries, 1); assert.ok(ui.document.querySelector('.sw-recent .ui-skeleton'));
  state = 'ready'; ui.refresh([]);
  assert.equal(ui.document.querySelector('.sw-recent .ui-skeleton'), null);
  assert.match(ui.document.querySelector('.sw-recent').textContent, /Запусти задачу/);
});

test('usage failure offers retry and clears its busy state', async () => {
  let fail = true;
  const ui = await boot({ usage: async () => { if (fail) throw new Error('Соединение прервано'); return { tok: 420 }; } });
  ui.open(base.id); await tick();
  assert.equal(ui.document.querySelector('.sw-chat-usage').getAttribute('aria-busy'), 'false');
  assert.match(ui.document.querySelector('.sw-usage-details').textContent, /Статистика недоступна/);
  fail = false; ui.click(ui.document.querySelector('.sw-usage-details .ui-state-action')); await tick();
  assert.match(ui.document.querySelector('.sw-chat-usage summary').textContent, /420/);
  assert.equal(ui.document.querySelector('.sw-usage-details .ui-state-action'), null);
});

test('new chat remembers each machine project and restores the selected host after restart', async () => {
  const ui = await boot();
  const machine = ui.document.querySelector('#newChatMachine'), cwd = ui.document.querySelector('#newChatDirectory');
  const input = path => { cwd.value = path; cwd.dispatchEvent(new ui.window.Event('input')); };
  const choose = async host => { machine.value = host; machine.dispatchEvent(new ui.window.Event('change')); await tick(); };
  input('/Users/me/main project');
  await choose('remote'); assert.equal(cwd.value, '', 'never reuse a local path on another machine');
  input('/home/coder/workspace/ticksly');
  await choose('local'); assert.equal(cwd.value, '/Users/me/main project');
  await choose('remote'); assert.equal(cwd.value, '/home/coder/workspace/ticksly');
  const restarted = await boot({ memory: ui.memory });
  assert.equal(restarted.document.querySelector('#newChatMachine').value, 'remote');
  assert.equal(restarted.document.querySelector('#newChatDirectory').value, '/home/coder/workspace/ticksly');
  assert.match(restarted.document.querySelector('.sw-hero h1').textContent, /ticksly/);
  await restarted.window.jarvisSessionWorkspace.newChat({ machine: 'remote', cwd: '/home/coder/another' });
  const selectedProject = await boot({ memory: ui.memory });
  assert.equal(selectedProject.document.querySelector('#newChatDirectory').value, '/home/coder/another');
});

test('an unavailable saved machine keeps its own project and cannot launch on a fallback host', async () => {
  const memory = new Map([['jarvis.chat.location.v1', JSON.stringify({ machine: 'missing', paths: { missing: '/remote/repo', local: '/local/repo' } })]]);
  const ui = await boot({ memory });
  assert.equal(ui.document.querySelector('#newChatMachine').value, 'missing');
  assert.equal(ui.document.querySelector('#newChatDirectory').value, '/remote/repo');
  const prompt = ui.document.querySelector('#newChatPrompt'); prompt.value = 'Task'; prompt.dispatchEvent(new ui.window.Event('input'));
  assert.equal(ui.document.querySelector('.sw-composer .sw-send').disabled, true);
});
