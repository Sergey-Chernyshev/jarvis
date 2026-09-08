/* Projects are saved locations plus discovered conversations. Saving/removing
 * a location never creates/deletes its files or starts an agent. */
(() => {
  'use strict';
  const keyOf = p => JSON.stringify([p.machine || p.remote || 'local', p.cwd || '']);
  const labelOf = p => p.name || p.project || p.cwd?.split('/').filter(Boolean).pop() || 'Без каталога';
  const provider = id => id === 'codex' ? 'Codex' : !id || id === 'claude' ? 'Claude' : id;
  function visibleProjects(projects, { query = '', machine = 'all', filter = 'all', sort = 'recent' } = {}) {
    const terms = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
    return projects.filter(p => (machine === 'all' || p.machine === machine)
      && (filter !== 'saved' || p.saved) && (filter !== 'active' || p.liveCount > 0)
      && terms.every(t => `${labelOf(p)} ${p.cwd || ''} ${p.machine} ${(p.agents || []).join(' ')} ${(p.sessions || []).map(s => s.title || '').join(' ')}`.toLocaleLowerCase().includes(t)))
      .sort((a, b) => Number(!!b.pinned) - Number(!!a.pinned) || (sort === 'name' ? labelOf(a).localeCompare(labelOf(b), 'ru') : (b.lastAt || 0) - (a.lastAt || 0)));
  }
  window.JarvisProjectState = { keyOf, labelOf, visibleProjects };
  const node = (tag, cls, text) => { const n = document.createElement(tag); n.className = cls || ''; if (text != null) n.textContent = text; return n; };
  const icon = (name, size = 18) => window.jarvisIcons.create(name, size);
  function button(label, glyph, action, cls = 'pr-button') {
    const b = node('button', cls); b.type = 'button'; b.setAttribute('aria-label', label); b.title = label;
    if (glyph) b.append(icon(glyph)); b.append(node('span', '', label)); b.addEventListener('click', action); return b;
  }
  function select(label, values, value, changed) {
    const s = node('select', 'pr-select'); s.setAttribute('aria-label', label);
    for (const [id, text] of values) { const o = node('option', '', text); o.value = id; s.append(o); }
    s.value = value; s.addEventListener('change', () => changed(s.value)); return s;
  }
  function when(ts) {
    if (!ts) return 'Пока без чатов';
    const diff = Math.max(0, Date.now() - ts);
    if (diff < 60000) return 'Только что';
    if (diff < 3600000) return `${Math.floor(diff / 60000)} мин назад`;
    if (new Date(ts).toDateString() === new Date().toDateString()) return 'Сегодня';
    return new Intl.DateTimeFormat('ru', { day: 'numeric', month: 'short' }).format(ts);
  }

  window.initProjects = function (api) {
    const host = document.getElementById('history'), bridge = window.jarvis;
    let projects = [], machines = [], warnings = [], loading = false, loaded = false, sequence = 0, error = '', notice = '';
    let selected = null, machine = 'all', filter = 'all', sort = 'recent', index = 0, queryBeforeDetail = '';
    let form = null, saving = false, browser = null, browserSequence = 0, avatarSequence = 0, resumePending = null;
    let liveSignature = '';
    let catalogScroll = 0;
    const resumeEvents = new Map();
    let resumeTimer;
    const visible = () => api.visible();
    const current = () => projects.find(p => keyOf(p) === selected);
    const connection = p => machines.find(m => m.id === p.machine) || p.connection || { online: false, name: p.machine };
    const goSettings = () => api.settings();
    const showError = e => { error = String(e?.message || e); render(); };
    const scroller = () => host.closest('.content') || host;
    const newChat = p => api.newChat(p).catch(showError);
    function updateLive() {
      const live = api.sessions();
      for (const p of projects) {
        const matches = live.filter(s => keyOf(s) === keyOf(p));
        p.liveCount = matches.filter(s => s.status === 'working').length;
        p.attentionCount = matches.filter(s => ['waiting', 'limit'].includes(s.status)).length;
        p.live = matches;
      }
    }
    async function load() {
      const request = ++sequence; loading = true; error = ''; render();
      const results = await Promise.allSettled([bridge.projectsList(), bridge.machinesList()]);
      if (request !== sequence) return;
      loading = false;
      const data = results[0].status === 'fulfilled' ? results[0].value : null;
      if (!data?.ok || !Array.isArray(data.projects)) { error = String(data?.error || results[0].reason || 'Не удалось загрузить проекты'); render(); return; }
      projects = data.projects; warnings = data.warnings || []; loaded = true;
      if (results[1].status === 'fulfilled' && Array.isArray(results[1].value)) machines = results[1].value;
      else warnings = [...warnings, { machine: '', error: 'Не удалось обновить список машин' }];
      if (selected && !current()) selected = null;
      updateLive(); render();
    }
    async function save(project, openSaved = true) {
      if (saving) return;
      saving = true; error = ''; render();
      try {
        const result = await bridge.projectsSave(project);
        if (!result?.ok) throw new Error(result?.error || 'Не удалось сохранить проект');
        if (openSaved) {
          if (!selected) { queryBeforeDetail = api.query(); api.setQuery(''); }
          selected = keyOf(result.project || project); form = null; browser = null; browserSequence++;
        }
        notice = 'Проект сохранён'; await load();
        window.dispatchEvent(new Event('jarvis-projects-changed'));
      } catch (e) { error = String(e.message || e); }
      finally { saving = false; render(); }
    }
    async function pin(p) {
      await save({ machine: p.machine, cwd: p.cwd, name: labelOf(p), pinned: !p.pinned }, false);
    }
    function avatar(p, className, onChoose) {
      if (window.JarvisProjectAvatars?.create) return window.JarvisProjectAvatars.create({ ...p, connection: connection(p) }, { className, onChoose });
      const mark = node('span', className); mark.append(icon('folder-simple', 23)); return mark;
    }
    function showForm(p, { findAvatar = false } = {}) {
      avatarSequence++;
      form = { machine: p?.machine || (machine === 'all' ? 'local' : machine), cwd: p?.cwd || '', name: p ? labelOf(p) : '', pinned: !!p?.pinned, editing: !!p?.saved, avatar: p?.avatar ? { ...p.avatar } : null, avatarCandidates: [], avatarBusy: false, avatarNotice: '', avatarError: '', avatarLookupKey: '' };
      error = ''; notice = ''; browser = null; browserSequence++; render(); scroller().scrollTop = 0; document.getElementById('projectName')?.focus({ preventScroll: true });
      if (findAvatar && window.JarvisProjectAvatars && form.cwd && connection(form).online) {
        const draft = form;
        discoverAvatars(false).then(() => {
          if (form !== draft) return;
          host.querySelector('.pr-avatar-editor')?.scrollIntoView({ block: 'center' });
          host.querySelector('.pr-avatar-candidate, .pr-avatar-edit-actions button')?.focus({ preventScroll: true });
        });
      }
    }
    function openProject(p) { catalogScroll = scroller().scrollTop; selected = keyOf(p); queryBeforeDetail = api.query(); api.setQuery(''); error = ''; notice = ''; render(); scroller().scrollTop = 0; }
    function back() {
      if (saving) return true;
      if (browser) { browser = null; browserSequence++; render(); return true; }
      if (form) { avatarSequence++; form = null; error = ''; render(); scroller().scrollTop = 0; return true; }
      if (selected) { selected = null; error = ''; notice = ''; api.setQuery(queryBeforeDetail); render(); scroller().scrollTop = catalogScroll; return true; }
      return false;
    }
    function machineName(p) { return p.machine === 'local' ? 'Этот компьютер' : connection(p).name || p.machine; }
    function render() {
      if (!visible()) return;
      const footerLabel = document.getElementById('primaryLabel'), footerKey = document.getElementById('primaryKey');
      if (footerLabel) footerLabel.textContent = selected || form ? 'Назад' : 'Открыть проект';
      if (footerKey) footerKey.textContent = selected || form ? 'esc' : '↵';
      host.classList.add('projects-workspace'); host.replaceChildren();
      const page = node('div', 'pr-page'); host.append(page);
      const heading = node('div', 'pr-heading');
      const copy = node('div'); copy.append(node('h1', '', selected ? labelOf(current() || {}) : 'Проекты'));
      copy.append(node('p', '', selected ? 'Чаты и рабочее окружение проекта' : 'Рабочие папки и разговоры на всех твоих машинах'));
      heading.append(copy, node('span', 'pr-spacer'));
      if (selected && current() && !form) heading.prepend(avatar(current(), 'pr-heading-avatar', () => showForm(current(), { findAvatar: true })));
      const refresh = button('Обновить', 'arrows-clockwise', load, 'pr-button pr-icon'); refresh.disabled = loading; heading.append(refresh);
      if (!selected && !form) heading.append(button('Добавить проект', 'plus', () => showForm(), 'pr-button primary'));
      if (selected) heading.prepend(button('Все проекты', 'arrow-left', back, 'pr-button pr-icon'));
      page.append(heading);
      if (error) { const alert = node('div', 'pr-message error', error); alert.setAttribute('role', 'alert'); page.append(alert); }
      if (notice) { const message = node('div', 'pr-message', notice); message.setAttribute('role', 'status'); page.append(message); }
      if (form) { renderForm(page); return; }
      if (selected && current()) { renderDetail(page, current()); return; }
      const controls = node('div', 'pr-controls');
      const filters = node('div', 'pr-filters');
      for (const [value, label] of [['all', 'Все'], ['saved', 'Сохранённые'], ['active', 'В работе']]) {
        const b = button(label, null, () => { filter = value; index = 0; render(); }, 'pr-filter' + (filter === value ? ' active' : '')); b.setAttribute('aria-pressed', String(filter === value)); filters.append(b);
      }
      controls.append(filters, node('span', 'pr-spacer'), select('Машина проектов', [['all', 'Все машины'], ...machines.map(m => [m.id, m.id === 'local' ? 'Этот компьютер' : m.name])], machine, value => { machine = value; index = 0; render(); }), select('Порядок проектов', [['recent', 'Недавние'], ['name', 'По названию']], sort, value => { sort = value; render(); }));
      page.append(controls);
      if (warnings.length) {
        const detail = node('details', 'pr-warnings'); detail.append(node('summary', '', `Не все машины доступны · ${warnings.length}`));
        for (const warning of warnings) detail.append(node('p', '', `${warning.machine || 'Подключения'}: ${warning.error}`));
        detail.append(button('Открыть подключения', 'plugs-connected', goSettings)); page.append(detail);
      }
      if (loading) { const loadingText = node('div', 'pr-loading', loaded ? 'Обновляем проекты…' : 'Собираем проекты и историю чатов…'); loadingText.setAttribute('role', 'status'); page.append(loadingText); }
      const list = visibleProjects(projects, { query: api.query(), machine, filter, sort });
      index = Math.min(index, Math.max(0, list.length - 1));
      const catalog = node('div', 'pr-catalog'); page.append(catalog);
      list.forEach((p, i) => {
        const row = node('div', 'pr-project-row');
        const open = button('', null, () => openProject(p), 'pr-project-open' + (i === index ? ' selected' : ''));
        open.dataset.projectKey = keyOf(p); open.setAttribute('aria-label', `${labelOf(p)}, ${machineName(p)}`);
        const mark = avatar(p, 'pr-folder', () => showForm(p, { findAvatar: true })); row.append(mark);
        const body = node('span', 'pr-project-copy'); body.append(node('strong', '', labelOf(p)), node('span', 'pr-path', p.cwd || 'Каталог неизвестен'));
        const meta = node('span', 'pr-project-meta');
        const host = node('span', 'pr-host' + (connection(p).online ? '' : ' offline')); host.append(icon(p.machine === 'local' ? 'desktop' : 'cloud', 14), machineName(p));
        meta.append(host, node('span', '', `${p.count || p.sessions?.length || 0} чатов`));
        if (p.liveCount) meta.append(node('span', 'pr-working', `${p.liveCount} в работе`));
        if (p.attentionCount) meta.append(node('span', 'pr-attention', `${p.attentionCount} ждут ответа`));
        body.append(meta); open.append(body, node('span', 'pr-time', when(p.lastAt))); row.append(open);
        const star = button(p.pinned ? 'Открепить проект' : 'Закрепить проект', 'star', () => pin(p), 'pr-button pr-icon pr-pin' + (p.pinned ? ' pinned' : '')); star.setAttribute('aria-pressed', String(!!p.pinned)); star.disabled = saving || !p.cwd; row.append(star); catalog.append(row);
      });
      if (!list.length && !loading) {
        const empty = node('div', 'pr-empty'); empty.append(icon('folder-open', 34), node('h2', '', projects.length ? 'Подходящих проектов нет' : 'Добавь первый проект'), node('p', '', projects.length ? 'Измени поиск или фильтр машины.' : 'Сохрани рабочую папку, выбери машину и начни разговор с агентом.'));
        empty.append(button('Добавить проект', 'plus', () => showForm(), 'pr-button primary')); catalog.append(empty);
      }
      const footer = node('div', 'pr-catalog-footer'); footer.append(node('span', '', `${list.length} проектов`), button('Машины и VM', 'plugs-connected', goSettings)); page.append(footer);
    }
    function renderDetail(page, p) {
      const online = !!connection(p).online;
      const overview = node('div', 'pr-overview');
      const location = node('div', 'pr-location'); location.append(icon(p.machine === 'local' ? 'desktop' : 'cloud', 23));
      const copy = node('div'); copy.append(node('strong', '', machineName(p)), node('span', '', p.cwd || 'Каталог неизвестен')); location.append(copy, node('span', 'pr-status' + (online ? ' online' : ''), online ? 'На связи' : 'Нет связи')); overview.append(location);
      const actions = node('div', 'pr-actions');
      const launch = button('Новый чат', 'plus', () => newChat(p), 'pr-button primary'); launch.disabled = !online || !p.cwd; actions.append(launch);
      actions.append(button(p.pinned ? 'Открепить' : 'Закрепить', 'star', () => pin(p)), button(p.saved ? 'Настроить проект' : 'Сохранить проект', 'sliders-horizontal', () => showForm(p)));
      const copyPath = button('Копировать путь', 'copy', async () => { try { await bridge.copyText(p.cwd); notice = 'Путь скопирован'; render(); } catch (e) { showError(e); } }); copyPath.disabled = !p.cwd; actions.append(copyPath); overview.append(actions);
      if (!online) overview.append(node('p', 'pr-offline-note', 'Чаты остаются здесь. Для запуска и продолжения восстанови подключение к машине.'));
      page.append(overview);
      const summary = node('div', 'pr-summary');
      for (const [value, label] of [[p.count || p.sessions?.length || 0, 'чатов'], [p.liveCount || 0, 'в работе'], [p.attentionCount || 0, 'ждут ответа']]) { const item = node('div'); item.append(node('strong', '', String(value)), node('span', '', label)); summary.append(item); }
      summary.append(node('span', 'pr-spacer'), node('span', 'pr-agent-names', (p.agents || []).map(provider).join(' · '))); page.append(summary);
      const chatsHead = node('div', 'pr-section-head'); chatsHead.append(node('h2', '', 'Разговоры'), node('span', '', 'Открой активный чат или продолжи прошлый')); page.append(chatsHead);
      const live = p.live || [], all = new Map((p.sessions || []).map(s => [s.id, { ...s, live: false }]));
      for (const s of live) all.set(s.id, { ...all.get(s.id), ...s, live: true });
      const terms = api.query().toLocaleLowerCase().trim();
      const chats = [...all.values()].filter(s => !terms || `${s.title || ''} ${s.agent || ''}`.toLocaleLowerCase().includes(terms)).sort((a, b) => (b.updatedAt || b.lastAt || 0) - (a.updatedAt || a.lastAt || 0));
      for (const s of chats) {
        const row = node('div', 'pr-conversation');
        const text = node('div', 'pr-conversation-copy'); text.append(node('strong', '', s.title || s.lastPrompt || `Чат ${String(s.agentId || s.id).slice(0, 8)}`));
        text.append(node('span', '', `${s.instanceLabel || provider(s.agent)} · ${s.model || (s.live ? ({ working: 'Работает', waiting: 'Нужен ответ', done: 'Готово', idle: 'Можно продолжить', limit: 'Лимит' }[s.status] || 'Подключён') : when(s.lastAt))}`));
        row.append(text);
        const action = button(s.live ? 'Открыть чат' : 'Продолжить', s.live ? 'chat-circle' : 'play', () => s.live ? api.openSession(s) : resume(p, s));
        action.disabled = !s.live && (!online || !!resumePending); row.append(action); page.append(row);
      }
      if (!chats.length) page.append(node('p', 'pr-empty-text', terms ? 'По этому запросу чатов нет.' : 'У проекта ещё нет разговоров. Начни с нового чата.'));
      const environment = node('details', 'pr-environment'); environment.append(node('summary', '', 'Рабочее окружение и изоляция'));
      environment.append(node('p', '', 'Машина определяет, где выполняется агент. Отдельная ветка изолирует правки внутри проекта; VM или контейнер изолируют программы и зависимости.'));
      environment.append(button('Настроить машины и VM', 'plugs-connected', goSettings)); page.append(environment);
    }
    async function resume(p, s) {
      if (resumePending) return;
      const pending = { machine: p.machine, cwd: p.cwd, id: s.id, launchId: null, projectKey: selected };
      resumePending = pending; notice = 'Возобновляем разговор…'; error = ''; render();
      try {
        const r = await bridge.launchSession(p.cwd, s.agent || 'claude', s.providerSessionId || s.agentId || s.id, p.machine, { mode: 'ask', ...(s.instanceId ? { instanceId: s.instanceId } : {}) });
        if (!r?.ok) throw new Error(r?.error || 'Не удалось продолжить разговор');
        if (resumePending !== pending) return;
        pending.launchId = r.launchId || null; pending.resolved = true; notice = 'Сессия запускается. Она появится в чатах после подключения.';
        resumeTimer = setTimeout(() => { if (resumePending === pending) { resumePending = null; notice = 'Если чат ещё не появился, проверь терминал и подключение машины.'; render(); } }, 95000);
        if (pending.launchId && resumeEvents.has(pending.launchId)) applyResumeEvent(resumeEvents.get(pending.launchId));
        else openResumed();
      } catch (e) { if (resumePending === pending) { resumePending = null; error = String(e.message || e); notice = ''; } }
      render();
    }
    function openResumed() {
      const p = resumePending;
      if (!p?.resolved || (p.launchId && !p.boundId)) return;
      const session = api.sessions().find(s => s.id === (p.boundId || p.id) && (s.remote || 'local') === p.machine);
      if (!session) return;
      resumePending = null; clearTimeout(resumeTimer); notice = 'Разговор готов к продолжению.';
      if (visible() && selected === p.projectKey && !form) api.openSession(session);
    }
    function applyResumeEvent(event) {
      if (!resumePending?.launchId || event.launchId !== resumePending.launchId) return;
      if (event.status === 'failed') { error = event.error || 'Не удалось продолжить чат'; notice = ''; resumePending = null; clearTimeout(resumeTimer); render(); }
      else if (event.sessionId) { resumePending.boundId = event.sessionId; openResumed(); }
    }
    function avatarLocationChanged() {
      avatarSequence++;
      form.avatarBusy = false; form.avatarCandidates = []; form.avatarError = ''; form.avatarNotice = ''; form.avatarLookupKey = '';
      if (form.avatar?.source === 'project') form.avatar = null;
      refreshAvatarEditor();
    }
    function refreshAvatarEditor() {
      if (!form) return;
      host.querySelector('.pr-avatar-editor')?.replaceWith(renderAvatarEditor());
      const submit = host.querySelector('.pr-form button[type="submit"]');
      if (submit) submit.disabled = saving || !!form.avatarBusy;
    }
    function beginAvatarRequest() {
      const request = { id: ++avatarSequence, form, location: keyOf(form) };
      form.avatarBusy = true; form.avatarError = ''; form.avatarNotice = ''; refreshAvatarEditor(); return request;
    }
    function currentAvatarRequest(request) {
      return form === request.form && request.id === avatarSequence && keyOf(form) === request.location;
    }
    function candidatePath(candidate) {
      const path = String(candidate.path || candidate.name || 'Картинка проекта');
      const prefix = form.cwd.replace(/\/$/, '') + '/';
      return path.startsWith(prefix) ? path.slice(prefix.length) : path;
    }
    const avatarLabel = value => Array.from(String(value || '').replace(/[\x00-\x1f\x7f]/g, ' ').trim()).slice(0, 160).join('');
    async function chooseAvatarCandidate(candidate) {
      if (!form || form.avatarBusy || saving) return;
      const request = beginAvatarRequest();
      try {
        const dataUrl = await window.JarvisProjectAvatars.normalize(candidate.dataUrl);
        if (!currentAvatarRequest(request)) return;
        form.avatar = { dataUrl, source: 'project', path: candidate.path, label: avatarLabel(candidate.name || candidatePath(candidate)) };
        form.avatarNotice = 'Картинка выбрана. Сохрани проект, чтобы применить её.';
      } catch (e) { if (currentAvatarRequest(request)) form.avatarError = String(e.message || e); }
      finally { if (currentAvatarRequest(request)) { form.avatarBusy = false; refreshAvatarEditor(); } }
    }
    async function discoverAvatars(refresh = true) {
      if (!form || form.avatarBusy || saving) return;
      const request = beginAvatarRequest();
      try {
        const result = await window.JarvisProjectAvatars.discover(form.machine, form.cwd.trim(), { refresh });
        if (!currentAvatarRequest(request)) return;
        if (!result?.ok) throw new Error(result?.error || 'Не удалось найти картинки в проекте.');
        form.avatarCandidates = Array.isArray(result.candidates) ? result.candidates : [];
        form.avatarLookupKey = keyOf(form);
        const count = form.avatarCandidates.length;
        if (!count) form.avatarNotice = 'Картинки проекта не найдены. Можно загрузить свою.';
        else if (count === 1 && !form.avatar) {
          const candidate = form.avatarCandidates[0];
          const dataUrl = await window.JarvisProjectAvatars.normalize(candidate.dataUrl);
          if (!currentAvatarRequest(request)) return;
          form.avatar = { dataUrl, source: 'project', path: candidate.path, label: avatarLabel(candidate.name || candidatePath(candidate)) };
          form.avatarNotice = 'Нашли и выбрали картинку проекта. Осталось сохранить.';
        } else form.avatarNotice = count === 1 ? 'Нашли картинку в проекте. Текущий выбор сохранён.' : `Найдено картинок: ${count}. Выбери подходящую.`;
        if (result.truncated) form.avatarNotice += ' Показана часть результатов.';
      } catch (e) { if (currentAvatarRequest(request)) form.avatarError = String(e.message || e); }
      finally { if (currentAvatarRequest(request)) { form.avatarBusy = false; refreshAvatarEditor(); } }
    }
    async function uploadAvatar(file) {
      if (!file || !form || form.avatarBusy || saving) return;
      const request = beginAvatarRequest();
      try {
        const dataUrl = await window.JarvisProjectAvatars.fromFile(file);
        if (!currentAvatarRequest(request)) return;
        form.avatar = { dataUrl, source: 'upload', label: avatarLabel(file.name) };
        form.avatarNotice = 'Картинка загружена. Сохрани проект, чтобы применить её.';
      } catch (e) { if (currentAvatarRequest(request)) form.avatarError = String(e.message || e); }
      finally { if (currentAvatarRequest(request)) { form.avatarBusy = false; refreshAvatarEditor(); } }
    }
    function renderAvatarEditor() {
      const section = node('section', 'pr-avatar-editor'); section.setAttribute('aria-label', 'Картинка проекта'); section.setAttribute('aria-busy', String(!!form.avatarBusy));
      const row = node('div', 'pr-avatar-edit-row');
      const autoCwd = form.editing || form.avatarLookupKey === keyOf(form) ? form.cwd : '';
      row.append(avatar({ ...form, cwd: autoCwd }, 'pr-avatar-preview'));
      const body = node('div', 'pr-avatar-edit-copy'); body.append(node('strong', '', 'Картинка проекта'));
      body.append(node('span', '', form.avatar ? form.avatar.label || (form.avatar.source === 'upload' ? 'Своя картинка' : 'Из проекта') : 'Автоматически из проекта или своя картинка'));
      const controls = node('div', 'pr-avatar-edit-actions');
      const input = node('input', 'pr-avatar-file'); input.type = 'file'; input.accept = 'image/*'; input.setAttribute('aria-label', 'Картинка проекта'); input.hidden = true; input.addEventListener('change', () => { const file = input.files?.[0]; input.value = ''; uploadAvatar(file); });
      const ready = !!window.JarvisProjectAvatars;
      const upload = button('Загрузить картинку', 'upload-simple', () => input.click()); upload.disabled = saving || !!form.avatarBusy || !ready;
      const find = button('Найти в проекте', 'magnifying-glass', () => discoverAvatars(true)); find.disabled = saving || !!form.avatarBusy || !ready || !form.cwd.trim().startsWith('/') || !machines.find(m => m.id === form.machine)?.online;
      controls.append(upload, find, input);
      if (form.avatar) {
        const remove = button('Убрать картинку', 'x', () => { avatarSequence++; form.avatar = null; form.avatarLookupKey = keyOf(form); form.avatarError = ''; form.avatarNotice = 'Будет использоваться автоматическая картинка проекта.'; refreshAvatarEditor(); });
        remove.disabled = saving || !!form.avatarBusy; controls.append(remove);
      }
      body.append(controls); row.append(body); section.append(row);
      const status = node('p', 'pr-avatar-status' + (form.avatarError ? ' error' : ''), form.avatarBusy ? 'Подготавливаем картинку…' : form.avatarError || form.avatarNotice || '');
      status.setAttribute('role', form.avatarError ? 'alert' : 'status'); section.append(status);
      if (form.avatarCandidates?.length) {
        const candidates = node('div', 'pr-avatar-candidates'); candidates.setAttribute('aria-label', 'Найденные картинки проекта');
        for (const candidate of form.avatarCandidates) {
          const path = candidatePath(candidate), selected = form.avatar?.source === 'project' && form.avatar.path === candidate.path;
          const choose = button(path, null, () => chooseAvatarCandidate(candidate), 'pr-avatar-candidate' + (selected ? ' selected' : '')); choose.setAttribute('aria-pressed', String(selected)); choose.disabled = saving || !!form.avatarBusy;
          const image = node('img'); image.src = candidate.dataUrl; image.alt = ''; image.loading = 'lazy'; image.decoding = 'async'; choose.prepend(image); candidates.append(choose);
        }
        section.append(candidates);
      }
      return section;
    }
    function renderForm(page) {
      const f = node('form', 'pr-form'); f.setAttribute('aria-label', form.editing ? 'Настройки проекта' : 'Добавить проект');
      f.append(node('h2', '', form.editing ? 'Настройки проекта' : 'Добавить рабочую папку'), node('p', '', 'Сохраняется расположение проекта. Папка и агент при этом не создаются.'));
      function field(label, input) { const l = node('label', 'pr-field'); l.append(node('span', '', label), input); f.append(l); }
      const name = node('input'); name.id = 'projectName'; name.value = form.name; name.disabled = saving; name.placeholder = 'Например, мобильное приложение'; name.maxLength = 120; name.addEventListener('input', () => { form.name = name.value; }); field('Название', name);
      const choices = machines.map(m => [m.id, (m.id === 'local' ? 'Этот компьютер' : m.name) + (m.online ? '' : ' · нет связи')]);
      if (!choices.some(([id]) => id === form.machine)) choices.push([form.machine, form.machine]);
      const machineSelect = select('Машина проекта', choices, form.machine, value => { form.machine = value; form.cwd = ''; avatarLocationChanged(); browser = null; browserSequence++; render(); }); machineSelect.disabled = saving || form.editing; field('Где находится проект', machineSelect);
      const pathRow = node('div', 'pr-path-field'), cwd = node('input'); cwd.id = 'projectPath'; cwd.value = form.cwd; cwd.placeholder = '/полный/путь/к/проекту'; cwd.required = true; cwd.disabled = saving || form.editing; cwd.addEventListener('input', () => { form.cwd = cwd.value; avatarLocationChanged(); });
      pathRow.append(cwd);
      const browse = button('Выбрать папку', 'folder-open', () => browsePath(form.cwd || null)); browse.disabled = saving || form.editing || !machines.find(m => m.id === form.machine)?.online; pathRow.append(browse); field('Каталог на выбранной машине', pathRow);
      f.append(renderAvatarEditor());
      const pinned = node('input'); pinned.type = 'checkbox'; pinned.checked = form.pinned; pinned.disabled = saving; pinned.addEventListener('change', () => { form.pinned = pinned.checked; }); const l = node('label', 'pr-pin-field'); l.append(pinned, 'Закрепить в начале списка'); f.append(l);
      if (browser) renderBrowser(f);
      const actions = node('div', 'pr-form-actions'); const cancel = button('Отмена', null, () => { avatarSequence++; form = null; browser = null; browserSequence++; error = ''; render(); }); cancel.disabled = saving; actions.append(cancel);
      const submit = button(saving ? 'Сохраняем…' : 'Сохранить проект', 'check', () => {}, 'pr-button primary'); submit.type = 'submit'; submit.disabled = saving || !!form.avatarBusy; actions.append(submit); f.append(actions);
      f.addEventListener('submit', e => { e.preventDefault(); if (form.avatarBusy) return; if (!form.cwd.trim().startsWith('/')) { error = 'Укажи полный путь, начиная с /'; render(); return; } save({ machine: form.machine, cwd: form.cwd.trim(), name: form.name.trim(), pinned: form.pinned, avatar: form.avatar || null }); });
      if (form.editing) {
        const forget = button('Убрать из сохранённых', 'bookmark-simple', async () => {
          if (saving) return; saving = true; render();
          try { const r = await bridge.projectsRemove(form.machine, form.cwd); if (!r?.ok) throw new Error(r?.error || 'Не удалось убрать проект'); form = null; notice = 'Убран из сохранённых. Файлы и история чатов сохранены.'; await load(); window.dispatchEvent(new Event('jarvis-projects-changed')); }
          catch (e) { error = String(e.message || e); } finally { saving = false; render(); }
        }); forget.disabled = saving; f.append(forget, node('small', 'pr-forget-note', 'Проект с существующими чатами останется в общей истории. Файлы не удаляются.'));
      }
      page.append(f);
    }
    async function browsePath(path) {
      const request = ++browserSequence, target = form?.machine; if (!target) return;
      browser = { busy: true, path: path || '' }; render();
      try {
        if (!path) { const places = await bridge.bundlePlaces(target); if (!places?.ok) throw new Error(places?.error || 'Не удалось открыть папки'); path = places.home; }
        const r = await bridge.bundleBrowse(target, path);
        if (request !== browserSequence || form?.machine !== target) return;
        if (!r?.ok) throw new Error(r?.error || 'Не удалось открыть папку');
        browser = { ...r, busy: false };
      } catch (e) { if (request !== browserSequence || form?.machine !== target) return; browser = { path: path || '', error: String(e.message || e) }; }
      render();
    }
    function renderBrowser(f) {
      const b = node('div', 'pr-browser'); b.append(node('strong', '', browser.path || 'Папки'));
      if (browser.busy) b.append(node('p', '', 'Загружаем…'));
      else if (browser.error) b.append(node('p', 'pr-message error', browser.error), button('Повторить', 'arrows-clockwise', () => browsePath(browser.path)));
      else {
        const actions = node('div', 'pr-actions');
        if (browser.parent) actions.append(button('Выше', 'arrow-up', () => browsePath(browser.parent)));
        actions.append(button('Выбрать эту папку', 'check', () => { form.cwd = browser.path; avatarLocationChanged(); browser = null; render(); }, 'pr-button primary')); b.append(actions);
        for (const dir of browser.dirs || []) { const name = typeof dir === 'string' ? dir : dir.name; b.append(button(name, 'folder-simple', () => browsePath(`${browser.path.replace(/\/$/, '')}/${name}`))); }
        if (!browser.dirs?.length) b.append(node('p', '', 'Вложенных папок нет'));
      }
      b.append(button('Закрыть выбор папки', 'x', () => { browser = null; browserSequence++; render(); })); f.append(b);
    }
    if (bridge.onLaunchTask) bridge.onLaunchTask(event => {
      if (!event.launchId) return;
      resumeEvents.set(event.launchId, event);
      if (resumeEvents.size > 30) resumeEvents.delete(resumeEvents.keys().next().value);
      applyResumeEvent(event);
    });
    window.jarvisProjects = {
      show() { if (!loaded && !loading) return load(); render(); },
      refresh: load, search: render, back,
      primary() { if (selected || form) api.back(); else { const list = visibleProjects(projects, { query: api.query(), machine, filter, sort }); if (list[index]) openProject(list[index]); } },
      enter(saved, changed) { if (changed) { sequence++; browserSequence++; avatarSequence++; loading = false; loaded = false; form = saved?.form ? { ...saved.form, avatarBusy: false } : null; browser = null; error = ''; notice = ''; selected = saved?.selected || null; machine = saved?.machine || 'all'; filter = saved?.filter || 'all'; sort = saved?.sort || 'recent'; index = saved?.index || 0; queryBeforeDetail = saved?.queryBeforeDetail || ''; } },
      snapshot: () => ({ selected, machine, filter, sort, index, queryBeforeDetail, form: form ? { ...form } : null }),
      stateChanged() { openResumed(); if (!visible()) return; const sig = JSON.stringify(api.sessions()); if (sig === liveSignature) return; liveSignature = sig; updateLive(); if (!form) render(); },
      key(e) {
        if (e.key === 'Escape') { e.preventDefault(); api.back(); return; }
        if (selected || form) return;
        const list = visibleProjects(projects, { query: api.query(), machine, filter, sort });
        if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); index = Math.max(0, Math.min(list.length - 1, index + (e.key === 'ArrowDown' ? 1 : -1))); render(); host.querySelector('.pr-project-open.selected')?.scrollIntoView({ block: 'nearest' }); }
        else if (e.key === 'Enter' && list[index]) { e.preventDefault(); openProject(list[index]); }
      },
    };
  };
})();
