/* Readiness onboarding. Rust owns the install job; this file only renders its snapshot. */
(function () {
  'use strict';

  const State = window.JarvisOnboardingState;
  const tauri = window.__TAURI__;
  const invoke = tauri && tauri.core ? tauri.core.invoke : async () => null;
  const listen = tauri && tauri.event ? tauri.event.listen : async () => () => {};
  const content = document.getElementById('content');
  const primary = document.getElementById('primary');
  const secondary = document.getElementById('secondary');
  const announcer = document.getElementById('announcer');
  const rail = document.getElementById('rail');

  const EMPTY = {
    coreReady: false,
    agents: [],
    transport: [],
    capabilities: [],
    warnings: [],
    proxyConfigured: false,
    job: { state: 'idle', kind: '', tasks: [], steps: [], failures: [] },
  };
  const modelMeta = [
    { key: 'whisper', id: 'whisper-turbo', title: 'Whisper large-v3-turbo', detail: 'Быстрая локальная диктовка', size: '~574 МБ', icon: 'W' },
    { key: 'qwen', id: 'qwen3-runtime', title: 'Qwen3-ASR MLX', detail: 'Точная локальная диктовка на Apple Silicon', size: '~1–3 ГБ', icon: 'Q' },
    { key: 'wake', id: 'hey_jarvis', title: 'Голосовая активация', detail: 'Фраза «Hey Jarvis», всегда локально', size: '~4 МБ', icon: 'H' },
    { key: 'silero', id: 'silero', title: 'Silero voice', detail: 'Локальная русская озвучка', size: '~1 ГБ', icon: 'S' },
  ];

  let snapshot = null;
  let screen = 'welcome';
  let proxyValue = '';
  let proxyTouched = false;
  const selection = { whisper: false, qwen: false, wake: false, silero: false, qwenSize: 'qwen3-0.6b' };

  function h(tag, attrs, children) {
    const node = document.createElement(tag);
    for (const [key, value] of Object.entries(attrs || {})) {
      if (key === 'class') node.className = value;
      else if (key === 'text') node.textContent = value;
      else if (key === 'dataset') Object.assign(node.dataset, value);
      else if (key.startsWith('on') && typeof value === 'function') node.addEventListener(key.slice(2), value);
      else if (value !== null && value !== undefined) node.setAttribute(key, String(value));
    }
    for (const child of children || []) {
      if (child) node.appendChild(typeof child === 'string' ? document.createTextNode(child) : child);
    }
    return node;
  }

  function findCapability(id) {
    return (snapshot.capabilities || []).find((item) => item.id === id) || null;
  }

  function eyebrow(text) {
    return h('div', { class: 'eyebrow' }, [h('span', { class: 'pulse' }), text]);
  }

  function heading(title, lead) {
    return [
      h('h1', { id: 'screen-title', text: title }),
      h('p', { class: 'lead', text: lead }),
    ];
  }

  function readinessItem(item) {
    const ready = Boolean(item.ready);
    const available = item.available !== false;
    return h('div', { class: 'item', dataset: { ready: String(ready) } }, [
      h('span', { class: 'status-icon', text: ready ? '✓' : (available ? '!' : '—'), 'aria-hidden': 'true' }),
      h('div', {}, [
        h('div', { class: 'item-title', text: item.label || item.id }),
        h('div', { class: 'item-detail', text: item.detail || item.action || '' }),
      ]),
      h('span', { class: 'badge', text: ready ? 'ready' : (available ? 'action' : 'not found') }),
    ]);
  }

  function warningList(values) {
    if (!values || !values.length) return null;
    return h('ul', { class: 'warnings' }, values.map((value) => h('li', { text: value })));
  }

  function renderWelcome() {
    const proxyWrap = h('div', { hidden: '' });
    const proxyButton = h('button', {
      class: 'proxy-toggle', type: 'button',
      text: snapshot && snapshot.proxyConfigured ? 'Сетевой прокси сохранён · изменить' : 'Нужен корпоративный прокси?',
      onclick: () => {
        proxyWrap.hidden = !proxyWrap.hidden;
        if (!proxyWrap.hidden) proxyWrap.querySelector('input').focus();
      },
    });
    const proxyInput = h('input', {
      class: 'proxy-field', type: 'password', value: proxyValue,
      placeholder: snapshot && snapshot.proxyConfigured ? 'Оставь пустым, чтобы не менять' : 'http://user:pass@host:port',
      autocomplete: 'off', spellcheck: 'false', 'aria-label': 'Egress-прокси',
      oninput: (event) => { proxyValue = event.target.value; proxyTouched = true; },
    });
    proxyWrap.appendChild(proxyInput);
    return [
      eyebrow('System readiness'),
      h('div', { class: 'hero-mark', text: 'J', 'aria-hidden': 'true' }),
      ...heading('Один центр управления агентами', 'Jarvis соединит уже установленные Claude Code и Codex с уведомлениями, диктовкой и живыми сессиями. Сначала — только быстрое локальное ядро.'),
      h('div', { class: 'system-note', text: 'Private · local · models on demand' }),
      proxyButton,
      proxyWrap,
    ];
  }

  function renderChecking() {
    return [
      eyebrow('Reading local signals'),
      h('div', { class: 'hero-mark', text: 'J', 'aria-hidden': 'true' }),
      ...heading('Собираю состояние системы', 'Проверяю CLI, hook-контракты, runtime socket, transport и локальные модели. Никаких сетевых запросов на этом шаге.'),
      h('div', { class: 'progress-card', role: 'status', 'aria-live': 'polite' }, [
        h('div', { class: 'progress-head' }, [h('span', { class: 'spinner', 'aria-hidden': 'true' }), h('span', { text: 'Локальная диагностика' })]),
        h('div', { class: 'progress-line', text: 'Сверяю фактическое состояние, а не сохранённый флаг установки' }),
        h('div', { class: 'progress-track' }, [h('div', { class: 'progress-fill', style: '--pct:36%' })]),
      ]),
    ];
  }

  function renderAgents() {
    const nodes = [
      eyebrow(snapshot.coreReady ? 'Core online' : 'Connection check'),
      ...heading('Подключим то, что уже есть', 'Jarvis регистрирует lifecycle hooks отдельно для каждого найденного агента. Чужие hooks сохраняются.'),
      h('div', { class: 'section-title', text: 'Агенты' }),
      h('div', { class: 'list' }, (snapshot.agents || []).map(readinessItem)),
      h('div', { class: 'section-title', text: 'Транспорт' }),
      h('div', { class: 'list' }, (snapshot.transport || []).map(readinessItem)),
    ];
    const warnings = warningList(snapshot.warnings);
    if (warnings) nodes.push(warnings);
    return nodes;
  }

  function capabilityRow(meta) {
    const current = findCapability(meta.id);
    const ready = Boolean(current && current.ready);
    const input = h('input', {
      type: 'checkbox',
      checked: selection[meta.key] ? '' : null,
      disabled: ready ? '' : null,
      'aria-label': meta.title,
      onchange: (event) => { selection[meta.key] = event.target.checked; render(); },
    });
    const side = h('div', { class: 'cap-side' }, [h('b', { text: ready ? 'УСТАНОВЛЕНО' : meta.size })]);
    if (meta.key === 'qwen' && !ready) {
      const select = h('select', {
        class: 'qwen-size', 'aria-label': 'Размер Qwen',
        onchange: (event) => { selection.qwenSize = event.target.value; },
        onclick: (event) => event.stopPropagation(),
      }, [
        h('option', { value: 'qwen3-0.6b', text: '0.6B · быстрее' }),
        h('option', { value: 'qwen3-1.7b', text: '1.7B · точнее' }),
      ]);
      select.value = selection.qwenSize;
      side.appendChild(select);
    }
    const card = h('div', { class: 'item' }, [
      h('span', { class: 'cap-check', text: '✓', 'aria-hidden': 'true' }),
      h('div', {}, [h('div', { class: 'item-title', text: meta.title }), h('div', { class: 'item-detail', text: meta.detail })]),
      side,
    ]);
    return h('label', { class: 'capability' }, [input, card]);
  }

  function renderCapabilities() {
    return [
      eyebrow('Optional · local first'),
      ...heading('Добавь только нужные возможности', 'Ничего не выбрано заранее. Любую модель можно установить или удалить позже в настройках.'),
      h('div', { class: 'section-title', text: 'Локальные модели' }),
      h('div', { class: 'list' }, modelMeta.map(capabilityRow)),
      h('div', { class: 'notice', text: 'Silero v4_ru и bundled wake-word weights имеют ограничения non-commercial use. Для коммерческого распространения выбери совместимые модели и лицензии.' }),
    ];
  }

  function renderInstalling() {
    const job = snapshot.job || EMPTY.job;
    const steps = Array.isArray(job.steps) ? job.steps : [];
    const latest = steps[steps.length - 1];
    const pct = latest && typeof latest.pct === 'number' ? latest.pct : null;
    return [
      eyebrow(job.kind === 'models' ? 'Capability install' : 'Core install'),
      ...heading(job.kind === 'models' ? 'Готовим выбранные модели' : 'Соединяем контур', 'Окно можно закрыть: установка продолжится, а прогресс восстановится при следующем открытии.'),
      h('div', { class: 'progress-card', role: 'status', 'aria-live': 'polite' }, [
        h('div', { class: 'progress-head' }, [h('span', { class: 'spinner', 'aria-hidden': 'true' }), h('span', { text: latest ? latest.phase : 'Запускаю…' })]),
        h('div', { class: 'progress-line', text: latest && latest.msg ? latest.msg : 'Проверяю окружение и зависимости' }),
        h('div', { class: 'progress-track' }, [h('div', { class: 'progress-fill', style: `--pct:${pct === null ? 18 : pct}%` })]),
      ]),
      h('div', { class: 'section-title', text: 'Последние этапы' }),
      h('div', { class: 'list' }, steps.slice(-4).map((step) => readinessItem({
        id: step.phase, label: step.scope && step.scope !== 'core' ? `${step.scope} · ${step.phase}` : step.phase,
        detail: step.msg, ready: step.state === 'done', available: true,
      }))),
    ];
  }

  function renderDegraded() {
    const failures = (snapshot.job && snapshot.job.failures) || [];
    const kind = State.derive(snapshot).failureKind;
    const recovery = {
      network: ['Канал загрузки недоступен', 'Проверю proxy/DNS и продолжу с последнего безопасного шага. Уже установленные части не скачиваются повторно.'],
      disk: ['Недостаточно места на диске', 'Освободи место и повтори. Частичный файл не считается готовой моделью и не повредит runtime.'],
      permission: ['Нужно подтверждение доступа', 'Claude/Codex могут ждать trust для hooks, а macOS — Accessibility или Microphone. Jarvis не обходит эти разрешения скрыто.'],
      hooks: ['Контур hooks повреждён', 'Jarvis перепишет только собственные регистрации; сторонние hooks останутся нетронутыми.'],
      unknown: ['Модуль ответил неожиданно', 'Рабочая часть системы сохранена. Повтор идемпотентен, точная причина остаётся ниже.'],
    }[kind || 'unknown'];
    return [
      eyebrow(`Recovery · ${kind || 'unknown'}`),
      h('div', { class: 'hero-mark', text: '!', 'aria-hidden': 'true' }),
      ...heading(recovery[0], recovery[1]),
      warningList(failures.length ? failures : snapshot.warnings),
      h('div', { class: 'notice', text: 'Проверь сеть, PATH и доступность Claude/Codex. Секреты прокси и токены в этот экран и operational logs не выводятся.' }),
    ];
  }

  function renderReady() {
    const readyCount = (snapshot.capabilities || []).filter((item) => item.ready).length;
    const runtimeState = State.derive(snapshot).runtimeState;
    const online = runtimeState === 'online';
    return [
      eyebrow(online ? 'Jarvis online' : 'Runtime warming'),
      h('div', { class: 'ready-orbit', text: '✓', 'aria-hidden': 'true' }),
      ...heading(
        online ? 'Система готова к работе' : 'Контур подключён',
        online
          ? 'Hooks подключены, события приходят в локальный runtime. Опциональные возможности не блокируют работу.'
          : 'Hooks готовы. Runtime socket поднимается в фоне; панель можно открыть уже сейчас.',
      ),
      h('div', {
        class: 'system-note',
        text: `${(snapshot.agents || []).filter((item) => item.ready).length} agents · ${readyCount} local modules · ${online ? 'online' : 'warming'}`,
      }),
      h('div', { class: 'shortcut' }, [h('kbd', { text: '⌘J' }), h('span', { text: 'открыть панель' })]),
      online ? null : h('div', { class: 'notice', text: 'Если socket не станет online после запуска панели, открой диагностику: hook registration останется целым.' }),
    ];
  }

  function effectiveScreen() {
    const derived = State.derive(snapshot);
    if (derived.screen === 'checking') return 'checking';
    if (derived.screen === 'installing' || derived.screen === 'degraded') return derived.screen;
    if (screen === 'agents' && snapshot.coreReady) return 'agents';
    if ((screen === 'capabilities' || screen === 'ready') && !snapshot.coreReady) return 'agents';
    return screen;
  }

  function setRail(active) {
    const logical = active === 'installing'
      ? ((snapshot && snapshot.job && snapshot.job.kind) === 'models' ? 'capabilities' : 'agents')
      : (active === 'degraded' ? ((snapshot && snapshot.job && snapshot.job.kind) === 'models' ? 'capabilities' : 'agents') : active);
    const order = ['welcome', 'agents', 'capabilities', 'ready'];
    const index = logical === 'checking' ? 0 : Math.max(0, order.indexOf(logical));
    rail.style.setProperty('--rail-fill', `${index * 33.333}%`);
    for (const button of rail.querySelectorAll('.rail-step')) {
      const itemIndex = order.indexOf(button.dataset.screen);
      button.dataset.state = itemIndex < index ? 'done' : (itemIndex === index ? 'active' : 'pending');
      button.disabled = effectiveScreen() === 'installing';
    }
  }

  function setActions(active) {
    primary.disabled = false;
    secondary.hidden = false;
    secondary.textContent = 'Назад';

    if (active === 'checking') {
      primary.textContent = 'Проверяю…';
      primary.disabled = true;
      secondary.hidden = true;
    } else if (active === 'welcome') {
      primary.textContent = 'Проверить систему';
      secondary.hidden = true;
    } else if (active === 'agents') {
      primary.textContent = snapshot.coreReady ? 'К возможностям' : 'Подключить агентов';
      secondary.textContent = 'Назад';
    } else if (active === 'capabilities') {
      const count = State.selectedPlan(selection).length;
      primary.textContent = count ? `Скачать выбранное · ${count}` : 'Продолжить без загрузок';
      secondary.textContent = 'Назад';
    } else if (active === 'installing') {
      primary.textContent = 'Установка идёт…';
      primary.disabled = true;
      secondary.textContent = 'Продолжить в фоне';
    } else if (active === 'degraded') {
      const kind = State.derive(snapshot).failureKind;
      primary.textContent = kind === 'network' ? 'Повторить соединение'
        : (kind === 'disk' ? 'Проверить место и повторить'
          : (kind === 'permission' ? 'Проверить доступ снова' : 'Повторить безопасно'));
      secondary.textContent = 'Открыть настройки';
    } else if (active === 'ready') {
      primary.textContent = 'Открыть Jarvis';
      secondary.textContent = 'Настройки';
    }
  }

  function render() {
    const active = effectiveScreen();
    let nodes;
    if (active === 'checking') nodes = renderChecking();
    else if (active === 'welcome') nodes = renderWelcome();
    else if (active === 'agents') nodes = renderAgents();
    else if (active === 'capabilities') nodes = renderCapabilities();
    else if (active === 'installing') nodes = renderInstalling();
    else if (active === 'degraded') nodes = renderDegraded();
    else nodes = renderReady();
    content.replaceChildren(...nodes.filter(Boolean));
    content.dataset.screen = active;
    content.scrollTop = 0;
    content.classList.remove('enter');
    requestAnimationFrame(() => content.classList.add('enter'));
    setRail(active);
    setActions(active);
    announcer.textContent = content.querySelector('h1')?.textContent || '';
  }

  async function refresh() {
    const next = await invoke('onboarding_get').catch(() => null);
    if (next) snapshot = next;
    render();
  }

  async function closeWindow() {
    await invoke('onboarding_close').catch(() => {});
  }

  primary.addEventListener('click', async () => {
    const active = effectiveScreen();
    if (active === 'welcome') {
      screen = 'agents';
      render();
      return;
    }
    if (active === 'agents') {
      if (snapshot.coreReady) {
        screen = 'capabilities';
        render();
      } else {
        const proxy = proxyTouched ? proxyValue.trim() : null;
        await invoke('onboarding_run', { proxy }).catch(() => null);
        await refresh();
      }
      return;
    }
    if (active === 'capabilities') {
      const ids = State.selectedPlan(selection);
      if (ids.length) {
        await invoke('models_install', { ids }).catch(() => null);
        await refresh();
      } else {
        screen = 'ready';
        render();
      }
      return;
    }
    if (active === 'degraded') {
      const job = snapshot.job || EMPTY.job;
      if (job.kind === 'models' && job.tasks && job.tasks.length) {
        await invoke('models_install', { ids: job.tasks }).catch(() => null);
      } else {
        await invoke('onboarding_run', { proxy: proxyTouched ? proxyValue.trim() : null }).catch(() => null);
      }
      await refresh();
      return;
    }
    if (active === 'ready') {
      await invoke('onboarding_open_panel').catch(() => invoke('onboarding_open_settings'));
      await closeWindow();
    }
  });

  secondary.addEventListener('click', async () => {
    const active = effectiveScreen();
    if (active === 'agents') screen = 'welcome';
    else if (active === 'capabilities') screen = 'agents';
    else if (active === 'installing') return closeWindow();
    else if (active === 'degraded' || active === 'ready') {
      await invoke('onboarding_open_settings').catch(() => {});
      return;
    }
    render();
  });

  document.getElementById('close').addEventListener('click', closeWindow);
  rail.addEventListener('click', (event) => {
    const button = event.target.closest('.rail-step');
    if (!button || effectiveScreen() === 'installing') return;
    const target = button.dataset.screen;
    if ((target === 'capabilities' || target === 'ready') && !snapshot.coreReady) return;
    screen = target;
    render();
  });

  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') closeWindow();
  });

  async function init() {
    render();
    await listen('install_job_changed', async (event) => {
      if (event.payload && event.payload.coreReady !== undefined) snapshot = event.payload;
      else if (event.payload) snapshot = { ...snapshot, job: event.payload };
      render();
      if (event.payload && event.payload.state && event.payload.state !== 'running') await refresh();
    });
    await listen('onboarding:done', (event) => {
      if (event.payload) snapshot = event.payload;
      screen = snapshot.coreReady ? 'capabilities' : 'agents';
      render();
    });
    await listen('models_install_all_done', async () => {
      await refresh();
      screen = snapshot.job && snapshot.job.state === 'failed' ? 'capabilities' : 'ready';
      render();
    });
    await refresh();
    screen = snapshot.coreReady ? 'ready' : 'welcome';
    render();
  }

  init();
})();
