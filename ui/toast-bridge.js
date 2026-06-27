/* Мост window.toast для окна тостов — контракт Electron-preload поверх Tauri. */

(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;

  let listeners = 0;
  const armed = () => {
    // оба слушателя на месте — демон может доливать буфер ранних тостов
    if (++listeners === 2) invoke('toast_ready');
  };

  window.toast = {
    onAdd: (cb) => { listen('toast-add', (e) => cb(e.payload)).then(armed); },
    onUpdate: (cb) => { listen('toast-update', (e) => cb(e.payload)).then(armed); },
    // нативный hover (курсор над окном тостов): WKWebView не шлёт mouseenter,
    // пока приложение неактивно — а тост всплывает именно в этот момент.
    // Не идёт через armed(): на готовность буфера влияют только onAdd/onUpdate.
    onHover: (cb) => { listen('toast-hover', (e) => cb(e.payload)); },
    // голос держит карточку, пока говорит (hold), и продлевает на N мс после (extend)
    onHold: (cb) => { listen('toast-hold', (e) => cb(e.payload)); },
    onExtend: (cb) => { listen('toast-extend', (e) => cb(e.payload)); },
    // снять карточку по id (вопрос ответили) — не через armed(), это не буфер
    onRemove: (cb) => { listen('toast-remove', (e) => cb(e.payload)); },
    click: (sessionId) => invoke('toast_click', { sessionId }),
    resize: (h) => invoke('toast_resize', { h }),
    continueSession: (sessionId) => invoke('session_continue', { sessionId }),
    // ответ на вопрос кликом по варианту (выбор клавишами идёт мимо — глобальный хоткей)
    answerQuestion: (sessionId, indices, multiSelect) =>
      invoke('question_answer', { sessionId, choice: { indices, multiSelect } }),
    // голосовая маршрутизация: фазы HUD + индикатор «слышу» (audio_state).
    // НЕ через armed(): на буфер ранних тостов влияют только onAdd/onUpdate.
    onVoiceHud: (cb) => { listen('voice-hud', (e) => cb(e.payload)); },
    onAudioState: (cb) => { listen('audio_state', (e) => cb(e.payload)); },
    // тап по варианту пикера (sessionId=null → отмена выбора) и «Отменить» стейджа
    voicePick: (nonce, sessionId) => invoke('voice_pick_resolve', { nonce, sessionId }),
    voiceCancel: (nonce) => invoke('voice_stage_cancel', { nonce }),
    // дотянуть текущее аудио-состояние на загрузке (VR-3)
    audioState: () => invoke('voice_audio_state'),
    // «Да/Отмена» на confirm-карточке управления (п/п-2)
    voiceConfirm: (nonce, approved) => invoke('voice_confirm_resolve', { nonce, approved }),
    // крестик в HUD = «стоп всё»: оборвать озвучку и завершить разговор
    voiceAbort: () => invoke('voice_abort'),
  };
})();
