import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
const AgentVm = require("./agent-vm.js");

const vm = (id, state, attrs, updatedAt = 1) => ({
  id: `vm.${id}`,
  kind: "vm",
  owner: "plugin:agent-vm",
  state,
  attrs,
  updatedAt,
  stale: false,
});

const run = (id, state, attrs, updatedAt = 1) => ({
  id: `agent_run.${id}`,
  kind: "agent_run",
  owner: "plugin:agent-vm",
  state,
  attrs: { runId: id, ...attrs },
  updatedAt,
  stale: false,
});

test("project models merge chat history with VM-only projects and latest runs", () => {
  const history = [
    {
      project: "jarvis",
      cwd: "/work/jarvis",
      count: 4,
      lastAt: 90,
      sessions: [],
    },
  ];
  const entities = [
    vm(
      "jarvis-vm",
      "running",
      {
        projectId: "p-jarvis",
        project: "jarvis",
        cwd: "/work/jarvis",
        shellCommand: "avm shell jarvis-vm",
      },
      100,
    ),
    vm(
      "api-vm",
      "stopped",
      {
        projectId: "p-api",
        project: "api",
        cwd: "/work/api",
      },
      80,
    ),
    run(
      "r-old",
      "completed",
      {
        projectId: "p-jarvis",
        cwd: "/work/jarvis",
        backend: "claude",
      },
      101,
    ),
    run(
      "r-live",
      "working",
      {
        projectId: "p-jarvis",
        cwd: "/work/jarvis",
        backend: "codex",
        latestEvent: {
          seq: 7,
          type: "assistant.delta",
          payload: { text: "Проверяю тесты" },
        },
      },
      102,
    ),
  ];

  const projects = AgentVm.deriveProjects(history, entities);

  assert.deepEqual(
    projects.map((project) => project.cwd),
    ["/work/jarvis", "/work/api"],
  );
  assert.equal(projects[0].vm.state, "running");
  assert.equal(projects[0].run.attrs.runId, "r-live");
  assert.equal(projects[0].summary, "Проверяю тесты");
  assert.equal(projects[1].history, null);
});

test("project catalog adds folders, marks favorites and preserves manual favorite order", () => {
  const projects = [
    { cwd: "/work/beta", name: "beta", projectId: "project-b", updatedAt: 30 },
    {
      cwd: "/work/alpha",
      name: "alpha",
      projectId: "project-a",
      updatedAt: 20,
    },
  ];
  const state = {
    folders: [
      { cwd: "/work/gamma", project: "gamma", projectId: "project-c" },
      { cwd: "/work/alpha", project: "alpha", projectId: "project-a" },
    ],
    favoriteProjectIds: ["project-c", "project-a"],
    view: "cards",
  };

  const merged = AgentVm.mergeProjectCatalog(projects, state);

  assert.deepEqual(
    merged.map((project) => project.cwd),
    ["/work/gamma", "/work/alpha", "/work/beta"],
  );
  assert.deepEqual(
    merged.map((project) => project.favoriteIndex),
    [0, 1, -1],
  );
  assert.equal(merged[0].catalogFolder.project, "gamma");
  assert.equal(merged[1].updatedAt, 20);
});

test("project search matches folder names and paths without chat metadata", () => {
  const projects = [
    {
      cwd: "/work/alpha",
      name: "alpha",
      summary: "needle only in agent transcript",
      history: { sessions: [{ title: "needle only in chat" }] },
    },
    { cwd: "/work/needle-folder", name: "beta" },
  ];

  assert.deepEqual(
    AgentVm.filterProjects(projects, "needle").map((project) => project.cwd),
    ["/work/needle-folder"],
  );
  assert.equal(AgentVm.filterProjects(projects, "  ALPHA  ").length, 1);
});

test("project primary action opens existing chat history while keeping Agent VM separate", () => {
  assert.equal(
    AgentVm.projectPrimaryTarget({
      cwd: "/work/with-history",
      history: { sessions: [{ id: "chat-1" }] },
    }),
    "history",
  );
  assert.equal(
    AgentVm.projectPrimaryTarget({
      cwd: "/work/vm-only",
      history: null,
    }),
    "agentvm",
  );
});

test("project catalog hides ephemeral history unless the folder was explicitly added", () => {
  const history = [
    {
      project: "real",
      cwd: "/Users/dev/work/real",
      exists: true,
      sessions: [],
    },
    {
      project: "gone",
      cwd: "/Users/dev/work/gone",
      exists: false,
      sessions: [],
    },
    {
      project: "scratch",
      cwd: "/private/tmp/scratch",
      exists: true,
      sessions: [],
    },
    { project: "home", cwd: "/Users/dev", exists: true, sessions: [] },
  ];
  const derived = AgentVm.deriveProjects(history, []);

  assert.deepEqual(
    derived.map((project) => project.cwd),
    ["/Users/dev/work/real"],
  );
  assert.deepEqual(
    AgentVm.mergeProjectCatalog(derived, {
      folders: [
        {
          projectId: "project-scratch",
          project: "scratch",
          cwd: "/private/tmp/scratch",
        },
      ],
    }).map((project) => project.cwd),
    ["/Users/dev/work/real", "/private/tmp/scratch"],
  );
  assert.equal(
    AgentVm.displayProjectPath("/Users/dev/work/real"),
    "~/work/real",
  );
});

