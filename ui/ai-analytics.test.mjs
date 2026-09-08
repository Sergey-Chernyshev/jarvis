import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';
const source = readFileSync(new URL('./ai-analytics.js', import.meta.url), 'utf8');
const tick = () => new Promise(resolve => setImmediate(resolve));
const session = (id = 'codex:s1') => ({
  id, providerSessionId: id.split(':')[1], agent: 'codex', cwd: '/repo',
  models: [{ model: 'test-model', requests: 2, inputTokens: 100, outputTokens: 30, cacheReadTokens: 0 }],
  prompts: { count: 1, signals: [{ id: 'paths', label: 'Файлы и пути', observed: 0, total: 1, pct: 0 }] },
  tools: { calls: 1, success: 0, errors: 1, unknown: 0, repeatedCalls: 0 },
  timing: { activeMs: 1000, wallMs: 2000, toolP50Ms: null, toolP95Ms: null },
  harness: { score: 0, dimensions: [{ id: 'tool', label: 'Надёжность инструментов', score: 0, observed: 0, total: 1, explanation: 'Одна ошибка' }], issues: [{ code: 'tool-error', severity: 'high', message: 'Инструмент завершился ошибкой', evidence: { tool: '<img src=x onerror=alert(1)>' } }] },
});
const report = (overrides = {}) => ({
  schemaVersion: 1, generatedAt: '2026-09-05T12:00:00Z', period: 'week',
  coverage: { filesScanned: 1, sessions: 1, errors: [], limited: false },
  summary: { sessions: 1, prompts: 1, toolCalls: 1, toolErrors: 1, toolUnknown: 0, activeMs: 1000, wallMs: 2000, harnessScore: 0 },
  sessions: [session()], projects: [{ path: '/repo', name: 'repo', sessions: 1, git: { ok: true, branch: 'main', changedFiles: 1, addedLines: 2, removedLines: 0, attribution: { aiPct: 50, unknownPct: 50, humanPct: null, aiMatchedLines: 1, unknownLines: 1 }, gitAi: { available: false } } }],
  models: [session().models[0]], outcomes: [], economics: { recorded: 0, complete: 0, accepted: 0, groups: [], comparisons: [] },
  ...overrides,
});
async function boot(overrides = {}) {
  const { window, document } = parseHTML('<html><body><div id="host"></div></body></html>');
  const calls = [];
  const api = { analyticsReport: async () => report(), analyticsSaveOutcome: async () => ({ ok: true }), ...overrides };
  window.jarvis = Object.fromEntries(Object.entries(api).filter(([, value]) => value).map(([key, fn]) => [key, (...args) => { calls.push([key, ...args]); return fn(...args); }]));
  new Function('window', 'document', source)(window, document);
  const host = document.getElementById('host');
  await window.jarvisAiAnalytics.render(host);
  const clickText = label => { const b = [...host.querySelectorAll('button')].find(b => b.textContent === label); assert.ok(b, `button ${label}`); b.click(); return b; };
  const input = (name, value) => { const input = host.querySelector(`[name="${name}"]`); assert.ok(input); if (input.tagName === 'SELECT') { [...input.options].forEach(o => { o.selected = false; }); const selected = [...input.options].find(o => o.value === value); if (selected) selected.selected = true; } else input.value = value; input.dispatchEvent(new window.Event('change', { bubbles: true })); return input; };
  const submit = () => host.querySelector('.ai-outcome-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  return { window, document, host, calls, clickText, input, submit };
}
test('score 0 and unknown authorship remain distinct; trace evidence is text', async () => {
  const { host } = await boot();
  assert.equal(host.querySelector('.ai-score')?.textContent, '0');
  assert.match(host.textContent, /Подтверждённый ручной кодНеизвестно/);
  assert.match(host.textContent, /50%/);
  assert.match(host.textContent, /Активность по событиям/);
  assert.equal(host.querySelector('img'), null);
  assert.match(host.textContent, /<img src=x onerror=alert\(1\)>/);
});
test('account labels and remote project provenance stay visible for identical provider session ids', async () => {
  const personal = { ...session('source:personal:same'), providerSessionId: 'same', sourceLabel: 'Codex Personal', instanceId: 'personal' };
  const work = { ...session('source:remote:vm:work:same'), providerSessionId: 'same', sourceLabel: 'VM · Codex Work', instanceId: 'work', machine: 'vm', project: 'remote://vm/repo' };
  const { host } = await boot({ analyticsReport: async () => report({ sessions: [personal, work],
    coverage: { filesScanned: 2, sessions: 2, roots: [{ label: 'Codex Personal', path: '/personal' }, { label: 'Codex Work', machine: 'vm', path: '/work' }], remote: { nodes: [{ machine: 'vm', connected: false }], caveat: 'Удалённый Git не проверен' } },
    projects: [{ path: 'remote://vm/repo', name: 'vm · repo', sessions: 1, git: { ok: false, status: 'not-inspected', error: 'Удалённый Git не проверен' } }] }) });
  assert.match(host.textContent, /Codex Personal/);
  assert.match(host.textContent, /VM · Codex Work/);
  assert.match(host.textContent, /vm: недоступен/);
  assert.match(host.textContent, /Удалённый Git не проверен/);
});
test('missing bridge and invalid report show errors, never a successful zero report', async () => {
  for (const overrides of [{ analyticsReport: null }, { analyticsReport: async () => ({ ok: true }) }]) {
    const { host } = await boot(overrides);
    assert.match(host.querySelector('[role="alert"]').textContent, /Не удалось/);
    assert.match(host.textContent, /Отчёт недоступен/);
    assert.equal(host.querySelector('.ai-score'), null);
  }
});
test('empty report explains collection and disables recording without a session', async () => {
  const { host } = await boot({ analyticsReport: async () => report({ sessions: [], projects: [], models: [], summary: { sessions: 0, harnessScore: null } }) });
  assert.match(host.textContent, /В этом периоде нет сессий/);
  assert.equal(host.querySelector('.ai-score').textContent, '—');
  assert.equal(host.querySelector('.ai-outcome-form button[type="submit"]').disabled, true);
});
test('period and project filters reach bridge; failed refresh does not masquerade as old data', async () => {
  let fail = false;
  const { host, calls, clickText } = await boot({ analyticsReport: async () => { if (fail) throw new Error('unreadable trace'); return report(); } });
  clickText('30 дней'); await tick();
  assert.equal(calls.at(-1)[1].period, 'month');
  fail = true; clickText('Обновить'); await tick();
  assert.equal(calls.at(-1)[1].refresh, true);
  assert.match(host.textContent, /unreadable trace/);
  assert.equal(host.querySelector('.ai-score'), null);
});
test('save preserves nulls and real zero; rejection retains editable draft and never claims success', async () => {
  const { host, input, submit, calls } = await boot({ analyticsSaveOutcome: async () => ({ ok: false, error: 'disk full' }) });
  input('sessionId', 'codex:s1'); input('taskType', 'bugfix'); input('harnessVersion', 'abc123'); input('reviewMinutes', '0');
  submit(); await tick();
  const saved = calls.find(([name]) => name === 'analyticsSaveOutcome'); assert.ok(saved);
  assert.equal(saved[1].reviewMinutes, 0); assert.equal(saved[1].aiMinutes, null); assert.equal(saved[1].baselineMinutes, null);
  assert.equal(saved[1].model, 'test-model');
  assert.equal(host.querySelector('[name="taskType"]').value, 'bugfix');
  assert.match(host.querySelector('.ai-form-status').textContent, /Не сохранено: disk full/);
  assert.equal(host.querySelector('.ai-outcome-form button[type="submit"]').disabled, false);
  assert.doesNotMatch(host.textContent, /Результат сохранён/);
});
test('acknowledged save refreshes economics once and validation stops negative values', async () => {
  const { host, input, submit, calls } = await boot();
  input('sessionId', 'codex:s1'); input('taskType', 'bugfix'); input('harnessVersion', 'abc123'); input('aiMinutes', '-1'); submit(); await tick();
  assert.equal(calls.filter(([name]) => name === 'analyticsSaveOutcome').length, 0);
  input('aiMinutes', '15'); submit(); submit(); await tick();
  assert.equal(calls.filter(([name]) => name === 'analyticsSaveOutcome').length, 1);
  assert.equal(calls.filter(([name]) => name === 'analyticsReport').length, 2);
  assert.match(host.textContent, /Результат сохранён/);
});
test('out-of-order report cannot overwrite newer filter results', async () => {
  let release;
  const { host, clickText } = await boot({ analyticsReport: ({ period }) => period === 'today' ? new Promise(resolve => { release = () => resolve(report({ summary: { sessions: 99, harnessScore: 99 } })); }) : Promise.resolve(report()) });
  clickText('Сегодня'); clickText('30 дней'); await tick(); release(); await tick();
  assert.equal(host.querySelector('.ai-score').textContent, '0');
});
test('save acknowledgement survives a report refresh failure and project is preserved', async () => {
  let reads = 0;
  const { host, input, submit, calls } = await boot({ analyticsReport: async () => { if (++reads > 1) throw new Error('refresh failed'); return report(); } });
  input('sessionId', 'codex:s1'); input('taskType', 'bugfix'); input('harnessVersion', 'abc123'); submit(); await tick();
  assert.equal(calls.find(([name]) => name === 'analyticsSaveOutcome')[1].project, '/repo');
  assert.match(host.textContent, /Результат сохранён. Обновление отчёта не удалось/);
  assert.doesNotMatch(host.textContent, /Метрики обновлены/);
});
test('editing existing outcome sends its ID rather than creating a new observation', async () => {
  const original = { id: 'outcome-1', sessionId: 'codex:s1', taskType: 'bugfix', model: 'test-model', harnessVersion: 'v1', outcome: 'rework', baselineSource: 'measured', currency: 'RUB', aiMinutes: 30 };
  const { host, calls, clickText, input, submit } = await boot({ analyticsReport: async () => report({ outcomes: [original] }) });
  clickText('Изменить'); input('outcome', 'accepted'); submit(); await tick();
  assert.ok(calls.find(([name]) => name === 'analyticsSaveOutcome'), host.querySelector('.ai-form-status').textContent);
  const saved = calls.find(([name]) => name === 'analyticsSaveOutcome')[1];
  assert.equal(saved.id, original.id); assert.equal(saved.currency, 'RUB'); assert.equal(saved.aiMinutes, 30); assert.equal(saved.outcome, 'accepted');
});
test('partial repository and unknown model usage expose coverage instead of looking complete', async () => {
  const data = report(); data.projects[0].git.limited = true; data.projects[0].git.excludedFiles = 3;
  data.models = [{ model: 'partial', requests: 4, tokenRecords: 2, missingUsageRequests: 2, inputTokens: 100, outputTokens: 0 }, { model: 'missing', requests: 1, tokenRecords: 0, missingUsageRequests: 1, inputTokens: null, outputTokens: null }];
  const { host } = await boot({ analyticsReport: async () => data });
  assert.match(host.querySelector('.ai-disclosure-meta').textContent, /неполное чтение/);
  assert.match(host.querySelector('.ai-repository-partial').textContent, /только к прочитанным исходникам/);
  assert.match(host.textContent, /Исключённых файлов3/);
  assert.match(host.textContent, /Частично · записей usage: 2 · запросов без usage: 2/);
  assert.match(host.textContent, /Учёт токенов неполный/);
  const modelTable = host.querySelector('[aria-label="Использование моделей"]');
  assert.match(modelTable.textContent, /missing1Нет данныхНет данных/);
  assert.match(host.textContent, /статус неизвестен:/);
});
test('choosing a multi-model or unknown session clears previous model autofill', async () => {
  const one = session(), multi = { ...session('codex:multi'), models: [{ model: 'a' }, { model: 'b' }] }, unknown = { ...session('codex:unknown'), models: [{ model: 'unknown' }] };
  const { host, input } = await boot({ analyticsReport: async () => report({ sessions: [one, multi, unknown] }) });
  input('sessionId', one.id); assert.equal(host.querySelector('[name="model"]').value, 'test-model');
  input('sessionId', multi.id); assert.equal(host.querySelector('[name="model"]').value, '');
  input('sessionId', one.id); input('sessionId', unknown.id); assert.equal(host.querySelector('[name="model"]').value, '');
});
test('context shows explicit compaction separately from counter resets with unknown window', async () => {
  const data = session(); data.context = { compactionEvents: 0, tokenCounterResets: 2, windowSamples: 0, peakInputWindowPct: null, lastWindowTokens: null, lastInputTokens: null };
  const { host } = await boot({ analyticsReport: async () => report({ sessions: [data] }) });
  assert.match(host.textContent, /Явные события сжатия0/);
  assert.match(host.textContent, /Сбросы счётчика токенов2/);
  assert.match(host.textContent, /Пиковое заполнение входомНеизвестно/);
});
test('JSON export copies current report and surfaces clipboard failure without losing analytics', async () => {
  let fail = false;
  const { host, calls, clickText } = await boot({ copyText: async () => { if (fail) throw new Error('clipboard denied'); } });
  clickText('Копировать JSON'); await tick();
  const copied = JSON.parse(calls.find(([name]) => name === 'copyText')[1]);
  assert.equal(copied.schemaVersion, 1); assert.equal(copied.sessions[0].id, 'codex:s1');
  assert.match(host.textContent, /JSON отчёта скопирован/);
  fail = true; clickText('Копировать JSON'); await tick();
  assert.match(host.querySelector('[role="alert"]').textContent, /clipboard denied/);
  assert.equal(host.querySelector('.ai-score').textContent, '0');
  const failed = await boot({ analyticsReport: async () => { throw new Error('no report'); } });
  assert.equal([...failed.host.querySelectorAll('button')].find(b => b.textContent === 'Копировать JSON').disabled, true);
});
const configFixture = () => ({ version: 1, autoDiscover: true, sources: [{ id: 'local-extra', format: 'codex', path: '~/old-traces', enabled: true }], limits: { maxFiles: 200, maxScanMiB: 256, maxFileMiB: 32, maxLines: 200000, maxProjects: 20 }, rules: { idleCapMinutes: 5, contextWarningPct: 85, minModelSamples: 5, harnessWeights: { toolReliability: 1, resultObservability: 1, verificationAfterEdit: 1 }, toolAliases: {} }, git: { sourceExtensions: ['rs', 'js'], excludeDirectories: ['.git', 'vendor'], excludeSuffixes: ['.lock'], excludeNameFragments: ['.min.'], minMatchChars: 12 } });
const configSubmit = ({ host, window }) => host.querySelector('.ai-config-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
const setConfigInput = ({ host, window }, selector, value) => { const control = host.querySelector(selector); assert.ok(control, selector); control.value = value; control.dispatchEvent(new window.Event('input', { bubbles: true })); return control; };
test('config source form loads lazily and saves portable source paths plus untouched advanced settings', async () => {
  const setup = await boot({ analyticsConfig: async () => configFixture(), analyticsSaveConfig: async config => ({ ok: true, config }) });
  assert.equal(setup.calls.some(([name]) => name === 'analyticsConfig'), false);
  setup.clickText('Настройки аналитики'); await tick();
  setup.clickText('Удалить источник'); setup.clickText('Добавить источник');
  setConfigInput(setup, '[data-config-source] [data-field="path"]', '~/custom-traces');
  setConfigInput(setup, '[name="config-idleCapMinutes"]', '7.5');
  setup.host.querySelector('[name="config-autoDiscover"]').checked = false;
  configSubmit(setup); await tick();
  const saved = setup.calls.find(([name]) => name === 'analyticsSaveConfig')[1];
  assert.equal(saved.autoDiscover, false); assert.equal(saved.sources.length, 1);
  assert.deepEqual(saved.sources[0], { id: 'source-1', path: '~/custom-traces', format: 'normalized', enabled: true });
  assert.equal(saved.rules.idleCapMinutes, 7.5); assert.deepEqual(saved.git, configFixture().git);
  assert.match(setup.host.querySelector('.ai-config-form').textContent, /Настройки сохранены/);
});
test('config save rejection keeps source edits and lets user retry', async () => {
  const setup = await boot({ analyticsConfig: async () => configFixture(), analyticsSaveConfig: async () => ({ ok: false, error: 'source path blocked' }) });
  setup.clickText('Настройки аналитики'); await tick();
  setConfigInput(setup, '[data-config-source] [data-field="path"]', '/my/traces'); configSubmit(setup); await tick();
  assert.equal(setup.host.querySelector('[data-config-source] [data-field="path"]').value, '/my/traces');
  assert.match(setup.host.querySelector('.ai-config-form [role="alert"]').textContent, /source path blocked/);
  assert.equal(setup.host.querySelector('.ai-config-form button[type="submit"]').disabled, false);
  assert.equal(setup.calls.filter(([name]) => name === 'analyticsReport').length, 1);
});
test('advanced JSON errors keep draft; valid advanced values are saved without stale simple fields overwriting them', async () => {
  const setup = await boot({ analyticsConfig: async () => configFixture(), analyticsSaveConfig: async config => ({ ok: true, config }) });
  setup.clickText('Настройки аналитики'); await tick();
  setConfigInput(setup, '.ai-config-json', '{ broken'); configSubmit(setup); await tick();
  assert.equal(setup.calls.filter(([name]) => name === 'analyticsSaveConfig').length, 0);
  assert.equal(setup.host.querySelector('.ai-config-json').value, '{ broken');
  assert.equal(setup.host.querySelector('.ai-config-simple').disabled, true);
  const advanced = configFixture(); advanced.limits.maxFiles = 500; advanced.rules.harnessWeights.toolReliability = 2;
  setConfigInput(setup, '.ai-config-json', JSON.stringify(advanced)); configSubmit(setup); await tick();
  const saved = setup.calls.find(([name]) => name === 'analyticsSaveConfig')[1];
  assert.equal(saved.limits.maxFiles, 500); assert.equal(saved.rules.harnessWeights.toolReliability, 2);
});
test('loading config defaults is an unsaved draft; export includes that draft', async () => {
  const defaults = configFixture(); defaults.sources = [];
  const setup = await boot({ analyticsConfig: async () => configFixture(), analyticsDefaults: async () => defaults, copyText: async () => {} });
  setup.clickText('Настройки аналитики'); await tick(); setup.clickText('Загрузить значения по умолчанию'); await tick();
  assert.equal(setup.host.querySelectorAll('[data-config-source]').length, 0);
  assert.equal(setup.calls.some(([name]) => name === 'analyticsSaveConfig'), false);
  assert.match(setup.host.querySelector('.ai-config-form').textContent, /Нажми «Сохранить настройки», чтобы применить/);
  setup.clickText('Копировать настройки'); await tick();
  assert.deepEqual(JSON.parse(setup.calls.find(([name]) => name === 'copyText')[1]).sources, []);
});
test('manual project path queries a repository outside discovered sources and rejects relative paths', async () => {
  const setup = await boot(); const form = setup.host.querySelector('.ai-manual-project');
  setConfigInput(setup, '.ai-manual-project input', 'relative/path'); form.dispatchEvent(new setup.window.Event('submit', { bubbles: true, cancelable: true })); await tick();
  assert.equal(setup.calls.filter(([name]) => name === 'analyticsReport').length, 1);
  setConfigInput(setup, '.ai-manual-project input', '~/other/project'); form.dispatchEvent(new setup.window.Event('submit', { bubbles: true, cancelable: true })); await tick();
  assert.equal(setup.calls.filter(([name]) => name === 'analyticsReport').at(-1)[1].project, '~/other/project');
});
test('outcomes accept a custom three-letter currency and uppercase it', async () => {
  const setup = await boot(); setup.input('sessionId', 'codex:s1'); setup.input('taskType', 'bugfix'); setup.input('harnessVersion', 'v1'); setup.input('currency', 'gbp'); setup.submit(); await tick();
  assert.equal(setup.calls.find(([name]) => name === 'analyticsSaveOutcome')[1].currency, 'GBP');
});
test('unreadable config can explicitly load default draft without overwriting storage', async () => {
  const setup = await boot({ analyticsConfig: async () => { throw new Error('corrupt config'); }, analyticsDefaults: async () => configFixture() });
  setup.clickText('Настройки аналитики'); await tick();
  assert.match(setup.host.textContent, /corrupt config/); assert.equal(setup.host.querySelector('.ai-config-form'), null);
  setup.clickText('Загрузить значения по умолчанию'); await tick();
  assert.ok(setup.host.querySelector('.ai-config-form'));
  assert.match(setup.host.querySelector('.ai-config-form').textContent, /чтобы заменить повреждённую конфигурацию/);
  assert.equal(setup.calls.some(([name]) => name === 'analyticsSaveConfig'), false);
});
test('new outcomes use configurable currency and keep zero hourly rate distinct from unknown', async () => {
  for (const [economics, currency, rate] of [[undefined, 'USD', ''], [{ currency: 'JPY', hourlyRate: 0 }, 'JPY', '0'], [{ currency: 'JPY', hourlyRate: null }, 'JPY', '']]) {
    const { host } = await boot({ analyticsReport: async () => report({ config: { economics } }) });
    assert.equal(host.querySelector('.ai-outcome-form [name="currency"]').value, currency);
    assert.equal(host.querySelector('.ai-outcome-form [name="hourlyRate"]').value, rate);
  }
});
test('economic preferences do not change existing outcome currency or hourly rate', async () => {
  const original = { id: 'saved-task', sessionId: 'codex:s1', taskType: 'fix', model: 'test-model', harnessVersion: 'v1', outcome: 'accepted', currency: 'GBP', hourlyRate: null };
  const setup = await boot({ analyticsReport: async () => report({ config: { economics: { currency: 'JPY', hourlyRate: 50 } }, outcomes: [original] }) });
  setup.clickText('Изменить');
  assert.equal(setup.host.querySelector('.ai-outcome-form [name="currency"]').value, 'GBP');
  assert.equal(setup.host.querySelector('.ai-outcome-form [name="hourlyRate"]').value, '');
});
test('config economic preferences save custom currency and preserve a zero hourly rate', async () => {
  const setup = await boot({ analyticsConfig: async () => configFixture(), analyticsSaveConfig: async config => ({ ok: true, config }) });
  setup.clickText('Настройки аналитики'); await tick();
  setConfigInput(setup, '[name="config-currency"]', 'jpy'); setConfigInput(setup, '[name="config-hourlyRate"]', '0'); configSubmit(setup); await tick();
  assert.deepEqual(setup.calls.find(([name]) => name === 'analyticsSaveConfig')[1].economics, { currency: 'JPY', hourlyRate: 0 });
});
