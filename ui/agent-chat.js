/* Чат с главным агентом Jarvis (фаза 7).
 * Шлёт сообщение в agent_send, слушает поток agent:event
 * (init/delta/tool_use/done/failed) и карточки подтверждения agent:confirm
 * (резолв через agent_confirm). Нить разговора переживает закрытие окна:
 * id хранится в настройках (agent_chat_state / agent_chat_reset), а прошлые
 * реплики приезжают из agent_chat_history.
 *
 * Мест у чата два — вкладка панели и отдельное окно из трея, — но логика одна:
 * mount() навешивается на готовую разметку и получает команды демона объектом.
 * Две копии стрима разъехались бы на первой же правке; в этом коде так уже
 * было с парой кнопок запуска. */

(() => {
  /** Смонтировать чат на разметку `els` поверх команд `api`. */
  function mount(els, api) {
    const { msgs, input, sendBtn, sub, tag, newBtn } = els;
    const hintTpl = msgs.querySelector('.hint').cloneNode(true);

    let sessionId = null; // для многоходового диалога (--resume)
    let curBubble = null; // текущий стриминговый пузырь ассистента
    let toolsRow = null; // группа подряд идущих тул-чипов
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
      toolsRow = null;
      const row = el('msg ' + kind);
      row.appendChild(child);
      msgs.appendChild(row);
      scroll();
      return child;
    }
    const addUser = (t) => addRow('user', el('bubble', t));
    const addErr = (t) => addRow('err', el('bubble', t));
    const addNote = (t) => addRow('note', el('bubble', t));
    const startBot = () => (curBubble = addRow('assistant', el('bubble', '')));

    // Тул-вызов — не реплика: тот же чип и та же группа, что у чата сессии
    // (renderer.js, addToolChip), иначе в одной панели было бы два языка для
    // одного и того же события.
    function addTool(name) {
      clearHint();
      if (!toolsRow) { toolsRow = el('msg tools'); msgs.appendChild(toolsRow); }
      const chip = el('chip');
      chip.appendChild(el('tverb', name));
      toolsRow.appendChild(chip);
      curBubble = null;
      scroll();
    }

    function setBusy(v) {
      busy = v;
      sendBtn.disabled = v;
      sendBtn.textContent = v ? '…' : '⏎';
      sub.textContent = v ? 'думает…' : 'готов';
    }

    // Метка в шапке: разговор тянется из прошлого запуска окна. Держим её до
    // «Нового чата» — иначе после первой реплики человек снова гадает.
    const setResumed = (on) => { tag.hidden = !on; };

    // Восстановление: и id разговора, и сами реплики живут у демона, а не в
    // этом окне. Без ленты «продолжение» выглядело потерей переписки.
    (async () => {
      try {
        const st = await api.state();
        if (st && st.sessionId) { sessionId = st.sessionId; setResumed(true); }
      } catch (e) {
        // Молчать нельзя: иначе окно тихо начнёт новый диалог вместо прошлого.
        addErr('Не удалось узнать про прошлый разговор: ' + e);
      }
      let res;
      try {
        res = await api.history();
      } catch (e) {
        addErr('Не удалось прочитать прошлую переписку: ' + e);
        return;
      }
      const items = (res && res.items) || [];
      for (const it of items) {
        if (it.kind === 'tool') addTool(it.text);
        else if (it.role === 'user') addUser(it.text);
        else addRow('assistant', el('bubble', it.text));
      }
      curBubble = null; // история дорисована: следующая дельта начнёт свой пузырь
      const total = (res && res.total) || items.length;
      if (items.length) {
        if (total > items.length) addNote(`Показаны последние ${items.length} реплик из ${total}.`);
      } else if (sessionId && res && res.reason) {
        // Пустая лента при живом разговоре — повод объясниться, а не молчать.
        setHint('Прошлых реплик здесь нет: ' + res.reason);
      } else if (sessionId) {
        setHint('Продолжаю прошлый разговор: агент помнит, о чём шла речь.');
      }
      // Разговора ещё не было — приглашение из разметки и есть нормальный вид.
    })();

    async function newChat() {
      try {
        await api.reset();
      } catch (e) {
        addErr('Не удалось начать новый чат: ' + e); // старый id остался — так и скажем
        return;
      }
      sessionId = null;
      setResumed(false);
      curBubble = null;
      toolsRow = null;
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
        await api.send(text, sessionId);
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
    api.onEvent((ev0) => {
      const ev = ev0 || {};
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
    api.onConfirm((c0) => {
      const c = c0 || {};
      const cd = c.card || {};
      clearHint();
      toolsRow = null;

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
        api.confirm(c.nonce, approved);
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
  }

  /* Вкладка панели: разметка лежит в index.html, команды идут через мост.
   * Вкладку открывают много раз — монтируем ровно однажды, чтобы не
   * получить два обработчика на одном поле ввода. */
  window.initAgentChat = (root) => {
    const q = (id) => root.querySelector('#' + id);
    if (!root.dataset.mounted) {
      root.dataset.mounted = '1';
      const j = window.jarvis;
      mount(
        { msgs: q('agLog'), input: q('agInput'), sendBtn: q('agSend'), sub: q('agSub'), tag: q('agTag'), newBtn: q('agNew') },
        {
          state: () => j.agentChatState(),
          history: () => j.agentChatHistory(),
          reset: () => j.agentChatReset(),
          send: (message, sessionId) => j.agentSend(message, sessionId),
          confirm: (nonce, approved) => j.agentConfirm(nonce, approved),
          onEvent: (cb) => j.onAgentEvent(cb),
          onConfirm: (cb) => j.onAgentConfirm(cb),
        },
      );
    }
    q('agInput').focus();
  };

  /* Отдельное окно из трея: общего моста в нём нет — зовём Tauri напрямую.
   * Признак окна — его собственная разметка; в панели её нет, и файл остаётся
   * просто библиотекой. */
  if (document.getElementById('msgs')) {
    const { invoke } = window.__TAURI__.core;
    const { listen } = window.__TAURI__.event;

    // тема и краска: окно живёт вне общего моста, поэтому подписывается само
    window.jarvis = Object.assign(window.jarvis || {}, {
      getSettings: () => invoke('settings_get'),
      setSettings: (patch) => invoke('settings_set', { patch }),
      onAppearance: (cb) => { listen('appearance', (e) => cb(e.payload)); },
    });

    const g = (id) => document.getElementById(id);
    mount(
      { msgs: g('msgs'), input: g('input'), sendBtn: g('send'), sub: g('sub'), tag: g('tag'), newBtn: g('newChat') },
      {
        state: () => invoke('agent_chat_state'),
        history: () => invoke('agent_chat_history'),
        reset: () => invoke('agent_chat_reset'),
        send: (message, sessionId) => invoke('agent_send', { message, sessionId }),
        confirm: (nonce, approved) => invoke('agent_confirm', { nonce, approved }),
        onEvent: (cb) => listen('agent:event', (e) => cb(e.payload)),
        onConfirm: (cb) => listen('agent:confirm', (e) => cb(e.payload)),
      },
    );
  }
})();