test("Agent VM slash suggestions rank prefix matches and ignore ordinary prompts", () => {
  const commands = [
    {
      name: "security-review",
      description: "Security review",
      source: "builtin",
    },
    { name: "review", description: "Review changes", source: "project" },
    { name: "resume", description: "Resume session", source: "builtin" },
    { name: "model", description: "Change model", source: "builtin" },
  ];

  assert.deepEqual(
    AgentVm.filterCommands(commands, "/re").map((command) => command.name),
    ["resume", "review", "security-review"],
  );
  assert.deepEqual(AgentVm.filterCommands(commands, "/review now"), []);
  assert.deepEqual(AgentVm.filterCommands(commands, "review"), []);
});

test("Agent VM image paths are appended to the managed prompt", () => {
  assert.equal(
    AgentVm.composePrompt("Проверь интерфейс", [
      "/home/dev.guest/.jarvis-vm/uploads/jarvis-1.png",
      "/home/dev.guest/.jarvis-vm/uploads/jarvis-2.jpg",
    ]),
    "Проверь интерфейс\n/home/dev.guest/.jarvis-vm/uploads/jarvis-1.png\n" +
      "/home/dev.guest/.jarvis-vm/uploads/jarvis-2.jpg",
  );
  assert.equal(
    AgentVm.composePrompt("", [
      "/home/dev.guest/.jarvis-vm/uploads/jarvis-1.png",
    ]),
    "/home/dev.guest/.jarvis-vm/uploads/jarvis-1.png",
  );
});

test("active environments combine VM lifecycle with the latest structured run state", () => {
  const entities = [
    vm(
      "ready",
      "running",
      { projectId: "p-ready", project: "ready", cwd: "/p/ready" },
      30,
    ),
    vm(
      "work",
      "running",
      { projectId: "p-work", project: "work", cwd: "/p/work" },
      20,
    ),
    vm(
      "wait",
      "running",
      { projectId: "p-wait", project: "wait", cwd: "/p/wait" },
      10,
    ),
    vm(
      "off",
      "stopped",
      { projectId: "p-off", project: "off", cwd: "/p/off" },
      40,
    ),
    run(
      "work-run",
      "working",
      { projectId: "p-work", cwd: "/p/work", backend: "codex" },
      21,
    ),
    run(
      "wait-run",
      "waiting",
      { projectId: "p-wait", cwd: "/p/wait", backend: "claude" },
      11,
    ),
  ];

  const active = AgentVm.activeEnvironments(entities);

  assert.deepEqual(
    active.map((item) => item.projectId),
    ["p-wait", "p-work", "p-ready"],
  );
  assert.deepEqual(
    active.map((item) => item.uiState),
    ["waiting", "working", "ready"],
  );
  assert.deepEqual(
    active.map((item) => item.run?.attrs?.runId || null),
    ["wait-run", "work-run", null],
  );
});

test("forgotten folders are hidden, but never anything the user still needs", () => {
  const now = 1_800_000_000_000;
  const day = 24 * 60 * 60 * 1000;
  const projects = [
    { cwd: "/fresh", favoriteIndex: -1, updatedAt: now - 2 * day },
    { cwd: "/old", favoriteIndex: -1, updatedAt: now - 200 * day },
    // Забыт давно, но в избранном — прятать нельзя.
    { cwd: "/starred", favoriteIndex: 0, updatedAt: now - 200 * day },
    // Забыт давно, но у него есть VM — это рабочая среда.
    { cwd: "/has-vm", favoriteIndex: -1, updatedAt: now - 200 * day, vm: { state: "stopped" } },
    // Прямо сейчас идёт прогон.
    { cwd: "/running", favoriteIndex: -1, updatedAt: 0, run: { state: "working" } },
    // Есть профиль автозапуска VM — это рабочая среда, не свалка.
    { cwd: "/pinned", favoriteIndex: -1, updatedAt: 0, agentVmProfile: {} },
    // Папка добавлена кнопкой «+»: история её ещё не знает (updatedAt = 0),
    // но пользователь выбрал её сам — спрятать сразу после добавления нельзя.
    { cwd: "/just-added", favoriteIndex: -1, updatedAt: 0, catalogFolder: {} },
    // Ни истории, ни VM, ни времени — разовая папка вроде orch_planner_*.
    { cwd: "/junk", favoriteIndex: -1, updatedAt: 0 },
  ];

  const { active, stale } = AgentVm.splitStaleProjects(projects, now);

  assert.deepEqual(
    active.map((p) => p.cwd),
    ["/fresh", "/starred", "/has-vm", "/running", "/pinned", "/just-added"],
  );
  assert.deepEqual(
    stale.map((p) => p.cwd),
    ["/old", "/junk"],
  );
});

