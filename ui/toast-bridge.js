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
    click: (sessionId) => invoke('toast_click', { sessionId }),
    resize: (h) => invoke('toast_resize', { h }),
  };
})();
