import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import test from 'node:test';

const require = createRequire(import.meta.url);
const AgentVm = require('./agent-vm.js');

const vm = (id, state, attrs, updatedAt = 1) => ({
  id: `vm.${id}`,
  kind: 'vm',
  owner: 'plugin:agent-vm',
  state,
  attrs,
  updatedAt,
  stale: false,
});

const run = (id, state, attrs, updatedAt = 1) => ({
  id: `agent_run.${id}`,
  kind: 'agent_run',
  owner: 'plugin:agent-vm',
  state,
  attrs: { runId: id, ...attrs },
  updatedAt,
  stale: false,
});

test('project models merge chat history with VM-only projects and latest runs', () => {
  const history = [{
    project: 'jarvis',
    cwd: '/work/jarvis',
    count: 4,
    lastAt: 90,
    sessions: [],
  }];
  const entities = [
    vm('jarvis-vm', 'running', {
      projectId: 'p-jarvis',
      project: 'jarvis',
      cwd: '/work/jarvis',
      shellCommand: 'avm shell jarvis-vm',
    }, 100),
    vm('api-vm', 'stopped', {
      projectId: 'p-api',
      project: 'api',
      cwd: '/work/api',
    }, 80),
    run('r-old', 'completed', {
      projectId: 'p-jarvis',
      cwd: '/work/jarvis',
      backend: 'claude',
    }, 101),
    run('r-live', 'working', {
      projectId: 'p-jarvis',
      cwd: '/work/jarvis',
      backend: 'codex',
      latestEvent: {
        seq: 7,
        type: 'assistant.delta',
        payload: { text: 'Проверяю тесты' },
      },
    }, 102),
  ];

  const projects = AgentVm.deriveProjects(history, entities);

  assert.deepEqual(projects.map((project) => project.cwd), ['/work/jarvis', '/work/api']);
  assert.equal(projects[0].vm.state, 'running');
  assert.equal(projects[0].run.attrs.runId, 'r-live');
  assert.equal(projects[0].summary, 'Проверяю тесты');
  assert.equal(projects[1].history, null);
});

test('active environments rank waiting and working ahead of ready VMs', () => {
  const entities = [
    vm('ready', 'running', { projectId: 'p-ready', project: 'ready', cwd: '/p/ready' }, 30),
    vm('work', 'running', { projectId: 'p-work', project: 'work', cwd: '/p/work' }, 20),
    vm('wait', 'running', { projectId: 'p-wait', project: 'wait', cwd: '/p/wait' }, 10),
    vm('off', 'stopped', { projectId: 'p-off', project: 'off', cwd: '/p/off' }, 40),
    run('work-run', 'working', { projectId: 'p-work', cwd: '/p/work', backend: 'codex' }, 21),
    run('wait-run', 'waiting', { projectId: 'p-wait', cwd: '/p/wait', backend: 'claude' }, 11),
  ];

  const active = AgentVm.activeEnvironments(entities);

  assert.deepEqual(active.map((item) => item.projectId), ['p-wait', 'p-work', 'p-ready']);
  assert.deepEqual(active.map((item) => item.uiState), ['waiting', 'working', 'ready']);
});

test('a run cannot report working before its project VM exists', () => {
  const startingRun = run('cold-start', 'starting', {
    projectId: 'p-cold',
    cwd: '/p/cold',
    backend: 'claude',
  });
  const workingRun = run('warm-run', 'working', {
    projectId: 'p-warm',
    cwd: '/p/warm',
    backend: 'codex',
  });

  assert.equal(AgentVm.environmentState(null, startingRun), 'starting');
  assert.equal(AgentVm.environmentState(null, workingRun), 'starting');
  assert.equal(
    AgentVm.environmentState(
      vm('warm', 'running', { projectId: 'p-warm', cwd: '/p/warm' }),
      workingRun,
    ),
    'working',
  );
});

test('a failed pre-session run is not reused but an active or resumable run is', () => {
  assert.equal(
    AgentVm.continuationRunId(
      run('failed-before-start', 'failed', { backend: 'claude' }),
      'claude',
      'failed-before-start',
    ),
    '',
  );
  assert.equal(
    AgentVm.continuationRunId(
      run('active', 'working', { backend: 'claude' }),
      'claude',
      'active',
    ),
    'active',
  );
  assert.equal(
    AgentVm.continuationRunId(
      run('completed', 'completed', {
        backend: 'claude',
        backendSessionId: 'session-safe-1',
      }),
      'claude',
      'completed',
    ),
    'completed',
  );
  assert.equal(
    AgentVm.continuationRunId(
      run('other-backend', 'working', { backend: 'codex' }),
      'claude',
      'other-backend',
    ),
    '',
  );
});

test('configured backends follow VM modules and default only before a record exists', () => {
  assert.deepEqual(AgentVm.configuredBackends(null), ['claude', 'codex']);
  assert.deepEqual(
    AgentVm.configuredBackends(vm('not-created', 'absent', {
      management: 'missing',
      modules: [],
    })),
    ['claude', 'codex'],
  );
  assert.deepEqual(
    AgentVm.configuredBackends(vm('claude-only', 'running', {
      modules: ['node', 'go', 'claude'],
    })),
    ['claude'],
  );
  assert.deepEqual(
    AgentVm.configuredBackends(vm('no-agents', 'running', { modules: ['node', 'go'] })),
    [],
  );
  assert.equal(
    AgentVm.backendAvailable(
      vm('claude-only', 'running', { modules: ['node', 'claude'] }),
      'codex',
    ),
    false,
  );
});

