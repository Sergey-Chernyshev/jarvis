/* Pure Agent VM view model.
 *
 * The sidecar owns lifecycle and journals. This module only merges the
 * EntityStore snapshot with Project Manager history and reduces replay/live
 * RunEvents into a deterministic UI model.
 */
(function agentVmModule(root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  if (root) root.JarvisAgentVm = api;
})(typeof window !== "undefined" ? window : globalThis, () => {
  const OWNER = "plugin:agent-vm";
  const TERMINAL_STATES = new Set([
    "completed",
    "failed",
    "cancelled",
    "interrupted",
    "waiting",
  ]);
  const ACTIVE_VM_STATES = new Set([
    "provisioning",
    "creating",
    "starting",
    "running",
    "ready",
    "working",
    "error",
  ]);
  const ACTIVE_RANK = {
    waiting: 0,
    error: 1,
    working: 2,
    starting: 3,
    reconnecting: 4,
    ready: 5,
  };

  const asString = (value) => (typeof value === "string" ? value : "");
  const asObject = (value) =>
    value && typeof value === "object" && !Array.isArray(value) ? value : {};

  function owned(entities, kind) {
    return (Array.isArray(entities) ? entities : []).filter(
      (entity) => entity && entity.owner === OWNER && entity.kind === kind,
    );
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
      if (!current || entityTime(entity) >= entityTime(current))
        out.set(key, entity);
    }
    return out;
  }

  function runSummary(entity) {
    if (!entity) return "";
    const attrs = asObject(entity.attrs);
    const latest = asObject(attrs.latestEvent);
    const payload = asObject(latest.payload);
    return (
      asString(payload.text) ||
      asString(payload.detail) ||
      asString(payload.relativePath) ||
      (entity.state === "waiting" ? "Ждёт ответа" : "") ||
      (entity.state === "working" ? "Агент работает" : "") ||
      (entity.state === "completed" ? "Последний запуск завершён" : "")
    );
  }

  function continuationRunId(run, backend, selectedRunId) {
    if (
      !run ||
      (!["starting", "working", "waiting"].includes(run.state) &&
        !asString(asObject(run.attrs).backendSessionId))
    ) {
      return "";
    }
    const attrs = asObject(run.attrs);
    const runId = asString(selectedRunId);
    if (
      !runId ||
      asString(attrs.runId) !== runId ||
      asString(attrs.backend) !== backend
    ) {
      return "";
    }
    return runId;
  }

  function configuredBackends(vm) {
    if (!vm) return ["claude", "codex"];
    const attrs = asObject(vm.attrs);
    if (asString(attrs.management) === "missing") {
      return ["claude", "codex"];
    }
    if (!Object.prototype.hasOwnProperty.call(attrs, "modules")) {
      return ["claude", "codex"];
    }
    const modules = Array.isArray(attrs.modules) ? attrs.modules : [];
    return ["claude", "codex"].filter((backend) => modules.includes(backend));
  }

  function backendAvailable(vm, backend) {
    return configuredBackends(vm).includes(backend);
  }

  function selectBackend(vm, requested = "claude") {
    const configured = configuredBackends(vm);
    return configured.includes(requested)
      ? requested
      : configured[0] || requested;
  }

  function ephemeralHistoryCwd(cwd) {
    const path = asString(cwd);
    return (
      /^(?:\/private)?\/tmp(?:\/|$)/.test(path) ||
      /^\/var\/folders(?:\/|$)/.test(path) ||
      /^\/Users\/[^/]+$/.test(path) ||
      /\/\.jarvis-dev(?:\/|$)/.test(path)
    );
  }

  function displayProjectPath(cwd) {
    return asString(cwd).replace(/^\/Users\/[^/]+(?=\/|$)/, "~");
  }

  function deriveProjects(history, entities) {
    const byCwd = new Map();
    for (const group of Array.isArray(history) ? history : []) {
      const cwd = asString(group && group.cwd);
      if (!cwd || group.exists === false || ephemeralHistoryCwd(cwd)) continue;
      byCwd.set(cwd, {
        key: cwd,
        cwd,
        name:
          asString(group.project) ||
          cwd.split("/").filter(Boolean).at(-1) ||
          cwd,
        projectId: "",
        history: group,
        vm: null,
        run: null,
        summary: "",
        updatedAt: Number(group.lastAt) || 0,
      });
    }

    for (const entity of owned(entities, "vm")) {
      const attrs = asObject(entity.attrs);
      const cwd = asString(attrs.cwd);
      if (!cwd) continue;
      const item = byCwd.get(cwd) || {
        key: cwd,
        cwd,
        name:
          asString(attrs.project) ||
          cwd.split("/").filter(Boolean).at(-1) ||
          cwd,
        projectId: "",
        history: null,
        vm: null,
        run: null,
        summary: "",
        updatedAt: 0,
      };
      if (!item.vm || entityTime(entity) >= entityTime(item.vm))
        item.vm = entity;
      item.projectId = asString(attrs.projectId) || item.projectId;
      item.updatedAt = Math.max(item.updatedAt, entityTime(entity));
      byCwd.set(cwd, item);
    }

    for (const entity of owned(entities, "agent_run")) {
      const attrs = asObject(entity.attrs);
      const cwd = asString(attrs.cwd);
      if (!cwd) continue;
      const item = byCwd.get(cwd) || {
        key: cwd,
        cwd,
        name:
          asString(attrs.project) ||
          cwd.split("/").filter(Boolean).at(-1) ||
          cwd,
        projectId: "",
        history: null,
        vm: null,
        run: null,
        summary: "",
        updatedAt: 0,
      };
      if (!item.run || entityTime(entity) >= entityTime(item.run))
        item.run = entity;
      item.projectId = asString(attrs.projectId) || item.projectId;
      item.updatedAt = Math.max(item.updatedAt, entityTime(entity));
      item.summary = runSummary(item.run);
      byCwd.set(cwd, item);
    }

    return [...byCwd.values()].sort(
      (a, b) => b.updatedAt - a.updatedAt || a.name.localeCompare(b.name),
    );
  }

  function mergeProjectCatalog(projects, state) {
    const manager = asObject(state);
    const byCwd = new Map();
    for (const project of Array.isArray(projects) ? projects : []) {
      const cwd = asString(project && project.cwd);
      if (!cwd) continue;
      byCwd.set(cwd, { ...project });
    }
    for (const rawFolder of Array.isArray(manager.folders)
      ? manager.folders
      : []) {
      const folder = asObject(rawFolder);
      const cwd = asString(folder.cwd);
      if (!cwd) continue;
      const current = byCwd.get(cwd);
      if (current) {
        current.projectId ||= asString(folder.projectId);
        current.catalogFolder = folder;
      } else {
        byCwd.set(cwd, {
          key: cwd,
          cwd,
          name:
            asString(folder.project) ||
            cwd.split("/").filter(Boolean).at(-1) ||
            cwd,
          projectId: asString(folder.projectId),
          history: null,
          vm: null,
          run: null,
          summary: "",
          updatedAt: 0,
          catalogFolder: folder,
        });
      }
    }

    const favorites = new Map();
    for (const projectId of Array.isArray(manager.favoriteProjectIds)
      ? manager.favoriteProjectIds
      : []) {
      const id = asString(projectId);
      if (id && !favorites.has(id)) favorites.set(id, favorites.size);
    }
    return [...byCwd.values()]
      .map((project) => ({
        ...project,
        favoriteIndex: favorites.get(asString(project.projectId)) ?? -1,
      }))
      .sort((left, right) => {
        const leftFavorite = left.favoriteIndex >= 0;
        const rightFavorite = right.favoriteIndex >= 0;
        if (leftFavorite !== rightFavorite) return leftFavorite ? -1 : 1;
        if (leftFavorite) return left.favoriteIndex - right.favoriteIndex;
        return (
          Number(right.updatedAt) - Number(left.updatedAt) ||
          asString(left.name).localeCompare(asString(right.name))
        );
      });
  }

  function filterProjects(projects, query) {
    const normalized = asString(query).trim().toLocaleLowerCase();
    if (!normalized) return Array.isArray(projects) ? projects : [];
    return (Array.isArray(projects) ? projects : []).filter((project) =>
      `${asString(project && project.name)} ${asString(project && project.cwd)}`
        .toLocaleLowerCase()
        .includes(normalized),
    );
  }

  function projectPrimaryTarget(project) {
    return asObject(project).history ? "history" : "agentvm";
  }

  function filterCommands(commands, input, limit = 12) {
    const value = asString(input);
    if (!value.startsWith("/") || /\s/.test(value.slice(1))) return [];
    const query = value.slice(1).toLocaleLowerCase();
    const boundedLimit = Math.max(1, Math.min(50, Number(limit) || 12));
    return (Array.isArray(commands) ? commands : [])
      .filter((command) =>
        asString(command && command.name)
          .toLocaleLowerCase()
          .includes(query),
      )
      .sort((left, right) => {
        const leftName = asString(left && left.name).toLocaleLowerCase();
        const rightName = asString(right && right.name).toLocaleLowerCase();
        const leftPrefix = leftName.startsWith(query) ? 0 : 1;
        const rightPrefix = rightName.startsWith(query) ? 0 : 1;
        return leftPrefix - rightPrefix || leftName.localeCompare(rightName);
      })
      .slice(0, boundedLimit);
  }

  function composePrompt(text, imagePaths) {
    const parts = [];
    const prompt = asString(text).trim();
    if (prompt) parts.push(prompt);
    for (const path of Array.isArray(imagePaths) ? imagePaths : []) {
      const value = asString(path).trim();
      if (value) parts.push(value);
    }
    return parts.join("\n");
  }

  function environmentState(vmEntity, runEntity) {
    if (
      runEntity &&
      !runEntity.stale &&
      ["failed", "error", "interrupted"].includes(runEntity.state)
    )
      return "error";
    if (vmEntity && vmEntity.stale) return "reconnecting";
    const state = vmEntity ? vmEntity.state : "absent";
    if (["provisioning", "creating", "starting"].includes(state))
      return "starting";
    if (state === "error") return "error";
    if (!["running", "ready", "working"].includes(state)) {
      return runEntity &&
        !runEntity.stale &&
        ["starting", "working", "waiting"].includes(runEntity.state)
        ? "starting"
        : "off";
    }
    if (runEntity && !runEntity.stale) {
      if (runEntity.state === "waiting") return "waiting";
      if (["starting", "working"].includes(runEntity.state)) return "working";
    }
    return "ready";
  }

  // Чаты проекта: обычные сессии из истории + прогоны Agent VM.
  //
  // Транскрипты гостя не покидают VM (config_mirror исключает `sessions`),
  // поэтому VM-чат — это, как правило, отдельная строка из runtime.runs, а не
  // бейдж на строке истории. backendSessionId нужен только чтобы не показать
  // один и тот же чат дважды в редком случае, когда сессия всё же видна хосту.
  function mergeProjectChats(sessions, runs, options = {}) {
    const bySession = new Map();
    for (const raw of Array.isArray(sessions) ? sessions : []) {
      const entry = asObject(raw);
      const id = asString(entry.id);
      if (!id) continue;
      bySession.set(id, {
        kind: "session",
        key: `session:${id}`,
        id,
        title: asString(entry.title) || id.slice(0, 8),
        agent: asString(entry.agent) || "claude",
        model: asString(entry.model),
        tokens: Number(entry.tokens) || 0,
        lastAt: Number(entry.lastAt) || 0,
        vm: "",
        state: "",
        runId: "",
        changedFiles: 0,
      });
    }

    const linkedSessions = asObject(options).linkedSessions;
    const linked =
      linkedSessions instanceof Map
        ? linkedSessions
        : new Map(Object.entries(asObject(linkedSessions)));

    const vmRows = [];
    for (const raw of Array.isArray(runs) ? runs : []) {
      const entry = asObject(raw);
      const runId = asString(entry.runId);
      if (!runId) continue;
      // Прогон и сессия истории — один и тот же чат: показываем один раз,
      // помечая строку истории как VM-чат.
      const sessionId = asString(linked.get(runId));
      const existing = sessionId ? bySession.get(sessionId) : null;
      const state = asString(entry.state);
      const lastAt = Number(entry.lastAt) || 0;
      if (existing) {
        existing.kind = "vm";
        existing.key = `vm:${runId}`;
        existing.runId = runId;
        existing.vm = asString(entry.vm);
        existing.state = state;
        existing.changedFiles = Number(entry.changedFiles) || 0;
        existing.lastAt = Math.max(existing.lastAt, lastAt);
        continue;
      }
      vmRows.push({
        kind: "vm",
        key: `vm:${runId}`,
        id: runId,
        title: asString(entry.project) || runId.slice(0, 8),
        agent: asString(entry.backend) || "claude",
        model: "",
        tokens: 0,
        lastAt,
        vm: asString(entry.vm),
        state,
        runId,
        changedFiles: Number(entry.changedFiles) || 0,
      });
    }

    return [...bySession.values(), ...vmRows].sort(
      (left, right) => right.lastAt - left.lastAt || left.key.localeCompare(right.key),
    );
  }

  // Снимок терминала обновляется только пока открыт экран проекта. Вне его
  // обновлять снимок некому, поэтому старая запись — не доказательство жизни
  // сессии: иначе карточка проекта показывает «работает» у мёртвого терминала.
  const TERMINAL_SNAPSHOT_TTL_MS = 4000;

  function terminalSnapshotLive(snapshot, now = Date.now()) {
    const entry = asObject(snapshot);
    if (!["ready", "working"].includes(asString(entry.state))) return false;
    const seenAt = Number(entry.seenAt);
    if (!Number.isFinite(seenAt) || seenAt <= 0) return false;
    return now - seenAt < TERMINAL_SNAPSHOT_TTL_MS;
  }

  function pluginRuntimeStatus(plugin, now = Date.now()) {
    if (!plugin) {
      return {
        state: "missing",
        tone: "error",
        step: 0,
        label: "Agent VM не найдена",
        detail: "Пакет sidecar недоступен",
        retryable: false,
      };
    }
    if (!plugin.enabled) {
      return {
        state: "stopped",
        tone: "off",
        step: 0,
        label: "Agent VM выключена",
        detail: "Включите плагин в настройках",
        retryable: false,
      };
    }

    const status = asObject(plugin.status);
    const state = asString(status.state) || "stopped";
    const error = asString(status.error)
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 180);
    if (state === "running") {
      return {
        state,
        tone: "ready",
        step: 2,
        label: "Agent VM подключена",
        detail: "Sidecar online",
        retryable: false,
      };
    }
    if (state === "starting") {
      const startedAt = Number(status.startedAt);
      const deadline = Number(status.handshakeDeadline);
      const elapsed =
        startedAt > 0
          ? `${Math.max(0, Math.ceil((now - startedAt) / 1000))}с`
          : "Sidecar запущен";
      const timeout =
        deadline > 0
          ? `таймаут через ${Math.max(0, Math.ceil((deadline - now) / 1000))}с`
          : "ожидаю регистрацию";
      return {
        state,
        tone: "starting",
        step: 1,
        label: "Handshake с Jarvis",
        detail: `${elapsed} · ${timeout}`,
        retryable: false,
      };
    }
    if (state === "backoff") {
      const retryAt = Number(status.retryAt);
      const retryIn =
        retryAt > 0
          ? Math.max(0, Math.ceil((retryAt - now) / 1000))
          : Math.max(0, Math.ceil(Number(status.retryInMs) / 1000));
      const attempt = Math.max(1, Number(status.restartAttempt) || 1);
      return {
        state,
        tone: "waiting",
        step: 0,
        label: `Повтор через ${retryIn}с`,
        detail: `Попытка ${attempt}${error ? ` · ${error}` : ""}`,
        retryable: true,
      };
    }
    if (state === "incompatible") {
      return {
        state,
        tone: "error",
        step: 1,
        label: "Несовместимая версия",
        detail: error || "Обновите Jarvis и Agent VM sidecar",
        retryable: false,
      };
    }
    if (state === "error") {
      return {
        state,
        tone: "error",
        step: 0,
        label: "Ошибка запуска",
        detail: error || "Sidecar не удалось запустить",
        retryable: true,
      };
    }
    return {
      state,
      tone: "starting",
      step: 0,
      label: "Запускаю sidecar",
      detail: "Ожидание supervisor",
      retryable: false,
    };
  }

  function activeEnvironments(entities) {
    const runs = newestByProject(entities, "agent_run");
    return owned(entities, "vm")
      .filter((entity) => ACTIVE_VM_STATES.has(entity.state) || entity.stale)
      .map((entity) => {
        const attrs = asObject(entity.attrs);
        const run = runs.get(projectKey(entity)) || null;
        return {
          id: entity.id,
          projectId: asString(attrs.projectId) || asString(attrs.cwd),
          cwd: asString(attrs.cwd),
          project:
            asString(attrs.project) ||
            asString(attrs.cwd).split("/").filter(Boolean).at(-1) ||
            "Project",
          vm: entity,
          run,
          uiState: environmentState(entity, run),
          updatedAt: Math.max(entityTime(entity), entityTime(run)),
        };
      })
      .filter((item) => item.uiState !== "off")
      .sort(
        (a, b) =>
          (ACTIVE_RANK[a.uiState] ?? 99) - (ACTIVE_RANK[b.uiState] ?? 99) ||
          b.updatedAt - a.updatedAt,
      );
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
      user: "",
      assistantDraft: "",
      assistant: "",
      tools: [],
      files: [],
      question: null,
      result: null,
      state: "working",
    };
  }

  function reduceRun(events) {
    const turnsById = new Map();
    let state = "idle";
    let backend = "";
    let vm = "";
    let runId = "";
    let sessionId = "";
    let model = "";

    const turnFor = (event) => {
      const id = asString(event.turnId) || `turn-${event.seq}`;
      if (!turnsById.has(id))
        turnsById.set(id, newTurn({ ...event, turnId: id }));
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

      if (type === "user.message") turn.user = asString(payload.text);
      if (type === "assistant.delta")
        turn.assistantDraft += asString(payload.text);
      if (type === "assistant.message") turn.assistant = asString(payload.text);
      if (type === "tool.started") {
        const id = asString(payload.id) || `tool-${event.seq}`;
        const existing = turn.tools.find((tool) => tool.id === id);
        if (existing) {
          Object.assign(existing, {
            name: asString(payload.name) || existing.name,
            detail: asString(payload.detail) || existing.detail,
            state: "working",
          });
        } else {
          turn.tools.push({
            id,
            name: asString(payload.name) || "tool",
            detail: asString(payload.detail),
            state: "working",
          });
        }
      }
      if (type === "tool.completed" || type === "tool.failed") {
        const id = asString(payload.id);
        const existing = turn.tools.find((tool) => tool.id === id);
        if (existing) {
          existing.state = type === "tool.failed" ? "failed" : "completed";
          existing.detail = asString(payload.detail) || existing.detail;
        } else {
          turn.tools.push({
            id: id || `tool-${event.seq}`,
            name: "tool",
            detail: asString(payload.detail),
            state: type === "tool.failed" ? "failed" : "completed",
          });
        }
      }
      if (type === "file.changed") {
        const path = asString(payload.path);
        const existing = turn.files.find((file) => file.path === path);
        const file = {
          path,
          relativePath:
            asString(payload.relativePath) || path.split("/").at(-1) || path,
          change: asString(payload.change) || "modified",
        };
        if (existing) Object.assign(existing, file);
        else if (path) turn.files.push(file);
      }
      if (type === "question.opened") {
        turn.question = payload.question || payload;
        turn.state = "waiting";
        state = "waiting";
      }
      if (type === "result.completed") {
        const files = Array.isArray(payload.files) ? payload.files : [];
        for (const value of files) {
          const file = asObject(value);
          const path = asString(file.path);
          if (!path || turn.files.some((item) => item.path === path)) continue;
          turn.files.push({
            path,
            relativePath: path.split("/").at(-1) || path,
            change: asString(file.change) || "modified",
          });
        }
        turn.result = { text: asString(payload.text), files: turn.files };
        turn.state = "completed";
        state = "completed";
      }
      if (type === "run.failed") {
        turn.state = "failed";
        turn.result = {
          text: asString(payload.error) || "Agent VM run failed",
          files: turn.files,
        };
        state = "failed";
      }
      if (type === "run.cancelled") {
        turn.state = "cancelled";
        state = "cancelled";
      }
      if (type === "run.interrupted") {
        turn.state = "interrupted";
        state = "interrupted";
      }
      if (type === "run.started" || type === "run.resumed") {
        turn.state = "working";
        if (!TERMINAL_STATES.has(state)) state = "working";
      }
    }

    const turns = [...turnsById.values()].map((turn) => ({
      ...turn,
      assistant: turn.assistant || turn.assistantDraft,
    }));
    return { runId, backend, vm, sessionId, model, state, turns };
  }

  function operationResult(entities, requestId) {
    const entity = (Array.isArray(entities) ? entities : []).find(
      (item) =>
        item &&
        item.owner === OWNER &&
        item.kind === "operation" &&
        asObject(item.attrs).requestId === requestId,
    );
    if (!entity || !["done", "error"].includes(entity.state)) return null;
    if (entity.state === "error") {
      return {
        ok: false,
        error:
          asString(asObject(entity.attrs).error) || "Agent VM operation failed",
        attrs: asObject(entity.attrs),
      };
    }
    return { ok: true, attrs: asObject(entity.attrs) };
  }

  function stateLabel(state) {
    return (
      {
        absent: "Нет VM",
        off: "Остановлена",
        stopped: "Остановлена",
        provisioning: "Создаётся",
        creating: "Создаётся",
        starting: "Запускается",
        running: "Готова",
        ready: "Готова",
        reconnecting: "Переподключаюсь",
        working: "Работает",
        waiting: "Ждёт тебя",
        completed: "Готово",
        cancelled: "Остановлено",
        interrupted: "Прервано",
        failed: "Ошибка",
        error: "Ошибка",
      }[state] || "Неизвестно"
    );
  }

  return {
    OWNER,
    activeEnvironments,
    backendAvailable,
    composePrompt,
    configuredBackends,
    continuationRunId,
    deriveProjects,
    displayProjectPath,
    environmentState,
    filterCommands,
    filterProjects,
    mergeProjectCatalog,
    mergeProjectChats,
    mergeEvents,
    operationResult,
    pluginRuntimeStatus,
    projectPrimaryTarget,
    reduceRun,
    runSummary,
    selectBackend,
    stateLabel,
    terminalSnapshotLive,
  };
});