// Тест на стык, а не на поле: папка проходит настоящий путь «нажал +» —
// mergeProjectCatalog по folders из настроек, — и только потом фильтр. Проверка
// поля в отрыве от этого пути уже один раз соврала: фильтр держал
// agentVmProfile, которого на этом пути не бывает, и папка исчезала.
test("a folder just added by hand stays visible before it has any history", () => {
  const projects = AgentVm.mergeProjectCatalog([], {
    folders: [
      { projectId: "a".repeat(24), project: "just-added", cwd: "/Users/me/just-added" },
    ],
    favoriteProjectIds: [],
  });
  const { active, stale } = AgentVm.splitStaleProjects(projects, 1_800_000_000_000);

  assert.deepEqual(
    active.map((p) => p.cwd),
    ["/Users/me/just-added"],
    "папка, добавленную кнопкой, нельзя прятать сразу после добавления",
  );
  assert.equal(stale.length, 0);
});

// Иначе вернувшийся после отпуска увидит «0 проектов» над пустым списком и
// решит, что каталог потерян.
test("nothing is hidden when there would be nothing left to show", () => {
  const now = 1_800_000_000_000;
  const day = 24 * 60 * 60 * 1000;
  const forgotten = [
    { cwd: "/a", favoriteIndex: -1, updatedAt: now - 200 * day },
    { cwd: "/b", favoriteIndex: -1, updatedAt: now - 300 * day },
  ];

  const { active, stale } = AgentVm.splitStaleProjects(forgotten, now);

  assert.deepEqual(active.map((p) => p.cwd), ["/a", "/b"]);
  assert.deepEqual(stale, [], "нечего сворачивать, если список станет пустым");
});

test("stale split survives missing and malformed input", () => {
  assert.deepEqual(AgentVm.splitStaleProjects(undefined), { active: [], stale: [] });
  assert.deepEqual(AgentVm.splitStaleProjects(null), { active: [], stale: [] });
  const { active, stale } = AgentVm.splitStaleProjects([null, {}], 1_800_000_000_000);
  assert.equal(active.length, 2, "мусор всё равно показываем: скрывать нечего");
  assert.equal(stale.length, 0);
});

test("project chats merge ordinary sessions with VM runs, newest first", () => {
  const sessions = [
    { id: "chat-old", title: "старый разговор", agent: "claude", lastAt: 10 },
    { id: "chat-new", title: "свежий разговор", agent: "codex", lastAt: 40 },
  ];
  const runs = [
    {
      runId: "run-1",
      project: "jarvis",
      backend: "claude",
      vm: "jarvis-vm",
      state: "completed",
      lastAt: 30,
      changedFiles: 3,
    },
  ];

  const chats = AgentVm.mergeProjectChats(sessions, runs);

  assert.deepEqual(
    chats.map((chat) => chat.key),
    ["session:chat-new", "vm:run-1", "session:chat-old"],
  );
  // VM-чат и обычный различимы: по этому полю рисуется бейдж.
  assert.deepEqual(
    chats.map((chat) => chat.kind),
    ["session", "vm", "session"],
  );
  const vmChat = chats.find((chat) => chat.kind === "vm");
  assert.equal(vmChat.runId, "run-1");
  assert.equal(
    vmChat.title,
    "jarvis",
    "без первой реплики заголовком остаётся имя проекта",
  );
  assert.equal(vmChat.vm, "jarvis-vm");
  assert.equal(vmChat.state, "completed");
  assert.equal(vmChat.changedFiles, 3);
});

test("a VM run visible in host history is shown once, marked as a VM chat", () => {
  // Транскрипты гостя обычно не доходят до хоста, но если дошли — не двоим.
  const sessions = [
    { id: "sid-42", title: "задача из VM", agent: "claude", lastAt: 10 },
  ];
  const runs = [
    { runId: "run-42", backend: "claude", vm: "jarvis-vm", state: "working", lastAt: 50 },
  ];

  const chats = AgentVm.mergeProjectChats(sessions, runs, {
    linkedSessions: { "run-42": "sid-42" },
  });

  assert.equal(chats.length, 1, "один чат, а не две строки об одном и том же");
  assert.equal(chats[0].kind, "vm");
  assert.equal(chats[0].title, "задача из VM", "заголовок из истории сохраняется");
  assert.equal(chats[0].runId, "run-42");
  assert.equal(chats[0].state, "working");
  assert.equal(chats[0].lastAt, 50, "берём более свежую отметку времени");
});

