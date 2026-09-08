/* Окно чата с агентом Jarvis (фаза 7).
 * Шлёт сообщение в agent_send, слушает поток agent:event (init/delta/tool_use/done)
 * и карточки подтверждения agent:confirm (резолв через agent_confirm). */

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
  const provider = document.getElementById('provider');
  const hint = msgs.querySelector('.hint');

  let sessionId = null; // для многоходового диалога (--resume)
  let curBubble = null; // текущий стриминговый пузырь ассистента
  let busy = false;
  let ready = false;
  let hasProvider = false;
  let sessionProvider = null;

  const el = (cls, text) => {
    const d = document.createElement('div');
    d.className = cls;
    if (text != null) d.textContent = text;
    return d;
  };
  const scroll = () => { msgs.scrollTop = msgs.scrollHeight; };
  const clearHint = () => { if (hint && hint.parentNode) hint.remove(); };

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
  const startBot = () => (curBubble = addRow('bot', el('bubble', '')));
  const addTool = (name) => { addRow('tool', el('chip', '→ ' + name)); curBubble = null; };

  function setBusy(v) {
    busy = v;
    sendBtn.disabled = v || !ready || !hasProvider;
    provider.disabled = v || !ready;
    sendBtn.textContent = v ? '…' : '⏎';
    sub.textContent = v ? 'думает…' : (!ready ? 'подключение…' : (hasProvider ? 'готов' : 'CLI не найден'));
  }

  async function send() {
    const text = input.value.trim();
    if (!text || busy || !ready || !hasProvider) return;
    input.value = '';
    input.style.height = 'auto';
    addUser(text);
    setBusy(true);
    curBubble = null;
    try {
      const result = await invoke('agent_send', {
        message: text, sessionId, provider: sessionProvider || provider.value,
      });
      if (result?.ok === false) throw new Error(result.error || 'Не удалось запустить агента');
      if (result?.provider) sessionProvider = result.provider;
    } catch (e) {
      addErr('Ошибка запуска агента: ' + e);
      setBusy(false);
    }
  }

  sendBtn.addEventListener('click', send);
  input.addEventListener('keydown', (e) => {
    if (!e.isComposing && e.keyCode !== 229 && e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); send(); }
  });

  provider.addEventListener('change', async () => {
    // Claude and Codex session IDs are not interchangeable.
    sessionId = null;
    sessionProvider = null;
    curBubble = null;
    hasProvider = !provider.selectedOptions[0]?.disabled;
    setBusy(false);
    if (msgs.querySelector('.msg')) addRow('tool', el('chip', 'Новый диалог · ' + provider.selectedOptions[0].textContent));
    try {
      const result = await invoke('settings_set', { patch: { agentProvider: provider.value } });
      if (result?.ok === false) throw new Error(result.error || 'Не удалось сохранить выбор');
    } catch (error) { addErr('Выбор действует в этом окне, но не сохранён: ' + error); }
  });
  // авто-рост поля ввода
  input.addEventListener('input', () => {
    input.style.height = 'auto';
    input.style.height = Math.min(120, input.scrollHeight) + 'px';
  });

  // поток ответа агента
  const eventsReady = listen('agent:event', (e) => {
    const ev = e.payload || {};
    switch (ev.type) {
      case 'init':
        if (ev.session_id) sessionId = ev.session_id;
        // агент инициализирован (ev.tools — гранто-фильтрованный набор)
        break;
      case 'delta':
        if (!curBubble) startBot();
        curBubble.textContent += ev.text || '';
        scroll();
        break;
      case 'tool_use':
        addTool(ev.name || '?');
        break;
      case 'failed':
        if (ev.session_id) sessionId = ev.session_id;
        addErr(ev.message || 'Агент завершился без ответа. Попробуй ещё раз.');
        setBusy(false);
        curBubble = null;
        break;
      case 'done':
        if (ev.session_id) sessionId = ev.session_id;
        // финальный текст, если дельт не было
        if (ev.result && (!curBubble || !curBubble.textContent)) startBot().textContent = ev.result;
        setBusy(false);
        curBubble = null;
        break;
    }
  });

  // карточка подтверждения side-effect (PanelConfirmer)
  const confirmsReady = listen('agent:confirm', (e) => {
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
    const choice = (cls, text) => {
      const button = document.createElement('button');
      button.type = 'button'; button.className = 'cbtn ' + cls; button.textContent = text;
      return button;
    };
    const yes = choice('yes', 'Разрешить');
    const no = choice('no', 'Отклонить');
    const decide = async (approved) => {
      if (yes.disabled) return;
      yes.disabled = no.disabled = true;
      try {
        const result = await invoke('agent_confirm', { nonce: c.nonce, approved });
        btns.remove();
        box.appendChild(el('cresult', result?.ok === false
          ? 'Запрос уже завершён или истёк' : (approved ? '✓ разрешено' : '✕ отклонено')));
      } catch (error) {
        yes.disabled = no.disabled = false;
        box.appendChild(el('cresult', 'Не удалось отправить решение: ' + error));
      }
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

  setBusy(false);
  Promise.all([eventsReady, confirmsReady, invoke('agent_hosts')]).then(([, , hosts]) => {
    if (hosts?.ok === false) throw new Error(hosts.error || 'Не удалось проверить CLI');
    for (const option of provider.options) {
      const host = hosts.providers?.find((item) => item.id === option.value);
      option.disabled = !host?.available;
      option.textContent = (host?.label || option.textContent) + (host?.available ? '' : ' · CLI не найден');
    }
    const selected = [...provider.options].find((option) => option.value === hosts.selected);
    provider.value = selected && !selected.disabled ? hosts.selected : 'auto';
    hasProvider = !provider.selectedOptions[0]?.disabled;
    ready = true;
    setBusy(false);
  }).catch((error) => { addErr('Не удалось подготовить агента: ' + error); sub.textContent = 'ошибка подключения'; });
  input.focus();
})();
