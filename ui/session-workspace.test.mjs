import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const window = {};
new Function('window', readFileSync(new URL('./session-workspace.js', import.meta.url), 'utf8'))(window);
const { groupSessions, createAttention, attentionKey, titleOf, createProjectView, capabilities, selectedModel } = window.JarvisSessionState;

const session = (id, patch = {}) => ({
  id, title: id, project: 'Jarvis', cwd: '/work/jarvis', agent: 'claude',
  status: 'working', createdAt: 100, updatedAt: 100, ...patch,
});

test('projects with the same path stay separate on local and remote machines', () => {
  const original = [
    session('local-new', { createdAt: 300 }),
    session('remote', { remote: 'build-box', createdAt: 500 }),
    session('local-pinned', { pinned: true, createdAt: 100 }),
    session('other-remote', { remote: 'review-box' }),
    session('local-older', { createdAt: 200 }),
  ];
  const groups = groupSessions(original);
  assert.deepEqual(groups.map(g => g.machine), ['local', 'build-box', 'review-box']);
  assert.equal(new Set(groups.map(g => g.key)).size, 3);
  assert.deepEqual(groups[0].sessions.map(s => s.id), ['local-pinned', 'local-new', 'local-older']);
  assert.deepEqual(original.map(s => s.id), ['local-new', 'remote', 'local-pinned', 'other-remote', 'local-older']);
});

test('project search matches all terms across provider, host, title and details', () => {
  const sessions = [
    session('remote-code', { remote: 'Build-Box', agent: 'codex', title: 'Чат оплаты', detail: 'Проверяет webhook' }),
    session('local-code', { agent: 'codex', title: 'Чат оплаты' }),
    session('remote-claude', { remote: 'Build-Box', title: 'Чат оплаты' }),
  ];
  assert.deepEqual(groupSessions(sessions, '  CODEX  build-box webhook ')[0].sessions.map(s => s.id), ['remote-code']);
  assert.deepEqual(groupSessions(sessions, 'missing project'), []);
});

test('project rows keep a stable alphabetical order when backend recency order changes', () => {
  const sessions = [session('z', { project: 'Zebra', cwd: '/z' }), session('b', { project: 'Beta', cwd: '/b' }), session('a', { project: 'Alpha', cwd: '/a' })];
  const before = groupSessions(sessions).map(g => g.key);
  assert.deepEqual(groupSessions(sessions).map(g => g.name), ['Alpha', 'Beta', 'Zebra']);
  const next = [...sessions].reverse().map((s, i) => ({ ...s, updatedAt: 999 + i, status: 'working' }));
  assert.deepEqual(groupSessions(next).map(g => g.key), before);
});

test('attention shows initial questions and limits, without flooding historical completions', () => {
  const attention = createAttention();
  const entries = attention.update([
    session('finished-before-open', { status: 'done', doneAt: 90 }),
    session('question', { status: 'waiting', question: { at: 110, text: 'Разрешить?' } }),
    session('quota', { status: 'limit', lifecycleRevision: 2 }),
    session('working'),
  ], null);
  assert.deepEqual(entries.map(s => s.id), ['question', 'quota']);
});

test('repeated snapshots and reconnects do not resurrect a read notification', () => {
  const waiting = session('question', { status: 'waiting', question: { at: 120, text: 'Продолжить?' } });
  const attention = createAttention();
  assert.equal(attention.update([waiting], null).length, 1);
  for (let updatedAt = 1000; updatedAt < 1010; updatedAt++) {
    assert.equal(attention.update([{ ...waiting, updatedAt }], null).length, 1);
  }
  attention.read(waiting);
  assert.deepEqual(attention.update([{ ...waiting, updatedAt: 2000 }], null), []);
  const afterRestart = createAttention(attention.saved());
  assert.deepEqual(afterRestart.update([{ ...waiting, updatedAt: 3000 }], null), []);
  assert.equal(afterRestart.update([{ ...waiting, question: { at: 121, text: 'Новый вопрос?' } }], null).length, 1);
});

