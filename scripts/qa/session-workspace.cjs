// Real Chromium renders production UI scripts against a strict synthetic bridge.
// This harness never reaches native IPC, SSH, real terminals, or real agents.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const os = require('node:os');
let playwright;
try { playwright = require('playwright'); }
catch { playwright = require(process.env.JARVIS_PLAYWRIGHT_PATH || path.join(os.homedir(), '.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright')); }

const root = path.resolve(__dirname, '../../ui');
const out = path.resolve(__dirname, '../../docs/qa/assets/session-workspace');
const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.woff2': 'font/woff2' };
const server = http.createServer((req, res) => {
  const file = path.resolve(root, '.' + new URL(req.url, 'http://localhost').pathname);
  if (!file.startsWith(root + path.sep)) { res.writeHead(403).end(); return; }
  fs.readFile(file, (error, data) => {
    if (error) res.writeHead(404).end();
    else res.writeHead(200, { 'Content-Type': mime[path.extname(file)] || 'application/octet-stream' }).end(data);
  });
});

function bridgeFixture() {
  const events = {}, calls = [], unknown = [], fail = {}, pendingSnapshots = [], pendingLaunchReplies = [];
  const now = Date.now();
  let saved = { theme: 'dark', mode: 'window', paint: 'clover', diagnostics: false };
  let sessions = [
    { id: 'local-chat', title: 'Собрать интерфейс проектов', project: 'Jarvis', cwd: '/work/jarvis', agent: 'claude', model: 'Sonnet', effort: 'high', status: 'idle', tmuxPane: '%1', branch: 'codex/chat-ui', createdAt: now - 300000, updatedAt: now - 60000, pinned: true },
    { id: 'build-box:remote-chat', title: 'Проверить подключение по SSH', project: 'Jarvis', cwd: '/work/jarvis', remote: 'build-box', agent: 'codex', instanceId: 'vm-personal', instanceLabel: 'VM Personal', model: 'GPT-5', effort: 'high', status: 'working', detail: '▸ cargo test', tmuxPane: '%2', branch: 'codex/remote', providerTurnId: 'turn-remote-1', createdAt: now - 240000, updatedAt: now - 30000 },
    { id: 'build-box:question', title: 'Добавить проверку перед запуском', project: 'Deploy tools', cwd: '/srv/deploy-tools', remote: 'build-box', agent: 'claude', model: 'Opus', effort: 'high', status: 'waiting', tmuxPane: '%3', question: { at: now - 10000, text: 'Запустить проверки перед сборкой?', options: ['Да', 'Нет'] }, createdAt: now - 180000, updatedAt: now - 10000 },
    { id: 'local-complete', title: 'Уточнить расходы на токены', project: 'Jarvis', cwd: '/work/jarvis', agent: 'codex', instanceId: 'personal', instanceLabel: 'Personal', model: 'GPT-5', effort: 'medium', status: 'done', tmuxPane: '%4', doneAt: now - 90000, createdAt: now - 360000, updatedAt: now - 90000 },
  ];
  const sessionUsage = {
    tok: 82400, cost: 0.38, billing: 'plan', model: 'Sonnet', inputTokens: 12800, outputTokens: 7200,
    cacheReadTokens: 62400, cacheWriteTokens: 0, reasoningTokens: 1600, cacheHitPct: 83, requests: 12,
    firstAt: now - 1800000, lastAt: now, costEstimated: true, costBasis: 'model-family-estimate', source: 'local-transcripts',
  };
  const usage = { total: { tok: 82400, api: 0, plan: 0.38, n: 12 }, window: { resetInMs: 0 }, series: [], byModel: [], byProject: [], sessions: [], byBilling: [] };
  const transcript = id => [
    { role: 'user', text: id === 'local-chat' ? 'Давай объединим мои локальные и удалённые сессии в чате с проектами.' : 'Проверь удалённое подключение и обработку повторных событий.', ts: now - 120000 },
    { role: 'assistant', text: 'Посмотрю, как устроены сессии, и сохраню их привязку к проекту и машине. Для терминала добавлю отдельную панель.', ts: now - 110000 },
    { role: 'assistant', kind: 'tool', text: 'Read · src/remote.rs', ts: now - 100000 },
    { role: 'assistant', kind: 'tool', text: 'Bash · cargo test', ts: now - 90000 },
    { role: 'assistant', text: 'Готово. Чаты сгруппированы по проектам, а локальная машина и **build-box** отображаются отдельно.\n\n- Черновик сохраняется при переключении чатов.\n- Повторные события не создают новые уведомления.\n- Расход токенов доступен в деталях.', ts: now - 80000 },
  ];
  const snapshot = id => ({ ok: true, text: `$ test-session ${id}\n✓ connected\n3 tests passed\n`, capturedAt: Date.now(), connection: { name: id.startsWith('build-box:') ? 'build-box' : 'Этот компьютер', pane: id.startsWith('build-box:') ? '%2' : '%1', canInput: true } });
  const fixture = {
    getSettings: () => saved, setSettings: patch => (saved = { ...saved, ...patch }, { ok: true }),
    getState: () => sessions, getMeta: () => ({ version: 'QA fixture', effortLevels: ['low', 'medium', 'high', 'xhigh'] }),
    getPlugins: () => [], getModels: () => [], getAgents: () => [], getCommands: () => [], getPrompts: () => [], getLimit: () => null,
    getUsage: () => usage, getSessionUsage: id => ({ ...sessionUsage, instanceLabel: sessions.find(session => session.id === id)?.instanceLabel }),
    getHistory: machine => [{ project: machine === 'build-box' ? 'Remote project' : 'Jarvis', cwd: machine === 'build-box' ? '/srv/remote-project' : '/work/jarvis', count: 1, lastAt: now, sessions: [] }],
    projectsIconCandidates: () => ({ ok: true, candidates: [] }),
    projectsList: machine => ({ ok: true, projects: [{ machine: machine || 'local', project: machine === 'build-box' ? 'Remote project' : 'Jarvis', cwd: machine === 'build-box' ? '/srv/remote-project' : '/work/jarvis', count: 1, lastAt: now, sessions: [] }], warnings: [] }),
    machinesList: () => [
      { id: 'local', name: 'Этот компьютер', kind: 'local', online: true },
      { id: 'build-box', name: 'build-box', kind: 'remote', online: true },
      { id: 'offline-box', name: 'offline-box', kind: 'remote', online: false },
    ],
    agentInstancesList: () => ({
      config: { entries: [], defaultCodexInstance: 'personal' }, defaultCodexInstance: 'personal', health: [],
      instances: ['personal', 'work'].map(id => ({ id, label: id === 'personal' ? 'Personal' : 'Work', agent: 'codex', enabled: true, machine: 'local', home: `/fixture/${id}`, canonicalHome: `/fixture/${id}`, exists: true,
        models: [{ value: 'gpt-6-astra', label: 'GPT-6 Astra' }, { value: 'gpt-5.6-sol', label: 'GPT-5.6 Sol' }] })),
    }),
    remotesList: () => [{ name: 'build-box', connected: true, sources: ['vm-personal', 'vm-work'].map(id => ({ id, label: id === 'vm-personal' ? 'VM Personal' : 'VM Work', agent: 'codex', enabled: true,
      models: [{ value: 'gpt-6-astra', label: 'GPT-6 Astra' }, { value: 'gpt-5.6-sol', label: 'GPT-5.6 Sol' }] })) }],
    agentsList: () => ({ ok: true, agents: [{ id: 'opencode', name: 'OpenCode', bin: 'opencode' }], presets: [] }),
    hotkeyBindings: () => ({ ok: true, bindings: [{ action: 'panel', label: 'Показать Jarvis', accel: 'Command+J' }] }),
    winIsFullscreen: () => false, reportError: () => null, hidePanel: () => null,
    wakeGet: () => ({ enabled: false, model_present: false, audio_state: 'idle' }),
    openChat: id => ({ ok: true, project: sessions.find(s => s.id === id)?.project || 'Jarvis', items: transcript(id), spans: [], cards: {}, llm: false }),
    closeChat: () => ({ ok: true }), launchSession: () => {
      const next = window.__sessionFixture.nextLaunch; window.__sessionFixture.nextLaunch = null;
      if (!next) return { ok: true, channel: 'node', machine: 'build-box' };
      if (next.hold) return new Promise(resolve => pendingLaunchReplies.push(() => resolve(next.result)));
      return next.result;
    },
    terminalAction: async (id, action, payload = {}) => {
      if (action === 'open') {
        const result = { ok: true, streamId: 'stream:' + id, cursor: 0, cols: 100, rows: 24,
          initial: Array.from(new TextEncoder().encode(`$ test-session ${id}\r\n✓ connected\r\n3 tests passed\r\n`)) };
        if (window.__sessionFixture.delaySnapshot === id) {
          window.__sessionFixture.delaySnapshot = null;
          return new Promise(resolve => pendingSnapshots.push(() => resolve(result)));
        }
        return result;
      }
      if (action === 'poll') { await new Promise(resolve => setTimeout(resolve, 250)); return { ok: true, cursor: payload.cursor || 0, chunks: [] }; }
      if (action === 'resize') return { ok: true, cols: payload.cols, rows: payload.rows };
      return { ok: true };
    },
    terminalSnapshot: id => snapshot(id),
    fileDialogState: () => ({ ok: true }),
    copyText: () => ({ ok: true }),
    terminalKey: () => ({ ok: true }), focusTerminal: () => ({ ok: true }),
    setModel: () => ({ ok: true }), setEffort: () => ({ ok: true }), sendReply: () => ({ ok: true }),
  };
  window.__sessionFixture = {
    events, calls, unknown, fail, pendingSnapshots, pendingLaunchReplies, delaySnapshot: null, nextLaunch: null,
    sessions: () => structuredClone(sessions),
    emitState(next) { sessions = structuredClone(next); for (const cb of events.onState || []) cb(structuredClone(sessions)); },
    emitLaunch(event) { for (const cb of events.onLaunchTask || []) cb(structuredClone(event)); },
    resolveSnapshots() { while (pendingSnapshots.length) pendingSnapshots.shift()(); },
    resolveLaunchReplies() { while (pendingLaunchReplies.length) pendingLaunchReplies.shift()(); },
  };
  window.jarvis = new Proxy({}, { get(_, name) {
    if (name.startsWith('on')) return callback => { (events[name] ||= []).push(callback); return () => {}; };
    return async (...args) => {
      calls.push({ name, args });
      if (fail[name]) { const error = fail[name]; delete fail[name]; throw new Error(error); }
      if (!(name in fixture)) { unknown.push(name); throw new Error('Unknown fixture method: ' + name); }
      return structuredClone(await fixture[name](...args));
    };
  } });
}

