/* One command catalogue powers the root list and the module switcher. */
(() => {
  'use strict';
  const sections = [
    ['agent', 'Джарвис', 'Разговор с главным агентом', '9', 'sparkle'],
    ['list', 'Чаты', 'Активные сессии Claude и Codex', '1', 'chats-circle'],
    ['history', 'Проекты', 'Начать задачу на компьютере или удалённой машине', '2', 'folder-simple'],
    ['machines', 'Машины', 'Серверы, SSH-подключения и виртуальные машины', '8', 'server', 'VM Teleport hosts connections'],
    ['voicehist', 'Диктовка', 'История речи, словарь и восстановление текста', '4', 'microphone'],
    ['meetings', 'Встречи', 'Запись разговора и расшифровка', '7', 'record'],
    ['loops', 'Автоматизация', 'Задачи, которые выполняются по расписанию', '5', 'arrows-clockwise'],
    ['bundle', 'Команды агентов', 'Совместная работа нескольких агентов', '6', 'users-three'],
    ['stats', 'Аналитика ИИ', 'Харнес, инструменты, код, эффект и расход', '3', 'chart-bar'],
    ['settings', 'Настройки', 'Микрофон, модели и клавиши', ',', 'gear-six'],
  ];
  const ids = { agent: 'tabAgent', list: 'tabSessions', history: 'tabHistory', machines: 'tabMachines', voicehist: 'tabVoice', meetings: 'tabMeetings', loops: 'tabLoops', bundle: 'tabBundle', stats: 'tabStats', settings: 'tabSettings' };
  const icon = (name, size = 18) => window.jarvisIcons.create(name, size);
  const node = (tag, cls, text) => { const n = document.createElement(tag); n.className = cls; if (text) n.textContent = text; return n; };
  const matches = (item, terms) => terms.every(term => `${item.label} ${item.desc} ${item.keywords || ''}`.toLocaleLowerCase().includes(term));
  function init(api) {
    const query = document.getElementById('query');
    const launcher = document.getElementById('launcher');
    const nav = launcher.querySelector('.tabs');
    const recents = document.getElementById('launcherRecents');
    let rootItems = [], rootSelection = null, rootView = 'home';
    const workspaceAction = !window.__JARVIS_WORKSPACE__ && window.jarvis.openWorkspace ? {
      id: 'action:workspace', label: 'Рабочее окно', desc: 'Проекты и чаты в отдельном большом окне', glyph: 'app-window',
      run: async () => { try { const r = await window.jarvis.openWorkspace({}); if (!r?.ok) throw new Error(r?.error || 'Не удалось открыть окно'); } catch (e) { api.toast(String(e?.message || e)); } },
    } : null;
    let workspaceButton;
    if (workspaceAction) {
      workspaceButton = node('button', 'tab'); workspaceButton.type = 'button'; workspaceButton.id = 'openWorkspace';
      const copy = node('span', 'module-copy'); copy.append(node('strong', '', workspaceAction.label), node('small', '', workspaceAction.desc));
      const mark = node('span', 'module-icon'); mark.append(icon(workspaceAction.glyph, 21));
      workspaceButton.append(mark, copy, icon('arrow-square-out', 13)); workspaceButton.setAttribute('aria-label', workspaceAction.label);
      workspaceButton.addEventListener('click', workspaceAction.run);
      workspaceButton.addEventListener('focus', () => { rootSelection = workspaceAction.id; paintSelection(); });
      nav.append(workspaceButton);
    }
    const staticIcons = { tlSettings: 'sliders-horizontal', chatBack: 'arrow-left', qBack: 'arrow-left', tpClose: 'x', qpClose: 'x', srchClose: 'x', chgClose: 'x', docClose: 'x', send: 'arrow-up' };
    for (const [id, name] of Object.entries(staticIcons)) document.getElementById(id)?.querySelector('svg')?.replaceWith(icon(name));
    document.querySelector('.cmdrow > svg')?.replaceWith(icon('magnifying-glass'));
    document.getElementById('pageBack').append(icon('arrow-left'));
    document.getElementById('pageBack').addEventListener('click', api.back);
    document.getElementById('pageHome').addEventListener('click', () => api.navigate('home'));
    for (const [id, label, desc, key, glyph] of sections) {
      const b = document.getElementById(ids[id]);
      b.textContent = ''; b.title = desc; b.setAttribute('aria-label', label); b.dataset.module = id;
      const copy = node('span', 'module-copy'); copy.append(node('strong', '', label), node('small', '', desc));
      const mark = node('span', 'module-icon'); mark.append(icon(glyph, 21));
      const shortcut = node('kbd', 'tabkey', window.jarvisKeys.k(key)); shortcut.dataset.key = key;
      b.append(mark, copy, shortcut, icon('caret-right', 13));
      nav.appendChild(b);
      if (id === 'meetings') b.addEventListener('click', () => api.navigate(id));
      b.addEventListener('focus', () => { rootSelection = `module:${id}`; paintSelection(); });
    }
    function catalogue() {
      return [
        ...(workspaceAction ? [workspaceAction] : []),
        ...sections.map(([id, label, desc, key, glyph, keywords]) => ({ id: `module:${id}`, view: id, label, desc, keywords, key: window.jarvisKeys.k(key), glyph, run: () => api.navigate(id) })),
        ...[...(api.sessions() || [])].sort((a, b) => (b.updatedAt || b.createdAt || 0) - (a.updatedAt || a.createdAt || 0)).map(s => ({ id: `chat:${s.id}`, label: window.JarvisSessionState?.titleOf(s) || s.title || 'Чат без названия', desc: `${s.instanceLabel || s.agent || 'Claude'} · ${s.project || ''} · ${s.remote || 'Этот компьютер'}`, glyph: 'chat-circle', run: () => api.openSession(s) })),
      ];
    }
    function paintSelection(scroll = false) {
      for (const item of rootItems) {
        const selected = item.id === rootSelection;
        item.element.classList.toggle('launcher-selected', selected);
        if (selected) { query.setAttribute('aria-activedescendant', item.element.id); if (scroll) item.element.scrollIntoView?.({ block: 'nearest' }); }
      }
      if (!rootItems.length) query.removeAttribute('aria-activedescendant');
    }
    function refresh() {
      if (rootView !== 'home') return;
      const terms = query.value.toLocaleLowerCase().trim().split(/\s+/).filter(Boolean);
      rootItems = []; recents.textContent = '';
      const catalogueItems = catalogue();
      for (const item of catalogueItems.filter(item => item.view || item.id === 'action:workspace')) {
        const b = item.view ? document.getElementById(ids[item.view]) : workspaceButton; b.hidden = !matches(item, terms);
        if (!b.hidden) rootItems.push({ ...item, element: b });
      }
      const matchingChats = catalogueItems.filter(item => item.id.startsWith('chat:') && matches(item, terms));
      const recentItems = terms.length ? matchingChats : matchingChats.slice(0, 6);
      if (recentItems.length) recents.append(node('div', 'launcher-label', terms.length ? 'Найденные чаты' : 'Недавние чаты'));
      for (const [i, item] of recentItems.entries()) {
        const b = node('button', 'launcher-chat'); b.type = 'button'; b.id = `launcher-chat-${i}`;
        const copy = node('span', 'module-copy'); copy.append(node('strong', '', item.label), node('small', '', item.desc));
        b.append(icon(item.glyph, 21), copy, icon('caret-right', 13));
        b.addEventListener('click', item.run);
        b.addEventListener('focus', () => { rootSelection = item.id; paintSelection(); });
        recents.append(b); rootItems.push({ ...item, element: b });
      }
      nav.hidden = !rootItems.some(item => item.view || item.id === 'action:workspace');
      launcher.querySelector('.launcher-label').hidden = nav.hidden;
      document.getElementById('launcherEmpty').hidden = rootItems.length > 0;
      if (!rootItems.some(item => item.id === rootSelection)) rootSelection = rootItems[0]?.id || null;
      paintSelection();
    }
    const dialog = document.getElementById('commandDialog');
    const input = document.getElementById('commandQuery');
    const results = document.getElementById('commandResults');
    let selected = 0, items = [], previousFocus;
    function paint() {
      const terms = input.value.toLocaleLowerCase().trim().split(/\s+/).filter(Boolean);
      items = catalogue().filter(item => matches(item, terms));
      selected = Math.max(0, Math.min(selected, items.length - 1)); results.textContent = '';
      input.removeAttribute('aria-activedescendant');
      if (!items.length) results.append(node('div', 'command-empty', 'Ничего не найдено. Попробуй название раздела или чата.'));
      items.forEach((item, i) => {
        const b = node('button', 'command-result' + (i === selected ? ' selected' : ''));
        b.id = `command-result-${i}`; b.type = 'button'; b.tabIndex = -1;
        b.setAttribute('role', 'option'); b.setAttribute('aria-selected', String(i === selected));
        const copy = node('span', 'command-copy'); copy.append(node('strong', '', item.label), node('small', '', item.desc));
        b.append(icon(item.glyph), copy, node('kbd', '', item.key || '↵'));
        b.addEventListener('click', () => run(item)); results.append(b);
        if (i === selected) input.setAttribute('aria-activedescendant', b.id);
      });
      results.children[selected]?.scrollIntoView?.({ block: 'nearest' });
    }
    function close() { dialog.hidden = true; input.setAttribute('aria-expanded', 'false'); if (previousFocus?.isConnected) previousFocus.focus(); }
    function open() { previousFocus = document.activeElement; dialog.hidden = false; input.value = ''; selected = 0; input.setAttribute('aria-expanded', 'true'); paint(); input.focus(); }
    function run(item) { close(); if (item) Promise.resolve().then(item.run).catch(e => api.toast(String(e))); }
    input.addEventListener('input', () => { selected = 0; paint(); });
    document.getElementById('commandClose').addEventListener('click', close);
    dialog.addEventListener('click', e => { if (e.target === dialog) close(); });
    window.addEventListener('keydown', e => {
      if (dialog.hidden || e.isComposing || e.keyCode === 229) return;
      e.stopImmediatePropagation();
      if (e.key === 'Escape' || ((window.jarvisKeys.isMac ? e.metaKey : e.ctrlKey) && window.jarvisKeys.matches(e, 'k'))) { e.preventDefault(); close(); }
      else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); selected = Math.max(0, Math.min(items.length - 1, selected + (e.key === 'ArrowDown' ? 1 : -1))); paint(); }
      else if (e.key === 'Enter') { e.preventDefault(); run(items[selected]); }
      else if (e.key === 'Tab') { e.preventDefault(); (document.activeElement === input ? document.getElementById('commandClose') : input).focus(); }
    }, true);
    for (const id of ['workspaceCommands', 'pageCommands']) document.getElementById(id).addEventListener('click', open);
    window.jarvisWorkspace = {
      toggleCommands: () => dialog.hidden ? open() : close(), refresh,
      selection: () => rootSelection,
      runSelected: () => rootItems.find(item => item.id === rootSelection)?.run(),
      rootKey(e) {
        if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
          const index = rootItems.findIndex(item => item.id === rootSelection);
          const next = Math.max(0, Math.min(rootItems.length - 1, index + (e.key === 'ArrowDown' ? 1 : -1)));
          rootSelection = rootItems[next]?.id || null; paintSelection(true); return true;
        }
        if (e.key === 'Enter') { this.runSelected(); return true; }
        return false;
      },
      changed(view, saved) {
        rootView = view;
        document.documentElement.dataset.view = view;
        document.getElementById('pageTitle').textContent = sections.find(item => item[0] === view)?.[1] || (view === 'chat' ? 'Чат' : 'Вопрос агента');
        if (saved?.moduleSelection) rootSelection = saved.moduleSelection;
        if (view === 'home') refresh();
      },
    };
  }
  window.initWorkspace = init;
})();
