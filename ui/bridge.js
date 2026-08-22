/* Мост window.jarvis: тот же контракт, что у Electron-preload, но поверх
 * Tauri IPC. renderer.js не знает, что под ним сменился рантайм.
 *
 * Каналы 'ns:method' стали командами 'ns_method'; payload событий — без
 * изменений. Требует withGlobalTauri (см. tauri.conf.json). */

(() => {
  const raw = window.__TAURI__.core.invoke;

  /**
   * Вызов команды с присмотром.
   *
   * Обещание, которое не завершается ни успехом, ни отказом, — худший вид
   * поломки: экран пуст, ошибок нет, ждать можно вечно. Так бывает, когда
   * команда на той стороне паникует: задача умирает, и ответить уже некому.
   * Сам вызов не трогаем — только замечаем вслух, что ответа нет.
   */
  const invoke = (cmd, args) => {
    const p = raw(cmd, args);
    let done = false;
    const stop = () => { done = true; };
    p.then(stop, stop);
    setTimeout(() => {
      if (done) return;
      try { raw('ui_error', { place: 'invoke', message: `команда ${cmd} не ответила за 10 с` }); } catch (e) { /* тишина */ }
    }, 10_000);
    return p;
  };
  const { listen } = window.__TAURI__.event;

  const on = (event, cb) => { listen(event, (e) => cb(e.payload)); };

  // собственный светофор оконного режима: декораций нет, кнопки рисуем сами
  const self = () => window.__TAURI__.window.getCurrentWindow();

  /* Отказ оконного вызова — в лог, не в тишину. Кнопки светофора уже были
   * декорацией по вине ACL: вызов отклонялся политикой разрешений, отказ
   * обещания глотался, и снаружи это выглядело как «не работают». Боковая
   * ветка catch помечает отказ обработанным, но само обещание возвращается
   * как есть — свои обработчики вызывающих продолжают работать. */
  const guard = (p, what) => {
    p.catch((e) => report(`window.${what}`, e));
    return p;
  };

  // Ошибки панели уезжают в общий лог: белый экран это почти всегда
  // исключение, оборвавшее отрисовку, и видеть его только в девтулзах —
  // значит не видеть вовсе.
  const report = (place, message) => {
    try { invoke('ui_error', { place, message: String(message) }); } catch (e) { /* лог не должен ронять панель */ }
  };
  window.addEventListener('error', (e) => {
    report(`${e.filename || '?'}:${e.lineno || 0}`, (e.error && e.error.stack) || e.message);
  });
  window.addEventListener('unhandledrejection', (e) => {
    const r = e.reason;
    report('promise', (r && r.stack) || (r && r.message) || r);
  });

  window.jarvis = {
    onState: (cb) => on('state', cb),
    onShown: (cb) => on('panel-shown', () => cb()),
    onOpenSession: (cb) => on('open-session', cb),
    getState: () => invoke('state_get'),
    clearFinished: () => invoke('state_clear'),
    hidePanel: () => invoke('panel_hide'),
    getSettings: () => invoke('settings_get'),
    // удалённые узлы (спека 2026-08-05): список, добавление, удаление, проверка связи
    remotesList: () => invoke('remotes_list'),
    remotesAdd: (cfg) => invoke('remotes_add', { cfg }),
    remotesRemove: (name) => invoke('remotes_remove', { name }),
    remotesTest: (name) => invoke('remotes_test', { name }),
    // установка узла с нуля: разведка машины, сама установка (ход едет
    // событиями) и публичный ssh-ключ для машин, куда доступа ещё нет
    remotesPreflight: (sshHost, jarvisDir) => invoke('remotes_preflight', { sshHost, jarvisDir }),
    remotesInstall: (cfg) => invoke('remotes_install', { cfg }),
    remotesSshKey: (create) => invoke('remotes_ssh_key', { create }),
    // разовый вход по паролю: кладём наш ключ в authorized_keys той машины
    remotesSshAuthorize: (sshHost, password) => invoke('remotes_ssh_authorize', { sshHost, password }),
    onRemoteInstallStep: (cb) => on('remote_install_progress', cb),
    onRemoteInstallDone: (cb) => on('remote_install_done', cb),
    // режим «Циклы»: рутина, которую агент крутит сам
    loopsGet: () => invoke('loops_get'),
    loopsDraft: (template) => invoke('loops_draft', { template }),
    loopsCatalog: () => invoke('loops_catalog'),
    // режим «Связка»: руки над одним проектом + очередь слияний
    bundleGet: () => invoke('bundle_get'),
    bundleDraft: () => invoke('bundle_draft'),
    bundleSave: (item) => invoke('bundle_save', { item }),
    bundleStart: (id) => invoke('bundle_start', { id }),
    bundleAddHand: (id, task, name) => invoke('bundle_add_hand', { id, task, name }),
    bundlePause: (id, on) => invoke('bundle_pause', { id, on }),
    bundleMerge: (id, hand) => invoke('bundle_merge', { id, hand }),
    bundleRemove: (id) => invoke('bundle_remove', { id }),
    bundlePlaces: (machine) => invoke('bundle_places', { machine }),
    bundleBrowse: (machine, path) => invoke('bundle_browse', { machine, path }),
    onBundleState: (cb) => on('bundle-state', cb),
    loopsCompose: (text, item) => invoke('loops_compose', { text, item }),
    // свои агенты: qwen/opencode/внутренние CLI — реестр в настройках
    agentsList: () => invoke('agents_list'),
    agentsSave: (agents) => invoke('agents_save', { agents }),
    loopsSave: (item) => invoke('loops_save', { item }),
    loopsRemove: (id) => invoke('loops_remove', { id }),
    loopsStart: (id) => invoke('loops_start', { id }),
    loopsStop: (id) => invoke('loops_stop', { id }),
    loopsIntervene: (id, text) => invoke('loops_intervene', { id, text }),
    loopsAnswer: (id, answer) => invoke('loops_answer', { id, answer }),
    loopsReview: (id, n, accept, comment) => invoke('loops_review', { id, n, accept, comment }),
    loopsResume: (id, extraTokens) => invoke('loops_resume', { id, extraTokens }),
    loopsDiff: (id) => invoke('loops_diff', { id }),
    onLoopsState: (cb) => on('loops-state', cb),

    // главный агент: нить разговора, прошлая переписка, отправка и подтверждения.
    // События демон шлёт всем окнам — вкладка и окно из трея видят один поток.
    agentChatState: () => invoke('agent_chat_state'),
    agentChatHistory: (chatId) => invoke('agent_chat_history', { chatId }),
    agentChatReset: () => invoke('agent_chat_reset'),
    // Список разговоров: по чату на проект. Каждая правка возвращает весь
    // список с пометкой открытого — второй ход за списком не нужен.
    agentChatsList: () => invoke('agent_chats_list'),
    agentChatSwitch: (chatId) => invoke('agent_chat_switch', { chatId }),
    agentChatCreate: (name) => invoke('agent_chat_create', { name }),
    agentChatRename: (chatId, name) => invoke('agent_chat_rename', { chatId, name }),
    agentChatDelete: (chatId) => invoke('agent_chat_delete', { chatId }),
    agentChatReorder: (chatId, toIndex) => invoke('agent_chat_reorder', { chatId, toIndex }),
    // Привязать к чату разговор, найденный на диске, и открыть его. Не «открыть
    // окно чата» — окно поднимает agent_chat_window.
    agentChatOpen: (sessionId) => invoke('agent_chat_open', { sessionId }),
    // Недописанная реплика — у каждого чата своя. Общее поле означало, что текст
    // для одного Джарвиса можно отправить другому, а чаты раздают промпты в
    // сессии с доступом к файлам. Хранилище своё (agent-drafts.json): в
    // settings.json такому потоку записей делать нечего.
    agentDraftsGet: () => invoke('agent_drafts_get'),
    agentDraftSet: (chatId, text, caret) => invoke('agent_draft_set', { chatId, text, caret }),
    // Убрать разговор из списка, оставив файл на диске, — и вернуть все убранные.
    // Забыть насовсем стирает транскрипт: подтверждение спрашивает окно, ядро
    // его не дублирует.
    agentHistoryHide: (sessionId) => invoke('agent_history_hide', { sessionId }),
    agentHistoryUnhideAll: () => invoke('agent_history_unhide_all'),
    agentHistoryForget: (sessionId) => invoke('agent_history_forget', { sessionId }),
    agentSend: (message, chatId, sessionId) => invoke('agent_send', { message, chatId, sessionId }),
    // Остановка хода. Рвёт ТОЛЬКО названный чат: у соседей свои ходы, и Esc в
    // одном разговоре не должен гасить второй. Дочерние CLI остаются жить —
    // их список приезжает ответом (и событием `stopped`), закрывает человек.
    agentStop: (chatId) => invoke('agent_stop', { chatId }),
    // Авто-продолжение: срез и возврат. Цепочку Esc гасит через agent_stop —
    // иначе остановленный ход через минуту сменился бы следующим; отсюда нужен
    // только срез (была ли она жива) и дорога назад.
    agentChainState: (chatId) => invoke('agent_chain_state', { chatId }),
    agentChainMode: (chatId, auto) => invoke('agent_chain_mode', { chatId, auto }),
    // Предложенный заход уходит по кнопке — в режиме «спроси меня» это
    // единственная дорога дальше. `text` — поправка человека; пусто значит
    // «уходит предложенное», и подменять его своей копией незачем.
    agentChainSend: (chatId, text) => invoke('agent_chain_send', { chatId, text }),
    agentChainResume: (chatId) => invoke('agent_chain_resume', { chatId }),
    agentChainStop: (chatId) => invoke('agent_chain_stop', { chatId }),
    // Свой канал цепочки: режим, заходы, расход и «ждёт тебя». Опрашивать это
    // командой на каждый ход значило бы узнавать про ночную работу с опозданием.
    onAgentChain: (cb) => on('agent:chain', cb),
    agentConfirm: (nonce, approved, armed) => invoke('agent_confirm', { nonce, approved, armed }),
    onAgentEvent: (cb) => on('agent:event', cb),
    onAgentConfirm: (cb) => on('agent:confirm', cb),

    // тема/краска сменились в другом окне (демон рассылает всем)
    onAppearance: (cb) => on('appearance', cb),
    reportError: report,
    winMinimize: () => guard(self().minimize(), 'minimize'),
    winZoom: () => guard(self().toggleMaximize(), 'toggleMaximize'),
    winClose: () => guard(self().close(), 'close'),  // CloseRequested перехвачен → просто прячет
    // зелёная кнопка macOS — фуллскрин (зум под Alt, как в системе)
    winIsFullscreen: () => guard(self().isFullscreen(), 'isFullscreen'),
    winToggleFullscreen: () => guard((async () => {
      const w = self();
      await w.setFullscreen(!(await w.isFullscreen()));
    })(), 'setFullscreen'),
    // светофор горит только у активного окна — как у системных кнопок
    onWinFocus: (cb) => { self().onFocusChanged(({ payload }) => cb(!!payload)); },
    setSettings: (patch) => invoke('settings_set', { patch }),
    openChat: (sessionId) => invoke('chat_open', { sessionId }),
    closeChat: () => invoke('chat_close'),
    summarizeTurn: (sessionId, turnKey) => invoke('chat_summarize', { sessionId, turnKey }),
    openFile: (sessionId, path, reveal) => invoke('file_open', { sessionId, path, reveal: !!reveal }),
    // вьюер документов (спека 2026-07-18 §3.1): чтение файла из фактов сессии
    readFile: (sessionId, path) => invoke('file_read', { sessionId, path }),
    // свод правок задачи: список файлов, дифф, приём и откат (changes.rs)
    sessionChanges: (sessionId) => invoke('session_changes', { sessionId }),
    sessionChangeDiff: (sessionId, path) => invoke('session_change_diff', { sessionId, path }),
    sessionCommit: (sessionId, message, paths) => invoke('session_commit', { sessionId, message, paths }),
    sessionRevert: (sessionId, path) => invoke('session_revert', { sessionId, path }),
    sessionPush: (sessionId) => invoke('session_push', { sessionId }),
    sessionReview: (sessionId) => invoke('session_review', { sessionId }),
    sessionSearch: (sessionId, query) => invoke('session_search', { sessionId, query }),
    previewOpen: (url) => invoke('preview_open', { url }),
    sessionTouched: (sessionId, path) => invoke('session_touched', { sessionId, path }),
    // дифф файла для таба «Изменения» (§3.2): git-ханки или mode:"none"
    diffFile: (sessionId, path) => invoke('file_diff', { sessionId, path }),
    // внешняя http(s)-ссылка из отрендеренного дока → системный браузер
    openUrl: (url) => invoke('url_open', { url }),
    onChatAppend: (cb) => on('chat:append', cb),
    onChatSummary: (cb) => on('chat:summary', cb),
    focusTerminal: (sessionId) => invoke('terminal_focus', { sessionId }),
    // machine: 'local' | имя узла — где запускать (вкладка «Проекты», шаг 1)
    // opts: { isolate: bool, mode: 'ask'|'plan'|'yolo' } — свойства ЗАДАЧИ,
    // а не общей настройки: разведать чужой код и переписать свой требуют
    // разного доверия
    launchSession: (cwd, agent, sessionId, machine, opts) => invoke('session_launch', {
      cwd: cwd ?? null, agent, sessionId: sessionId ?? null, machine: machine ?? null,
      isolate: !!(opts && opts.isolate), mode: (opts && opts.mode) || 'ask',
      task: (opts && opts.task) || null,
      container: !!(opts && opts.container),
    }),
    machinesList: () => invoke('machines_list'),
    sendReply: (sessionId, text) => invoke('session_reply', { sessionId, text }),
    // вставленная картинка → временный файл; путь уйдёт агенту в промпте
    saveImage: (dataBase64, ext) => invoke('session_save_image', { dataBase64, ext }),
    pingTerminal: (sessionId) => invoke('terminal_ping', { sessionId }),
    answerQuestion: (sessionId, choice) => invoke('question_answer', { sessionId, choice }),
    // действие с доски задач → редактируемый текст-инструкция (НЕ отправка)
    taskAction: (sessionId, taskRef, action) => invoke('task_action', { sessionId, taskRef, action }),
    // голос (инкремент 7): состояние, выбор спикера, тест, mute
    voiceGet: () => invoke('voice_get'),
    voiceSetSpeaker: (speaker) => invoke('voice_set_speaker', { speaker }),
    voiceSetRate: (rate) => invoke('voice_set_rate', { rate }),
    voiceTest: () => invoke('voice_test'),
    voiceSetMute: (on) => invoke('voice_set_mute', { on }),
    voiceSetDuck: (on) => invoke('voice_set_duck', { on }),
    voiceSetBluetoothOnly: (on) => invoke('voice_set_bluetooth_only', { on }),
    getCommands: (sessionId) => invoke('commands_get', { sessionId }),
    setModel: (sessionId, model) => invoke('session_set_model', { sessionId, model }),
    setEffort: (sessionId, level) => invoke('session_set_effort', { sessionId, level }),
    setPin: (sessionId, pinned) => invoke('session_set_pin', { sessionId, pinned }),
    // своё имя чата вместо автозаголовка; пустая строка — вернуть автозаголовок
    renameSession: (sessionId, title) => invoke('session_rename', { sessionId, title }),
    // завершить сессию: закрыть пану, если жива, и убрать из списка в любом случае
    killSession: (sessionId) => invoke('session_kill', { sessionId }),
    getMeta: () => invoke('app_meta'),
    updateCheckInstall: () => invoke('update_check_install'),
    relaunch: () => invoke('app_relaunch'),
    onPlugins: (cb) => on('plugins', cb),
    getPlugins: () => invoke('plugins_status'),
    pluginCmd: (id, cmd, args) => invoke('plugins_cmd', { id, cmd, args: args ?? null }),
    getUsage: (period) => invoke('usage_summary', { period }),
    getLimit: () => invoke('limit_get'),
    onLimitState: (cb) => on('limit-state', cb),
    getSessionUsage: (id) => invoke('usage_session', { id }),
    getHistory: (machine) => invoke('history_get', { machine: machine ?? null }),
    // интеграция и модели (настройки)
    integrationGet: () => invoke('integration_get'),
    integrationRemove: () => invoke('integration_remove'),
    onboardingOpen: () => invoke('onboarding_open'),
    modelDelete: (id) => invoke('model_delete', { id }),
    modelsGet: () => invoke('models_get'),
    transcriptsGet: () => invoke('transcripts_get'),
    transcriptsClear: () => invoke('transcripts_clear'),
    transcriptDelete: (id) => invoke('transcript_delete', { id }),
    transcriptRetranscribe: (id) => invoke('transcript_retranscribe', { id }),
    transcriptEnhance: (text, style) => invoke('transcript_enhance', { text, style }),
    // умные промпты: библиотека + флаг «умный режим»
    promptsGet: () => invoke('prompts_get'),
    promptsGetSettings: () => invoke('prompts_get_settings'),
    promptsSetSmart: (on) => invoke('prompts_set_smart', { on }),
    quietSet: (on) => invoke('quiet_set', { on }),
    onGotoSettings: (cb) => on('goto-settings', cb),
    onGotoVoicehist: (cb) => on('goto-voicehist', cb),
    // STT — диктовка (инкремент 9): состояние, выбор движка, тест
    sttGet: () => invoke('stt_get'),
    sttSetEngine: (engine) => invoke('stt_set_engine', { engine }),
    sttSetHotkey: (hotkey) => invoke('stt_set_hotkey', { hotkey }),
    // Хоткеи — единый реестр действий (рекордер в настройках)
    hotkeyBindings: () => invoke('hotkey_bindings'),
    hotkeyAssign: (action, accel, steal) => invoke('hotkey_assign', { action, accel, steal: !!steal }),
    hotkeysSuspend: (on) => invoke('hotkeys_suspend', { on }),
    sttSetNoiseGate: (on) => invoke('stt_set_noise_gate', { on }),
    sttTest: () => invoke('stt_test'),
    sttInputDevices: () => invoke('stt_input_devices'),
    sttSetInputDevice: (name) => invoke('stt_set_input_device', { name }),
    sttInstallWhisper: () => invoke('stt_install_whisper'),
    sttInstallSidecar: () => invoke('stt_install_sidecar'),
    sttInstallQwen: (key) => invoke('stt_install_qwen', { key }),
    onSttInstallProgress: (cb) => on('stt_install_progress', cb),
    onSttInstallDone: (cb) => on('stt_install_done', cb),
    // Мультизагрузка моделей (онбординг + панель): единые события по id модели.
    modelsInstall: (ids) => invoke('models_install', { ids }),
    onModelInstallProgress: (cb) => on('model_install_progress', cb),
    onModelInstallDone: (cb) => on('model_install_done', cb),
    onModelsInstallAllDone: (cb) => on('models_install_all_done', cb),

    // «Под капотом» — служебный LLM (Claude/Codex) + установка Codex-SDK сайдкара
    serviceGet: () => invoke('service_get'),
    serviceSetBackend: (backend) => invoke('service_set_backend', { backend }),
    serviceSetModel: (model) => invoke('service_set_model', { model }),
    serviceSetEffort: (effort) => invoke('service_set_effort', { effort }),
    serviceSetProxy: (proxy) => invoke('service_set_proxy', { proxy }),
    serviceTest: () => invoke('service_test'),
    claudeAuthGet: () => invoke('claude_auth_get'),
    claudeAuthConnect: (mode, value) => invoke('claude_auth_connect', { mode, value }),
    claudeAuthDisconnect: () => invoke('claude_auth_disconnect'),
    codexInstallSidecar: () => invoke('codex_install_sidecar'),
    onCodexInstallProgress: (cb) => on('codex_install_progress', cb),
    onCodexInstallDone: (cb) => on('codex_install_done', cb),

    // Wake-word + общий аудио-вход (инкремент 10)
    wakeGet: () => invoke('wake_get'),
    wakeSetEnabled: (val) => invoke('wake_set_enabled', { on: val }),
    wakeSetThreshold: (threshold) => invoke('wake_set_threshold', { threshold }),
    audioSetMute: (val) => invoke('audio_set_mute', { on: val }),
    wakeInstallModels: () => invoke('wake_install_models'),
    voiceInstallSilero: () => invoke('voice_install_silero'),
    onAudioState: (cb) => on('audio_state', cb),
    onWake: (cb) => on('wake', cb),
    onWakeInstallDone: (cb) => on('wake_install_done', cb),
  };

  // navigator.clipboard в WKWebView капризен (secure context, жесты) —
  // подменяем на надёжный плагин Tauri, API тот же.
  const writeText = (text) =>
    invoke('plugin:clipboard-manager|write_text', { text: String(text) });
  try {
    Object.defineProperty(navigator, 'clipboard', {
      value: { writeText },
      configurable: true,
    });
  } catch {
    /* не вышло переопределить — останется нативный */
  }
})();