test("a VM chat is titled by its first prompt, like an ordinary chat", () => {
  // Иначе все прогоны одного проекта выглядят одинаково — именем проекта.
  const chats = AgentVm.mergeProjectChats([], [
    {
      runId: "run-9",
      project: "sup",
      title: "почини сборку на CI",
      backend: "claude",
      state: "completed",
      lastAt: 7,
    },
  ]);

  assert.equal(chats[0].title, "почини сборку на CI");
});

test("merging chats never mutates the caller's history objects", () => {
  // История переиспользуется между перерисовками: порча её объектов означала бы
  // накопление VM-полей на обычных сессиях.
  const session = { id: "sid-7", title: "разговор", agent: "claude", lastAt: 5 };
  const sessions = [session];
  const snapshot = JSON.stringify(session);

  AgentVm.mergeProjectChats(sessions, [
    { runId: "run-7", backend: "codex", vm: "vm-1", state: "failed", lastAt: 9 },
  ], { linkedSessions: { "run-7": "sid-7" } });

  assert.equal(JSON.stringify(session), snapshot, "исходная сессия не тронута");
  assert.equal(sessions.length, 1);
});

test("project chats survive missing, malformed and empty inputs", () => {
  assert.deepEqual(AgentVm.mergeProjectChats(undefined, undefined), []);
  assert.deepEqual(AgentVm.mergeProjectChats(null, []), []);
  // Записи без идентификатора не превращаются в безымянные строки.
  assert.deepEqual(AgentVm.mergeProjectChats([{ title: "нет id" }], [{}]), []);
  const chats = AgentVm.mergeProjectChats([{ id: "x" }], []);
  assert.equal(chats[0].title, "x", "без заголовка показываем идентификатор");
  assert.equal(chats[0].agent, "claude");
});

test("a stale terminal snapshot stops proving the session is alive", () => {
  const now = 100_000;
  // Снимок обновляется только на экране проекта; вне его свежесть и решает.
  assert.equal(
    AgentVm.terminalSnapshotLive({ state: "ready", seenAt: now - 500 }, now),
    true,
  );
  assert.equal(
    AgentVm.terminalSnapshotLive({ state: "working", seenAt: now - 500 }, now),
    true,
  );
  assert.equal(
    AgentVm.terminalSnapshotLive({ state: "ready", seenAt: now - 60_000 }, now),
    false,
    "снимок часовой давности не доказывает, что терминал жив",
  );
  // Мёртвые состояния не оживают даже свежим снимком.
  assert.equal(
    AgentVm.terminalSnapshotLive(
      { state: "disconnected", seenAt: now - 100 },
      now,
    ),
    false,
  );
  assert.equal(
    AgentVm.terminalSnapshotLive({ state: "absent", seenAt: now }, now),
    false,
  );
  // Записи без отметки времени (и отсутствующие) живыми не считаются.
  assert.equal(AgentVm.terminalSnapshotLive({ state: "ready" }, now), false);
  assert.equal(AgentVm.terminalSnapshotLive(undefined, now), false);
});

test("a run cannot report working before its project VM exists", () => {
  const startingRun = run("cold-start", "starting", {
    projectId: "p-cold",
    cwd: "/p/cold",
    backend: "claude",
  });
  const workingRun = run("warm-run", "working", {
    projectId: "p-warm",
    cwd: "/p/warm",
    backend: "codex",
  });

  assert.equal(AgentVm.environmentState(null, startingRun), "starting");
  assert.equal(AgentVm.environmentState(null, workingRun), "starting");
  assert.equal(
    AgentVm.environmentState(
      vm("warm", "running", { projectId: "p-warm", cwd: "/p/warm" }),
      workingRun,
    ),
    "working",
  );
});

test("a failed pre-session run is not reused but an active or resumable run is", () => {
  assert.equal(
    AgentVm.continuationRunId(
      run("failed-before-start", "failed", { backend: "claude" }),
      "claude",
      "failed-before-start",
    ),
    "",
  );
  assert.equal(
    AgentVm.continuationRunId(
      run("active", "working", { backend: "claude" }),
      "claude",
      "active",
    ),
    "active",
  );
  assert.equal(
    AgentVm.continuationRunId(
      run("completed", "completed", {
        backend: "claude",
        backendSessionId: "session-safe-1",
      }),
      "claude",
      "completed",
    ),
    "completed",
  );
  assert.equal(
    AgentVm.continuationRunId(
      run("other-backend", "working", { backend: "codex" }),
      "claude",
      "other-backend",
    ),
    "",
  );
});

