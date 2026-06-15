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
    voiceTest: () => invoke('voice_test'),
    voiceSetMute: (on) => invoke('voice_set_mute', { on }),
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