test('completions notify once per turn and viewing a chat marks its attention read', () => {
  const attention = createAttention();
  const working = session('background');
  attention.update([working], null);
  const completed = { ...working, status: 'done', doneAt: 500 };
  assert.equal(attention.update([completed], null).length, 1);
  assert.deepEqual(attention.update([{ ...completed, updatedAt: 600 }], 'background'), []);
  assert.ok(attention.saved().includes(attentionKey(completed)));
  assert.deepEqual(attention.update([{ ...completed, updatedAt: 700 }], null), []);
  attention.update([{ ...working, providerTurnId: 'turn-2' }], null);
  assert.equal(attention.update([{ ...completed, doneAt: 800, providerTurnId: 'turn-2' }], null).length, 1);
  assert.deepEqual(attention.update([], null), [], 'closed sessions disappear from the inbox');
});

test('project disclosure and show-more survive restart and independent snapshots', () => {
  const memory = new Map();
  const storage = { getItem: k => memory.get(k), setItem: (k, v) => memory.set(k, v) };
  const project = createProjectView(storage);
  assert.equal(project.isOpen('local'), true);
  assert.equal(project.count('local'), 5);
  project.toggle('local'); project.more('remote');
  const restored = createProjectView(storage);
  assert.equal(restored.isOpen('local'), false);
  assert.equal(restored.isOpen('remote'), true);
  assert.equal(restored.count('remote'), 15);
  restored.toggle('local'); project.read();
  assert.equal(project.isOpen('local'), true, 'another window can update disclosure state');
  storage.setItem(project.key, '{broken');
  assert.equal(createProjectView(storage).isOpen('local'), true);
});

test('titles ignore known service envelopes, preserve real code/XML, and stay bounded', () => {
  assert.equal(titleOf({ title: '<environment_context> cwd', lastPrompt: 'Исправить меню' }), 'Исправить меню');
  assert.equal(titleOf({ title: 'codex-auto-review', project: 'Jarvis' }), 'Чат без названия');
  assert.equal(titleOf({ title: '<button>  Привет\n </button>' }), '<button> Привет </button>');
  assert.ok(titleOf({ title: 'a'.repeat(500) }).length <= 160);
  assert.equal(titleOf(), 'Чат без названия');
});

test('cached host context never becomes a title and the following real request is retained', () => {
  for (const tag of ['environment_context', 'recommended_plugins', 'permissions instructions', 'user_instructions',
    'system-reminder', 'task-notification', 'developer_instructions', 'app-context', 'skills_instructions',
    'collaboration_mode', 'local-command-stdout', 'local-command-caveat', 'turn_aborted']) {
    assert.equal(titleOf({ title: `<${tag}>cached truncated text`, project: 'Jarvis' }), 'Чат без названия', tag);
    assert.equal(titleOf({ title: `<${tag}>generated text</${tag}>\nПочини меню` }), 'Почини меню', tag);
  }
  assert.equal(titleOf({ title: '<recommended_plugins>long cache', lastPrompt: '<environment_context>cwd</environment_context>\nОбнови интерфейс' }), 'Обнови интерфейс');
  assert.equal(titleOf({ title: '# AGENTS.md instructions for /repo\n<INSTRUCTIONS>generated</INSTRUCTIONS>\nПроверь поиск' }), 'Проверь поиск');
  assert.equal(titleOf({ title: '# AGENTS.md instructions for /repo truncated', project: 'repo' }), 'Чат без названия');
  assert.equal(titleOf({ title: '>>> APPROVAL REQUEST START\n{}\n>>> APPROVAL REQUEST END\nПроверь тесты' }), 'Проверь тесты');
  assert.equal(titleOf({ title: '```xml\n<recommended_plugins>sample</recommended_plugins>\n```' }), '```xml <recommended_plugins>sample</recommended_plugins> ```');
  assert.equal(titleOf({ title: 'Explain <recommended_plugins> in this example' }), 'Explain <recommended_plugins> in this example');
  assert.equal(titleOf({ title: '<in-app-browser-context source="ambient-ui-state">This bloc' }), 'Чат без названия');
  assert.equal(titleOf({ title: '<in-app-browser-context source="ambient-ui-state">ambient</in-app-browser-context>\nПроверь кнопку' }), 'Проверь кнопку');
  assert.equal(titleOf({ title: '<in-app-browser-context-example>real XML</in-app-browser-context-example>' }), '<in-app-browser-context-example>real XML</in-app-browser-context-example>');
});