test("configured backends follow VM modules and default only before a record exists", () => {
  assert.deepEqual(AgentVm.configuredBackends(null), ["claude", "codex"]);
  assert.deepEqual(
    AgentVm.configuredBackends(
      vm("not-created", "absent", {
        management: "missing",
        modules: [],
      }),
    ),
    ["claude", "codex"],
  );
  assert.deepEqual(
    AgentVm.configuredBackends(
      vm("claude-only", "running", {
        modules: ["node", "go", "claude"],
      }),
    ),
    ["claude"],
  );
  assert.deepEqual(
    AgentVm.configuredBackends(
      vm("no-agents", "running", { modules: ["node", "go"] }),
    ),
    [],
  );
  assert.equal(
    AgentVm.backendAvailable(
      vm("claude-only", "running", { modules: ["node", "claude"] }),
      "codex",
    ),
    false,
  );
  assert.equal(
    AgentVm.selectBackend(
      vm("claude-only", "running", { modules: ["node", "claude"] }),
      "codex",
    ),
    "claude",
  );
  assert.equal(
    AgentVm.selectBackend(
      vm("both", "running", { modules: ["node", "claude", "codex"] }),
      "codex",
    ),
    "codex",
  );
});

test("run reducer deduplicates replay/live events and builds turns, tools, files and result", () => {
  const event = (seq, turnId, type, payload = {}) => ({
    runId: "run-1",
    turnId,
    seq,
    at: seq,
    type,
    payload,
    backend: "claude",
    vm: "project-vm",
  });
  const events = [
    event(1, "turn-1", "user.message", { text: "Сделай smoke" }),
    event(2, "turn-1", "assistant.delta", { text: "Де" }),
    event(3, "turn-1", "assistant.delta", { text: "лаю" }),
    event(3, "turn-1", "assistant.delta", { text: "дубликат" }),
    event(4, "turn-1", "tool.started", {
      id: "tool-1",
      name: "command",
      detail: "npm test",
    }),
    event(5, "turn-1", "tool.completed", { id: "tool-1" }),
    event(6, "turn-1", "file.changed", {
      path: "/work/project/smoke.txt",
      relativePath: "smoke.txt",
      change: "created",
    }),
    event(7, "turn-1", "assistant.message", { text: "Готово" }),
    event(8, "turn-1", "result.completed", {
      text: "Smoke завершён",
      files: [{ path: "/work/project/smoke.txt", change: "created" }],
    }),
  ];

  const merged = AgentVm.mergeEvents(events.slice(0, 4), events.slice(3));
  const view = AgentVm.reduceRun(merged);

  assert.deepEqual(
    merged.map((item) => item.seq),
    [1, 2, 3, 4, 5, 6, 7, 8],
  );
  assert.equal(view.turns.length, 1);
  assert.equal(view.turns[0].assistant, "Готово");
  assert.equal(view.turns[0].tools[0].state, "completed");
  assert.equal(view.turns[0].files[0].relativePath, "smoke.txt");
  assert.equal(view.turns[0].result.text, "Smoke завершён");
  assert.equal(view.state, "completed");
});

test("operation lookup returns only terminal responses for the matching request", () => {
  const started = {
    id: "operation.agent-vm-7",
    kind: "operation",
    owner: "plugin:agent-vm",
    state: "started",
    attrs: { requestId: "agent-vm-7", command: "runtime.send" },
  };
  const done = {
    ...started,
    state: "done",
    attrs: { ...started.attrs, runId: "run-1" },
  };

  assert.equal(AgentVm.operationResult([started], "agent-vm-7"), null);
  assert.deepEqual(AgentVm.operationResult([done], "agent-vm-7"), {
    ok: true,
    attrs: done.attrs,
  });
});

test("plugin runtime status exposes handshake, retry countdown and connected phases", () => {
  const now = 100_000;

  assert.deepEqual(
    AgentVm.pluginRuntimeStatus(
      {
        enabled: true,
        status: {
          state: "starting",
          startedAt: 95_000,
          handshakeDeadline: 105_000,
          restartAttempt: 0,
        },
      },
      now,
    ),
    {
      state: "starting",
      tone: "starting",
      step: 1,
      label: "Handshake с Jarvis",
      detail: "5с · таймаут через 5с",
      retryable: false,
    },
  );
  assert.deepEqual(
    AgentVm.pluginRuntimeStatus(
      {
        enabled: true,
        status: {
          state: "backoff",
          retryAt: 104_200,
          restartAttempt: 3,
          error: "plugin process exited with code 1",
        },
      },
      now,
    ),
    {
      state: "backoff",
      tone: "waiting",
      step: 0,
      label: "Повтор через 5с",
      detail: "Попытка 3 · plugin process exited with code 1",
      retryable: true,
    },
  );
  assert.deepEqual(
    AgentVm.pluginRuntimeStatus(
      {
        enabled: true,
        status: { state: "running", registeredAt: 99_900, restartAttempt: 0 },
      },
      now,
    ),
    {
      state: "running",
      tone: "ready",
      step: 2,
      label: "Agent VM подключена",
      detail: "Sidecar online",
      retryable: false,
    },
  );
});