const records = [];
(async () => {
  fs.mkdirSync(out, { recursive: true });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const browser = await playwright.chromium.launch({ channel: 'chrome', headless: true });
  const page = await browser.newPage({ viewport: { width: 1280, height: 850 }, colorScheme: 'dark', reducedMotion: 'reduce' });
  const errors = []; page.on('pageerror', error => errors.push(error.message)); page.setDefaultTimeout(9000);
  const settle = () => page.waitForFunction(() => !document.getAnimations().some(a => a.playState === 'running' && Number.isFinite(a.effect?.getComputedTiming().endTime)));
  const capture = async name => { await settle(); await page.waitForFunction(() => !document.querySelector('.toast')); await page.screenshot({ path: path.join(out, name + '.png') }); records.push(name); };
  const calls = name => page.evaluate(name => window.__sessionFixture.calls.filter(c => c.name === name), name);
  const openChat = async id => {
    if (!await page.locator('#sessionSidebar').isVisible()) await page.getByRole('button', { name: 'Проекты и чаты', exact: true }).click();
    await page.locator(`#sessionSidebar .sw-session[data-session-id="${id}"]`).click();
    await page.waitForFunction(id => document.documentElement.dataset.view === 'chat' && document.querySelector(`#sessionSidebar [data-session-id="${id}"]`)?.getAttribute('aria-current') === 'page', id);
    await settle();
  };
  const assertLayout = async () => {
    const dimensions = await page.evaluate(() => {
      const panel = document.getElementById('panel'), frame = document.getElementById('sessionFrame');
      return { viewport: innerWidth, panel: panel.getBoundingClientRect().width, frame: frame.getBoundingClientRect().width, pageWidth: document.documentElement.scrollWidth };
    });
    assert.ok(dimensions.panel <= dimensions.viewport + 1 && dimensions.frame <= dimensions.viewport + 1 && dimensions.pageWidth <= dimensions.viewport + 1, `Horizontal overflow: ${JSON.stringify(dimensions)}`);
  };
  try {
    await page.route('**/bridge.js', route => route.fulfill({ contentType: 'text/javascript', body: '(' + bridgeFixture.toString() + ')();' }));
    await page.goto(`http://127.0.0.1:${server.address().port}/index.html`);
    await page.waitForFunction(() => document.documentElement.dataset.view === 'home');
    assert.equal(await page.locator('#launcher').isVisible(), true, 'home remains the Jarvis launcher');
    await page.locator('#tabSessions').click();
    await page.locator('#newChatPrompt').waitFor();
    await page.waitForFunction(() => document.querySelectorAll('#newChatMachine option').length === 3);
    assert.equal(await page.locator('#newChatMachine option[value="offline-box"]').isDisabled(), true);

    const groups = await page.locator('#sessionSidebar .sw-project').evaluateAll(nodes => nodes.map(n => ({
      name: n.querySelector('.sw-project-toggle').textContent,
      host: n.querySelector('.sw-project-host').textContent,
      ids: [...n.querySelectorAll('[data-session-id]')].map(s => s.dataset.sessionId),
    })));
    assert.equal(groups.filter(g => g.name === 'Jarvis').length, 2);
    assert.deepEqual(groups.find(g => g.name === 'Jarvis' && g.host === 'Этот компьютер').ids, ['local-chat', 'local-complete']);
    assert.deepEqual(groups.find(g => g.name === 'Jarvis' && g.host === 'build-box').ids, ['build-box:remote-chat']);
    records.push('same-directory-projects-separated-by-machine');
    await capture('welcome-dark-desktop');

    const choose = async (selector, value) => {
      const source = page.locator(selector);
      const label = await source.evaluate((select, selected) => [...select.options].find(option => option.value === selected)?.label, value);
      assert.ok(label, `Missing ${selector} option: ${value}`);
      await source.locator('..').locator('.jselect-trigger').click();
      await page.getByRole('listbox').getByRole('option', { name: label, exact: true }).click();
      assert.equal(await source.inputValue(), value);
    };
    await choose('#newChatMachine', 'build-box');
    await page.waitForFunction(() => [...document.querySelectorAll('#chatProjectPaths option')].some(o => o.value === '/srv/remote-project'));
    await page.locator('#newChatDirectory').fill('/work/jarvis');
    await choose('#newChatProvider', 'codex');
    const permissions = page.getByRole('combobox', { name: 'Разрешения новой задачи', exact: true });
    await permissions.click();
    assert.equal(await page.getByRole('option', { name: 'Планирование', exact: true }).getAttribute('aria-disabled'), 'true', 'unsupported Codex plan mode cannot be selected');
    await permissions.press('Escape');
    await page.waitForFunction(() => [...document.querySelectorAll('#newChatInstance option')].some(option => option.value === 'vm-personal'));
    await choose('#newChatInstance', 'vm-personal');
    await choose('#newChatModel', 'gpt-6-astra');
    await choose('select[aria-label="Разрешения новой задачи"]', 'yolo');
    await page.getByLabel('Отдельная ветка', { exact: true }).check();
    await page.locator('#newChatPrompt').fill('Synthetic QA task: проверить события');
    await page.getByRole('button', { name: 'Начать задачу', exact: true }).click();
    await page.waitForFunction(() => window.__sessionFixture.calls.some(c => c.name === 'launchSession'));
    assert.deepEqual((await calls('launchSession')).at(-1).args, ['/work/jarvis', 'codex', null, 'build-box', { mode: 'yolo', isolate: true, task: 'Synthetic QA task: проверить события', container: false, instanceId: 'vm-personal', model: 'gpt-6-astra' }]);
    assert.equal(await page.locator('#newChatPrompt').inputValue(), '');
    assert.equal(await page.getByRole('button', { name: 'Начать задачу', exact: true }).isDisabled(), true, 'a pending launch cannot be accidentally repeated');
    await page.evaluate(() => {
      const f = window.__sessionFixture; f.launchBase = f.sessions();
      f.emitState([...f.launchBase, { ...f.launchBase[0], id: 'local-unrelated-launch', agent: 'codex', status: 'idle' }]);
    });
    assert.equal(await page.locator('html').getAttribute('data-view'), 'list', 'a same-path launch on the wrong machine must not open');
    await page.evaluate(() => {
      const f = window.__sessionFixture;
      f.emitState([...f.sessions(), { ...f.launchBase[1], id: 'build-box:wrong-profile', instanceId: 'vm-work', instanceLabel: 'VM Work', title: 'Other account task', status: 'idle' }]);
    });
    assert.equal(await page.locator('html').getAttribute('data-view'), 'list', 'a same-path launch from the wrong profile must not open');
    await page.evaluate(() => {
      const f = window.__sessionFixture;
      f.emitState([...f.sessions(), { ...f.launchBase[1], id: 'build-box:launched-chat', title: 'Новая задача проверки', status: 'idle' }]);
    });
    await page.waitForFunction(() => document.querySelector('#sessionSidebar [data-session-id="build-box:launched-chat"]')?.getAttribute('aria-current') === 'page');
    await page.getByRole('button', { name: 'Новый чат', exact: true }).click();
    await page.evaluate(() => { const f = window.__sessionFixture; f.emitState(f.launchBase); });
    records.push('remote-launch-preserves-machine-profile-model-provider-task-and-permissions');

    // A failed launch keeps the draft available for correction/retry.
    await page.evaluate(() => { window.__sessionFixture.fail.launchSession = 'Synthetic SSH connection failure'; });
    await page.locator('#newChatPrompt').fill('Черновик после ошибки подключения');
    await page.getByRole('button', { name: 'Начать задачу', exact: true }).click();
    await page.locator('.sw-launch-status').filter({ hasText: 'Synthetic SSH connection failure' }).waitFor();
    assert.equal(await page.locator('#newChatPrompt').inputValue(), 'Черновик после ошибки подключения');
    records.push('launch-failure-retains-draft');

    // A tmux pane is not confirmation that the initial prompt was delivered.
    await page.evaluate(() => { window.__sessionFixture.nextLaunch = { result: { ok: true, launchId: 'qa-delivery', pane: '%5', cwd: '/work/jarvis' } }; });
    await page.locator('#newChatPrompt').fill('Synthetic delivery acknowledgement task');
    await page.getByRole('button', { name: 'Начать задачу', exact: true }).click();
    await page.waitForFunction(() => document.getElementById('newChatPrompt').value === '');
    await page.evaluate(() => {
      const f = window.__sessionFixture;
      f.emitState([...f.launchBase, { ...f.launchBase[1], id: 'build-box:delivery', tmuxPane: '%5', status: 'idle' }]);
      f.emitLaunch({ launchId: 'unrelated-delivery', sessionId: 'build-box:delivery', status: 'sent' });
    });
    assert.equal(await page.locator('html').getAttribute('data-view'), 'list', 'pane discovery and unrelated delivery events must not complete this launch');
    await page.evaluate(() => window.__sessionFixture.emitLaunch({ launchId: 'qa-delivery', sessionId: 'build-box:delivery', status: 'sent' }));
    await page.waitForFunction(() => document.querySelector('#sessionSidebar [data-session-id="build-box:delivery"]')?.getAttribute('aria-current') === 'page');
    await page.getByRole('button', { name: 'Новый чат', exact: true }).click();
    await page.evaluate(() => { const f = window.__sessionFixture; f.emitState(f.launchBase); });
    records.push('launch-id-waits-for-matching-prompt-delivery');

    // Delivery events can beat the invoke response across IPC transports.
    await page.evaluate(() => { window.__sessionFixture.nextLaunch = { hold: true, result: { ok: true, launchId: 'qa-early-delivery', pane: '%6', cwd: '/work/jarvis' } }; });
    await page.locator('#newChatPrompt').fill('Synthetic early delivery task');
    await page.getByRole('button', { name: 'Начать задачу', exact: true }).click();
    await page.waitForFunction(() => window.__sessionFixture.pendingLaunchReplies.length === 1);
    await page.evaluate(() => {
      const f = window.__sessionFixture;
      f.emitState([...f.launchBase, { ...f.launchBase[1], id: 'build-box:early-delivery', tmuxPane: '%6', status: 'idle' }]);
      f.emitLaunch({ launchId: 'qa-early-delivery', sessionId: 'build-box:early-delivery', status: 'sent' });
    });
    assert.equal(await page.locator('html').getAttribute('data-view'), 'list');
    await page.evaluate(() => window.__sessionFixture.resolveLaunchReplies());
    await page.waitForFunction(() => document.querySelector('#sessionSidebar [data-session-id="build-box:early-delivery"]')?.getAttribute('aria-current') === 'page');
    await page.getByRole('button', { name: 'Новый чат', exact: true }).click();
    await page.evaluate(() => { const f = window.__sessionFixture; f.emitState(f.launchBase); });
    records.push('early-delivery-event-survives-invoke-response-race');

    await page.evaluate(() => { window.__sessionFixture.nextLaunch = { result: { ok: true, launchId: 'qa-failed-delivery', pane: '%7', cwd: '/work/jarvis' } }; });
    await page.locator('#newChatPrompt').fill('Сохранить исходную задачу после отказа доставки');
    await page.getByRole('button', { name: 'Начать задачу', exact: true }).click();
    await page.waitForFunction(() => document.getElementById('newChatPrompt').value === '');
    await page.evaluate(() => {
      const f = window.__sessionFixture;
      f.emitState([...f.launchBase, { ...f.launchBase[1], id: 'build-box:failed-delivery', tmuxPane: '%7', status: 'idle' }]);
    });
    assert.equal(await page.locator('html').getAttribute('data-view'), 'list', 'a discovered pane cannot hide a pending delivery failure');
    await page.evaluate(() => window.__sessionFixture.emitLaunch({ launchId: 'qa-failed-delivery', status: 'failed', error: 'Synthetic initial prompt delivery failed' }));
    await page.locator('.sw-launch-status').filter({ hasText: 'Synthetic initial prompt delivery failed' }).waitFor();
    assert.equal(await page.locator('#newChatPrompt').inputValue(), 'Сохранить исходную задачу после отказа доставки');
    assert.equal(await page.getByRole('button', { name: 'Начать задачу', exact: true }).isDisabled(), false);
    await page.evaluate(() => { const f = window.__sessionFixture; f.emitState(f.launchBase); });
    records.push('failed-prompt-delivery-restores-original-task');

    await openChat('local-chat');
    assert.equal(await page.locator('.sw-terminal').isVisible(), false);
    assert.equal((await calls('terminalAction')).length, 0, 'opening a chat does not start terminal polling');
    await page.locator('#reply').fill('Локальный черновик, который ещё не отправлен');
    await openChat('build-box:remote-chat');
    assert.equal(await page.locator('#reply').inputValue(), '');
    await page.locator('#reply').fill('Удалённый черновик');
    await openChat('local-chat');
    assert.equal(await page.locator('#reply').inputValue(), 'Локальный черновик, который ещё не отправлен');
    assert.equal((await calls('sendReply')).length, 0);
    records.push('per-chat-drafts-survive-switching-without-sending');

    // Details should stay out of the default conversation until explicitly opened.
    await page.waitForFunction(() => window.__sessionFixture.calls.some(c => c.name === 'getSessionUsage'));
    const usageDetails = page.locator('.sw-chat-usage');
    assert.equal(await usageDetails.evaluate(node => node.tagName === 'DETAILS' ? node.open : !node.hidden), false, 'usage details are collapsed by default');
    await capture('chat-dark-desktop');
    await page.locator('.sw-chat-actions > summary').click();
    await usageDetails.locator('summary').click();
    assert.equal(await usageDetails.evaluate(node => node.open), true);
    assert.match(await usageDetails.innerText(), /Кэш 83%/);
    assert.match(await usageDetails.innerText(), /≈ \$0\.38/);
    assert.match(await usageDetails.innerText(), /не списание и не лимит/);
    await capture('usage-details-desktop');
    await usageDetails.locator('summary').click();
    await page.locator('.sw-chat-actions > summary').click();
    records.push('usage-breakdown-opt-in-with-explicit-estimate');

    await page.evaluate(() => {
      const f = window.__sessionFixture; f.externalBase = f.sessions();
      f.emitState([...f.externalBase, { ...f.externalBase[0], id: 'external-desktop', agent: 'codex', instanceId: 'personal', instanceLabel: 'Personal', controlMode: 'external', tmuxPane: '%stale', title: 'External desktop observer', model: 'gpt-6-astra' }]);
    });
    await openChat('external-desktop');
    await page.locator('.sw-capability').filter({ hasText: 'Только просмотр' }).waitFor();
    assert.equal(await page.locator('#chat .chatinput').isVisible(), false);
    assert.equal(await page.locator('#reply').isDisabled(), true);
    assert.equal(await page.getByRole('combobox', { name: 'Модель текущего чата', exact: true }).isVisible(), false);
    await page.waitForFunction(() => window.__sessionFixture.calls.some(call => call.name === 'getSessionUsage' && call.args[0] === 'external-desktop'));
    await page.locator('.sw-chat-actions > summary').click();
    await usageDetails.locator('summary').click();
    assert.match(await usageDetails.innerText(), /Personal/);
    assert.match(await usageDetails.innerText(), /не списание и не лимит/);
    assert.equal((await calls('sendReply')).length, 0);
    await capture('external-readonly-usage-desktop');
    await page.locator('.sw-chat-actions > summary').click();
    await page.evaluate(() => { const f = window.__sessionFixture; f.emitState(f.externalBase); });
    await openChat('local-chat');
    records.push('external-chat-keeps-source-scoped-readonly-usage-without-message-controls');

    await page.evaluate(() => { window.__sessionFixture.delaySnapshot = 'local-chat'; });
    await page.getByRole('button', { name: 'Терминал', exact: true }).click();
    await page.waitForFunction(() => window.__sessionFixture.pendingSnapshots.length === 1);
    await openChat('build-box:remote-chat');
    assert.equal(await page.locator('.sw-terminal').isVisible(), false, 'a different chat starts with its own collapsed terminal');
    await page.getByRole('button', { name: 'Терминал', exact: true }).click();
    await page.locator('.tw-screen').filter({ hasText: 'test-session build-box:remote-chat' }).waitFor();
    await page.evaluate(() => window.__sessionFixture.resolveSnapshots());
    await page.waitForFunction(() => window.__sessionFixture.pendingSnapshots.length === 0);
    assert.doesNotMatch(await page.locator('.tw-screen').innerText(), /STALE SNAPSHOT|test-session local-chat/);
    await page.getByRole('button', { name: 'Включить ввод', exact: true }).click();
    await page.locator('.xterm-helper-textarea').press('Enter');
    await page.waitForFunction(() => window.__sessionFixture.calls.some(c => c.name === 'terminalAction' && c.args[1] === 'input'));
    const inputCall = (await calls('terminalAction')).find(c => c.args[1] === 'input');
    assert.equal(inputCall.args[0], 'build-box:remote-chat');
    assert.deepEqual(inputCall.args[2].data, [13]);
    await capture('terminal-remote-desktop');
    await page.getByRole('button', { name: 'Терминал', exact: true }).click();
    assert.equal(await page.locator('.sw-terminal').isVisible(), false);
    records.push('terminal-opt-in-and-stale-snapshot-does-not-cross-sessions');

    // A real state transition creates one item; polling timestamps do not create more.
    await page.getByRole('button', { name: 'Новый чат', exact: true }).click();
    await page.evaluate(() => {
      const f = window.__sessionFixture, next = f.sessions();
      next.find(s => s.id === 'build-box:remote-chat').status = 'done';
      next.find(s => s.id === 'build-box:remote-chat').doneAt = Date.now();
      for (let i = 0; i < 4; i++) { next.forEach(s => s.updatedAt = Date.now() + i); f.emitState(next); }
    });
    await page.getByRole('button', { name: 'Входящие', exact: true }).click();
    await page.locator('.sw-inbox').waitFor();
    assert.equal(await page.locator('.sw-inbox .sw-session').count(), 2, 'one new completion plus one initial question');
    assert.equal(await page.locator('.sw-inbox .sw-session[data-session-id="build-box:remote-chat"]').count(), 1);
    await capture('attention-inbox-desktop');
    await page.getByRole('button', { name: 'Прочитать всё', exact: true }).click();
    await page.evaluate(() => { const f = window.__sessionFixture; const next = f.sessions(); next.forEach(s => s.updatedAt = Date.now()); f.emitState(next); });
    assert.equal(await page.locator('.sw-inbox .sw-session').count(), 0);
    records.push('attention-deduplicates-snapshots-and-keeps-read-state');

    for (const theme of ['dark', 'light']) {
      await page.setViewportSize({ width: 1280, height: 850 });
      await page.evaluate(theme => window.jarvisTheme.adopt({ theme, mode: 'window' }), theme);
      await page.getByRole('button', { name: 'Новый чат', exact: true }).click();
      await choose('#newChatMachine', 'local');
      await page.waitForFunction(() => [...document.querySelectorAll('#chatProjectPaths option')].some(o => o.value === '/work/jarvis'));
      await choose('#newChatProvider', 'claude');
      await choose('select[aria-label="Разрешения новой задачи"]', 'ask');
      await page.getByLabel('Отдельная ветка', { exact: true }).uncheck();
      await page.locator('#newChatPrompt').fill('');
      await page.locator('#newChatDirectory').fill('/work/jarvis');
      await page.locator('#newChatPrompt').focus();
      await assertLayout();
      await capture(`welcome-${theme}-desktop`);
      await openChat('local-chat');
      if (await page.locator('.sw-terminal').isVisible()) await page.getByRole('button', { name: 'Терминал', exact: true }).click();
      await assertLayout();
      await capture(`chat-${theme}-desktop`);
      await page.setViewportSize({ width: 600, height: 700 });
      await assertLayout();
      await capture(`chat-${theme}-narrow`);
      assert.equal(await page.locator('#sessionSidebar').isVisible(), false);
      await page.getByRole('button', { name: 'Проекты и чаты', exact: true }).click();
      assert.equal(await page.locator('#sessionSidebar').isVisible(), true);
      await page.getByRole('button', { name: 'Новый чат', exact: true }).click();
      if (await page.locator('#sessionSidebar').isVisible()) await page.getByRole('button', { name: 'Проекты и чаты', exact: true }).click();
      await assertLayout();
      await capture(`welcome-${theme}-narrow`);
    }
    const unknown = await page.evaluate(() => window.__sessionFixture.unknown);
    assert.deepEqual(unknown, []); assert.deepEqual(errors, []);
    const report = { ok: true, scope: 'Chromium with strict synthetic bridge; no native IPC, agents, or SSH connections', records, errors, unknown };
    fs.writeFileSync(path.join(out, 'report.json'), JSON.stringify(report, null, 2));
    fs.rmSync(path.join(out, 'failure.png'), { force: true });
    console.log(JSON.stringify({ ok: true, records: records.length, errors, unknown }));
  } catch (error) {
    console.error(error);
    console.error(JSON.stringify({ unknown: await page.evaluate(() => window.__sessionFixture?.unknown), errors }));
    await page.screenshot({ path: path.join(out, 'failure.png') });
    process.exitCode = 1;
  } finally {
    await browser.close(); server.close();
  }
})().catch(error => { console.error(error); server.close(); process.exitCode = 1; });
