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
    click: (sessionId) => invoke('toast_click', { sessionId }),
    resize: (h) => invoke('toast_resize', { h }),
  };
})();