test("main panel exposes Agent VM workspace, bridge and keyboard contract", () => {
  const html = readFileSync(new URL("./index.html", import.meta.url), "utf8");
  const bridge = readFileSync(new URL("./bridge.js", import.meta.url), "utf8");
  const renderer = readFileSync(
    new URL("./renderer.js", import.meta.url),
    "utf8",
  );
  const ipc = readFileSync(
    new URL("../src-tauri/src/ipc.rs", import.meta.url),
    "utf8",
  );
  const sendUi = renderer.slice(
    renderer.indexOf("async function sendAgentVmMessage()"),
    renderer.indexOf("async function stopAgentVmTerminal()"),
  );
  const projectCardUi = renderer.slice(
    renderer.indexOf("function renderProjectCard(project)"),
    renderer.indexOf("async function pickProjectManagerFolder()"),
  );

  assert.match(html, /id="activeEnvironments"/);
  assert.match(html, /id="agentVmWorkspace"/);
  assert.match(html, /id="agentVmFeed"/);
  assert.match(html, /Управляемая задача Agent VM/);
  assert.match(html, /id="agentVmTerminalScreen"/);
  assert.match(html, /data-agent-vm-key="C-c"/);
  assert.match(html, /class="vm-key-menu"/);
  assert.match(html, /class="vmws-prompt-mark"/);
  assert.doesNotMatch(html, /class="vm-terminal-keys"/);
  assert.match(html, /id="agentVmPrompt"/);
  assert.match(html, /id="agentVmCommandPalette"/);
  assert.match(html, /id="agentVmAttachments"/);
  assert.match(html, /id="agentVmAttach"/);
  assert.match(html, /id="agentVmImagePicker"/);
  assert.match(html, /id="agentVmAutostart"/);
  assert.match(html, /agent-vm\.js/);
  assert.match(bridge, /getEntities/);
  assert.match(bridge, /onEntities/);
  assert.match(bridge, /agentVmOperationAck/);
  assert.match(bridge, /agentVmTerminalEnsure/);
  assert.match(bridge, /agentVmTerminalSnapshot/);
  assert.match(bridge, /agentVmTerminalInput/);
  assert.match(bridge, /getAgentVmCommands/);
  assert.match(bridge, /agentVmTerminalUpload/);
  assert.match(bridge, /agentVmTerminalStop/);
  assert.match(bridge, /agentVmFileRead/);
  assert.match(bridge, /getAgentVmProfiles/);
  assert.match(bridge, /setAgentVmProfile/);
  assert.match(bridge, /getProjectManagerState/);
  assert.match(bridge, /pickProjectManagerFolder/);
  assert.match(bridge, /setProjectManagerFavorite/);
  assert.match(bridge, /moveProjectManagerFavorite/);
  assert.match(bridge, /setProjectManagerView/);
  assert.match(bridge, /setAgentVmFocus/);
  assert.match(renderer, /renderAgentVmWorkspace/);
  assert.match(renderer, /ensureAgentVmTerminal/);
  assert.match(renderer, /warmAgentVmTerminal/);
  // Подключение и завершение терминала были написаны, но недостижимы из UI:
  // кнопки должны существовать и быть привязаны к обработчикам.
  assert.match(html, /id="agentVmConnect"/);
  assert.match(html, /id="agentVmStopAgent"/);
  assert.match(
    renderer,
    /agentVmConnectEl\.addEventListener\('click', \(\) => warmAgentVmTerminal\(\)\)/,
  );
  assert.match(
    renderer,
    /agentVmStopAgentEl\.addEventListener\('click', \(\) => stopAgentVmTerminal\(\)\)/,
  );
  assert.match(renderer, /agentVmTerminalEnsurePromises/);
  assert.match(renderer, /agentVmTerminalInput/);
  assert.match(renderer, /ResizeObserver/);
  assert.match(sendUi, /agentVmCommand\('runtime\.send'/);
  assert.doesNotMatch(sendUi, /window\.jarvis\.agentVmTerminalInput\(/);
  assert.doesNotMatch(renderer, /runtime\.replay/);
  assert.match(projectCardUi, /openProjectPrimary\(project\)/);
  assert.match(projectCardUi, /openAgentVmProject\(project\)/);
  // Проект открывается своими чатами, а не пультом VM.
  const primaryUi = renderer.slice(
    renderer.indexOf("function openProjectPrimary(project)"),
    renderer.indexOf("async function renderHistory()"),
  );
  assert.match(primaryUi, /openHistProject\(project\.cwd\)/);
  assert.doesNotMatch(primaryUi, /openAgentVmProject/);
  // Список чатов проекта объединяет обычные сессии с прогонами Agent VM,
  // помечает VM-строку бейджем и ведёт в рабочее место на нужном прогоне.
  const chatsUi = renderer.slice(
    renderer.indexOf("function renderHistChats(g, q)"),
    renderer.indexOf("function paintHistSel()"),
  );
  assert.match(chatsUi, /AgentVmModel\.mergeProjectChats\(g\.sessions/);
  assert.match(chatsUi, /className: 'hbadge vm'/);
  assert.match(chatsUi, /loadHistRuns\(g\.cwd\)/);
  assert.match(renderer, /agentVmCommand\('runtime\.runs'/);
  assert.match(
    renderer,
    /openAgentVmProject\(project, row\.chat\.agent, row\.chat\.runId\)/,
  );
  assert.match(html, /\.hrow \.hbadge/);
  // Обновление сущностей/профилей не должно выбрасывать из открытого проекта
  // обратно в список проектов: уровень выбирает renderHistLevel.
  assert.match(renderer, /function renderHistLevel\(\)/);
  assert.doesNotMatch(
    renderer,
    /if \(view === 'history'\) renderHistProjects\(/,
    "перерисовка уровня 1 в обход renderHistLevel роняет список чатов",
  );
  // Список прогонов перезапрашивается при каждом входе в проект.
  const openUi = renderer.slice(
    renderer.indexOf("function openHistProject(key)"),
    renderer.indexOf("function openProjectPrimary(project)"),
  );
  assert.match(openUi, /histRuns\.delete\(key\)/);
  // Без Agent VM «прогонов нет» — это ответ, а не «ещё грузим»: иначе список
  // чатов у обычного пользователя навсегда остаётся в состоянии загрузки.
  const loadUi = renderer.slice(
    renderer.indexOf("async function loadHistRuns(cwd)"),
    renderer.indexOf("function renderHistLevel()"),
  );
  assert.match(
    loadUi,
    /if \(!agentVmPluginReady\(\)\) \{\s*\n\s*histRuns\.set\(cwd, \[\]\);/,
    "при неподнятом плагине список должен получить пустой результат",
  );
  // Кэш образов освобождается вручную: автоудаление стоило бы повторной
  // загрузки образа, поэтому нужна именно кнопка.
  assert.match(html, /id="agentVmReleaseCache"/);
  assert.match(renderer, /agentVmCommand\('runtime\.releaseCache'/);
  assert.match(
    renderer,
    /agentVmReleaseCacheEl\.addEventListener\('click', releaseAgentVmCache\)/,
  );
  assert.match(renderer, /onOpenAgentVm/);
  assert.match(renderer, /requestedRunId/);
  assert.match(renderer, /renderAgentVmRuntimeStatus/);
  assert.match(renderer, /agentVmTerminalAlive/);
  assert.match(renderer, /mergeProjectCatalog/);
  assert.match(renderer, /filterProjects/);
  assert.match(renderer, /pm-card-grid/);
  assert.doesNotMatch(renderer, /className: 'pm-card-rail'/);
  assert.doesNotMatch(html, /aspect-ratio:\s*1/);
  assert.doesNotMatch(ipc, /PICK_FOLDER_SCRIPT|osascript/);
  assert.match(html, /Добавить папку/);
  assert.match(html, /pm-card-grid\.cards/);
});

test("folder picker stays async and releases project UI after cancel or error", () => {
  const renderer = readFileSync(
    new URL("./renderer.js", import.meta.url),
    "utf8",
  );
  const ipc = readFileSync(
    new URL("../src-tauri/src/ipc.rs", import.meta.url),
    "utf8",
  );
  const main = readFileSync(
    new URL("../src-tauri/src/main.rs", import.meta.url),
    "utf8",
  );
  const folderPicker = readFileSync(
    new URL("../src-tauri/src/project_folder_picker.rs", import.meta.url),
    "utf8",
  );
  const pickerUi = renderer.slice(
    renderer.indexOf("async function pickProjectManagerFolder()"),
    renderer.indexOf("async function setProjectManagerFavorite("),
  );

  assert.match(folderPicker, /NSOpenPanel/);
  assert.match(folderPicker, /beginWithCompletionHandler/);
  assert.doesNotMatch(folderPicker, /runModal/);
  assert.match(main, /project_folder_picker::is_active\(\)/);
  assert.match(ipc, /Ok\(None\)[\s\S]*?"cancelled": true/);
  assert.match(pickerUi, /if \(result\.cancelled\) return;/);
  assert.match(pickerUi, /catch \(error\)/);
  assert.match(pickerUi, /finally\s*\{\s*projectManagerSaving = false;/);

  // Ошибка записи не доказывает, что запись не применилась: ответ мог
  // потеряться уже после сохранения. Каждая запись каталога обязана
  // пересинхронизироваться с бэкендом, иначе UI навсегда расходится с диском.
  assert.match(renderer, /async function resyncProjectManagerState\(\)/);
  assert.match(
    renderer,
    /projectManagerState = state;/,
    "пересинхронизация берёт состояние у бэкенда, а не угадывает его",
  );
  const writers = [
    ["async function pickProjectManagerFolder()", "async function setProjectManagerFavorite("],
    ["async function setProjectManagerFavorite(", "async function moveProjectManagerFavorite("],
    ["async function moveProjectManagerFavorite(", "async function setProjectManagerView("],
    ["async function setProjectManagerView(", "projectManagerAddEl.addEventListener"],
  ];
  for (const [from, to] of writers) {
    const start = renderer.indexOf(from);
    const end = renderer.indexOf(to);
    assert.ok(start >= 0 && end > start, `не найден блок ${from}`);
    assert.match(
      renderer.slice(start, end),
      /await resyncProjectManagerState\(\);/,
      `${from} должен пересинхронизироваться после ошибки`,
    );
  }
});

// Фазы запуска должны соответствовать тому, что плагин реально делает
// (service.rs::plan_ensure), иначе экран рассказывает про работу, которой нет.
test("план запуска зависит от состояния VM, как plan_ensure", () => {
  assert.equal(AgentVm.ensurePlan(null), "create", "нет VM → создаём");
  assert.equal(AgentVm.ensurePlan({ state: "stopped" }), "start");
  assert.equal(AgentVm.ensurePlan({ state: "running" }), "ready");
  assert.equal(
    AgentVm.ensurePlan({ state: "running", attrs: { management: "orphaned" } }),
    "ready",
    "чужую VM мы не трогаем",
  );
});

// Текст не должен обещать работу, которой не будет: у готовой VM ничего не
// «собирается», иначе экран врёт при каждом втором запуске.
test("строки запуска соответствуют тому, что делают с этой VM", () => {
  const fresh = AgentVm.bootLines(null).join(" ");
  assert.match(fresh, /виртуальную машину/);
  const ready = AgentVm.bootLines({ state: "running" }).join(" ");
  assert.doesNotMatch(
    ready,
    /Собираю|Раскладываю образ/,
    "готовую VM не собирают заново",
  );
  assert.deepEqual(AgentVm.bootLines(null, "agent"), AgentVm.bootLines({ state: "running" }, "agent"));
});

test("строка перебирается по кругу и не выходит за пределы фазы", () => {
  const lines = AgentVm.bootLines(null);
  assert.equal(AgentVm.bootLine(null, "env", 0), lines[0]);
  assert.equal(AgentVm.bootLine(null, "env", 1), lines[1]);
  assert.equal(
    AgentVm.bootLine(null, "env", lines.length),
    lines[0],
    "после последней строки начинаем заново",
  );
  // Мусорный тик не должен ломать показ.
  assert.equal(AgentVm.bootLine(null, "env", -5), lines[0]);
  assert.equal(AgentVm.bootLine(null, "env", NaN), lines[0]);
});

test("фаза агента не показывает строки про создание машины", () => {
  const agent = AgentVm.bootLine(null, "agent", 0);
  assert.match(agent, /агента/);
  assert.doesNotMatch(agent, /виртуальную машину/);
});

test("all environments include stopped machines, active ones first", () => {
  // Вкладка «Среды» показывает парк целиком: остановленная машина — не мусор,
  // её надо видеть, чтобы запустить. activeEnvironments для этого не годится:
  // она сознательно отбрасывает всё, что не работает прямо сейчас.
  const entities = [
    vm("sleepy", "stopped", { projectId: "p1", cwd: "/Users/dev/ticksly", project: "ticksly" }, 10),
    vm("busy", "running", { projectId: "p2", cwd: "/Users/dev/sup", project: "sup" }, 20),
    vm("broken", "error", { projectId: "p3", cwd: "/Users/dev/api", project: "api" }, 30),
  ];

  const all = AgentVm.allEnvironments(entities);
  assert.equal(all.length, 3, "видны все машины, включая остановленную");

  const stopped = all.find((e) => e.project === "ticksly");
  assert.ok(stopped, "остановленная машина не пропала");
  assert.equal(stopped.uiState, "off");

  const at = (name) => all.findIndex((e) => e.project === name);
  assert.ok(at("sup") < at("ticksly"), "работающая машина выше остановленной");
  assert.ok(at("api") < at("ticksly"), "сломанная машина выше остановленной");
});
