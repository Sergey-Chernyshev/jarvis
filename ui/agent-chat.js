/* Окно чата с агентом Jarvis (фаза 7).
 * Шлёт сообщение в agent_send, слушает поток agent:event
 * (init/delta/tool_use/done/failed) и карточки подтверждения agent:confirm
 * (резолв через agent_confirm). Нить разговора переживает закрытие окна:
 * id хранится в настройках (agent_chat_state / agent_chat_reset). */

(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;

  // тема и краска: окно живёт вне общего моста, поэтому подписывается само
  window.jarvis = Object.assign(window.jarvis || {}, {
    getSettings: () => invoke('settings_get'),
    setSettings: (patch) => invoke('settings_set', { patch }),
    onAppearance: (cb) => { listen('appearance', (e) => cb(e.payload)); },
  });

  const msgs = document.getElementById('msgs');
  const input = document.getElementById('input');
  const sendBtn = document.getElementById('send');
  const sub = document.getElementById('sub');
  const tag = document.getElementById('tag');
  const newBtn = document.getElementById('newChat');
  const hintTpl = msgs.querySelector('.hint').cloneNode(true);

  let sessionId = null; // для многоходового диалога (--resume)
  let curBubble = null; // текущий стриминговый пузырь ассистента
  let busy = false;

  const el = (cls, text) => {
    const d = document.createElement('div');
    d.className = cls;
    if (text != null) d.textContent = text;
    return d;
  };
  const scroll = () => { msgs.scrollTop = msgs.scrollHeight; };
  const clearHint = () => { const h = msgs.querySelector('.hint'); if (h) h.remove(); };
  const setHint = (text) => { const h = msgs.querySelector('.hint'); if (h) h.textContent = text; };

  function addRow(kind, child) {
    clearHint();
    const row = el('msg ' + kind);
    row.appendChild(child);
    msgs.appendChild(row);
    scroll();
    return child;
  }
  const addUser = (t) => addRow('user', el('bubble', t));
  const addErr = (t) => addRow('err', el('bubble', t));
  const addNote = (t) => addRow('note', el('bubble', t));
  const startBot = () => (curBubble = addRow('bot', el('bubble', '')));
  const addTool = (name) => { addRow('tool', el('chip', '→ ' + name)); curBubble = null; };

  function setBusy(v) {
    busy = v;
    sendBtn.disabled = v;
    sendBtn.textContent = v ? '…' : '⏎';
    sub.textContent = v ? 'думает…' : 'готов';
  }

  // Метка в шапке: разговор тянется из прошлого запуска окна. Держим её до
  // «Нового чата» — иначе после первой реплики человек снова гадает.
  const setResumed = (on) => { tag.hidden = !on; };

  // Восстановление: id разговора живёт в настройках, а не в этой переменной —
  // окно закрывается, нить нет.
  (async () => {
    try {
      const st = await invoke('agent_chat_state');
      if (!st || !st.sessionId) return;
      sessionId = st.sessionId;
      setResumed(true);
      setHint('Продолжаю прошлый разговор: агент помнит, о чём шла речь. Сами реплики остались в его памяти, а не в этой ленте — она начинается отсюда.');
    } catch (e) {
      // Молчать нельзя: иначе окно тихо начнёт новый диалог вместо прошлого.
      addErr('Не удалось узнать про прошлый разговор: ' + e);
    }
  })();

  async function newChat() {
    try {
      await invoke('agent_chat_reset');
    } catch (e) {
      addErr('Не удалось начать новый чат: ' + e); // старый id остался — так и скажем
      return;
    }
    sessionId = null;
    setResumed(false);
    curBubble = null;
    msgs.textContent = '';
    msgs.appendChild(hintTpl.cloneNode(true));
    setBusy(false);
    input.focus();
  }
  newBtn.addEventListener('click', newChat);

  async function send() {
    const text = input.value.trim();
    if (!text || busy) return;
    input.value = '';
    input.style.height = 'auto';
    addUser(text);
    setBusy(true);
    curBubble = null;
    try {
      await invoke('agent_send', { message: text, sessionId });
    } catch (e) {
      addErr('Ошибка запуска агента: ' + e);
      setBusy(false);
    }
  }

  sendBtn.addEventListener('click', send);
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); send(); }
  });
  // авто-рост поля ввода
  input.addEventListener('input', () => {
    input.style.height = 'auto';
    input.style.height = Math.min(120, input.scrollHeight) + 'px';
  });

  // поток ответа агента
  listen('agent:event', (e) => {
    const ev = e.payload || {};
    switch (ev.type) {
      case 'init':
        // агент инициализирован (ev.tools — гранто-фильтрованный набор)
        if (ev.session_id) sessionId = ev.session_id;
        break;
      case 'delta':
        if (!curBubble) startBot();
        curBubble.textContent += ev.text || '';
        scroll();
        break;
      case 'tool_use':
        addTool(ev.name || '?');
        break;
      case 'done':
        if (ev.session_id) sessionId = ev.session_id;
        // финальный текст, если дельт не было
        if (ev.result && (!curBubble || !curBubble.textContent)) startBot().textContent = ev.result;
        setBusy(false);
        curBubble = null;
        break;
      case 'failed':
        // Отказ агента — вслух. Тихо снять «думает…» значило бы соврать, что он ответил.
        setBusy(false);
        curBubble = null;
        addErr(ev.message || 'агент не ответил');
        if (ev.lost_session) {
          sessionId = null;
          setResumed(false);
          addNote('Прошлый разговор не открылся — дальше говорим с чистого листа. Он не удалён: транскрипт остался на диске.');
        }
        break;
    }
  });

  // карточка подтверждения side-effect (PanelConfirmer)
  listen('agent:confirm', (e) => {
    const c = e.payload || {};
    const cd = c.card || {};
    clearHint();

    const box = el('cbox');
    box.appendChild(el('ctitle', 'Агент хочет выполнить: ' + (c.id || '?')));

    let desc;
    if (cd.kind === 'session') {
      desc = (cd.label || 'сессия');
      if (cd.text) desc += ' · «' + cd.text + '»';
      if (cd.model) desc += ' · модель ' + cd.model;
      if (cd.effort) desc += ' · effort ' + cd.effort;
    } else if (cd.kind === 'settings') {
      const keys = Object.keys(cd.diff || {});
      desc = 'изменить настройки: ' + (keys.length ? keys.join(', ') : '—');
    } else {
      desc = JSON.stringify(cd.args || cd);
    }
    box.appendChild(el('cdesc', desc));
    if (c.provenance === 'untrusted') box.appendChild(el('cwarn', '⚠ данные из недоверенного источника'));

    const btns = el('cbtns');
    const yes = el('cbtn yes', 'Разрешить');
    const no = el('cbtn no', 'Отклонить');
    const decide = (approved) => {
      invoke('agent_confirm', { nonce: c.nonce, approved });
      btns.remove();
      box.appendChild(el('cresult', approved ? '✓ разрешено' : '✕ отклонено'));
      scroll();
    };
    yes.addEventListener('click', () => decide(true));
    no.addEventListener('click', () => decide(false));
    btns.append(yes, no);
    box.appendChild(btns);

    const row = el('msg confirm');
    row.appendChild(box);
    msgs.appendChild(row);
    scroll();
  });

  input.focus();
})();