test('run reducer deduplicates replay/live events and builds turns, tools, files and result', () => {
  const event = (seq, turnId, type, payload = {}) => ({
    runId: 'run-1',
    turnId,
    seq,
    at: seq,
    type,
    payload,
    backend: 'claude',
    vm: 'project-vm',
  });
  const events = [
    event(1, 'turn-1', 'user.message', { text: 'Сделай smoke' }),
    event(2, 'turn-1', 'assistant.delta', { text: 'Де' }),
    event(3, 'turn-1', 'assistant.delta', { text: 'лаю' }),
    event(3, 'turn-1', 'assistant.delta', { text: 'дубликат' }),
    event(4, 'turn-1', 'tool.started', { id: 'tool-1', name: 'command', detail: 'npm test' }),
    event(5, 'turn-1', 'tool.completed', { id: 'tool-1' }),
    event(6, 'turn-1', 'file.changed', {
      path: '/work/project/smoke.txt',
      relativePath: 'smoke.txt',
      change: 'created',
    }),
    event(7, 'turn-1', 'assistant.message', { text: 'Готово' }),
    event(8, 'turn-1', 'result.completed', {
      text: 'Smoke завершён',
      files: [{ path: '/work/project/smoke.txt', change: 'created' }],
    }),
  ];

  const merged = AgentVm.mergeEvents(events.slice(0, 4), events.slice(3));
  const view = AgentVm.reduceRun(merged);

  assert.deepEqual(merged.map((item) => item.seq), [1, 2, 3, 4, 5, 6, 7, 8]);
  assert.equal(view.turns.length, 1);
  assert.equal(view.turns[0].assistant, 'Готово');
  assert.equal(view.turns[0].tools[0].state, 'completed');
  assert.equal(view.turns[0].files[0].relativePath, 'smoke.txt');
  assert.equal(view.turns[0].result.text, 'Smoke завершён');
  assert.equal(view.state, 'completed');
});

test('operation lookup returns only terminal responses for the matching request', () => {
  const started = {
    id: 'operation.agent-vm-7',
    kind: 'operation',
    owner: 'plugin:agent-vm',
    state: 'started',
    attrs: { requestId: 'agent-vm-7', command: 'runtime.send' },
  };
  const done = {
    ...started,
    state: 'done',
    attrs: { ...started.attrs, runId: 'run-1' },
  };

  assert.equal(AgentVm.operationResult([started], 'agent-vm-7'), null);
  assert.deepEqual(AgentVm.operationResult([done], 'agent-vm-7'), {
    ok: true,
    attrs: done.attrs,
  });
});

test('plugin runtime status exposes handshake, retry countdown and connected phases', () => {
  const now = 100_000;

  assert.deepEqual(
    AgentVm.pluginRuntimeStatus({
      enabled: true,
      status: {
        state: 'starting',
        startedAt: 95_000,
        handshakeDeadline: 105_000,
        restartAttempt: 0,
      },
    }, now),
    {
      state: 'starting',
      tone: 'starting',
      step: 1,
      label: 'Handshake с Jarvis',
      detail: '5с · таймаут через 5с',
      retryable: false,
    },
  );
  assert.deepEqual(
    AgentVm.pluginRuntimeStatus({
      enabled: true,
      status: {
        state: 'backoff',
        retryAt: 104_200,
        restartAttempt: 3,
        error: 'plugin process exited with code 1',
      },
    }, now),
    {
      state: 'backoff',
      tone: 'waiting',
      step: 0,
      label: 'Повтор через 5с',
      detail: 'Попытка 3 · plugin process exited with code 1',
      retryable: true,
    },
  );
  assert.deepEqual(
    AgentVm.pluginRuntimeStatus({
      enabled: true,
      status: { state: 'running', registeredAt: 99_900, restartAttempt: 0 },
    }, now),
    {
      state: 'running',
      tone: 'ready',
      step: 2,
      label: 'Agent VM подключена',
      detail: 'Sidecar online',
      retryable: false,
    },
  );
});

test('main panel exposes Agent VM workspace, bridge and keyboard contract', () => {
  const html = readFileSync(new URL('./index.html', import.meta.url), 'utf8');
  const bridge = readFileSync(new URL('./bridge.js', import.meta.url), 'utf8');
  const renderer = readFileSync(new URL('./renderer.js', import.meta.url), 'utf8');

  assert.match(html, /id="activeEnvironments"/);
  assert.match(html, /id="agentVmWorkspace"/);
  assert.match(html, /id="agentVmFeed"/);
  assert.match(html, /id="agentVmPrompt"/);
  assert.match(html, /id="agentVmAutostart"/);
  assert.match(html, /agent-vm\.js/);
  assert.match(bridge, /getEntities/);
  assert.match(bridge, /onEntities/);
  assert.match(bridge, /agentVmOperationAck/);
  assert.match(bridge, /agentVmFileRead/);
  assert.match(bridge, /getAgentVmProfiles/);
  assert.match(bridge, /setAgentVmProfile/);
  assert.match(bridge, /setAgentVmFocus/);
  assert.match(renderer, /renderAgentVmWorkspace/);
  assert.match(renderer, /runtime\.send/);
  assert.match(renderer, /runtime\.replay/);
  assert.match(renderer, /onOpenAgentVm/);
  assert.match(renderer, /requestedRunId/);
  assert.match(renderer, /renderAgentVmRuntimeStatus/);
  assert.match(renderer, /const busy = \['starting', 'working', 'waiting'\]/);
});