test('attachment title prefixes expose the actual request or an unnamed fallback', () => {
  const attachment = '# Files mentioned by the user:\n- /private/report.txt\n\n## My request:\nПроверь отчёт\nи найди ошибки';
  assert.equal(titleOf({ title: attachment }), 'Проверь отчёт и найди ошибки');
  assert.equal(titleOf({ title: '# Files mentioned by the user: /private/report', lastPrompt: attachment }), 'Проверь отчёт и найди ошибки');
  assert.equal(titleOf({ title: '# Files mentioned by the user: /private/report', project: 'Jarvis' }), 'Чат без названия');
  assert.equal(titleOf({ title: 'Explain the heading ## My request: in this format' }), 'Explain the heading ## My request: in this format');
});


test('chat capability states never confuse a terminal, an observer and a disconnected machine', () => {
  const online = [{ id: 'vm', online: true }];
  const managed = session('managed', { tmuxPane: '%1', status: 'idle' });
  assert.equal(capabilities(managed, online).canConfigure, true);
  assert.equal(capabilities({ ...managed, status: 'working' }, online).canConfigure, false);
  assert.equal(capabilities({ ...managed, status: 'working' }, online).canSend, true);
  assert.equal(capabilities({ ...managed, status: 'waiting' }, online).canConfigure, false);
  for (const remote of [null, 'vm']) {
    const observer = capabilities({ ...managed, remote, controlMode: 'external' }, online);
    assert.equal(observer.state, 'readonly'); assert.equal(observer.readOnly, true);
    assert.equal(observer.canSend, false); assert.equal(observer.canConfigure, false); assert.equal(observer.canTerminal, false);
  }
  const disconnected = capabilities({ ...managed, remote: 'vm' }, []);
  assert.equal(disconnected.state, 'disconnected'); assert.equal(disconnected.readOnly, false); assert.equal(disconnected.canSend, false);
  const missingPane = capabilities({ ...managed, tmuxPane: null, controlMode: 'tmux' }, online);
  assert.equal(missingPane.state, 'unavailable'); assert.equal(missingPane.readOnly, true);
  assert.equal(capabilities({ ...managed, remote: 'vm', controlMode: 'external' }, []).label, 'Нет связи · история чата');
  assert.equal(capabilities({ ...managed, agent: 'custom' }, online).canConfigure, false);
});

test('friendly model labels resolve to exact slugs without inventing a matching model', () => {
  assert.equal(selectedModel([['sonnet', 'Sonnet'], ['opus', 'Opus']], 'Sonnet'), 'sonnet');
  assert.equal(selectedModel([['gpt-5.5', 'GPT-5.5'], ['gpt-5.6-sol', 'GPT-5.6 Sol']], 'gpt-5.6-sol'), 'gpt-5.6-sol');
  assert.equal(selectedModel([['gpt-5.5', 'GPT-5.5'], ['gpt-5.6-sol', 'GPT-5.6 Sol']], 'GPT-5'), 'GPT-5');
});

test('API error model markers are never treated as selectable models', () => {
  const options = [['sonnet', 'Sonnet'], ['opus', 'Opus']];
  for (const value of ['<synthetic>', 'synthetic', '<unknown>', 'bad\nmodel', null]) {
    assert.equal(selectedModel(options, value), '');
  }
  assert.equal(selectedModel(options, 'my-org/custom-model-v2'), 'my-org/custom-model-v2');
});
