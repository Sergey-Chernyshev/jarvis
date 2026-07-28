/* Pure Agent VM view model.
 *
 * The sidecar owns lifecycle and journals. This module only merges the
 * EntityStore snapshot with Project Manager history and reduces replay/live
 * RunEvents into a deterministic UI model.
 */
(function agentVmModule(root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  if (root) root.JarvisAgentVm = api;
})(typeof window !== 'undefined' ? window : globalThis, () => {
  const OWNER = 'plugin:agent-vm';
  const TERMINAL_STATES = new Set(['completed', 'failed', 'cancelled', 'interrupted', 'waiting']);
  const ACTIVE_VM_STATES = new Set([
    'provisioning',
    'creating',
    'starting',
    'running',
    'ready',
    'working',
    'error',
  ]);
  const ACTIVE_RANK = {
    waiting: 0,
    error: 1,
    working: 2,
    starting: 3,
    reconnecting: 4,
    ready: 5,
  };

  const asString = (value) => (typeof value === 'string' ? value : '');
  const asObject = (value) =>
    value && typeof value === 'object' && !Array.isArray(value) ? value : {};

  function owned(entities, kind) {
    return (Array.isArray(entities) ? entities : [])
      .filter((entity) => entity && entity.owner === OWNER && entity.kind === kind);
  }

  function entityTime(entity) {
    return Number(entity && entity.updatedAt) || 0;
  }

  function projectKey(entity) {
    const attrs = asObject(entity && entity.attrs);
    return asString(attrs.projectId) || asString(attrs.cwd);
  }

  function newestByProject(entities, kind) {
    const out = new Map();
    for (const entity of owned(entities, kind)) {
      const key = projectKey(entity);
      if (!key) continue;
      const current = out.get(key);
      if (!current || entityTime(entity) >= entityTime(current)) out.set(key, entity);
    }
    return out;
  }

  function runSummary(entity) {
    if (!entity) return '';
    const attrs = asObject(entity.attrs);
    const latest = asObject(attrs.latestEvent);
    const payload = asObject(latest.payload);
    return (
      asString(payload.text)
      || asString(payload.detail)
      || asString(payload.relativePath)
      || (entity.state === 'waiting' ? 'Ждёт ответа' : '')
      || (entity.state === 'working' ? 'Агент работает' : '')
      || (entity.state === 'completed' ? 'Последний запуск завершён' : '')
    );
  }

  function deriveProjects(history, entities) {
    const byCwd = new Map();
    for (const group of Array.isArray(history) ? history : []) {
      const cwd = asString(group && group.cwd);
      if (!cwd) continue;
      byCwd.set(cwd, {
        key: cwd,
        cwd,
        name: asString(group.project) || cwd.split('/').filter(Boolean).at(-1) || cwd,
        projectId: '',
        history: group,
        vm: null,
        run: null,
        summary: '',
        updatedAt: Number(group.lastAt) || 0,
      });
    }

    for (const entity of owned(entities, 'vm')) {
      const attrs = asObject(entity.attrs);
      const cwd = asString(attrs.cwd);
      if (!cwd) continue;
      const item = byCwd.get(cwd) || {
        key: cwd,
        cwd,
        name: asString(attrs.project) || cwd.split('/').filter(Boolean).at(-1) || cwd,
        projectId: '',
        history: null,
        vm: null,
        run: null,
        summary: '',
        updatedAt: 0,
      };
      if (!item.vm || entityTime(entity) >= entityTime(item.vm)) item.vm = entity;
      item.projectId = asString(attrs.projectId) || item.projectId;
      item.updatedAt = Math.max(item.updatedAt, entityTime(entity));
      byCwd.set(cwd, item);
    }

    for (const entity of owned(entities, 'agent_run')) {
      const attrs = asObject(entity.attrs);
      const cwd = asString(attrs.cwd);
      if (!cwd) continue;
      const item = byCwd.get(cwd) || {
        key: cwd,
        cwd,
        name: asString(attrs.project) || cwd.split('/').filter(Boolean).at(-1) || cwd,
        projectId: '',
        history: null,
        vm: null,
        run: null,
        summary: '',
        updatedAt: 0,
      };
      if (!item.run || entityTime(entity) >= entityTime(item.run)) item.run = entity;
      item.projectId = asString(attrs.projectId) || item.projectId;
      item.updatedAt = Math.max(item.updatedAt, entityTime(entity));
      item.summary = runSummary(item.run);
      byCwd.set(cwd, item);
    }

    return [...byCwd.values()].sort((a, b) =>
      b.updatedAt - a.updatedAt || a.name.localeCompare(b.name));
  }

  function environmentState(vmEntity, runEntity) {
    if (runEntity && !runEntity.stale) {
      if (runEntity.state === 'waiting') return 'waiting';
      if (['starting', 'working'].includes(runEntity.state)) return 'working';
      if (['failed', 'error', 'interrupted'].includes(runEntity.state)) return 'error';
    }
    if (vmEntity && vmEntity.stale) return 'reconnecting';
    const state = vmEntity ? vmEntity.state : 'absent';
    if (['provisioning', 'creating', 'starting'].includes(state)) return 'starting';
    if (state === 'error') return 'error';
    if (['running', 'ready', 'working'].includes(state)) return 'ready';
    return 'off';
  }

  function activeEnvironments(entities) {
    const runs = newestByProject(entities, 'agent_run');
    return owned(entities, 'vm')
      .filter((entity) => ACTIVE_VM_STATES.has(entity.state) || entity.stale)
      .map((entity) => {
        const attrs = asObject(entity.attrs);
        const key = projectKey(entity);
        const run = runs.get(key) || null;
        return {
          id: entity.id,
          projectId: asString(attrs.projectId) || asString(attrs.cwd),
          cwd: asString(attrs.cwd),
          project: asString(attrs.project)
            || asString(attrs.cwd).split('/').filter(Boolean).at(-1)
            || 'Project',
          vm: entity,
          run,
          uiState: environmentState(entity, run),
          updatedAt: Math.max(entityTime(entity), entityTime(run)),
        };
      })
      .filter((item) => item.uiState !== 'off')
      .sort((a, b) =>
        (ACTIVE_RANK[a.uiState] ?? 99) - (ACTIVE_RANK[b.uiState] ?? 99)
        || b.updatedAt - a.updatedAt);
  }

  function mergeEvents(current, incoming) {
    const bySeq = new Map();
    for (const event of [...(current || []), ...(incoming || [])]) {
      const seq = Number(event && event.seq);
      if (!Number.isSafeInteger(seq) || seq <= 0) continue;
      bySeq.set(seq, event);
    }
    return [...bySeq.values()].sort((a, b) => a.seq - b.seq);
  }

  function newTurn(event) {
    return {
      id: asString(event.turnId),
      startedAt: Number(event.at) || 0,
      user: '',
      assistantDraft: '',
      assistant: '',
      tools: [],
      files: [],
      question: null,
      result: null,
      state: 'working',
    };
  }

  function reduceRun(events) {
    const turnsById = new Map();
    let state = 'idle';
    let backend = '';
    let vm = '';
    let runId = '';
    let sessionId = '';
    let model = '';

    const turnFor = (event) => {
      const id = asString(event.turnId) || `turn-${event.seq}`;
      if (!turnsById.has(id)) turnsById.set(id, newTurn({ ...event, turnId: id }));
      return turnsById.get(id);
    };

    for (const event of mergeEvents([], events)) {
      const type = asString(event.type);
      const payload = asObject(event.payload);
      const turn = turnFor(event);
      backend = asString(event.backend) || backend;
      vm = asString(event.vm) || vm;
      runId = asString(event.runId) || runId;
      sessionId = asString(payload.backendSessionId) || sessionId;
      model = asString(payload.model) || model;

      if (type === 'user.message') turn.user = asString(payload.text);
      if (type === 'assistant.delta') turn.assistantDraft += asString(payload.text);
      if (type === 'assistant.message') turn.assistant = asString(payload.text);
      if (type === 'tool.started') {
        const id = asString(payload.id) || `tool-${event.seq}`;
        const existing = turn.tools.find((tool) => tool.id === id);
        if (existing) {
          Object.assign(existing, {
            name: asString(payload.name) || existing.name,
            detail: asString(payload.detail) || existing.detail,
            state: 'working',
          });
        } else {
          turn.tools.push({
            id,
            name: asString(payload.name) || 'tool',
            detail: asString(payload.detail),
            state: 'working',
          });
        }
      }
      if (type === 'tool.completed' || type === 'tool.failed') {
        const id = asString(payload.id);
        const existing = turn.tools.find((tool) => tool.id === id);
        if (existing) {
          existing.state = type === 'tool.failed' ? 'failed' : 'completed';
          existing.detail = asString(payload.detail) || existing.detail;
        } else {
          turn.tools.push({
            id: id || `tool-${event.seq}`,
            name: 'tool',
            detail: asString(payload.detail),
            state: type === 'tool.failed' ? 'failed' : 'completed',
          });
        }
      }
      if (type === 'file.changed') {
        const path = asString(payload.path);
        const existing = turn.files.find((file) => file.path === path);
        const file = {
          path,
          relativePath: asString(payload.relativePath) || path.split('/').at(-1) || path,
          change: asString(payload.change) || 'modified',
        };
        if (existing) Object.assign(existing, file);
        else if (path) turn.files.push(file);
      }
      if (type === 'question.opened') {
        turn.question = payload.question || payload;
        turn.state = 'waiting';
        state = 'waiting';
      }
      if (type === 'result.completed') {
        const files = Array.isArray(payload.files) ? payload.files : [];
        for (const value of files) {
          const file = asObject(value);
          const path = asString(file.path);
          if (!path || turn.files.some((item) => item.path === path)) continue;
          turn.files.push({
            path,
            relativePath: path.split('/').at(-1) || path,
            change: asString(file.change) || 'modified',
          });
        }
        turn.result = { text: asString(payload.text), files: turn.files };
        turn.state = 'completed';
        state = 'completed';
      }
      if (type === 'run.failed') {
        turn.state = 'failed';
        turn.result = {
          text: asString(payload.error) || 'Agent VM run failed',
          files: turn.files,
        };
        state = 'failed';
      }
      if (type === 'run.cancelled') {
        turn.state = 'cancelled';
        state = 'cancelled';
      }
      if (type === 'run.interrupted') {
        turn.state = 'interrupted';
        state = 'interrupted';
      }
      if (type === 'run.started' || type === 'run.resumed') {
        turn.state = 'working';
        if (!TERMINAL_STATES.has(state)) state = 'working';
      }
    }

    const turns = [...turnsById.values()].map((turn) => ({
      ...turn,
      assistant: turn.assistant || turn.assistantDraft,
    }));
    return { runId, backend, vm, sessionId, model, state, turns };
  }

  function operationResult(entities, requestId) {
    const entity = (Array.isArray(entities) ? entities : []).find((item) =>
      item
      && item.owner === OWNER
      && item.kind === 'operation'
      && asObject(item.attrs).requestId === requestId);
    if (!entity || !['done', 'error'].includes(entity.state)) return null;
    if (entity.state === 'error') {
      return {
        ok: false,
        error: asString(asObject(entity.attrs).error) || 'Agent VM operation failed',
        attrs: asObject(entity.attrs),
      };
    }
    return { ok: true, attrs: asObject(entity.attrs) };
  }

  function stateLabel(state) {
    return {
      absent: 'Нет VM',
      off: 'Остановлена',
      stopped: 'Остановлена',
      provisioning: 'Создаётся',
      creating: 'Создаётся',
      starting: 'Запускается',
      running: 'Готова',
      ready: 'Готова',
      reconnecting: 'Переподключаюсь',
      working: 'Работает',
      waiting: 'Ждёт тебя',
      completed: 'Готово',
      cancelled: 'Остановлено',
      interrupted: 'Прервано',
      failed: 'Ошибка',
      error: 'Ошибка',
    }[state] || 'Неизвестно';
  }

  return {
    OWNER,
    activeEnvironments,
    deriveProjects,
    environmentState,
    mergeEvents,
    operationResult,
    reduceRun,
    runSummary,
    stateLabel,
  };
});
