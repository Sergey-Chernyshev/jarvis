/* Project chat above existing local/SSH sessions. The daemon remains the source
 * of truth: no synthetic transcript messages or guessed terminal connections. */
(() => {
  'use strict';
  const statusText = { working: 'Работает', waiting: 'Нужен ответ', limit: 'Лимит', done: 'Готово', idle: 'Готов к задаче' };
  const providerLabel = id => id === 'codex' ? 'Codex' : !id || id === 'claude' ? 'Claude' : id;
  const accessLabel = s => s.controlMode === 'external' || !s.tmuxPane ? 'История чата' : statusText[s.status] || 'Подключён';
  const projectKey = s => JSON.stringify([s.remote || 'local', s.cwd || s.project || '']);
  const titleContextTags = [
    'environment_context', 'recommended_plugins', 'permissions instructions',
    'user_instructions', 'system-reminder', 'task-notification', 'developer_instructions',
    'app-context', 'skills_instructions', 'collaboration_mode', 'local-command-stdout',
    'local-command-caveat', 'turn_aborted', 'in-app-browser-context',
  ];
  function cleanTitle(value) {
    let rest = String(value || '').trim();
    // Match the known leading host envelopes used by the backend normalizer.
    // This only produces a label; transcript content is never changed here.
    while (rest) {
      if (/^# Files mentioned by the user:/i.test(rest)) {
        const request = /## My request:\s*/i.exec(rest);
        rest = request ? rest.slice(request.index + request[0].length).trim() : '';
        continue;
      }
      if (/^# AGENTS\.md instructions\b/i.test(rest)) {
        const end = /<\/INSTRUCTIONS>/i.exec(rest);
        rest = end ? rest.slice(end.index + end[0].length).trim() : '';
        continue;
      }
      const lower = rest.toLowerCase();
      const tag = titleContextTags.find(tag => lower.startsWith('<' + tag) && /[\s>]/.test(lower.charAt(tag.length + 1)));
      if (tag) {
        const closing = '</' + tag + '>', end = lower.indexOf(closing);
        rest = end < 0 ? '' : rest.slice(end + closing.length).trim();
        continue;
      }
      if (rest.startsWith('>>> APPROVAL REQUEST')) {
        const closing = '>>> APPROVAL REQUEST END', end = rest.indexOf(closing);
        rest = end < 0 ? '' : rest.slice(end + closing.length).trim();
        continue;
      }
      break;
    }
    return /^codex-auto-review$/i.test(rest) ? '' : rest.replace(/\s+/g, ' ').trim();
  }
  function titleOf(s = {}) {
    // Titles are labels, never an extra surface for generated protocol context.
    // The transcript normalizer owns content cleaning; real XML/code stays intact.
    for (const value of [s.title, s.lastPrompt]) {
      const text = cleanTitle(value);
      if (!text) continue;
      return text.length > 160 ? text.slice(0, 157) + '…' : text;
    }
    return 'Чат без названия';
  }
  function createProjectView(storage) {
    const key = 'jarvis.chat.projects.v1';
    let collapsed = new Set(), counts = new Map();
    function read() {
      try {
        const value = JSON.parse(storage?.getItem(key) || '{}');
        collapsed = new Set((Array.isArray(value.collapsed) ? value.collapsed : []).filter(v => typeof v === 'string'));
        counts = new Map(Object.entries(value.counts || {}).filter(([k, n]) => typeof k === 'string' && Number.isInteger(n) && n >= 5).map(([k, n]) => [k, Math.min(n, 10000)]));
      } catch { collapsed = new Set(); counts = new Map(); }
    }
    function save() { try { storage?.setItem(key, JSON.stringify({ collapsed: [...collapsed].slice(-500), counts: Object.fromEntries([...counts].slice(-500)) })); } catch {} }
    read();
    return { key, read, isOpen: id => !collapsed.has(id), count: id => counts.get(id) || 5,
      toggle(id) { collapsed.has(id) ? collapsed.delete(id) : collapsed.add(id); save(); },
      more(id) { counts.set(id, Math.min((counts.get(id) || 5) + 10, 10000)); save(); },
    };
  }
  function createLaunchLocation(storage) {
    const key = 'jarvis.chat.location.v1';
    function read() {
      try { const value = JSON.parse(storage?.getItem(key) || '{}'); return value && typeof value === 'object' && !Array.isArray(value) ? value : {}; }
      catch { return {}; }
    }
    const valid = path => typeof path === 'string' && path.startsWith('/') && !/[\x00-\x1f\x7f]/.test(path);
    return {
      machine() { const value = read().machine; return typeof value === 'string' ? value : ''; },
      path(machine) { const value = read().paths?.[machine]; return valid(value) ? value : ''; },
      remember(machine, path) {
        if (!machine) return;
        const value = read(); value.machine = machine;
        if (typeof path === 'string' && (path.trim() === '' || valid(path.trim()))) {
          value.paths = { ...(value.paths && typeof value.paths === 'object' ? value.paths : {}), [machine]: path.trim() };
        }
        try { storage?.setItem(key, JSON.stringify(value)); } catch {}
      },
    };
  }
  function groupSessions(sessions, query = '') {
    const terms = query.toLocaleLowerCase().trim().split(/\s+/).filter(Boolean);
    const groups = new Map();
    for (const s of sessions) {
      if (!terms.every(t => `${s.title || ''} ${s.project || ''} ${s.cwd || ''} ${s.remote || 'Этот компьютер'} ${s.instanceLabel || ''} ${s.agent || 'claude'} ${s.detail || ''}`.toLocaleLowerCase().includes(t))) continue;
      const key = projectKey(s);
      if (!groups.has(key)) groups.set(key, { key, name: s.project || s.cwd?.split('/').filter(Boolean).pop() || 'Без проекта', cwd: s.cwd, machine: s.remote || 'local', sessions: [] });
      groups.get(key).sessions.push(s);
    }
    for (const g of groups.values()) g.sessions.sort((a, b) => Number(!!b.pinned) - Number(!!a.pinned) || (b.createdAt || 0) - (a.createdAt || 0) || a.id.localeCompare(b.id));
    return [...groups.values()].sort((a, b) =>
      a.name.localeCompare(b.name, 'ru', { numeric: true, sensitivity: 'base' }) ||
      Number(a.machine !== 'local') - Number(b.machine !== 'local') ||
      a.machine.localeCompare(b.machine, 'ru') || a.key.localeCompare(b.key));
  }
  // A status snapshot can repeat after SSH reconnect. Completion is keyed by
  // turn/revision, waiting by the question, never by the polling timestamp.
  function attentionKey(s) {
    return JSON.stringify([s.id, s.status, s.status === 'done' ? s.doneAt : s.question?.at || s.providerTurnId || s.lifecycleRevision || s.lastPrompt || s.createdAt]);
  }
  function createAttention(saved = []) {
    const seen = new Set(saved), entries = new Map(), previous = new Map();
    return {
      update(sessions, activeId) {
        const live = new Set(sessions.map(s => s.id));
        for (const id of entries.keys()) if (!live.has(id)) entries.delete(id);
        for (const s of sessions) {
          const before = previous.get(s.id), key = attentionKey(s);
          const actionable = s.status === 'waiting' || s.status === 'limit' || (s.status === 'done' && before && before.status !== 'done');
          if (entries.has(s.id) && attentionKey(entries.get(s.id)) !== key) entries.delete(s.id);
          if (activeId === s.id) { seen.add(key); entries.delete(s.id); }
          else if (actionable && !seen.has(key)) entries.set(s.id, s);
          else if (entries.has(s.id)) entries.set(s.id, s);
          previous.set(s.id, s);
        }
        for (const id of previous.keys()) if (!live.has(id)) previous.delete(id);
        return [...entries.values()].sort((a, b) => Number(b.status !== 'done') - Number(a.status !== 'done'));
      },
      read(s) { seen.add(attentionKey(s)); entries.delete(s.id); },
      saved: () => [...seen].slice(-250),
    };
  }
  function capabilities(session, machines = []) {
    if (!session) return { state: 'unavailable', label: 'Чат недоступен', detail: 'Сессия больше не подключена.', readOnly: true, canSend: false, canConfigure: false, canTerminal: false };
    const external = session.controlMode === 'external', pane = !!session.tmuxPane;
    const readOnly = external || !pane;
    const online = !session.remote || !!machines.find(machine => machine.id === session.remote)?.online;
    const supported = !session.agent || ['claude', 'codex'].includes(session.agent);
    const base = { readOnly, online, canContinue: readOnly && online && supported && !!session.cwd, canSend: !readOnly && online, canConfigure: !readOnly && online && supported && ['idle', 'done'].includes(session.status), canTerminal: !readOnly && online };
    if (external) return { ...base, state: online ? 'readonly' : 'disconnected', label: online ? 'Продолжить этот чат' : 'Нет связи · история чата', detail: online ? 'Открой отдельное продолжение с историей в том же профиле и проекте. После подключения можно писать сообщения и работать с терминалом.' : 'Показана сохранённая история. Продолжение станет доступно после подключения машины.' };
    if (!pane) return { ...base, state: 'unavailable', label: 'Терминал недоступен', detail: 'История доступна для чтения. Подключённого терминала для сообщений и выбора модели сейчас нет.' };
    if (!online) return { ...base, state: 'disconnected', label: 'Нет связи', detail: `Подключаемся к ${session.remote}. Можно подготовить черновик; отправка станет доступна после подключения.` };
    if (session.status === 'waiting') return { ...base, state: 'waiting', label: 'Нужен ответ', detail: 'Ответь на вопрос агента. Модель можно изменить после завершения вопроса.' };
    if (session.status === 'working') return { ...base, state: 'working', label: 'Агент работает', detail: 'Можно дополнить задачу. Выбор модели станет доступен после ответа.' };
    if (session.status === 'limit') return { ...base, state: 'waiting', label: 'Достигнут лимит', detail: 'Дождись сброса лимита или продолжи в приложении агента.' };
    return { ...base, state: 'ready', label: 'Управляется из Jarvis', detail: 'Сообщения и настройки отправляются в этот чат.' };
  }
  function modelName(value) {
    if (typeof value !== 'string' || value.length > 256 || /[\x00-\x1f\x7f<>]/.test(value)) return '';
    const name = value.trim();
    return /^synthetic$/i.test(name) ? '' : name;
  }
  function selectedModel(options, value) {
    value = modelName(value);
    const text = value.toLocaleLowerCase();
    const matches = options.filter(([id, label]) => id.toLocaleLowerCase() === text || label.toLocaleLowerCase() === text);
    return matches.length === 1 ? matches[0][0] : value;
  }
  window.JarvisSessionState = { groupSessions, createAttention, attentionKey, projectKey, titleOf, createProjectView, createLaunchLocation, capabilities, selectedModel, modelName };

  const node = (tag, cls, text) => { const n = document.createElement(tag); n.className = cls || ''; if (text != null) n.textContent = text; return n; };
  const icon = name => window.jarvisIcons.create(name, 17);
  function button(label, glyph, run, cls = 'sw-button') {
    const b = node('button', cls); b.type = 'button'; b.title = label; b.setAttribute('aria-label', label);
    if (glyph) b.append(icon(glyph)); if (label) b.append(node('span', '', label));
    b.addEventListener('click', run); return b;
  }
  function select(label, options) {
    const s = node('select'); s.setAttribute('aria-label', label); s.title = label;
    for (const [value, text] of options) { const o = node('option', '', text); o.value = value; s.append(o); }
    return s;
  }
  const tokens = n => new Intl.NumberFormat('ru', { notation: 'compact', maximumFractionDigits: 1 }).format(n || 0);

  window.initSessionWorkspace = function (api) {
    const asyncUI = window.JarvisAsyncState;
    const bridge = window.jarvis, sidebar = document.getElementById('sessionSidebar'), welcome = document.getElementById('chatWelcome');
    const frame = document.getElementById('sessionFrame'), reply = document.getElementById('reply');
    let view = 'home', sessions = [], machines = [], inbox = false, notifications = [], launching = false, terminalOpen = false;
    let machineRequest = 0, projectRequest = 0, usageRequest = 0, usageId = null, usageAt = 0, launchTimer;
    let activeId = null, signature = '', pendingLaunch = null;
    let saved = []; try { saved = JSON.parse(localStorage.getItem('jarvis.chat.read') || '[]'); } catch {}
    const attention = createAttention(Array.isArray(saved) ? saved : []), projectView = createProjectView(localStorage);
    let focusedProject = null, projectFocusRequest = 0;
    const launchFiles = []; let readingLaunchFiles = 0;
    const launchLocation = createLaunchLocation(localStorage);
    let preferences = {}; try { preferences = JSON.parse(localStorage.getItem('jarvis.chat.launch.v1') || '{}'); } catch {}
    const drafts = new Map(), expanded = new Map(), launchEvents = new Map(), modelCatalogs = new Map();
    let catalogRequest = 0, catalogLoaded = false, changingControl = null;
    let projectArtwork = new Map(), artworkRequest = 0, artworkLoaded = false;
    async function loadArtwork() {
      const request = ++artworkRequest;
      try {
        const settings = await bridge.getSettings();
        if (request !== artworkRequest) return;
        projectArtwork = new Map((Array.isArray(settings?.projects) ? settings.projects : []).map(p => [JSON.stringify([p.machine || 'local', p.cwd]), p]));
        artworkLoaded = true; renderSidebar();
      } catch { /* Saved artwork stays visible when settings cannot refresh. */ }
    }
    const saveRead = () => { try { localStorage.setItem('jarvis.chat.read', JSON.stringify(attention.saved())); } catch {} };
    const current = () => sessions.find(s => s.id === activeId);
    const navigate = next => { inbox = false; frame.classList.remove('sidebar-open'); api.navigate(next); };
    const settings = pane => { if (pane === 'remotes') { navigate('machines'); return; } navigate('settings'); api.settingsPane(pane); };
    const open = s => { inbox = false; api.openSession(s); frame.classList.remove('sidebar-open'); };
    async function openWorkspace(route, detached = true) {
      try {
        const result = await bridge.openWorkspace({ ...route, detached });
        if (!result?.ok) throw new Error(result?.error || 'Не удалось открыть окно');
      } catch (e) { api.toast(String(e?.message || e)); }
    }
    async function copyText(text) {
      try { const result = await bridge.copyText(text); if (result?.ok === false) throw new Error(result.error); api.toast('Скопировано'); }
      catch (e) { api.toast('Не удалось скопировать: ' + String(e?.message || e)); }
    }
    function actionMenu(label, actions, cls = '') {
      const menu = node('details', 'sw-actions ' + cls), summary = node('summary');
      summary.setAttribute('aria-label', label); summary.title = label; summary.append(icon('dots-three'));
      const body = node('div', 'sw-action-menu'); body.setAttribute('role', 'group'); body.setAttribute('aria-label', label);
      for (const [text, glyph, run] of actions) body.append(button(text, glyph, () => { menu.open = false; run(); }));
      menu.append(summary, body);
      menu.addEventListener('toggle', () => { if (menu.open) for (const other of document.querySelectorAll('.sw-actions[open]')) if (other !== menu) other.open = false; });
      return menu;
    }
    document.addEventListener('click', e => { for (const menu of document.querySelectorAll('.sw-actions[open]')) if (!menu.contains(e.target)) menu.open = false; });
    function dismissMenu() {
      const menus = [...document.querySelectorAll('.sw-actions[open]')];
      for (const menu of menus) { menu.open = false; menu.querySelector('summary')?.focus(); }
      return menus.length > 0;
    }
    window.addEventListener('keydown', e => {
      if (e.isComposing || e.keyCode === 229 || window.jarvisShortcutRecording || e.key !== 'Escape' || !dismissMenu()) return;
      e.preventDefault(); e.stopImmediatePropagation();
    }, true);

    const top = node('div', 'sw-side-top');
    top.append(button('Новый чат', 'plus', () => { inbox = false; navigate('list'); prompt.focus(); }, 'sw-new'));
    const inboxButton = button('Входящие', 'tray', () => { inbox = !inbox; navigateInbox(); });
    const inboxCount = node('span', 'sw-count', '0'); inboxButton.append(inboxCount); top.append(inboxButton);
    const search = node('input', 'sw-search'); search.type = 'search'; search.placeholder = 'Найти чат или проект'; search.setAttribute('aria-label', 'Найти чат или проект');
    top.append(search); sidebar.append(top);
    const projectFilter = node('div', 'sw-project-filter'); projectFilter.hidden = true;
    projectFilter.append(button('Все проекты', 'arrow-left', () => { focusedProject = null; projectFilter.hidden = true; renderSidebar(); })); sidebar.append(projectFilter);
    const sideList = node('div', 'sw-projects'); sidebar.append(sideList);
    const sideBottom = node('div', 'sw-side-bottom');
    sideBottom.append(button('Все проекты', 'folder-simple', () => navigate('history')), button('Подключения', 'plugs-connected', () => settings('remotes')), button('Использование', 'chart-bar', () => navigate('stats')));
    const connectionCount = node('div', 'sw-connection-count', 'Проверяем подключения…'); sideBottom.append(connectionCount); sidebar.append(sideBottom);
    search.addEventListener('input', renderSidebar);

    const mobileToggle = button('Проекты и чаты', 'sidebar-simple', () => frame.classList.toggle('sidebar-open'), 'sw-mobile-toggle');
    document.getElementById('pageNavigation').prepend(mobileToggle);

    const hero = node('div', 'sw-hero');
    const title = node('h1', '', 'Что сделаем сегодня?');
    hero.append(node('div', 'sw-welcome-mark', 'jarvis'), title, node('p', 'sw-intro', 'Один чат с твоими агентами. На любой машине.'));
    const composer = node('form', 'sw-composer');
    const prompt = node('textarea', 'sw-prompt'); prompt.id = 'newChatPrompt'; prompt.rows = 3; prompt.placeholder = 'Опиши задачу, задай вопрос или продолжи идею…'; prompt.setAttribute('aria-label', 'Задача для агента');
    composer.append(prompt);
    const toolbar = node('div', 'sw-composer-tools');
    const provider = select('Агент нового чата', [['claude', 'Claude'], ['codex', 'Codex']]); provider.id = 'newChatProvider';
    const instance = select('Профиль Codex для новой задачи', []); instance.id = 'newChatInstance'; instance.hidden = true;
    const launchModel = select('Модель новой задачи', []); launchModel.id = 'newChatModel';
    function modelsForSession(session) {
      const catalog = modelCatalogs.get(JSON.stringify([session.remote || 'local', session.instanceId || 'default']));
      const options = session?.agent === 'codex' && catalog?.length ? catalog : api.models(session?.agent);
      return options.filter(([id, label]) => modelName(id) && modelName(label));
    }
    const modelPreferenceKey = () => JSON.stringify([machine.value || 'local', provider.value, instance.value || 'default']);
    function rememberLaunchPreferences() {
      preferences.provider = provider.value; preferences.models ||= {}; preferences.models[modelPreferenceKey()] = launchModel.value;
      try { localStorage.setItem('jarvis.chat.launch.v1', JSON.stringify(preferences)); } catch {}
    }
    launchModel.addEventListener('change', rememberLaunchPreferences);
    function updateLaunchModels() {
      const supported = ['claude', 'codex'].includes(provider.value);
      launchModel.hidden = !supported;
      const available = supported ? modelsForSession({ agent: provider.value, remote: machine.value === 'local' ? null : machine.value, instanceId: instance.value }) : [];
      const preferred = preferences.models?.[modelPreferenceKey()] ?? '';
      optionsFor(launchModel, [['', 'Модель по умолчанию'], ...available], preferred);
    }
    function rememberModels(profiles, host, defaultId) {
      for (const profile of profiles) {
        const options = (profile.models || []).filter(m => m.value && m.label).map(m => [m.value, m.label]);
        modelCatalogs.set(JSON.stringify([host, profile.id || profile.sourceId]), options);
        if ((profile.id || profile.sourceId) === defaultId) modelCatalogs.set(JSON.stringify([host, 'default']), options);
      }
    }
    async function loadChatModels(session) {
      if (session?.agent !== 'codex' || session.remote || catalogLoaded || !bridge.agentInstancesList) return;
      catalogLoaded = true; const request = ++catalogRequest;
      try {
        const result = await bridge.agentInstancesList();
        if (request !== catalogRequest) return;
        rememberModels(result.instances || [], 'local', result.defaultCodexInstance);
        updateLaunchModels(); if (view === 'chat') updateChat();
      } catch { catalogLoaded = false; }
    }
    let instanceRequest = 0, instancesReady = false;
    async function loadInstances() {
      const request = ++instanceRequest;
      instance.hidden = provider.value !== 'codex';
      if (instance.hidden || !bridge.agentInstancesList) { updateLaunchModels(); updateLaunchGate(); return; }
      const old = instance.value, host = machine.value || 'local';
      instancesReady = false; instance.replaceChildren(node('option', '', 'Загружаем профили…')); instance.setAttribute('aria-busy', 'true');
      instance.disabled = true; updateLaunchGate();
      try {
        const result = host === 'local' ? await bridge.agentInstancesList() : await bridge.remotesList();
        if (request !== instanceRequest) return;
        if (launchStatus.textContent.startsWith('Не удалось получить профили:')) launchStatus.textContent = '';
        const remote = (Array.isArray(result) ? result : result.remotes || []).find(r => r.name === host);
        const profiles = host === 'local' ? result.instances || [] : remote?.sources || [];
        rememberModels(profiles, host, result.defaultCodexInstance);
        instance.replaceChildren();
        for (const i of profiles.filter(i => i.agent === 'codex' && i.enabled !== false)) {
          const option = node('option', '', i.label || i.name || 'Codex'); option.value = i.id || i.sourceId; instance.append(option);
        }
        if ([...instance.options].some(o => o.value === old)) instance.value = old;
        else if (host === 'local') instance.value = result.defaultCodexInstance || instance.options[0]?.value || '';
        instancesReady = instance.options.length > 0 || (host !== 'local' && !!remote && !Array.isArray(remote.sources));
        if (!instance.options.length) { const option = node('option', '', instancesReady ? 'Профиль узла по умолчанию' : 'Профиль не найден'); option.value = ''; instance.append(option); }
      } catch (e) { if (request === instanceRequest) launchStatus.textContent = `Не удалось получить профили: ${e?.message || e}`; }
      finally { if (request === instanceRequest) { instance.disabled = false; instance.setAttribute('aria-busy', 'false'); updateLaunchModels(); updateLaunchGate(); } }
    }
    instance.addEventListener('change', updateLaunchModels);
    const permission = select('Разрешения новой задачи', [['ask', 'С подтверждением'], ['plan', 'Планирование'], ['yolo', 'Полный доступ']]);
    provider.addEventListener('change', () => {
      const plan = [...permission.options].find(o => o.value === 'plan'); plan.disabled = provider.value !== 'claude';
      if (plan.disabled && permission.value === 'plan') permission.value = 'ask';
      preferences.provider = provider.value;
      try { localStorage.setItem('jarvis.chat.launch.v1', JSON.stringify(preferences)); } catch {}
      loadInstances();
    });
    const isolateLabel = node('label', 'sw-isolate'), isolate = node('input'); isolate.type = 'checkbox'; isolateLabel.append(isolate, 'Отдельная ветка'); isolate.title = 'Создать отдельный worktree для этой задачи';
    if (['claude', 'codex'].includes(preferences.provider)) provider.value = preferences.provider;
    const launchAttachments = node('div', 'sw-attachments'); launchAttachments.hidden = true; composer.append(launchAttachments);
    function renderLaunchFiles() { window.JarvisAttachments.render(launchAttachments, launchFiles, id => { const index = launchFiles.findIndex(file => file.id === id); if (index >= 0) launchFiles.splice(index, 1); renderLaunchFiles(); }); updateLaunchGate(); }
    async function addLaunchFile(file) {
      if (launching || pendingLaunch) return;
      if (launchFiles.length >= 8) { api.toast('Не больше 8 файлов'); return; }
      readingLaunchFiles++;
      const placeholder = { id: 'reading-' + Date.now() + '-' + readingLaunchFiles, name: file.name || 'Изображение', loading: true };
      launchFiles.push(placeholder); renderLaunchFiles();
      try { const attachment = await window.JarvisAttachments.read(file); const index = launchFiles.indexOf(placeholder); if (index >= 0) launchFiles.splice(index, 1, attachment); renderLaunchFiles(); }
      catch (error) { const index = launchFiles.indexOf(placeholder); if (index >= 0) launchFiles.splice(index, 1); renderLaunchFiles(); asyncUI.status(launchStatus, error.message, 'error'); }
      finally { readingLaunchFiles--; updateLaunchGate(); }
    }
    window.JarvisAttachments?.bind(composer, addLaunchFile);
    const launchFile = node('input'); launchFile.type = 'file'; launchFile.multiple = true; launchFile.hidden = true;
    launchFile.addEventListener('change', () => { for (const file of launchFile.files) addLaunchFile(file); launchFile.value = ''; });
    const launchAttach = button('Прикрепить файл', 'paperclip', () => window.JarvisAttachments.pick(launchFile), 'sw-icon-button');
    const launchButton = button('Начать задачу', 'arrow-up', () => {}, 'sw-send'); launchButton.type = 'submit';
    toolbar.append(provider, instance, launchModel, permission, isolateLabel, launchAttach, launchFile, launchButton); composer.append(toolbar);
    const location = node('div', 'sw-location');
    const machine = select('Машина новой задачи', []); machine.id = 'newChatMachine';
    const cwd = node('input'); cwd.id = 'newChatDirectory'; cwd.placeholder = '/путь/к/проекту'; cwd.setAttribute('aria-label', 'Каталог проекта на выбранной машине'); cwd.setAttribute('list', 'chatProjectPaths'); cwd.autocomplete = 'off';
    const paths = node('datalist'); paths.id = 'chatProjectPaths';
    location.append(icon('desktop'), machine, icon('folder-simple'), cwd, paths);
    const connectionStatus = node('div', 'sw-composer-status');
    const launchStatus = node('div', 'sw-launch-status'); launchStatus.setAttribute('role', 'status');
    hero.append(composer, location, connectionStatus, launchStatus); welcome.append(hero);
    const startup = node('section', 'sw-startup'); startup.hidden = true; startup.setAttribute('aria-live', 'polite');
    const startupText = node('div', 'sw-startup-message'), startupState = node('p');
    const startupProgress = node('div', 'sw-startup-progress');
    const startupSteps = ['Подготовка сообщения', 'Запуск агента', 'Подключение чата'].map(text => node('span', '', text));
    startupProgress.append(...startupSteps);
    const startupNote = node('p', 'sw-startup-note'); let slowLaunchTimer;
    function launchStage(index, text) {
      startup.setAttribute('aria-busy', 'true');
      startupSteps.forEach((step, i) => { step.dataset.stage = i < index ? 'done' : i === index ? 'active' : 'pending'; if (i === index) step.setAttribute('aria-current', 'step'); else step.removeAttribute('aria-current'); });
      startupState.hidden = index !== 0; startupState.textContent = text;
      launchStatus.hidden = true;
    }
    function finishLaunchState() { launchStatus.hidden = false; clearTimeout(slowLaunchTimer); startup.setAttribute('aria-busy', 'false'); }
    startup.append(startupText, startupProgress, startupState, startupNote); hero.append(startup);
    const recent = node('div', 'sw-recent'); welcome.append(recent);
    const inboxPane = node('div', 'sw-inbox'); inboxPane.hidden = true; welcome.append(inboxPane);
    function navigateInbox() { frame.classList.remove('sidebar-open'); api.navigate('list'); renderWelcome(); }
    function projectTitle() { const path = cwd.value.trim().replace(/\/+$/, ''); title.textContent = path ? `Что сделаем в ${path.split('/').pop()}?` : 'Что сделаем сегодня?'; }
    cwd.addEventListener('input', () => { launchLocation.remember(machine.value, cwd.value); projectTitle(); updateLaunchGate(); });
    cwd.addEventListener('change', () => launchLocation.remember(machine.value, cwd.value));
    prompt.addEventListener('input', updateLaunchGate);
    function renderWelcome() {
      hero.hidden = inbox; recent.hidden = inbox; inboxPane.hidden = !inbox;
      inboxPane.replaceChildren();
      const loadState = api.sessionLoadState?.() || 'ready';
      if (loadState !== 'ready' && !sessions.length) {
        const root = inbox ? inboxPane : recent;
        if (root.dataset.loadState !== loadState || !root.children.length) {
          root.dataset.loadState = loadState;
          root.replaceChildren(loadState === 'loading' ? asyncUI.skeleton('list', 'Загружаем чаты') : asyncUI.message({ title: 'Не удалось загрузить чаты', detail: api.sessionLoadError?.() || 'Проверь подключение.', kind: 'error', action: 'Повторить', onAction: () => api.retrySessions?.() }));
        }
        return;
      }
      delete recent.dataset.loadState;
      if (inbox) {
        const h = node('div', 'sw-inbox-head'); h.append(node('h1', '', 'Входящие'), button('Прочитать всё', 'checks', () => { notifications.forEach(s => attention.read(s)); saveRead(); refresh(sessions); })); inboxPane.append(h);
        inboxPane.append(node('p', 'sw-intro', 'Ответы, вопросы и лимиты, которым нужно твоё внимание.'));
        for (const s of notifications) inboxPane.append(sessionButton(s, true));
        if (!notifications.length) inboxPane.append(node('div', 'sw-empty', 'Всё спокойно. Новые вопросы и результаты появятся здесь.'));
      } else {
        const wanted = [...sessions].sort((a, b) => (b.createdAt || 0) - (a.createdAt || 0) || a.id.localeCompare(b.id)).slice(0, 4);
        const existing = new Map([...recent.querySelectorAll('[data-session-id]')].map(el => [el.dataset.sessionId, el]));
        const children = [recent.querySelector('h2') || node('h2', '', 'Последние чаты')];
        for (const session of wanted) {
          const row = existing.get(session.id) || sessionButton(session, true);
          updateSessionButton(row, session, true); children.push(row);
        }
        if (!wanted.length) children.push(recent.querySelector('.sw-empty') || node('p', 'sw-empty', 'Запусти задачу здесь или открой Claude / Codex в терминале.'));
        for (let i = 0; i < children.length; i++) if (recent.children[i] !== children[i]) recent.insertBefore(children[i], recent.children[i] || null);
        for (const child of [...recent.children]) if (!children.includes(child)) child.remove();
      }
    }
    function sessionButton(s, wide = false) {
      const b = button('', null, () => open(sessions.find(session => session.id === s.id) || s), 'sw-session' + (s.id === activeId ? ' selected' : '') + (wide ? ' wide' : ''));
      b.dataset.sessionId = s.id;
      b.setAttribute('aria-label', `${titleOf(s)}, ${accessLabel(s)}`);
      b.title = `${titleOf(s)}\n${s.instanceLabel || providerLabel(s.agent)} · ${accessLabel(s)}`;
      if (s.id === activeId) b.setAttribute('aria-current', 'page');
      const dot = node('span', 'sw-dot ' + s.status); dot.setAttribute('aria-hidden', 'true');
      const copy = node('span', 'sw-session-copy');
      copy.append(node('strong', '', titleOf(s)));
      if (wide) copy.append(node('small', '', `${s.project || 'Проект'} · ${s.instanceLabel || s.remote || 'Этот компьютер'} · ${accessLabel(s)}`));
      b.append(dot, copy);
      if (s.pinned) b.append(icon('push-pin'));
      if (notifications.some(n => n.id === s.id)) b.append(node('span', 'sw-unread'));
      return b;
    }
    function updateSessionButton(button, session, wide = false) {
      button.querySelector('.sw-dot').className = 'sw-dot ' + session.status;
      button.querySelector('strong').textContent = titleOf(session);
      if (wide) button.querySelector('small').textContent = `${session.project || 'Проект'} · ${session.instanceLabel || session.remote || 'Этот компьютер'} · ${accessLabel(session)}`;
      button.setAttribute('aria-label', `${titleOf(session)}, ${accessLabel(session)}`);
      button.title = `${titleOf(session)}\n${session.instanceLabel || providerLabel(session.agent)} · ${accessLabel(session)}`;
    }
    let sidebarSignature = '';
    function sessionRow(s) {
      const row = node('div', 'sw-session-row' + (s.id === activeId ? ' selected' : ''));
      const actions = [];
      if (bridge.openWorkspace) actions.push(['В отдельном окне', 'arrow-square-out', () => openWorkspace({ sessionId: s.id, project: s.cwd, remote: s.remote })]);
      if (s.controlMode === 'external' && !s.remote || s.tmuxPane) actions.push([s.controlMode === 'external' && !s.remote ? 'Открыть Codex' : 'Открыть терминал', 'app-window', () => api.terminal(s)]);
      if (bridge.setPin) actions.push([s.pinned ? 'Открепить' : 'Закрепить', 'push-pin', async () => {
        try { const r = await bridge.setPin(s.id, !s.pinned); if (r?.ok === false) throw new Error(r.error); }
        catch (e) { api.toast(String(e?.message || e)); }
      }]);
      actions.push(['Копировать название', 'copy', () => copyText(titleOf(s))]);
      row.append(sessionButton(s), actionMenu('Действия чата «' + titleOf(s) + '»', actions));
      return row;
    }
    function renderSidebar() {
      const groups = groupSessions(sessions, search.value).filter(g => !focusedProject || g.key === focusedProject);
      const loadState = api.sessionLoadState?.() || 'ready';
      if (!sessions.length && loadState !== 'ready') {
        if (sideList.dataset.loadState !== loadState) { sideList.dataset.loadState = loadState; sideList.replaceChildren(loadState === 'loading' ? asyncUI.skeleton('list', 'Загружаем проекты и чаты') : asyncUI.message({ title: 'Чаты недоступны', detail: 'Не удалось получить список.', kind: 'error', action: 'Повторить', onAction: () => api.retrySessions?.() })); }
        sidebarSignature = ''; return;
      }
      delete sideList.dataset.loadState;
      const next = JSON.stringify([groups.map(g => [g.key, projectArtwork.get(g.key), projectView.isOpen(g.key), projectView.count(g.key), g.sessions.map(s => [s.id, titleOf(s), s.pinned, s.controlMode, !!s.tmuxPane])]), search.value, activeId, notifications.map(s => s.id)]);
      if (sidebarSignature === next) {
        for (const row of sideList.querySelectorAll('[data-session-id]')) { const session = sessions.find(s => s.id === row.dataset.sessionId); if (session) updateSessionButton(row, session); }
        return;
      }
      sidebarSignature = next;
      const focusId = document.activeElement?.dataset.sessionId, scrollTop = sideList.scrollTop;
      sideList.replaceChildren();
      for (const g of groups) {
        const block = node('section', 'sw-project');
        const head = node('div', 'sw-project-head');
        const project = projectArtwork.get(g.key);
        const name = project?.name || g.name;
        const isOpen = projectView.isOpen(g.key);
        const toggle = button(name, isOpen ? 'folder-open' : 'folder-simple', () => {
          projectView.toggle(g.key); renderSidebar();
          [...sideList.querySelectorAll('.sw-project-toggle')].find(el => el.dataset.projectKey === g.key)?.focus();
        }, 'sw-project-toggle');
        toggle.dataset.projectKey = g.key;
        const caret = icon(isOpen ? 'caret-down' : 'caret-right'); caret.classList.add('sw-project-caret'); toggle.prepend(caret);
        toggle.title = `${g.cwd || g.name}\n${g.machine === 'local' ? 'Этот компьютер' : g.machine}`; toggle.setAttribute('aria-expanded', String(isOpen));
        const add = button('Новый чат в ' + g.name, 'plus', () => { navigate('list'); machine.value = g.machine; cwd.value = g.cwd || ''; launchLocation.remember(machine.value, cwd.value); projectTitle(); loadProjects(false); prompt.focus(); }, 'sw-project-add');
        const actions = [];
        if (bridge.openWorkspace) actions.push(['В отдельном окне', 'arrow-square-out', () => openWorkspace({ project: g.cwd, remote: g.machine === 'local' ? undefined : g.machine })]);
        if (g.cwd) actions.push(['Копировать путь', 'copy', () => copyText(g.cwd)]);
        head.append(toggle, add); if (actions.length) head.append(actionMenu('Действия проекта «' + name + '»', actions));
        const host = node('div', 'sw-project-host', g.machine === 'local' ? 'Этот компьютер' : g.machine); host.hidden = g.machine === 'local';
        block.append(head, host);
        if (isOpen) {
          const count = search.value.trim() ? g.sessions.length : projectView.count(g.key);
          for (const s of g.sessions.slice(0, count)) block.append(sessionRow(s));
          if (g.sessions.length > count) block.append(button('Показать больше', null, () => { projectView.more(g.key); renderSidebar(); }, 'sw-show-more'));
        }
        sideList.append(block);
      }
      if (!groups.length) sideList.append(node('p', 'sw-empty', sessions.length ? 'Ничего не найдено' : 'Твои проекты появятся здесь'));
      if (focusId) [...sideList.querySelectorAll('[data-session-id]')].find(n => n.dataset.sessionId === focusId)?.focus({ preventScroll: true });
      sideList.scrollTop = scrollTop;
    }
    window.addEventListener('storage', event => { if (event.key === projectView.key) { projectView.read(); renderSidebar(); } });
    async function loadMachines() {
      if (!machines.length) { asyncUI.status(connectionStatus, 'Подключаем машины…'); machine.disabled = true; }
      const request = ++machineRequest;
      try {
        const result = await bridge.machinesList(); if (request !== machineRequest) return;
        if (!Array.isArray(result)) throw new Error('Не удалось прочитать подключения');
        const before = machine.value;
        machines = result; machine.disabled = false; connectionStatus.replaceChildren(); machine.replaceChildren();
        for (const m of machines) { const o = node('option', '', `${m.kind === 'local' ? 'Этот компьютер' : m.name}${m.online ? '' : ' · нет связи'}`); o.value = m.id; o.disabled = !m.online; machine.append(o); }
        const selected = before || launchLocation.machine();
        if (selected) {
          if (!machines.some(m => m.id === selected)) { const o = node('option', '', `${selected} · недоступна`); o.value = selected; o.disabled = true; machine.append(o); }
          machine.value = selected;
        }
        if (!before && !cwd.value.trim()) { cwd.value = launchLocation.path(machine.value); projectTitle(); }
        connectionCount.textContent = `${machines.filter(m => m.online).length} из ${machines.length} машин на связи`;
        renderSidebar();
        updateLaunchGate(); if (!before) await loadProjects(false); if (view === 'chat') updateChat();
      } catch (e) { if (request !== machineRequest) return; machine.disabled = false; connectionCount.textContent = 'Подключения недоступны'; connectionStatus.replaceChildren(asyncUI.message({ title: 'Не удалось обновить подключения', detail: String(e?.message || e), kind: 'error', action: 'Повторить', onAction: loadMachines })); launchButton.disabled = true; }
    }
    async function loadProjects(clear = true) {
      const request = ++projectRequest, id = machine.value; paths.replaceChildren();
      loadInstances();
      if (clear) { cwd.value = launchLocation.path(id); projectTitle(); }
      updateLaunchGate();
      if (!id) return;
      asyncUI.status(connectionStatus, 'Загружаем проекты…');
      try {
        const result = await bridge.projectsList(id); if (request !== projectRequest || id !== machine.value) return;
        if (!result?.ok || !Array.isArray(result.projects)) throw new Error(result?.error || 'Не удалось загрузить проекты');
        connectionStatus.replaceChildren();
        for (const p of result.projects) if (p.cwd) { const o = node('option'); o.value = p.cwd; o.label = p.name || p.project || p.cwd; paths.append(o); }
      } catch (e) { if (request !== projectRequest || id !== machine.value) return; connectionStatus.replaceChildren(asyncUI.message({ title: 'Не удалось загрузить проекты', detail: 'Укажи каталог вручную или повтори загрузку.', kind: 'error', action: 'Повторить', onAction: () => loadProjects(false) })); }
    }
    function updateLaunchGate() { launchButton.disabled = readingLaunchFiles > 0 || launching || !!pendingLaunch || (provider.value === 'codex' && (instance.disabled || !instancesReady)) || (!prompt.value.trim() && !launchFiles.length) || !cwd.value.trim() || !machines.find(m => m.id === machine.value)?.online; asyncUI.button(launchButton, launching || !!pendingLaunch || readingLaunchFiles > 0, readingLaunchFiles ? 'Читаем файлы…' : launching || pendingLaunch ? 'Создаём чат…' : 'Начать задачу');
      if (readingLaunchFiles) asyncUI.status(launchStatus, `Читаем файлы: ${readingLaunchFiles}`); else if (launchStatus.dataset.state === 'loading' && !launching && !pendingLaunch) { launchStatus.replaceChildren(); delete launchStatus.dataset.state; }
    }
    machine.addEventListener('change', () => { launchLocation.remember(machine.value); launchStatus.textContent = ''; loadProjects(); });
    composer.addEventListener('submit', async e => {
      e.preventDefault(); if (launching || launchButton.disabled) return;
      const text = prompt.value.trim(), dir = cwd.value.trim(), host = machine.value;
      const files = launchFiles.slice(), agent = provider.value, chosenModel = launchModel.value || null, chosenInstance = agent === 'codex' ? instance.value || null : null, mode = permission.value, isolated = isolate.checked;
      if ((!text && !files.length) || !dir) { launchStatus.textContent = !dir ? 'Укажи каталог проекта на выбранной машине.' : 'Напиши задачу для агента.'; (!dir ? cwd : prompt).focus(); return; }
      if (!dir.startsWith('/')) { launchStatus.textContent = 'Укажи полный путь к каталогу, начиная с /.'; cwd.focus(); return; }
      launchLocation.remember(host, dir);
      launching = true; updateLaunchGate(); launchStatus.textContent = files.length ? 'Загружаем вложения…' : 'Запускаем агента…';
      composer.hidden = location.hidden = true; startup.hidden = false; startupText.textContent = [text, ...files.map(file => file.name)].filter(Boolean).join('\n');
      startupNote.textContent = `${providerLabel(agent)} · ${chosenModel || 'Модель по умолчанию'}`; launchStage(files.length ? 0 : 1, launchStatus.textContent);
      clearTimeout(slowLaunchTimer); slowLaunchTimer = setTimeout(() => { if (launching || pendingLaunch) startupNote.textContent = 'Запуск занимает больше обычного. Можно открыть другой чат — задача продолжит подключаться.'; }, 8000);
      const launched = { ids: new Set(sessions.map(s => s.id)), host, dir, agent: provider.value, instanceId: provider.value === 'codex' ? instance.value || null : null, at: Date.now(), text };
      try {
        const paths = await window.JarvisAttachments.save(files, host, (done, total) => { if (total) launchStage(0, `Загружено файлов: ${done} из ${total}`); });
        launchStage(1, 'Запускаем агента…');
        const task = window.JarvisAttachments.prompt(text, paths);
        const result = await bridge.launchSession(dir, agent, null, host, { mode, isolate: isolated, task, container: false, model: chosenModel, instanceId: chosenInstance });
        if (!result?.ok) throw new Error(result?.error || 'Не удалось запустить агента');
        if (prompt.value.trim() === text) prompt.value = '';
        launchFiles.splice(0, files.length); renderLaunchFiles();
        pendingLaunch = { ...launched, task, files, model: chosenModel, dir: result.cwd || dir, pane: result.pane || null, launchId: result.launchId || null };
        launchStatus.textContent = 'Агент запускается. Чат откроется после подключения сессии.';
        launchStage(2, 'Подключаем сессию…');
        clearTimeout(launchTimer); launchTimer = setTimeout(() => {
          if (pendingLaunch) {
            if (!prompt.value) prompt.value = pendingLaunch.text;
            launchFiles.push(...pendingLaunch.files); renderLaunchFiles();
            pendingLaunch = null; composer.hidden = location.hidden = false; startup.hidden = true; finishLaunchState();
            launchStatus.textContent = 'Сессия ещё не подключилась. Проверь терминал и интеграцию в настройках.';
            updateLaunchGate();
          }
        }, 95000);
        if (result.launchId && launchEvents.has(result.launchId)) applyLaunchEvent(launchEvents.get(result.launchId));
        maybeOpenLaunched();
      } catch (e) { finishLaunchState(); launchStatus.dataset.state = 'error'; launchStatus.replaceChildren(asyncUI.message({ title: 'Не удалось создать чат', detail: String(e?.message || e), kind: 'error', action: 'Повторить', onAction: () => composer.requestSubmit() })); composer.hidden = location.hidden = false; startup.hidden = true; }
      finally { launching = false; updateLaunchGate(); }
    });
    prompt.addEventListener('keydown', e => { if (e.key === 'Enter' && !e.shiftKey && !e.altKey && !e.isComposing && e.keyCode !== 229) { e.preventDefault(); composer.requestSubmit(); } });
    function maybeOpenLaunched() {
      if (!pendingLaunch) return;
      const p = pendingLaunch;
      const matches = sessions.filter(s => (p.boundId ? s.id === p.boundId : !p.launchId && !p.ids.has(s.id) && (p.pane ? s.tmuxPane === p.pane : s.cwd === p.dir)) && (s.remote || 'local') === p.host && (s.agent || 'claude') === p.agent && (!p.instanceId || s.instanceId === p.instanceId));
      if (matches.length === 1) {
        composer.hidden = location.hidden = false; startup.hidden = true; finishLaunchState();
        api.trackLaunchMessage?.(matches[0].id, {
          key: p.launchId || `launch:${p.at}`, text: p.task, files: p.files,
          displayText: p.text,
          status: p.delivered ? 'Отправлено' : 'Отправляем…', kind: p.delivered ? 'success' : 'loading',
        });
        const shouldOpen = !p.opened && view === 'list' && !inbox;
        p.opened = true;
        if (p.delivered || !p.launchId) { pendingLaunch = null; clearTimeout(launchTimer); }
        updateLaunchGate(); launchStatus.textContent = 'Сессия подключена.';
        if (shouldOpen) open(matches[0]);
      }
    }

    // Existing transcript and attachments stay in renderer.js. This toolbar
    // invokes the same model/effort/reply commands as its keyboard palette.
    const chat = document.getElementById('chat'), inputBox = chat.querySelector('.chatinput');
    const chatHead = chat.querySelector('.chathead');
    const workspaceOpen = button('Открыть рабочее окно', 'arrow-square-out', () => {
      const s = current(); if (s) openWorkspace({ sessionId: s.id, project: s.cwd, remote: s.remote }, false);
    }, 'sw-workspace-open');
    workspaceOpen.hidden = true; chatHead.append(workspaceOpen);
    const chatMenu = actionMenu('Действия чата', [], 'sw-chat-actions');
    const chatMenuBody = chatMenu.querySelector('.sw-action-menu');
    if (bridge.openWorkspace) chatMenuBody.append(button('В отдельном окне', 'arrow-square-out', () => {
      chatMenu.open = false; const s = current(); if (s) openWorkspace({ sessionId: s.id, project: s.cwd, remote: s.remote });
    }));
    chatMenuBody.append(button('Копировать название', 'copy', () => { chatMenu.open = false; if (current()) copyText(titleOf(current())); }));
    const metadata = node('div', 'sw-chat-metadata'), projectPath = node('span'); metadata.append(projectPath);
    for (const id of ['chatModel', 'chatRemote', 'chatBundle', 'chatSub']) { const el = document.getElementById(id); if (el) metadata.append(el); }
    chatMenuBody.append(metadata);
    for (const id of ['sumToggle', 'previewBtn', 'searchBtn', 'changesBtn']) {
      const el = document.getElementById(id); if (el) { chatMenuBody.append(el); el.addEventListener('click', () => { chatMenu.open = false; }); }
    }
    chatHead.append(chatMenu);
    inputBox.querySelector('svg')?.remove(); inputBox.querySelector('.cmdhintpill')?.remove();
    const replyTools = node('div', 'sw-reply-tools');
    const model = select('Модель текущего чата', []), effort = select('Глубина рассуждений текущего чата', []);
    const modelLabel = node('span', 'sw-model-static');
    const deliveryStatus = node('div', 'sw-composer-status');
    const controlStatus = node('div', 'sw-control-status'); controlStatus.setAttribute('role', 'status');
    const attach = button('Прикрепить файл', 'paperclip', () => window.JarvisAttachments.pick(file), 'sw-icon-button');
    const file = node('input'); file.type = 'file'; file.multiple = true; file.hidden = true;
    file.addEventListener('change', () => { for (const f of file.files) api.attach(f); file.value = ''; });
    const send = button('Отправить сообщение', 'arrow-up', () => api.send(), 'sw-send'); send.id = 'chatSend';
    replyTools.append(model, effort, modelLabel, node('span', 'sw-spacer'), attach, file, send); inputBox.append(replyTools, controlStatus, deliveryStatus);
    const capabilityBar = node('div', 'sw-capability'); capabilityBar.setAttribute('role', 'status');
    const capabilityIcon = node('span', 'sw-capability-icon'), capabilityCopy = node('div', 'sw-capability-copy');
    const capabilityTitle = node('strong'), capabilityDetail = node('span'); capabilityCopy.append(capabilityTitle, capabilityDetail); capabilityBar.append(capabilityIcon, capabilityCopy);
    inputBox.before(capabilityBar);
    const chatContext = node('div', 'sw-chat-context');
    const contextLabel = node('span'); const terminalToggle = button('Терминал', 'terminal-window', () => { terminalOpen = !terminalOpen; terminal.hidden = !terminalOpen; terminalToggle.setAttribute('aria-expanded', String(terminalOpen)); pollTerminal(); });
    terminalToggle.setAttribute('aria-expanded', 'false');
    const externalOpen = button('Открыть Codex', 'arrow-square-out', () => { const s = current(); if (s) api.terminal(s); }); externalOpen.hidden = true;
    const continuations = new Map();
    const continueButton = button('Продолжить в Jarvis', 'arrow-right', continueChat); continueButton.classList.add('sw-continue'); continueButton.hidden = true;
    const continueStatus = node('span', 'sw-continue-status'); continueStatus.setAttribute('role', 'status');
    chatContext.append(contextLabel, continueButton, continueStatus, externalOpen, terminalToggle); inputBox.after(chatContext);
    function openContinuation(sourceId, explicit = false) {
      const pending = continuations.get(sourceId);
      const next = pending?.boundId && sessions.find(s => s.id === pending.boundId);
      if (next?.id === sourceId) { clearTimeout(pending.timer); continuations.delete(sourceId); return false; }
      if (next && (!pending.opened || explicit) && view === 'chat' && activeId === sourceId) { pending.opened = true; open(next); return true; }
      return false;
    }
    function applyContinuationEvent(event) {
      for (const [sourceId, pending] of continuations) {
        if (!pending.launchId || event.launchId !== pending.launchId) continue;
        if (event.status === 'failed') {
          pending.status = 'failed'; pending.error = event.error || 'Не удалось подключить продолжение';
        } else if (event.sessionId) { pending.boundId = event.sessionId; pending.status = 'ready'; }
        if (pending.status !== 'starting') clearTimeout(pending.timer);
        if (!openContinuation(sourceId) && activeId === sourceId) updateChat();
      }
    }
    async function continueChat() {
      const source = current();
      if (!bridge.continueSession || !capabilities(source, machines).canContinue) return;
      if (openContinuation(source.id, true)) return;
      if (continuations.get(source.id)?.terminalId) {
        terminalOpen = true; terminal.hidden = false; terminalToggle.setAttribute('aria-expanded', 'true'); pollTerminal(); return;
      }
      if (continuations.get(source.id)?.status === 'starting') return;
      const pending = { status: 'starting' }; continuations.set(source.id, pending); updateChat();
      pending.timer = setTimeout(() => {
        if (pending.status !== 'starting') return;
        pending.status = 'failed'; pending.error = 'Подключение заняло больше обычного. Открой терминал запуска и проверь состояние агента.';
        if (activeId === source.id) updateChat();
      }, 100000);
      try {
        const result = await bridge.continueSession(source.id);
        if (!result?.ok) throw new Error(result?.error || 'Не удалось открыть продолжение');
        pending.launchId = result.launchId; pending.boundId = result.sessionId; pending.terminalId = result.terminalId;
        if (!pending.launchId && !pending.boundId) throw new Error('Агент не подтвердил запуск продолжения');
        if (pending.boundId) { pending.status = 'ready'; clearTimeout(pending.timer); }
        if (result.launchId && launchEvents.has(result.launchId)) applyContinuationEvent(launchEvents.get(result.launchId));
        openContinuation(source.id);
      } catch (error) { clearTimeout(pending.timer); pending.status = 'failed'; pending.error = error?.message || String(error); }
      if (activeId === source.id) updateChat();
    }
    const usage = node('details', 'sw-chat-usage'); usage.hidden = true; chatMenuBody.append(usage);
    const usageSummary = node('summary', '', 'Использование'), usageBody = node('div', 'sw-usage-details'); usage.append(usageSummary, usageBody);
    usage.addEventListener('toggle', () => { if (activeId) expanded.set(activeId, { ...expanded.get(activeId), usage: usage.open }); });
    const terminalView = window.JarvisTerminal?.create({ bridge, toast: api.toast, openExternal: () => { const s = current(); if (s) api.terminal(s); } });
    const terminal = terminalView?.root || node('section', 'sw-terminal', 'Терминал не загрузился. Перезапусти Jarvis.');
    terminal.hidden = true; chat.append(terminal);
    function pollTerminal() {
      const s = current();
      const startup = continuations.get(activeId)?.terminalId;
      terminalView?.setSession(startup || activeId, terminalOpen && view === 'chat' && !document.hidden && (!!startup || s?.controlMode !== 'external'), s?.remote || 'Этот компьютер');
    }
    document.addEventListener('visibilitychange', () => { pollTerminal(); refresh(sessions); if (!document.hidden && ['list', 'chat', 'history'].includes(view)) loadMachines(); });
    window.addEventListener('focus', () => refresh(sessions));
    async function changeValue(control, fn) {
      const id = activeId, session = current();
      if (!capabilities(session, machines).canConfigure || changingControl) { updateChat(); return; }
      const value = control.value;
      if (!modelName(value)) { updateChat(); return; }
      changingControl = id; controlStatus.textContent = 'Применяем настройку…'; updateChat();
      try {
        const result = await fn(id, value);
        if (!result?.ok) {
          if (id === activeId && result?.showTerminal) { terminalOpen = true; terminal.hidden = false; terminalToggle.setAttribute('aria-expanded', 'true'); pollTerminal(); }
          throw new Error(result?.error || 'Не удалось изменить настройку');
        }
        if (id === activeId) controlStatus.textContent = 'Настройка применена';
      } catch (error) { if (id === activeId) controlStatus.textContent = error?.message || String(error); api.toast(error?.message || String(error)); }
      finally { if (changingControl === id) changingControl = null; if (id === activeId) updateChat(); }
    }
    model.addEventListener('change', () => changeValue(model, bridge.setModel)); effort.addEventListener('change', () => changeValue(effort, bridge.setEffort));
    function optionsFor(control, options, value) {
      value = selectedModel(options, value);
      if (!options.some(([id]) => id === value) && value) options = [[value, value], ...options];
      const key = JSON.stringify([options, value]); if (control.dataset.options === key) { control.value = value ?? options[0]?.[0] ?? ''; return; }
      control.dataset.options = key; control.replaceChildren();
      for (const [id, text] of options) { const o = node('option', '', text); o.value = id; control.append(o); }
      control.value = value ?? options[0]?.[0] ?? '';
    }
    function updateChat() {
      const s = current(), access = capabilities(s, machines);
      if (s && openContinuation(s.id)) return;
      const continuation = s && continuations.get(s.id);
      continueButton.hidden = !bridge.continueSession || !access.readOnly || !s || !['claude', 'codex', undefined, null].includes(s.agent);
      continueButton.disabled = !access.canContinue || continuation?.status === 'starting';
      continueButton.querySelector('span').textContent = continuation?.status === 'starting' ? 'Подключаем…' : continuation?.boundId ? 'Открыть продолжение' : continuation?.terminalId ? 'Открыть терминал запуска' : 'Продолжить в Jarvis';
      continueStatus.textContent = continuation?.error || (continuation?.status === 'starting' ? 'Загружаем историю в новый чат…' : '');
      loadChatModels(s);
      const availableModels = s && (!s.agent || ['claude', 'codex'].includes(s.agent)) ? modelsForSession(s) : [];
      const currentModel = modelName(s?.model);
      optionsFor(model, currentModel ? availableModels : [['', 'Модель не определена'], ...availableModels], currentModel);
      if (!currentModel) model.options[0].disabled = true;
      optionsFor(effort, s?.agent !== 'codex' ? api.efforts(currentModel).filter(([id]) => !['auto', 'ultracode'].includes(id)) : [], s?.effort);
      model.hidden = !access.canConfigure; effort.hidden = !access.canConfigure || s?.agent === 'codex';
      model.disabled = effort.disabled = !access.canConfigure || changingControl === s?.id;
      modelLabel.hidden = access.canConfigure; modelLabel.textContent = currentModel || providerLabel(s?.agent);
      modelLabel.title = access.detail;
      const delivery = api.deliveryState?.(), reading = api.readingFiles?.() || 0;
      const deliveryKey = JSON.stringify([activeId, delivery, reading]);
      if (deliveryStatus.dataset.key !== deliveryKey) {
        deliveryStatus.dataset.key = deliveryKey;
        if (reading) asyncUI.status(deliveryStatus, `Читаем файлы: ${reading}`);
        else if (delivery?.kind === 'error') deliveryStatus.replaceChildren(asyncUI.message({ title: delivery.title || 'Не удалось отправить', detail: delivery.text + '. Черновик сохранён.', kind: 'error', action: delivery.title ? null : 'Повторить', onAction: () => api.send() }));
        else if (delivery) asyncUI.status(deliveryStatus, delivery.text, delivery.kind);
        else deliveryStatus.replaceChildren();
      }
      asyncUI.button(send, reading || delivery?.kind === 'loading', reading ? 'Читаем файлы…' : delivery?.kind === 'loading' ? 'Отправляем…' : 'Отправить сообщение');
      send.disabled = reading > 0 || !access.canSend || reply.disabled || changingControl === s?.id || (!reply.value.trim() && !api.hasAttachments());
      attach.disabled = !access.canSend || reply.disabled;
      attach.title = 'Прикрепить файл';
      contextLabel.textContent = s ? `${s.instanceLabel || providerLabel(s.agent)}  /  ${s.remote || 'Этот компьютер'}` : '';
      contextLabel.title = s?.cwd || s?.project || '';
      chat.dataset.capability = access.state;
      chat.classList.toggle('sw-external', access.readOnly);
      chat.classList.toggle('sw-startup-terminal', !!continuation?.terminalId);
      inputBox.hidden = access.readOnly;
      capabilityBar.dataset.state = access.state;
      capabilityTitle.textContent = access.label; capabilityDetail.textContent = access.detail;
      capabilityIcon.replaceChildren(icon(access.readOnly ? 'eye' : access.state === 'disconnected' ? 'plugs' : access.state === 'waiting' ? 'chat-circle-dots' : 'check-circle'));
      if (access.state === 'working') { const spinner = node('span', 'ui-spinner'); spinner.setAttribute('aria-hidden', 'true'); capabilityIcon.replaceChildren(spinner); }
      capabilityDetail.hidden = access.state === 'ready';
      const external = access.readOnly && s?.controlMode === 'external' && !s?.remote && s?.agent === 'codex';
      workspaceOpen.hidden = !bridge.openWorkspace || !!window.__JARVIS_WORKSPACE__ || !s;
      projectPath.textContent = s?.cwd || s?.project || ''; projectPath.title = projectPath.textContent;
      const heading = document.getElementById('chatTitle');
      if (s && heading) { heading.textContent = titleOf(s); heading.title = titleOf(s); }
      externalOpen.hidden = !external; terminalToggle.hidden = !s || (access.readOnly && !continuation?.terminalId); terminalToggle.disabled = !access.canTerminal && !(continuation?.terminalId && access.online);
      terminalToggle.querySelector('span').textContent = continuation?.terminalId ? 'Терминал запуска' : 'Терминал';
      if (external) {
        externalOpen.querySelector('span').textContent = 'Открыть Codex';
        externalOpen.title = `Продолжить чат в ${s.instanceLabel || 'Codex'}`;
      }
      if (access.readOnly) {
        if (!continuation?.terminalId) { terminalOpen = false; terminal.hidden = true; terminalToggle.setAttribute('aria-expanded', 'false'); }
        pollTerminal();
        document.getElementById('tmuxHint').hidden = true;
      }
      if (!s || usageId === s.id && Date.now() - usageAt < 15000) return;
      usageId = s.id; usageAt = Date.now(); const request = ++usageRequest;
      usage.hidden = false; usage.setAttribute('aria-busy', 'true'); usageSummary.textContent = 'Использование';
      usageBody.replaceChildren(asyncUI.skeleton('list', 'Загружаем использование'));
      bridge.getSessionUsage(s.id).then(u => {
        if (request !== usageRequest || activeId !== s.id) return;
        usage.setAttribute('aria-busy', 'false');
        if (!u) { usageBody.replaceChildren(asyncUI.message({ title: 'Данных пока нет', detail: 'Статистика появится после первого ответа агента.' })); return; }
        usageBody.replaceChildren(); usage.hidden = false;
        usageSummary.textContent = u.tok == null ? 'Использование · нет данных' : `Использование · ${tokens(u.tok)} токенов`;
        if (u.inputTokens != null) usageBody.append(node('span', '', `Вход без кэша ${tokens(u.inputTokens)} · выход ${tokens(u.outputTokens)}`));
        if (u.cacheHitPct != null) usageBody.append(node('span', '', `Кэш ${Math.round(u.cacheHitPct)}% · чтение ${tokens(u.cacheReadTokens)} · запись ${tokens(u.cacheWriteTokens)}`));
        if (u.requests != null) usageBody.append(node('span', '', `${u.requests} ответов`));
        if (Number.isFinite(u.cost)) usageBody.append(node('span', '', `≈ $${u.cost.toFixed(2)} по API-тарифу`));
        const remoteUsage = u.source === 'remote-transcripts';
        if (u.instanceLabel || u.machine) usageBody.append(node('span', 'sw-usage-note', u.instanceLabel || u.machine));
        if (u.stale) usageBody.append(node('span', 'sw-usage-note', u.cachedAt ? 'Показаны последние прочитанные данные: обновить расход не удалось.' : 'Расход этой сессии пока неизвестен.'));
        else if (u.partial) usageBody.append(node('span', 'sw-usage-note', 'Прочитана часть трейса; полный расход неизвестен.'));
        if (u.error) usageBody.append(node('span', 'sw-usage-note', u.error));
        usageBody.append(node('span', 'sw-usage-note', `Оценка по ${remoteUsage ? 'транскрипту на узле' : 'локальным транскриптам'}, не списание и не лимит подписки.`));
        usage.title = 'Расход конкретной сессии по прочитанным событиям. Стоимость приблизительная; квота аккаунта учитывается отдельно.';
      }).catch(error => {
        if (request !== usageRequest || activeId !== s.id) return;
        usage.setAttribute('aria-busy', 'false'); usageBody.replaceChildren(asyncUI.message({ title: 'Статистика недоступна', detail: String(error?.message || error), kind: 'error', action: 'Повторить', onAction: () => { usageId = null; updateChat(); } }));
      });
    }
    reply.addEventListener('input', updateChat);
    function refresh(list) {
      sessions = list;
      notifications = attention.update(sessions, view === 'chat' && !document.hidden && document.hasFocus() ? activeId : null); saveRead();
      inboxCount.textContent = String(notifications.length); inboxCount.hidden = !notifications.length;
      const next = JSON.stringify([api.sessionLoadState?.(), sessions.map(s => [s.id, titleOf(s), s.project, s.cwd, s.remote, s.instanceLabel, s.agent, s.createdAt, s.status, s.pinned, s.controlMode, !!s.tmuxPane]), notifications.map(s => s.id), activeId, inbox]);
      if (next !== signature) { signature = next; renderSidebar(); renderWelcome(); }
      if (view === 'chat') updateChat(); maybeOpenLaunched();
    }
    window.jarvisSessionWorkspace = {
      refresh, dismissMenu,
      saveDraft(id, text, images) { if (id) drafts.set(id, { text, images: images.slice() }); },
      draft: id => drafts.get(id),
      clearDraft: id => drafts.delete(id),
      composerChanged: updateChat,
      connectionReady: () => capabilities(current(), machines).canSend,
      capabilities: session => capabilities(session, machines),
      async newChat(project) {
        const request = ++projectFocusRequest;
        navigate('list');
        await loadMachines();
        if (request !== projectFocusRequest || view !== 'list') return;
        machine.value = project.machine || 'local'; cwd.value = project.cwd || '';
        if (!machine.value) { api.toast('Машина проекта больше не подключена'); return; }
        launchLocation.remember(machine.value, cwd.value);
        projectTitle(); await loadProjects(false); updateLaunchGate(); prompt.focus();
      },
      async focusProject(path, remote) {
        focusedProject = JSON.stringify([remote || 'local', path]);
        projectFilter.hidden = false;
        await this.newChat({ cwd: path, machine: remote || 'local' });
        renderSidebar();
      },
      changed(next) {
        const previousId = activeId;
        if (previousId) expanded.set(previousId, { terminal: terminalOpen, usage: usage.open });
        view = next; activeId = next === 'chat' ? api.currentId() : null;
        if (previousId !== activeId) { const detail = expanded.get(activeId); terminalOpen = !!detail?.terminal; usage.open = !!detail?.usage; terminal.hidden = !terminalOpen; terminalToggle.setAttribute('aria-expanded', String(terminalOpen)); }
        const enabled = ['list', 'chat', 'history'].includes(next);
        frame.classList.toggle('with-sessions', enabled); sidebar.hidden = !enabled; welcome.hidden = next !== 'list'; mobileToggle.hidden = !enabled;
        document.documentElement.classList.toggle('session-workspace', enabled);
        if (previousId !== activeId) { controlStatus.textContent = ''; usageRequest++; usageId = null; usage.hidden = true;  }
        pollTerminal();
        if (enabled) { loadMachines(); if (!artworkLoaded) loadArtwork(); } refresh(api.sessions());
      },
    };
    function applyLaunchEvent(event) {
      if (!pendingLaunch || event.launchId !== pendingLaunch.launchId) return;
      if (event.status === 'failed') {
        if (pendingLaunch.boundId) api.trackLaunchMessage?.(pendingLaunch.boundId, { key: pendingLaunch.launchId, status: event.error || 'Не удалось отправить', kind: 'error' });
        launchStatus.dataset.state = 'error';
        launchStatus.textContent = event.error || 'Не удалось доставить начальную задачу. Открой чат и отправь её ещё раз.';
        if (!prompt.value) prompt.value = pendingLaunch.text;
        api.toast(launchStatus.textContent);
        launchFiles.push(...pendingLaunch.files); renderLaunchFiles();
        pendingLaunch = null; composer.hidden = location.hidden = false; startup.hidden = true; finishLaunchState(); clearTimeout(launchTimer); updateLaunchGate();
      } else if (event.sessionId) { pendingLaunch.boundId = event.sessionId; pendingLaunch.delivered = ['sent', 'ready'].includes(event.status); maybeOpenLaunched(); }
    }
    if (bridge.onLaunchTask) bridge.onLaunchTask(event => {
      if (!event.launchId) return;
      launchEvents.set(event.launchId, event);
      if (launchEvents.size > 30) launchEvents.delete(launchEvents.keys().next().value);
      applyLaunchEvent(event);
      applyContinuationEvent(event);
    });
    window.addEventListener('jarvis-projects-changed', () => { loadProjects(false); loadArtwork(); });
    if (bridge.onProjectsChanged) bridge.onProjectsChanged(loadArtwork);
    setInterval(() => { if (!document.hidden && ['list', 'chat', 'history'].includes(view)) { loadMachines(); if (view === 'chat') updateChat(); } }, 15000);
    bridge.agentsList().then(r => { if (r?.ok) for (const a of r.agents || []) { const o = node('option', '', a.name || a.id); o.value = a.id; provider.append(o); } }).catch(() => {});
  };
})();
