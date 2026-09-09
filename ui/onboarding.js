/* Preparation UI. The native snapshot owns installation and progress. */
(() => {
  'use strict';
  const State = window.JarvisOnboardingState;
  const tauri = window.__TAURI__;
  const invoke = tauri?.core?.invoke || (async () => { throw new Error('Окно не подключено к Jarvis. Открой подготовку из приложения.'); });
  const listen = tauri?.event?.listen || (async () => () => {});
  window.jarvis = Object.assign(window.jarvis || {}, {
    getSettings: () => invoke('settings_get'),
    setSettings: patch => invoke('settings_set', { patch }),
    onAppearance: cb => { listen('appearance', event => cb(event.payload)).catch(() => {}); },
  });
  const $ = id => document.getElementById(id);
  const content = $('content'), primary = $('primary'), secondary = $('secondary'), rail = $('rail');
  const EMPTY = { coreReady: false, agents: [], transport: [], capabilities: [], warnings: [], proxyConfigured: false, job: { id: 0, state: 'idle', kind: '', tasks: [], steps: [], failures: [] } };
  const models = [
    { key: 'whisper', id: 'whisper-turbo', title: 'Диктовка и расшифровки', detail: 'Whisper Turbo · распознаёт речь на устройстве', size: '574 МБ' },
    { key: 'qwen', id: 'qwen3-runtime', title: 'Альтернативное распознавание', detail: 'Qwen3-ASR · для Mac с Apple Silicon', size: '1–3 ГБ' },
    { key: 'wake', id: 'hey_jarvis', title: 'Активация голосом', detail: 'Обращение «Hey Jarvis» без нажатия клавиш', size: '4 МБ' },
    { key: 'silero', id: 'silero', title: 'Голосовые ответы', detail: 'Silero · русская речь на устройстве', size: '1 ГБ' },
  ];
  const agentLabels = { claude: ['Claude Code', 'Состояние задач и запросы подтверждения'], codex: ['Codex', 'Состояние задач и запросы подтверждения'] };
  const transportLabels = { hook: ['События приложений', 'Передаёт обновления агентов в Jarvis'], tmux: ['Управление терминалом', 'Для сессий, запущенных через tmux'], socket: ['Локальное соединение', 'Связь панели с Jarvis на этом компьютере'] };
  const phaseLabels = { 'STT-Whisper': 'Распознавание Whisper', 'STT-Qwen': 'Модель Qwen', 'STT-MLX': 'Окружение Qwen', 'wake-word': 'Активация голосом', 'Codex-SDK': 'Подключение Codex', 'Голос': 'Голосовые ответы', 'Хуки': 'События агентов', 'Транспорт': 'Соединение с терминалом', 'PATH': 'Команды Jarvis' };
  let snapshot = null, screen = 'welcome', previousScreen = '', pending = false, operationError = '', checking = false;
  let readSequence = 0, eventVersion = 0, firstSnapshot = true, dismissedJob = null, closing = false;
  let proxyValue = '', proxyOpen = false, proxyRemove = false;
  let agentsDeferred = false;
  const selection = { whisper: false, qwen: false, wake: false, silero: false, qwenSize: 'qwen3-0.6b' };

  function h(tag, attrs = {}, children = []) {
    const node = document.createElement(tag);
    for (const [key, value] of Object.entries(attrs)) {
      if (key === 'class') node.className = value;
      else if (key === 'text') node.textContent = value;
      else if (key === 'dataset') Object.assign(node.dataset, value);
      else if (key.startsWith('on') && typeof value === 'function') node.addEventListener(key.slice(2), value);
      else if (value !== null && value !== undefined && value !== false) node.setAttribute(key, String(value));
    }
    for (const child of children) if (child != null) node.appendChild(typeof child === 'string' ? document.createTextNode(child) : child);
    return node;
  }
  function icon(name, size = 20) { return window.jarvisIcons?.create(name, size) || h('span', { 'aria-hidden': 'true', text: '·' }); }
  function mark(state = 'idle') { return h('div', { class: 'mark', dataset: { state }, 'aria-hidden': 'true' }, [h('img', { src: './onboarding-mark.svg', alt: '', draggable: 'false' })]); }
  function heading(title, lead) { return [h('h1', { id: 'screen-title', text: title }), h('p', { class: 'lead', text: lead })]; }
  function notice(text, error = false) { return h('div', { class: `notice${error ? ' error' : ''}`, role: error ? 'alert' : null, text }); }
  function details(title, messages, open = false) { return messages?.length ? h('details', { class: 'details', open: open ? '' : null }, [h('summary', { text: title }), h('ul', {}, messages.map(text => h('li', { text: safeError(text) })))]) : null; }
  function safeError(error) { return String(error?.message || error || 'Неизвестная ошибка').replace(/([a-z]+:\/\/)[^\s/@]+:[^\s/@]+@/gi, '$1•••@'); }
  function jobKey() { return `${snapshot?.job?.id || 0}:${snapshot?.job?.kind || ''}`; }
  function activeScreen() { if (!snapshot && operationError) return 'unavailable'; if (checking && !snapshot) return 'checking'; return State.navigation(screen, snapshot, dismissedJob === jobKey(), agentsDeferred); }
  function selectedPlan() { return State.selectedPlan(selection, snapshot?.capabilities); }
  function missingAgents() { return snapshot && !(snapshot.agents || []).some(item => item.available !== false); }

  function proxyControl() {
    const field = h('input', { class: 'proxy-field', type: 'password', value: proxyValue, placeholder: 'http://proxy.example:8080', autocomplete: 'off', spellcheck: 'false', 'aria-label': 'Адрес прокси', dataset: { focus: 'proxy' }, oninput: event => { proxyValue = event.target.value; proxyRemove = false; } });
    const nodes = [h('summary', { text: snapshot?.proxyConfigured ? 'Сетевой прокси сохранён' : 'Параметры сети' }), field, h('p', { class: 'proxy-help', text: 'Прокси используется для загрузок. Пустое поле оставит сохранённый адрес без изменений.' })];
    if (snapshot?.proxyConfigured) nodes.push(h('button', { class: 'btn', type: 'button', style: 'margin-top:9px;font-size:11px;min-height:30px', text: proxyRemove ? 'Прокси будет отключён' : 'Отключить сохранённый прокси', onclick: () => { proxyRemove = true; proxyValue = ''; render(); } }));
    return h('details', { class: 'proxy', open: proxyOpen ? '' : null, ontoggle: event => { proxyOpen = event.target.open; } }, nodes);
  }
  function readinessItem(item, group = 'agents') {
    const ready = Boolean(item.ready), available = item.available !== false;
    const labels = (group === 'agents' ? agentLabels : transportLabels)[item.id] || [item.label || item.id, item.detail || ''];
    const copy = ready ? labels[1] : !available ? (group === 'agents' ? 'Установи CLI и проверь подключение снова' : 'Не настроено · можно добавить позже') : (item.id === 'socket' ? 'Ещё не отвечает · проверим при открытии панели' : 'Нужно завершить подключение');
    return h('div', { class: 'item', dataset: { ready: String(ready) } }, [h('span', { class: 'status-icon', 'aria-hidden': 'true' }, [icon(ready ? 'check' : !available ? 'minus' : group === 'agents' ? 'terminal-window' : 'plugs-connected', 17)]), h('div', { class: 'item-copy' }, [h('div', { class: 'item-title', text: labels[0] }), h('div', { class: 'item-detail', text: copy })]), h('span', { class: 'badge', text: ready ? 'Подключено' : !available ? 'Не найдено' : 'Настроить' })]);
  }
  function feature(name, title, text) { return h('div', { class: 'feature' }, [icon(name, 23), h('div', {}, [h('strong', { text: title }), h('p', { text })])]); }
  function welcome() { return [mark(), ...heading('Меньше переключений.\nБольше внимания делу.', 'Подключим агентов, а затем подготовим голос — если он тебе нужен.'), h('div', { class: 'feature-list' }, [feature('terminal-window', 'Задачи рядом', 'Claude Code и Codex в одном окне'), feature('waveform', 'Голос под рукой', 'Диктовка, ответы и записи встреч'), feature('command', 'Управление с клавиатуры', 'Команды и переходы без лишних кликов')])]; }
  function checkingView() { return [mark('working'), ...heading('Проверяем этот компьютер', 'Узнаем, какие агенты и голосовые модели уже готовы.'), progressCard({ phase: 'Локальная проверка', msg: 'Читаем состояние установленных компонентов.' }, null), notice('На этом шаге ничего не устанавливается.')]; }
  function agents() { return [...heading('Свяжем агентов с Jarvis', 'Подключение передаёт состояние задач и запросы подтверждения. Вход в аккаунт остаётся в самом агенте.'), h('div', { class: 'section-title', text: 'На этом компьютере' }), h('div', { class: 'list' }, (snapshot.agents || []).map(item => readinessItem(item))), missingAgents() ? notice('Агенты не найдены. Для диктовки и записи встреч они не нужны — можно подготовить голос сейчас, а агентов добавить позже.') : null, missingAgents() ? h('button', { class: 'btn', type: 'button', style: 'margin-top:12px', text: 'Проверить агентов снова', disabled: pending ? '' : null, onclick: () => run(refresh) }) : null, h('details', { class: 'details' }, [h('summary', { text: 'Состояние соединений' }), h('div', { class: 'list' }, (snapshot.transport || []).map(item => readinessItem(item, 'transport')))]), proxyControl(), details('Дополнительные сведения', snapshot.warnings)]; }
  function capabilityRow(meta) {
    const current = snapshot.capabilities.find(item => item.id === meta.id);
    if (!current) return null;
    const ready = Boolean(current.ready), available = current.available !== false;
    const checkbox = h('input', { type: 'checkbox', checked: ready || selection[meta.key] ? '' : null, disabled: ready || !available ? '' : null, 'aria-label': meta.title, dataset: { focus: meta.key }, onchange: event => { selection[meta.key] = event.target.checked; setActions(activeScreen()); } });
    const label = h('label', { class: 'cap-label' }, [checkbox, h('div', { class: 'item-copy' }, [h('div', { class: 'item-title', text: meta.title }), h('div', { class: 'item-detail', text: !available ? current.detail || 'Недоступно на этом устройстве или в этой сборке' : meta.detail })]), h('span', { class: 'cap-size', text: ready ? 'Установлено' : available ? meta.size : 'Недоступно' })]);
    const nodes = [label];
    if (meta.key === 'qwen' && !ready && available) {
      const select = h('select', { class: 'qwen-size', 'aria-label': 'Размер модели Qwen', dataset: { focus: 'qwen-size' }, onchange: event => { selection.qwenSize = event.target.value; } }, [h('option', { value: 'qwen3-0.6b', text: '0.6B · меньше памяти' }), h('option', { value: 'qwen3-1.7b', text: '1.7B · более крупная модель' })]);
      select.value = selection.qwenSize; nodes.push(select);
    }
    return h('div', { class: 'capability' }, nodes);
  }
  function capabilities() { return [...heading('Выбери, что подготовить', 'Голосовые модели работают локально. Ничего не выбрано заранее — можно вернуться к этому позже.'), h('div', { class: 'section-title' }, ['Голосовые возможности', h('span', { text: 'Загрузки по выбору' })]), h('div', { class: 'list' }, models.map(capabilityRow).filter(Boolean)), details('О моделях и лицензиях', ['Silero v4_ru и включённые wake-word модели имеют ограничения на коммерческое использование. Учитывай лицензии, если распространяешь Jarvis или используешь его в коммерческом продукте.']), proxyControl()]; }
  function progressCard(step, pct) {
    const known = pct !== null;
    const track = h('div', { class: 'progress-track', role: 'progressbar', 'aria-label': 'Прогресс текущего этапа', 'aria-valuemin': '0', 'aria-valuemax': '100', 'aria-valuenow': known ? String(pct) : null, 'aria-valuetext': known ? `${pct}% текущего этапа` : 'Этап выполняется; процент не предоставлен', dataset: { indeterminate: String(!known) } }, [h('div', { class: 'progress-fill', style: known ? `--pct:${pct}%` : null })]);
    return h('div', { class: 'progress-card' }, [h('div', { class: 'progress-head' }, [h('span', { class: 'spinner', 'aria-hidden': 'true' }), h('b', { text: phaseLabels[step?.phase] || step?.phase || 'Подготовка' }), h('span', { text: known ? `${pct}%` : 'В процессе' })]), h('div', { class: 'progress-line', text: safeError(step?.msg || 'Ожидаем первый этап от установщика.') }), track]);
  }
  function installing() {
    const job = snapshot.job || EMPTY.job;
    const progress = State.stepProgress(job);
    const rows = progress.steps.slice(-4).map(step => {
      const status = step.state === 'done' ? icon('check', 16)
        : ['error', 'warn'].includes(step.state) ? icon('warning', 16)
        : h('span', { class: 'spinner' });
      return h('div', { class: 'item', dataset: { ready: String(step.state === 'done') } }, [
        h('span', { class: 'status-icon' }, [status]),
        h('div', { class: 'item-copy' }, [
          h('div', { class: 'item-title', text: phaseLabels[step.phase] || step.phase }),
          h('div', { class: 'item-detail', text: safeError(step.msg) }),
        ]),
      ]);
    });
    return [
      ...heading(job.kind === 'models' ? 'Готовим голосовые функции' : 'Подключаем агентов', 'Можно закрыть окно. Подготовка продолжится, а состояние сохранится до завершения работы Jarvis.'),
      progressCard(progress.latest, progress.pct),
      h('div', { class: 'section-title' }, ['Этапы подготовки', h('span', { text: progress.done ? `${progress.done} завершено` : progress.steps.length ? 'В работе' : 'Ждём первый результат' })]),
      h('div', { class: 'list' }, rows),
    ];
  }

  function degraded() { const view = State.derive(snapshot), copy = State.failureCopy(view.failureKind); return [h('div', { class: 'status-error', 'aria-hidden': 'true' }, [icon('warning', 24)]), ...heading(copy[0], copy[1]), details('Причина остановки', snapshot.job?.failures?.length ? snapshot.job.failures : snapshot.warnings, true), view.failureKind === 'network' ? proxyControl() : null, notice('Можно вернуться назад и изменить выбор. Уже установленные компоненты останутся.')]; }
  function ready() {
    const view = State.derive(snapshot), online = view.runtimeState === 'online';
    const hotkey = window.jarvisKeys?.k('J') || 'Ctrl+J';
    const lead = !snapshot.coreReady
      ? (view.readyCapabilities ? 'Голосовые модули подготовлены. Агентов можно подключить позже в настройках.' : 'Jarvis можно открыть уже сейчас. Голос и агентов можно настроить позже.')
      : online ? 'Агенты подключены, локальное соединение готово. Jarvis будет рядом, когда понадобится.' : 'Агенты подключены. Локальное соединение ещё запускается — панель можно открыть уже сейчас.';
    return [notice('Перезапусти уже открытые сессии агентов и оболочку терминала (exec zsh), чтобы подключение заработало и в них.'), mark('ready'), ...heading('Можно начинать', lead), h('div', { class: 'ready-summary' }, [h('span', {}, [icon(snapshot.coreReady ? 'check' : 'minus', 14), snapshot.coreReady ? `${view.readyAgents} агентов подключено` : 'Агенты · позже']), h('span', {}, [icon('waveform', 14), `${view.readyCapabilities} голосовых модулей`])]), h('div', { class: 'shortcut' }, [h('kbd', { text: hotkey }), h('span', { text: 'Открыть Jarvis из любого приложения' })]), snapshot.coreReady && !online ? notice('Если состояние не изменится после запуска, проверь соединения в настройках.') : null];
  }
  function setRail(active) {
    const logical = ['installing', 'degraded'].includes(active) ? (snapshot?.job?.kind === 'models' ? 'capabilities' : 'agents') : ['checking', 'unavailable'].includes(active) ? 'welcome' : active;
    const order = ['welcome', 'agents', 'capabilities', 'ready'], index = Math.max(0, order.indexOf(logical));
    for (const button of rail.querySelectorAll('.rail-step')) {
      const i = order.indexOf(button.dataset.screen), skipped = i === 1 && i < index && !snapshot?.coreReady;
      button.dataset.state = skipped ? 'skipped' : i < index ? 'done' : i === index ? 'active' : 'pending';
      button.querySelector('.rail-dot').textContent = skipped ? '−' : i < index ? '✓' : String(i + 1);
      button.setAttribute('aria-current', i === index ? 'step' : 'false');
      button.disabled = pending || active === 'installing' || !snapshot || (i > 1 && !snapshot.coreReady && !agentsDeferred);
    }
  }
  function setActions(active) {
    primary.disabled = pending; secondary.disabled = pending; secondary.hidden = false; secondary.textContent = 'Назад';
    if (operationError) { primary.textContent = 'Проверить состояние'; secondary.hidden = !snapshot; }
    else if (active === 'checking') { primary.textContent = 'Проверяем…'; primary.disabled = true; secondary.hidden = true; }
    else if (active === 'welcome') { primary.textContent = 'Начать подготовку'; secondary.hidden = true; }
    else if (active === 'agents') { primary.textContent = missingAgents() ? 'Настроить голос' : snapshot.coreReady ? 'Продолжить' : 'Подключить агентов'; if (!snapshot.coreReady) secondary.textContent = 'Пока без агентов'; }
    else if (active === 'capabilities') { const count = selectedPlan().length; primary.textContent = count ? `Подготовить выбранное (${count})` : 'Продолжить без загрузок'; }
    else if (active === 'installing') { primary.textContent = 'Подготовка идёт'; primary.disabled = true; secondary.textContent = 'Продолжить в фоне'; secondary.disabled = false; }
    else if (active === 'degraded') { primary.textContent = 'Повторить подготовку'; }
    else if (active === 'ready') { primary.textContent = 'Открыть Jarvis'; secondary.textContent = 'Настройки'; }
    if (pending && !['checking', 'installing'].includes(active)) primary.textContent = 'Один момент…';
    $('footer-note').textContent = active === 'installing' ? 'Закрытие окна не отменяет подготовку' : active === 'capabilities' ? 'Выбор можно изменить в настройках' : 'Настройки можно изменить позже';
  }
  function render() {
    const active = activeScreen(), changed = active !== previousScreen;
    const focused = document.activeElement?.dataset?.focus, scroll = content.scrollTop;
    const cursor = focused === 'proxy' ? [document.activeElement.selectionStart, document.activeElement.selectionEnd] : null;
    const views = { welcome, checking: checkingView, agents, capabilities, installing, degraded, ready, unavailable: () => [mark(), ...heading('Не удалось проверить Jarvis', 'Повтори проверку. Если окно открыто в браузере, запусти подготовку из приложения.')] };
    content.replaceChildren(...views[active]().filter(Boolean));
    if (operationError) content.appendChild(notice(operationError, true));
    content.dataset.screen = active; content.setAttribute('aria-busy', String(checking || pending));
    content.scrollTop = changed ? 0 : scroll;
    if (changed) { content.classList.remove('enter'); requestAnimationFrame(() => content.classList.add('enter')); $('announcer').textContent = content.querySelector('h1')?.textContent || ''; }
    if (!changed && focused) { const next = content.querySelector(`[data-focus="${focused}"]`); next?.focus({ preventScroll: true }); if (cursor && next?.setSelectionRange) next.setSelectionRange(...cursor); }
    previousScreen = active; setRail(active); setActions(active);
  }
  async function timedInvoke(command, args) {
    let timer;
    try { return await Promise.race([invoke(command, args), new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Ответ от Jarvis задерживается. Проверь состояние снова; уже начатая подготовка продолжится в фоне.')), 15000); })]); }
    finally { clearTimeout(timer); }
  }
  async function refresh() {
    const request = ++readSequence, version = eventVersion;
    checking = true; render();
    try {
      const next = await timedInvoke('onboarding_get');
      if (request !== readSequence) return;
      if (!next || typeof next.coreReady !== 'boolean') throw new Error('Jarvis вернул неполное состояние подготовки. Повтори проверку.');
      const wasRunning = snapshot?.job?.state === 'running';
      snapshot = State.mergeSnapshot(snapshot, version !== eventVersion && snapshot ? { ...next, job: snapshot.job } : next);
      operationError = '';
      if (snapshot.job.kind === 'models') agentsDeferred = true;
      if (firstSnapshot) { screen = snapshot.coreReady || (agentsDeferred && snapshot.job.state === 'done') ? 'ready' : 'welcome'; firstSnapshot = false; }
      else if (wasRunning && snapshot.job.state === 'done') screen = snapshot.job.kind === 'models' ? 'ready' : 'capabilities';
    } catch (error) { if (request === readSequence) operationError = safeError(error); }
    finally { if (request === readSequence) { checking = false; render(); } }
  }
  function proxyArgument() {
    if (proxyRemove) return '';
    if (!proxyValue.trim()) return null;
    let parsed; try { parsed = new URL(proxyValue.trim()); } catch (_) { throw new Error('Укажи полный адрес прокси, например http://proxy.example:8080.'); }
    if (!['http:', 'https:', 'socks5:'].includes(parsed.protocol) || !parsed.hostname) throw new Error('Для загрузок нужен HTTP-, HTTPS- или SOCKS5-прокси.');
    return proxyValue.trim();
  }
  async function run(action) {
    if (pending) return;
    pending = true; operationError = ''; render();
    try { await action(); }
    catch (error) { operationError = safeError(error); }
    finally { pending = false; render(); }
  }
  function acceptJob(job) {
    if (!job || !['running', 'done', 'failed'].includes(job.state)) throw new Error('Jarvis не подтвердил запуск. Проверь состояние подготовки перед повтором.');
    snapshot = State.mergeSnapshot(snapshot || EMPTY, job); dismissedJob = null; eventVersion += 1;
  }
  async function startModels(ids) {
    // Model installation reads the saved proxy. Save an explicit edited value
    // before submitting the job; core setup is not re-run just to change it.
    const proxy = proxyArgument();
    if (proxy !== null) {
      const saved = await timedInvoke('service_set_proxy', { proxy });
      if (saved?.ok === false) throw new Error(saved.error || 'Не удалось сохранить прокси');
    }
    acceptJob(await timedInvoke('models_install', { ids }));
  }
  function deferAgents() {
    agentsDeferred = true; dismissedJob = jobKey(); screen = 'capabilities';
    render(); content.focus({ preventScroll: true });
  }
  primary.addEventListener('click', () => {
    if (pending || primary.disabled) return;
    if (operationError) return run(refresh);
    const active = activeScreen();
    if (active === 'welcome') { screen = 'agents'; render(); content.focus({ preventScroll: true }); return; }
    if (active === 'agents') {
      if (missingAgents()) return deferAgents();
      if (snapshot.coreReady) { screen = 'capabilities'; render(); content.focus({ preventScroll: true }); return; }
      return run(async () => { acceptJob(await timedInvoke('onboarding_run', { proxy: proxyArgument() })); });
    }
    if (active === 'capabilities') { const ids = selectedPlan(); if (!ids.length) { screen = 'ready'; render(); content.focus({ preventScroll: true }); return; } return run(() => startModels(ids)); }
    if (active === 'degraded') return run(async () => { if (snapshot.job.kind === 'models' && snapshot.job.tasks?.length) await startModels(snapshot.job.tasks); else acceptJob(await timedInvoke('onboarding_run', { proxy: proxyArgument() })); });
    if (active === 'ready') return run(async () => { await timedInvoke('onboarding_open_panel'); await closeWindow(); });
  });
  secondary.addEventListener('click', () => {
    if (pending) return;
    const active = activeScreen(); operationError = '';
    if (active === 'installing') return closeWindow();
    if (active === 'agents' && !snapshot.coreReady) return deferAgents();
    if (active === 'ready') return run(async () => { await timedInvoke('onboarding_open_settings'); await closeWindow(); });
    if (active === 'degraded') { dismissedJob = jobKey(); screen = snapshot.job.kind === 'models' && (snapshot.coreReady || agentsDeferred) ? 'capabilities' : 'agents'; }
    else screen = active === 'capabilities' ? 'agents' : 'welcome';
    render(); content.focus({ preventScroll: true });
  });
  async function closeWindow() {
    if (closing) return; closing = true;
    try { await timedInvoke('onboarding_close'); }
    catch (error) { closing = false; operationError = safeError(error); render(); }
  }
  $('close').addEventListener('click', closeWindow);
  rail.addEventListener('click', event => {
    const button = event.target.closest('.rail-step');
    if (!button || button.disabled || pending || activeScreen() === 'installing') return;
    dismissedJob = jobKey(); operationError = ''; screen = button.dataset.screen; render(); content.focus({ preventScroll: true });
  });
  document.addEventListener('keydown', event => {
    if (event.isComposing) return;
    if (event.key === 'Escape') { event.preventDefault(); closeWindow(); }
    if (event.key === 'Enter' && !event.metaKey && !event.ctrlKey && !event.altKey && [document.body, content].includes(event.target) && !primary.disabled) { event.preventDefault(); primary.click(); }
  });
  async function receive(event) {
    const payload = event.payload; if (!payload || typeof payload !== 'object') return;
    const wasRunning = snapshot?.job?.state === 'running';
    snapshot = State.mergeSnapshot(snapshot || EMPTY, payload); eventVersion += 1;
    if (snapshot.job.state === 'running') dismissedJob = null;
    if (wasRunning && snapshot.job.state === 'done') screen = snapshot.job.kind === 'models' ? 'ready' : 'capabilities';
    render();
    if (snapshot.job.state !== 'running') await refresh();
  }
  async function init() {
    render();
    const events = ['install_job_changed', 'onboarding:done', 'models_install_all_done'];
    // Failure to subscribe must not prevent a readable, retryable first screen.
    await Promise.all(events.map(name => listen(name, receive).catch(() => {})));
    await refresh();
    // Events are a fast path. A lost completion event must not strand an open
    // window on a running job; the native snapshot remains authoritative.
    const poll = setInterval(() => {
      if (!closing && !checking && !pending && snapshot?.job?.state === 'running') refresh();
    }, 3000);
    window.addEventListener('pagehide', () => clearInterval(poll), { once: true });
  }
  init();
})();
