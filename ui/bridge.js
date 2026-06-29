/* Мост window.jarvis: тот же контракт, что у Electron-preload, но поверх
 * Tauri IPC. renderer.js не знает, что под ним сменился рантайм.
 *
 * Каналы 'ns:method' стали командами 'ns_method'; payload событий — без
 * изменений. Требует withGlobalTauri (см. tauri.conf.json). */

(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;

  const on = (event, cb) => { listen(event, (e) => cb(e.payload)); };

  window.jarvis = {
    onState: (cb) => on('state', cb),
    onShown: (cb) => on('panel-shown', () => cb()),
    onOpenSession: (cb) => on('open-session', cb),
    getState: () => invoke('state_get'),
    clearFinished: () => invoke('state_clear'),
    hidePanel: () => invoke('panel_hide'),
    getSettings: () => invoke('settings_get'),
    setSettings: (patch) => invoke('settings_set', { patch }),
    openChat: (sessionId) => invoke('chat_open', { sessionId }),
    closeChat: () => invoke('chat_close'),
    onChatAppend: (cb) => on('chat:append', cb),
    focusTerminal: (sessionId) => invoke('terminal_focus', { sessionId }),
    sendReply: (sessionId, text) => invoke('session_reply', { sessionId, text }),
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
    getCommands: (sessionId) => invoke('commands_get', { sessionId }),
    setModel: (sessionId, model) => invoke('session_set_model', { sessionId, model }),
    setEffort: (sessionId, level) => invoke('session_set_effort', { sessionId, level }),
    setPin: (sessionId, pinned) => invoke('session_set_pin', { sessionId, pinned }),
    getMeta: () => invoke('app_meta'),
    onPlugins: (cb) => on('plugins', cb),
    getPlugins: () => invoke('plugins_status'),
    pluginCmd: (id, cmd, args) => invoke('plugins_cmd', { id, cmd, args: args ?? null }),
    getUsage: (period) => invoke('usage_summary', { period }),
    getLimit: () => invoke('limit_get'),
    onLimitState: (cb) => on('limit-state', cb),
    getSessionUsage: (id) => invoke('usage_session', { id }),
    getHistory: () => invoke('history_get'),
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
    sttTest: () => invoke('stt_test'),
    sttInputDevices: () => invoke('stt_input_devices'),
    sttSetInputDevice: (name) => invoke('stt_set_input_device', { name }),
    sttInstallWhisper: () => invoke('stt_install_whisper'),
    sttInstallSidecar: () => invoke('stt_install_sidecar'),
    sttInstallQwen: (key) => invoke('stt_install_qwen', { key }),
    onSttInstallProgress: (cb) => on('stt_install_progress', cb),
    onSttInstallDone: (cb) => on('stt_install_done', cb),

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
