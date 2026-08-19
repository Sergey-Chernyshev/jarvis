/* Режим «Циклы»: рутина, которую агент крутит сам.
 *
 * Экранов один — список, — а всё остальное состояния над ним: библиотека
 * шаблонов (когда циклов ещё нет), конструктор, пульт живого цикла, экран
 * итерации, вопрос цикла, сработавший ограничитель и утренний отчёт. Это не
 * семь разных страниц: у них общая шапка и общий список слева, а меняется то,
 * что цикл про себя рассказывает прямо сейчас.
 *
 * Всё, что тут нарисовано, приходит от демона: конфигурации, журнал итераций,
 * расход, вердикты критика. Ни одного придуманного числа. */

(() => {
  /**
   * Узел из строки-селектора, необязательных атрибутов и детей.
   *
   * Вторым аргументом идут атрибуты — но ТОЛЬКО если это обычный объект. Узел
   * или массив узлов там означают детей: так короче писать, и именно так этот
   * помощник и звали в половине мест. Раньше он честно пытался разложить
   * массив в атрибуты, получал имя «0» — недопустимое для атрибута — и падал,
   * унося с собой весь экран режима.
   */
  const el = (tag, attrs, ...kids) => {
    const [name, ...cls] = tag.split('.');
    const n = document.createElement(name || 'div');
    if (cls.length) n.className = cls.join(' ');
    const isProps =
      attrs != null && typeof attrs === 'object' && !Array.isArray(attrs) && !attrs.nodeType;
    if (isProps) {
      for (const [k, v] of Object.entries(attrs)) {
        if (v == null || v === false) continue;
        if (k === 'text') n.textContent = v;
        else if (k === 'html') n.innerHTML = v;
        else if (k.startsWith('on')) n.addEventListener(k.slice(2), v);
        else n.setAttribute(k, v === true ? '' : v);
      }
    } else if (attrs != null) {
      kids.unshift(attrs);
    }
    for (const kid of kids.flat(Infinity)) if (kid) n.appendChild(kid);
    return n;
  };

  /* ---------- состояние ---------- */

  let state = { loops: [], templates: [], busy: false };
  /* Что открыто: null — список, иначе { id, screen, n } */
  let open = null;
  let root = null;
  /* Черновик конструктора: правки живут тут, пока не нажали «Сохранить».
   * Иначе каждое нажатие клавиши уезжало бы на диск и обратно, а поле ввода
   * дёргалось бы на каждом снимке от демона. */
  let draft = null;

  const byId = (id) => state.loops.find((l) => l.id === id);

  const fmtTokens = (n) => (n >= 1000 ? `${Math.round(n / 1000)}k` : String(n || 0));
  const fmtMoney = (n) => `$${(n || 0).toFixed(2)}`;
  const fmtTime = (ms) => (ms ? new Date(ms).toLocaleTimeString('ru', { hour: '2-digit', minute: '2-digit' }) : '—');
  const fmtWhen = (ms) => {
    if (!ms) return '—';
    const left = ms - Date.now();
    if (left <= 0) return 'сейчас';
    const h = Math.floor(left / 3600000);
    const m = Math.round((left % 3600000) / 60000);
    return h ? `через ${h} ч ${m} м` : `через ${m} м`;
  };

  const VERDICT = {
    running: ['идёт', 'run'],
    passed: ['прошла', 'ok'],
    returned: ['возврат критика', 'warn'],
    gateFailed: ['красный гейт', 'bad'],
    failed: ['сорвалась', 'bad'],
  };

  const STOP = {
    exit: 'условие выхода выполнено',
    tokens: 'ограничитель: токены',
    iterations: 'ограничитель: итерации',
    time: 'ограничитель: время',
    drift: 'ушёл от цели',
    stopped: 'остановлен вручную',
    failed: 'сорвался',
  };

  /* ---------- обмен с демоном ---------- */

  async function pull() {
    const res = await window.jarvis.loopsGet();
    if (res && res.ok) { state = res; render(); }
  }

  function apply(res) {
    if (res && res.ok) { state = res; render(); }
  }

  /* Ошибку показываем на месте, а не глотаем: цикл не запустился — человек
   * обязан узнать почему, иначе он будет ждать результата всю ночь. */
  function note(msg, bad) {
    const bar = root && root.querySelector('.lp-note');
    if (!bar) return;
    bar.textContent = msg || '';
    bar.hidden = !msg;
    bar.classList.toggle('bad', !!bad);
  }

  async function call(fn, okMsg) {
    const res = await fn();
    if (res && res.ok === false) { note(res.error || 'не вышло', true); return false; }
    if (okMsg) note(okMsg, false);
    await pull();
    return true;
  }

  /* ---------- список циклов слева ---------- */

  function loopRow(l) {
    const run = l.run;
    const state5 = run ? run.state : 'idle';
    const badge =
      state5 === 'running' ? 'идёт'
      : state5 === 'asking' ? 'спрашивает'
      : state5 === 'done' ? 'завершён'
      : state5 === 'stopped' ? (STOP[run.stop] || 'остановлен')
      : 'не запущен';
    const row = el('div.lp-row', {
      onclick: () => { open = { id: l.id, screen: run && run.state === 'asking' ? 'ask' : 'console' }; draft = null; render(); },
    },
      el('div.lp-row-name', { text: l.name || 'без имени' }),
      el('div.lp-row-sub', { text: `${l.wakeLabel} · ${badge}` }),
    );
    if (open && open.id === l.id) row.classList.add('active');
    if (state5 === 'running') row.classList.add('running');
    if (state5 === 'asking' || l.pendingReview > 0) row.classList.add('needs-you');
    if (l.pendingReview > 0) row.appendChild(el('span.lp-dot', { title: `${l.pendingReview} ждёт взгляда` }));
    return row;
  }

  /* ---------- библиотека шаблонов: первый вход ---------- */

  function library() {
    return el('div.lp-lib',
      el('div.lp-h1', { text: 'Библиотека шаблонов' }),
      el('div.lp-h2', { text: 'рутина, которую агент будет крутить сам — ночью или по расписанию. Возьми шаблон или собери с нуля: откроется конструктор, ничего не сохранится, пока не нажмёшь «Создать цикл»' }),
      el('div.lp-cards', state.templates.map((t) =>
        el('div.lp-card', { onclick: () => createFrom(t.id) },
          el('div.lp-card-name', { text: t.name }),
          el('div.lp-card-hint', { text: t.hint }),
        ))),
      el('div.lp-scratch',
        el('button.j-btn.is-primary', { text: 'Описать словами', onclick: () => createFrom(null) }),
        el('button.j-btn', { text: 'Собрать с нуля', onclick: () => createFrom(null) }),
        el('span.lp-hint', { text: 'шаблон — это заготовка: шаги и ограничители всё равно твои' }),
      ),
    );
  }

  /* Заготовка живёт в панели, пока её не сохранят. Раньше создание сразу
   * писало пустой цикл на диск, и передумавший на первом же поле человек
   * оставлял в списке «без имени» навсегда. */
  async function createFrom(template) {
    const res = await window.jarvis.loopsDraft(template || null);
    if (!res || !res.ok) { note((res && res.error) || 'не удалось собрать заготовку', true); return; }
    draft = res.item;
    open = { id: null, screen: 'new' };
    render();
  }

  /* ---------- конструктор: пять шагов и ограничители ---------- */

  /* Справочник конструктора: модели агентов и каталог заготовок. Грузится один
   * раз при входе в режим — это статика, дёргать её на каждый показ незачем.
   * Старый бэкенд без команды — не повод ломаться: остаётся встроенный список
   * моделей, а каталог честно скажет, что ему нужна свежая сборка. */
  let catalog = null;
  const FALLBACK_MODELS = {
    claude: [
      { id: 'fable', label: 'Fable' }, { id: 'opus', label: 'Opus' },
      { id: 'sonnet', label: 'Sonnet' }, { id: 'haiku', label: 'Haiku' },
    ],
    codex: [
      { id: 'gpt-5.5', label: 'GPT-5.5' }, { id: 'gpt-5-codex', label: 'Codex' },
      { id: 'gpt-5', label: 'GPT-5' },
    ],
  };
  async function loadCatalog() {
    if (catalog) return catalog;
    if (typeof window.jarvis.loopsCatalog !== 'function') return null;
    try {
      const res = await window.jarvis.loopsCatalog();
      if (res && res.ok) catalog = res;
    } catch (e) { /* останемся на встроенных списках */ }
    return catalog;
  }
  /* Каталог агентов панели; в отдельном окне его нет — живём на loopsCatalog. */
  const agentsApi = () => (typeof window !== 'undefined' && window.JarvisAgents) || null;
  const agentIds = () => {
    const api = agentsApi();
    if (api) return api.present().map((a) => a.id);
    if (catalog && catalog.models) return Object.keys(catalog.models);
    return Object.keys(FALLBACK_MODELS);
  };
  /* У codex модель и reasoning критика задаёт он сам — селект был бы враньём. */
  const picksItsOwnModel = (agent) => {
    const api = agentsApi();
    return api ? !api.hasSeparateEffort(agent) : agent === 'codex';
  };
  const modelsFor = (agent) => {
    const own = catalog && catalog.models && catalog.models[agent];
    if (own && own.length) return own;
    const api = agentsApi();
    const known = api ? api.models(agent).map((m) => ({ id: m.id, label: m.name })) : null;
    return (known && known.length) ? known : (FALLBACK_MODELS[agent] || FALLBACK_MODELS.claude);
  };
  const modelLabel = (agent, id) => {
    const hit = modelsFor(agent).find((m) => m.id === id);
    return hit ? hit.label : id;
  };

  const field = (label, value, oninput, hint, extra) => {
    const input = el('input.lp-input', {
      value: value == null ? '' : String(value),
      oninput: (e) => oninput(e.target.value),
    });
    return el('label.lp-field',
      el('span.lp-field-label', { text: label }),
      extra ? el('div.lp-withrow', input, extra) : input,
      hint ? el('span.lp-hint', { text: hint }) : null,
    );
  };

  const check = (label, on, onchange, hint) => {
    const box = el('input', { type: 'checkbox', onchange: (e) => onchange(e.target.checked) });
    box.checked = !!on;
    return el('label.lp-check', box, el('span', { text: label }), hint ? el('span.lp-hint', { text: hint }) : null);
  };

  /* Выбор из известного — вместо ввода по памяти. Незнакомое сохранённое
   * значение не выбрасываем, а показываем как есть: выбор человека старше
   * нашего списка. */
  const selectField = (label, options, value, onchange, hint) => {
    const sel = el('select.lp-input', options.map((o) => el('option', { value: o.id, text: o.label })));
    if (value && !options.some((o) => o.id === value)) {
      sel.appendChild(el('option', { value, text: value }));
    }
    sel.value = value || (options[0] && options[0].id) || '';
    sel.addEventListener('change', () => onchange(sel.value));
    return el('label.lp-field',
      el('span.lp-field-label', { text: label }),
      sel,
      hint ? el('span.lp-hint', { text: hint }) : null,
    );
  };

  /* ---------- каталог заготовок: выбирать, а не вспоминать ---------- */

  /**
   * Оверлей каталога: поиск, разделы, карточки. Выбор вставляет команду в
   * обычное поле — она остаётся редактируемой. Много кастомизации, мало
   * запоминания: человек выбирает по названию и описанию, а синтаксис
   * приезжает сам.
   */
  function openCatalog(slot, onPick) {
    const overlay = el('div.lp-shade');
    const close = () => {
      overlay.remove();
      if (document.removeEventListener) document.removeEventListener('keydown', onKey);
    };
    const onKey = (e) => { if (e.key === 'Escape') close(); };
    overlay.addEventListener('click', (e) => { if (e.target === overlay) close(); });
    if (document.addEventListener) document.addEventListener('keydown', onKey);

    const grid = el('div.lp-cat-grid');
    const chips = el('div.lp-cat-chips');
    const search = el('input.lp-input', { placeholder: 'найти заготовку…' });
    const box = el('div.lp-cat',
      el('div.lp-cat-head',
        el('span.lp-cat-title', { text: slot === 'source' ? 'Источники задач' : 'Гейты-проверки' }),
        el('button.j-btn.lp-ghost', { text: '×', title: 'закрыть', onclick: close }),
      ),
      search, chips, grid,
    );
    overlay.appendChild(box);
    root.appendChild(overlay);

    let activeCat = null; // раздел; null — все
    const paint = (items) => {
      const q = (search.value || '').trim().toLowerCase();
      const shown = items.filter((p) =>
        (!activeCat || p.category === activeCat) &&
        (!q || `${p.name} ${p.hint} ${p.command} ${p.category}`.toLowerCase().includes(q)));
      grid.textContent = '';
      if (!shown.length) {
        grid.appendChild(el('div.lp-empty', {
          text: 'Ничего не нашлось. Свою команду всегда можно вписать прямо в поле — каталог лишь избавляет от набора по памяти.',
        }));
        return;
      }
      shown.forEach((p) => grid.appendChild(
        el('div.lp-cat-card', { onclick: () => { onPick(p); close(); } },
          el('div.lp-cat-name', { text: p.name }),
          el('div.lp-cat-hint', { text: p.hint }),
          el('div.lp-cat-cmd', { text: p.command, title: p.command }),
        )));
    };

    Promise.resolve(loadCatalog()).then((c) => {
      const items = ((c && c.presets) || []).filter((p) => p.slot === slot);
      if (!items.length) {
        grid.appendChild(el('div.lp-empty', { text: 'Каталог недоступен — нужна свежая сборка приложения.' }));
        return;
      }
      const cats = [...new Set(items.map((p) => p.category))];
      const chip = (label, val) => {
        const c2 = el('button.lp-chip', {
          text: label,
          onclick: () => {
            activeCat = val;
            [...chips.children].forEach((x) => x.classList && x.classList.remove && x.classList.remove('on'));
            c2.classList.add('on');
            paint(items);
          },
        });
        return c2;
      };
      const first = chip('все', null);
      first.classList.add('on');
      chips.appendChild(first);
      cats.forEach((cName) => chips.appendChild(chip(cName, cName)));
      search.addEventListener('input', () => paint(items));
      paint(items);
      try { search.focus(); } catch (e) { /* подставному DOM тестов фокус не нужен */ }
    });
  }

  /* ---------- «как это будет работать» ---------- */

  const razWord = (n) => {
    const d10 = n % 10, d100 = n % 100;
    if (d10 >= 2 && d10 <= 4 && (d100 < 12 || d100 > 14)) return 'раза';
    return 'раз';
  };

  /**
   * Вся конфигурация — одним человеческим абзацем.
   *
   * Форма из пяти шагов отвечает на вопрос «что настроить», но не на вопрос
   * «что произойдёт». Ровно из этой дыры и растёт когнитивная нагрузка:
   * человек собирает картину в голове из десятка полей. Абзац собирает её за
   * него и переписывается на каждое нажатие клавиши.
   */
  function explain(d) {
    const wake = d.schedule.wake;
    const when = typeof wake === 'string' || !wake ? 'По нажатию «Запустить»'
      : wake.daily ? `Каждый день в ${wake.daily.at || '…'}`
      : wake.every ? (Number(wake.every.minutes) >= 60 && Number(wake.every.minutes) % 60 === 0
          ? `Каждые ${wake.every.minutes / 60} ч`
          : `Каждые ${wake.every.minutes || '…'} мин`)
      : 'По нажатию «Запустить»';
    const repo = (d.sandbox.repo || '').trim();
    const place = repo
      ? (d.sandbox.worktree ? `в отдельном worktree репозитория ${repo}` : `прямо в ${repo}`)
      : '…репозиторий пока не указан';
    const cmd = (d.source.command || '').trim();
    const goal = (d.source.goal || '').trim();
    const src = cmd
      ? 'возьмёт задачи у команды-источника'
      : goal ? `пойдёт к цели «${goal.length > 70 ? goal.slice(0, 70) + '…' : goal}»` : '…источник задач пока пуст';
    const g = (d.exit.gates || []).filter((x) => (x.command || '').trim());
    const gates = g.length
      ? `прогонит ${g.length === 1 ? 'гейт' : 'гейты'}: ${g.map((x) => x.name || x.command).slice(0, 4).join(', ')}`
      : 'детерминированных гейтов нет';
    const critic = d.exit.critic.enabled
      ? `дифф посмотрит критик на ${modelLabel(d.agent || 'claude', d.exit.critic.model) || 'модели по умолчанию'}`
      : 'критик выключен';
    const streak = Math.max(1, Number(d.exit.streak) || 1);
    const walls = [
      d.limits.tokens ? `${fmtTokens(d.limits.tokens)} токенов` : null,
      d.limits.iterations ? `${d.limits.iterations} итераций` : null,
      d.limits.minutes ? `${Math.round(d.limits.minutes / 60 * 10) / 10} ч` : null,
    ].filter(Boolean).join(' · ');
    const sample = d.sampling.every
      ? `Каждая ${d.sampling.every}-я итерация ждёт твоего взгляда.`
      : 'Выборочная проверка выключена — цикл покажет только итог.';
    const memory = d.memory.enabled ? ` Выводы каждой итерации лягут в ${d.memory.file || 'дневник'}.` : '';
    return `${when} агент ${d.agent || 'claude'} ${src}, сделает один шаг ${place}, ${gates}; ${critic}. ` +
      `Цикл завершится, когда всё будет зелёным ${streak} ${razWord(streak)} подряд, ` +
      `и остановится сам, израсходовав ${walls || '…стен нет — так нельзя'}. ${sample}${memory}`;
  }

  /**
   * Описать словами — и получить заполненную форму.
   *
   * Конструктор спрашивает «что настроить» двенадцатью полями, а человек
   * думает задачей: «каждую ночь чини флаки и не трогай CI». Здесь описание
   * уходит модели, та раскладывает его по полям, а человек проверяет и правит.
   * Ничего не сохраняется и не запускается само: подтверждение остаётся за
   * человеком — команды-то будут выполняться всю ночь без надзора.
   */
  function composer(d, onFilled) {
    const area = el('textarea.lp-input.lp-ask-text', {
      rows: 3,
      placeholder: 'Каждую ночь чинить флаки-тесты в этом репозитории, гейты — тесты и clippy, критик Opus, не больше 20 итераций',
    });
    const go = el('button.j-btn.is-primary', { text: 'Заполнить за меня' });
    const state = el('span.lp-hint');
    go.addEventListener('click', async () => {
      const text = (area.value || '').trim();
      if (text.length < 8) { state.textContent = 'опиши задачу хотя бы одной фразой'; return; }
      go.disabled = true;
      state.textContent = 'раскладываю по полям…';
      const res = await window.jarvis.loopsCompose(text, d);
      go.disabled = false;
      if (!res || !res.ok) {
        state.textContent = (res && res.error) || 'не вышло — заполни поля руками';
        return;
      }
      // Черновик заменяем целиком и перерисовываем форму: поля заполнены, но
      // это всё ещё черновик — на диск ничего не ушло.
      draft = res.item;
      onFilled(res.problems || []);
    });
    return el('section.lp-ask',
      el('div.lp-ask-h',
        el('span.lp-ask-title', { text: 'Опиши словами' }),
        el('span.lp-ask-sub', { text: 'модель разложит по полям — проверишь и поправишь' }),
      ),
      area,
      el('div.lp-ask-row', go, state),
    );
  }

  function builder(l, isNew) {
    const d = draft || (draft = JSON.parse(JSON.stringify(l)));
    const step = (n, title, sub, ...body) =>
      el('section.lp-step',
        el('div.lp-step-h', el('span.lp-step-n', { text: String(n) }), el('span.lp-step-t', { text: title }),
          sub ? el('span.lp-step-s', { text: sub }) : null),
        el('div.lp-step-b', body),
      );

    const summary = el('div.lp-explain-text', { text: explain(d) });
    const refresh = () => { summary.textContent = explain(d); };

    /* Смена агента меняет и модели критика — перерисовываем целиком;
     * черновик это переживает, он живёт отдельно от DOM. */
    const seg = el('div.lp-seg',
      agentIds().map((a) => {
        const b = el('button', {
          text: a,
          onclick: () => { if (d.agent !== a) { d.agent = a; render(); } },
        });
        if ((d.agent || 'claude') === a) b.classList.add('on');
        return b;
      }));

    const sourceInput = el('input.lp-input', {
      value: d.source.command || '',
      placeholder: 'команда, чей stdout — список задач',
      oninput: (e) => { d.source.command = e.target.value; },
    });
    const sourceRow = el('div.lp-withrow', sourceInput,
      el('button.j-btn.lp-ghost', {
        text: 'из каталога',
        onclick: () => openCatalog('source', (p) => {
          d.source.command = p.command;
          sourceInput.value = p.command;
          refresh();
        }),
      }));

    const gates = el('div.lp-gates');
    const paintGates = () => {
      gates.textContent = '';
      d.exit.gates.forEach((g, i) => {
        gates.appendChild(el('div.lp-gate',
          el('input.lp-input.narrow', { value: g.name, placeholder: 'имя', oninput: (e) => { g.name = e.target.value; } }),
          el('input.lp-input', { value: g.command, placeholder: 'команда', oninput: (e) => { g.command = e.target.value; } }),
          el('button.j-btn.lp-ghost', {
            text: '×', title: 'убрать гейт',
            onclick: () => { d.exit.gates.splice(i, 1); paintGates(); refresh(); },
          }),
        ));
      });
      gates.appendChild(el('div.lp-gate-add',
        el('button.j-btn.lp-ghost', {
          text: '+ из каталога',
          onclick: () => openCatalog('gate', (p) => {
            d.exit.gates.push({ name: p.name, command: p.command });
            paintGates();
            refresh();
          }),
        }),
        el('button.j-btn.lp-ghost', {
          text: '+ свой',
          onclick: () => { d.exit.gates.push({ name: '', command: '' }); paintGates(); },
        }),
      ));
    };
    paintGates();

    /* Варианты приходят от serde в нижнем регистре: "manual" строкой,
     * { daily: { at } }, { every: { minutes } }. */
    const wake = d.schedule.wake;
    const wakeKind = typeof wake === 'string' ? 'manual' : Object.keys(wake)[0];
    const wakeAt = wakeKind === 'daily' ? wake.daily.at : '02:00';
    const wakeEvery = wakeKind === 'every' ? wake.every.minutes : 60;
    const setWake = (kind, arg) => {
      if (kind === 'manual') d.schedule.wake = 'manual';
      else if (kind === 'daily') d.schedule.wake = { daily: { at: arg } };
      else d.schedule.wake = { every: { minutes: Number(arg) || 0 } };
    };
    const wakeSel = el('select.lp-input',
      el('option', { value: 'manual', text: 'только руками' }),
      el('option', { value: 'daily', text: 'каждый день в…' }),
      el('option', { value: 'every', text: 'каждые N минут' }));
    wakeSel.value = wakeKind;
    const wakeArg = el('input.lp-input.narrow', {
      value: wakeKind === 'every' ? String(wakeEvery) : wakeAt,
      oninput: (e) => setWake(wakeSel.value, e.target.value),
    });
    wakeArg.hidden = wakeKind === 'manual';
    wakeSel.addEventListener('change', () => {
      wakeArg.hidden = wakeSel.value === 'manual';
      wakeArg.value = wakeSel.value === 'every' ? '60' : '02:00';
      setWake(wakeSel.value, wakeArg.value);
    });

    const problems = el('div.lp-problems');
    const paintProblems = (list) => {
      problems.textContent = '';
      (list || []).forEach((p) => problems.appendChild(el('div.lp-problem', { text: p })));
    };
    paintProblems(l.problems);

    /* Модели ЕГО агента: раньше здесь жёстко стояли claude, и выбор был чужим. */
    const criticAgent = d.agent || 'claude';
    const criticModel = picksItsOwnModel(criticAgent)
      ? el('div.lp-hint', { text: `модель и усилие критика задаёт сам ${criticAgent} — в его настройках` })
      : selectField('модель критика', modelsFor(criticAgent), d.exit.critic.model,
          (v) => { d.exit.critic.model = v; refresh(); },
          'на ревью обычно ставят сильнее, чем на исполнение');

    /* Режим цикла: простой (один сценарий: агент → гейты → критик) или
     * пайплайн (граф шагов). Переключатель, а не два разных экрана: цикл
     * остаётся одной сущностью, меняется только его середина. */
    const isPipe = !!d.pipeline;
    const modeSeg = el('div.lp-seg',
      [['простой', false], ['пайплайн', true]].map(([word, want]) => {
        const b = el('button', {
          text: word,
          onclick: () => {
            if (want === isPipe) return;
            // Черновик другого режима не выбрасываем молча: человек мог
            // случайно ткнуть, а собранный граф жалко.
            if (want) d.pipeline = d.pipelineDraft || { start: '', steps: [] };
            else { d.pipelineDraft = d.pipeline; d.pipeline = null; }
            render();
          },
        });
        if (want === isPipe) b.classList.add('on');
        return b;
      }));

    const pipeBox = el('div.pl-editor');
    if (isPipe && typeof JarvisPipeline !== 'undefined') {
      JarvisPipeline.renderTo(pipeBox, d.pipeline, () => refresh());
    }

    const box = el('div.lp-builder',
      el('div.lp-h1', { text: isNew ? 'Новый цикл' : `Настройка · ${l.name || 'без имени'}` }),
      el('div.lp-h2', { text: 'пять шагов и ограничители — а внизу, человеческим языком, что из этого выйдет' }),
      composer(d, (problems) => {
        note(problems.length
          ? 'заполнил — проверь поля, кое-чего ещё не хватает: ' + problems.join('; ')
          : 'заполнил — проверь поля и запускай', !!problems.length);
        render();
      }),
      el('div.lp-headrow',
        field('имя цикла', d.name, (v) => { d.name = v; }),
        el('label.lp-field', el('span.lp-field-label', { text: 'агент' }), seg),
        el('label.lp-field', el('span.lp-field-label', { text: 'как крутить' }), modeSeg),
      ),
      isPipe ? step(1, 'шаги пайплайна', 'что делается и куда ход идёт дальше', pipeBox) : null,
      isPipe ? null : step(1, 'откуда берутся задачи', 'источник',
        field('цель цикла', d.source.goal, (v) => { d.source.goal = v; }, 'своими словами — она уйдёт в промт каждой итерации'),
        el('label.lp-field',
          el('span.lp-field-label', { text: 'команда' }),
          sourceRow,
          el('span.lp-hint', { text: 'её stdout станет списком задач; можно оставить пустой или взять готовую' }),
        ),
      ),
      step(2, 'песочница агента', 'радиус поражения — ветка',
        field('репозиторий', d.sandbox.repo, (v) => { d.sandbox.repo = v; }, 'путь к git-репозиторию на этой машине'),
        field('ветка', d.sandbox.branch, (v) => { d.sandbox.branch = v; }, '{name} и {n} подставятся'),
        check('отдельный worktree', d.sandbox.worktree, (v) => { d.sandbox.worktree = v; },
          'без него агент правит рабочее дерево, в котором ты сам работаешь'),
      ),
      isPipe ? null : step(3, 'условие выхода', 'когда цикл поймёт, что сделал',
        el('div.lp-sub', { text: 'детерминированные гейты' }), gates,
        check('субагент-критик', d.exit.critic.enabled, (v) => { d.exit.critic.enabled = v; },
          'мнение полезно, но выпускать работу в мир по одному мнению нельзя'),
        criticModel,
        field('свой промт критика', d.exit.critic.prompt, (v) => { d.exit.critic.prompt = v; }, 'пусто — возьмётся встроенный'),
        field('итераций подряд', d.exit.streak, (v) => { d.exit.streak = Number(v) || 1; },
          'одной мало: гейт мог пройти случайно — ровно тот флаки-тест, ради которого цикл и заводят'),
      ),
      step(4, 'что переживает итерацию', 'дневник цикла',
        check('вести дневник', d.memory.enabled, (v) => { d.memory.enabled = v; },
          'без него каждая итерация начинается с чистого листа — день сурка'),
        field('файл', d.memory.file, (v) => { d.memory.file = v; }),
      ),
      step(5, 'когда просыпаться', '',
        el('div.lp-wake', wakeSel, wakeArg),
        check('возобновлять после сброса лимита', d.schedule.resumeAfterLimit, (v) => { d.schedule.resumeAfterLimit = v; }),
        check('машина не уснёт, пока цикл крутится', d.schedule.keepAwake, (v) => { d.schedule.keepAwake = v; }),
      ),
      el('section.lp-step.limits',
        el('div.lp-step-h', el('span.lp-step-t', { text: 'ограничители' }), el('span.lp-step-s', { text: 'цикл остановится сам' })),
        el('div.lp-step-b',
          field('токенов за запуск', d.limits.tokens, (v) => { d.limits.tokens = Number(v) || 0; }, '0 — без ограничения'),
          field('итераций', d.limits.iterations, (v) => { d.limits.iterations = Number(v) || 0; }),
          field('минут', d.limits.minutes, (v) => { d.limits.minutes = Number(v) || 0; }),
          check('стоп при дрейфе намерения', d.limits.stopOnDrift, (v) => { d.limits.stopOnDrift = v; }),
          field('выборочная проверка: каждая N-я', d.sampling.every, (v) => { d.sampling.every = Number(v) || 0; },
            '0 — не показывать ничего; смысл автономности в том, чтобы дверь была приоткрыта'),
        ),
      ),
      el('div.lp-explain',
        el('div.lp-explain-label', { text: 'как это будет работать' }),
        summary,
      ),
      problems,
      el('div.lp-actions',
        el('button.j-btn.is-primary', {
          text: isNew ? 'Создать цикл' : 'Сохранить',
          onclick: async () => {
            const res = await window.jarvis.loopsSave(d);
            if (!res || !res.ok) { note((res && res.error) || 'не сохранилось', true); return; }
            paintProblems(res.problems);
            const left = res.problems && res.problems.length;
            note(left ? 'сохранено, но запустить пока нельзя: ' + res.problems.join('; ') : 'сохранено', !!left);
            draft = null;
            await pull();
            open = { id: res.id, screen: left ? 'builder' : 'console' };
            render();
          },
        }),
        el('button.j-btn', {
          text: 'Сохранить и запустить',
          onclick: async () => {
            const saved = await window.jarvis.loopsSave(d);
            if (!saved || !saved.ok) { note((saved && saved.error) || 'не сохранилось', true); return; }
            if (saved.problems && saved.problems.length) {
              paintProblems(saved.problems);
              note('цикл не заполнен: ' + saved.problems.join('; '), true);
              draft = null;
              await pull();
              open = { id: saved.id, screen: 'builder' };
              render();
              return;
            }
            draft = null;
            await pull();
            if (await call(() => window.jarvis.loopsStart(saved.id), 'цикл пошёл')) {
              open = { id: saved.id, screen: 'console' };
              render();
            }
          },
        }),
        isNew
          ? el('button.j-btn.lp-ghost', {
              text: 'Отменить',
              onclick: () => { draft = null; open = null; render(); },
            })
          : el('button.j-btn.lp-ghost', {
              text: 'Удалить цикл',
              onclick: async () => { draft = null; await call(() => window.jarvis.loopsRemove(l.id)); open = null; render(); },
            }),
      ),
    );
    /* Абзац «как это будет работать» переписывается на каждое нажатие: слушаем
     * контейнер, а не каждое поле по отдельности. change — ради чекбоксов и
     * селектов, у которых input случается не везде. */
    box.addEventListener('input', refresh);
    box.addEventListener('change', refresh);
    return box;
  }

  /* ---------- пульт живого цикла ---------- */

  function metric(value, label, sub) {
    return el('div.lp-metric',
      el('div.lp-metric-v', { text: value }),
      el('div.lp-metric-l', { text: label }),
      sub ? el('div.lp-metric-s', { text: sub }) : null);
  }

  function journal(l, run) {
    const rows = [...(run.iterations || [])].reverse().map((it) => {
      const [word, kind] = VERDICT[it.verdict] || ['—', ''];
      const row = el('div.lp-it', { onclick: () => { open = { id: l.id, screen: 'iteration', n: it.n }; render(); } },
        el('span.lp-it-n', { text: String(it.n) }),
        el('span.lp-it-sum', { text: it.summary || '…' }),
        el('span.lp-it-v', { text: word, 'data-kind': kind }),
        el('span.lp-it-t', { text: fmtTokens(it.tokens) }),
      );
      if (it.sampled && !it.reviewed) row.appendChild(el('span.lp-it-eye', { text: 'выборка · посмотри' }));
      return row;
    });
    return el('div.lp-journal',
      el('div.lp-sub', { text: 'журнал итераций' }),
      rows.length ? rows : el('div.lp-empty', { text: 'итераций пока нет' }));
  }

  function console_(l) {
    const run = l.run;
    if (!run) {
      return el('div.lp-console',
        el('div.lp-h1', { text: l.name }),
        el('div.lp-h2', { text: `${l.wakeLabel} · цикл ещё не запускался` }),
        l.problems.length
          ? el('div.lp-problems', l.problems.map((p) => el('div.lp-problem', { text: p })))
          : null,
        el('div.lp-actions',
          el('button.j-btn.is-primary', { text: 'Запустить цикл', onclick: () => call(() => window.jarvis.loopsStart(l.id), 'цикл пошёл') }),
          el('button.j-btn', { text: 'Настроить', onclick: () => { open = { id: l.id, screen: 'builder' }; draft = null; render(); } }),
        ));
    }
    const last = run.iterations[run.iterations.length - 1];
    const live = run.state === 'running';
    const head = live && last ? `итерация ${last.n} · ${VERDICT[last.verdict] ? VERDICT[last.verdict][0] : ''}` : STOP[run.stop] || run.state;

    return el('div.lp-console',
      el('div.lp-h1', { text: l.name }),
      el('div.lp-h2', { text: `запуск ${run.n} · ${head}` }),
      el('div.lp-metrics',
        metric(fmtTokens(run.tokens), 'токены за запуск', fmtMoney(run.costUsd)),
        metric(String(run.iterations.length), 'итераций', `выход: ${l.exit.streak} подряд`),
        metric(String(run.iterations.filter((i) => i.verdict === 'returned').length), 'возвраты критика'),
        metric(String(l.pendingReview), 'ждут твоего взгляда', l.sampling.every ? `выборка: каждая ${l.sampling.every}-я` : 'выборка выключена'),
        metric(fmtWhen(l.nextWake), 'следующее пробуждение', l.wakeLabel),
      ),
      run.state === 'stopped' && run.stop && run.stop !== 'stopped' ? stopped(l, run) : null,
      journal(l, run),
      el('div.lp-intervene',
        (() => {
          const inp = el('input.lp-input', { placeholder: 'Вмешаться в цикл — уточнить цель, добавить ограничение…' });
          inp.addEventListener('keydown', async (e) => {
            if (e.key !== 'Enter' || !inp.value.trim()) return;
            const text = inp.value.trim();
            inp.value = '';
            await call(() => window.jarvis.loopsIntervene(l.id, text), 'уйдёт в следующую итерацию');
          });
          return inp;
        })(),
      ),
      el('div.lp-actions',
        live
          ? el('button.j-btn', { text: 'Остановить', onclick: () => call(() => window.jarvis.loopsStop(l.id), 'остановлен — ветка цела') })
          : el('button.j-btn.is-primary', { text: 'Запустить снова', onclick: () => call(() => window.jarvis.loopsStart(l.id), 'цикл пошёл') }),
        el('button.j-btn', { text: 'Настроить', onclick: () => { open = { id: l.id, screen: 'builder' }; draft = null; render(); } }),
      ),
    );
  }

  /* ---------- сработал ограничитель ---------- */

  function stopped(l, run) {
    const box = el('div.lp-stopped',
      el('div.lp-stopped-h', { text: STOP[run.stop] || 'остановлен' }),
      el('div.lp-stopped-b', {
        text: `Ветка ${run.branch} и worktree целы, состояние записано в память. ${run.stopNote || ''}`,
      }),
    );
    if (run.stop === 'tokens' || run.stop === 'iterations' || run.stop === 'time') {
      const label = run.stop === 'tokens' ? 'Возобновить · +50k токенов'
        : run.stop === 'iterations' ? 'Возобновить · +5 итераций' : 'Возобновить · +1 ч';
      box.appendChild(el('div.lp-actions',
        el('button.j-btn.is-primary', { text: label, onclick: () => call(() => window.jarvis.loopsResume(l.id, null), 'продолжаю') }),
      ));
    }
    return box;
  }

  /* ---------- цикл спрашивает ---------- */

  function ask(l) {
    const run = l.run;
    if (!run || !run.ask) return console_(l);
    const inp = el('textarea.lp-input.lp-answer', { rows: 3, placeholder: 'Ответ уйдёт в цикл — он продолжит с него' });
    return el('div.lp-console',
      el('div.lp-h1', { text: `${l.name} · спрашивает` }),
      el('div.lp-h2', { text: `итерация ${run.ask.iteration} · цикл на паузе` }),
      el('div.lp-ask', { text: run.ask.question }),
      inp,
      el('div.lp-actions',
        el('button.j-btn.is-primary', {
          text: 'Ответить и продолжить',
          onclick: async () => {
            const text = inp.value.trim();
            if (!text) { note('пустой ответ цикл не сдвинет', true); return; }
            await call(() => window.jarvis.loopsAnswer(l.id, text), 'цикл продолжает');
          },
        }),
        el('button.j-btn', { text: 'Остановить цикл', onclick: () => call(() => window.jarvis.loopsStop(l.id), 'остановлен — ветка цела') }),
      ),
      journal(l, run),
    );
  }

  /* ---------- экран итерации: выборочная проверка ---------- */

  function iteration(l, n) {
    const run = l.run;
    const it = run && (run.iterations || []).find((x) => x.n === n);
    if (!it) return console_(l);
    const [word] = VERDICT[it.verdict] || ['—'];
    const diffBox = el('pre.lp-diff', { text: 'дифф грузится…' });
    window.jarvis.loopsDiff(l.id).then((res) => {
      diffBox.textContent = res && res.ok ? (res.diff || 'дифф пуст') : (res && res.error) || 'диффа нет';
    });
    const comment = el('textarea.lp-input.lp-answer', { rows: 2, placeholder: 'возврат уйдёт критику как твой фидбэк' });

    return el('div.lp-iteration',
      el('div.lp-h1', { text: `${l.name} · итерация ${it.n}` }),
      el('div.lp-h2', { text: `${fmtTime(it.startedAt)} · ${fmtTokens(it.tokens)} · ${word}` }),
      el('div.lp-summary', { text: it.summary || '—' }),
      it.files && it.files.length
        ? el('div.lp-files', el('div.lp-sub', { text: 'файлы' }), it.files.map((f) => el('div.lp-file', { text: f })))
        : null,
      it.gates && it.gates.length
        ? el('div.lp-gates-run', el('div.lp-sub', { text: 'гейты' }), it.gates.map((g) =>
            el('div.lp-gate-run', { 'data-ok': g.ok ? 'да' : 'нет' },
              el('span.lp-gate-n', { text: g.name }),
              el('span.lp-gate-v', { text: g.ok ? '✓' : '✗' }),
              g.output ? el('pre.lp-gate-o', { text: g.output }) : null)))
        : null,
      it.critic ? el('div.lp-critic', el('div.lp-sub', { text: 'критик' }), el('div', { text: it.critic })) : null,
      el('div.lp-sub', { text: 'дифф' }), diffBox,
      comment,
      el('div.lp-actions',
        el('button.j-btn.is-primary', {
          text: 'Принять итерацию',
          onclick: async () => {
            await call(() => window.jarvis.loopsReview(l.id, it.n, true, ''), 'принято');
            open = { id: l.id, screen: 'console' }; render();
          },
        }),
        el('button.j-btn', {
          text: 'Вернуть с комментарием',
          onclick: async () => {
            const text = comment.value.trim();
            if (!text) { note('возврат без причины ничего не объяснит циклу', true); return; }
            await call(() => window.jarvis.loopsReview(l.id, it.n, false, text), 'вернул — уйдёт в следующую итерацию');
            open = { id: l.id, screen: 'console' }; render();
          },
        }),
        el('button.j-btn.lp-ghost', { text: '‹ к пульту', onclick: () => { open = { id: l.id, screen: 'console' }; render(); } }),
      ),
    );
  }

  /* ---------- утренний отчёт ---------- */

  function report(l) {
    const run = l.run;
    const passed = run.iterations.filter((i) => i.verdict === 'passed').length;
    const returned = run.iterations.filter((i) => i.verdict === 'returned').length;
    return el('div.lp-report',
      el('div.lp-h1', { text: `${l.name} — цикл завершён` }),
      el('div.lp-h2', {
        text: `${fmtTime(run.startedAt)}–${fmtTime(run.endedAt)} · ${STOP[run.stop] || ''}`,
      }),
      el('div.lp-metrics',
        metric(String(passed), 'итераций прошло'),
        metric(String(returned), 'возвратов критика'),
        metric(fmtMoney(run.costUsd), 'расход', fmtTokens(run.tokens)),
        metric(String(l.pendingReview), 'ждут твоего взгляда'),
      ),
      journal(l, run),
      el('div.lp-actions',
        el('button.j-btn', { text: 'К пульту', onclick: () => { open = { id: l.id, screen: 'console' }; render(); } }),
      ),
    );
  }

  /* ---------- сборка ---------- */

  function render() {
    if (!root) return;
    root.textContent = '';
    const note_ = el('div.lp-note', { hidden: true });

    /* Колонка слева есть ВСЕГДА, и вход в конструктор — её первая строка.
     * Раньше он прятался: библиотека исчезала, стоило открыть любой цикл, а
     * «+ цикл» был прижат к низу колонки — то есть новый цикл было негде
     * начать, если один уже есть. */
    const side = el('aside.lp-side',
      el('button.j-btn.is-primary.lp-new', { text: '+ Новый цикл', onclick: () => createFrom(null) }),
      el('div.lp-side-h', { text: 'мои циклы' }),
    );
    if (state.loops.length) state.loops.forEach((l) => side.appendChild(loopRow(l)));
    else side.appendChild(el('div.lp-empty', { text: 'пока ни одного' }));
    side.appendChild(el('div.lp-side-h', { text: 'из шаблона' }));
    state.templates.forEach((t) =>
      side.appendChild(el('div.lp-row.tmpl', { onclick: () => createFrom(t.id) },
        el('div.lp-row-name', { text: t.name }),
        el('div.lp-row-sub', { text: t.hint }))));

    let body;
    if (open && open.screen === 'new') body = builder(draft, true);
    else if (!open) body = library();
    else {
      const l = byId(open.id);
      if (!l) { open = null; body = library(); }
      else if (open.screen === 'builder') body = builder(l, false);
      else if (open.screen === 'iteration') body = iteration(l, open.n);
      else if (open.screen === 'ask') body = ask(l);
      else if (l.run && l.run.state === 'asking') body = ask(l);
      else if (l.run && (l.run.state === 'done' || (l.run.state === 'stopped' && l.run.stop === 'exit'))) body = report(l);
      else body = console_(l);
    }

    root.appendChild(el('div.lp-wrap', side, el('main.lp-main', note_, body)));
  }

  /* ---------- вход ---------- */

  window.initLoops = (mount) => {
    root = mount;
    root.classList.add('lp');
    render();
    pull();
    loadCatalog(); // статика: греем один раз, чтобы конструктор открылся уже с ней
    if (!window.__loopsBound) {
      window.__loopsBound = true;
      window.jarvis.onLoopsState((s) => apply(s));
    }
  };
})();
