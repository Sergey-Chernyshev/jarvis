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
  const el = (tag, attrs, ...kids) => {
    const [name, ...cls] = tag.split('.');
    const n = document.createElement(name || 'div');
    if (cls.length) n.className = cls.join(' ');
    if (attrs && attrs.nodeType) { n.appendChild(attrs); }
    else if (attrs) {
      for (const [k, v] of Object.entries(attrs)) {
        if (v == null || v === false) continue;
        if (k === 'text') n.textContent = v;
        else if (k === 'html') n.innerHTML = v;
        else if (k.startsWith('on')) n.addEventListener(k.slice(2), v);
        else n.setAttribute(k, v === true ? '' : v);
      }
    }
    for (const kid of kids.flat()) if (kid) n.appendChild(kid);
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
      el('div.lp-h2', { text: 'рутина, которую агент будет крутить сам — ночью или по расписанию' }),
      el('div.lp-cards', state.templates.map((t) =>
        el('div.lp-card', { onclick: () => createFrom(t.id) },
          el('div.lp-card-name', { text: t.name }),
          el('div.lp-card-hint', { text: t.hint }),
        ))),
      el('div.lp-scratch',
        el('button.j-btn', { text: 'Собрать с нуля', onclick: () => createFrom(null) }),
        el('span.lp-hint', { text: 'шаблон — это заготовка: шаги и ограничители всё равно твои' }),
      ),
    );
  }

  async function createFrom(template) {
    const res = await window.jarvis.loopsCreate(template || null, null);
    if (res && res.ok) { await pull(); open = { id: res.id, screen: 'builder' }; draft = null; render(); }
    else note((res && res.error) || 'не удалось создать цикл', true);
  }

  /* ---------- конструктор: пять шагов и ограничители ---------- */

  const field = (label, value, oninput, hint) =>
    el('label.lp-field',
      el('span.lp-field-label', { text: label }),
      el('input.lp-input', { value: value == null ? '' : String(value), oninput: (e) => oninput(e.target.value) }),
      hint ? el('span.lp-hint', { text: hint }) : null,
    );

  const check = (label, on, onchange, hint) => {
    const box = el('input', { type: 'checkbox', onchange: (e) => onchange(e.target.checked) });
    box.checked = !!on;
    return el('label.lp-check', box, el('span', { text: label }), hint ? el('span.lp-hint', { text: hint }) : null);
  };

  function builder(l) {
    const d = draft || (draft = JSON.parse(JSON.stringify(l)));
    const step = (n, title, sub, ...body) =>
      el('section.lp-step',
        el('div.lp-step-h', el('span.lp-step-n', { text: String(n) }), el('span.lp-step-t', { text: title }),
          sub ? el('span.lp-step-s', { text: sub }) : null),
        el('div.lp-step-b', body),
      );

    const gates = el('div.lp-gates');
    const paintGates = () => {
      gates.textContent = '';
      d.exit.gates.forEach((g, i) => {
        gates.appendChild(el('div.lp-gate',
          el('input.lp-input.narrow', { value: g.name, placeholder: 'имя', oninput: (e) => { g.name = e.target.value; } }),
          el('input.lp-input', { value: g.command, placeholder: 'команда', oninput: (e) => { g.command = e.target.value; } }),
          el('button.j-btn.lp-ghost', { text: '×', title: 'убрать гейт', onclick: () => { d.exit.gates.splice(i, 1); paintGates(); } }),
        ));
      });
      gates.appendChild(el('button.j-btn.lp-ghost', {
        text: '+ гейт',
        onclick: () => { d.exit.gates.push({ name: '', command: '' }); paintGates(); },
      }));
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

    return el('div.lp-builder',
      el('div.lp-h1', { text: 'Конструктор — все пять шагов и ограничители' }),
      el('div.lp-h2', { text: 'полная конфигурация перед запуском' }),
      field('имя цикла', d.name, (v) => { d.name = v; }),
      step(1, 'откуда берутся задачи', 'источник',
        field('цель цикла', d.source.goal, (v) => { d.source.goal = v; }, 'своими словами — она уйдёт в промт каждой итерации'),
        field('команда', d.source.command, (v) => { d.source.command = v; }, 'её stdout станет списком задач; можно оставить пустой'),
      ),
      step(2, 'песочница агента', 'радиус поражения — ветка',
        field('репозиторий', d.sandbox.repo, (v) => { d.sandbox.repo = v; }, 'путь к git-репозиторию на этой машине'),
        field('ветка', d.sandbox.branch, (v) => { d.sandbox.branch = v; }, '{name} и {n} подставятся'),
        check('отдельный worktree', d.sandbox.worktree, (v) => { d.sandbox.worktree = v; },
          'без него агент правит рабочее дерево, в котором ты сам работаешь'),
      ),
      step(3, 'условие выхода', 'когда цикл поймёт, что сделал',
        el('div.lp-sub', { text: 'детерминированные гейты' }), gates,
        check('субагент-критик', d.exit.critic.enabled, (v) => { d.exit.critic.enabled = v; },
          'мнение полезно, но выпускать работу в мир по одному мнению нельзя'),
        field('модель критика', d.exit.critic.model, (v) => { d.exit.critic.model = v; }),
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
      problems,
      el('div.lp-actions',
        el('button.j-btn.is-primary', {
          text: 'Сохранить',
          onclick: async () => {
            const res = await window.jarvis.loopsSave(d);
            if (res && res.ok) {
              paintProblems(res.problems);
              note(res.problems && res.problems.length ? 'сохранено, но запустить пока нельзя' : 'сохранено', false);
              draft = null;
              await pull();
            } else note((res && res.error) || 'не сохранилось', true);
          },
        }),
        el('button.j-btn', {
          text: 'Запустить цикл',
          onclick: async () => {
            const saved = await window.jarvis.loopsSave(d);
            if (saved && saved.ok && saved.problems && saved.problems.length) {
              paintProblems(saved.problems);
              note('цикл не заполнен: ' + saved.problems.join('; '), true);
              return;
            }
            draft = null;
            if (await call(() => window.jarvis.loopsStart(l.id), 'цикл пошёл')) {
              open = { id: l.id, screen: 'console' };
              render();
            }
          },
        }),
        el('button.j-btn.lp-ghost', {
          text: 'Удалить цикл',
          onclick: async () => { draft = null; await call(() => window.jarvis.loopsRemove(l.id)); open = null; render(); },
        }),
      ),
    );
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

    const list = el('div.lp-list');
    state.loops.forEach((l) => list.appendChild(loopRow(l)));
    list.appendChild(el('button.j-btn.lp-ghost.lp-new', { text: '+ цикл', onclick: () => createFrom(null) }));

    let body;
    if (!state.loops.length) body = library();
    else if (!open) body = library();
    else {
      const l = byId(open.id);
      if (!l) { open = null; body = library(); }
      else if (open.screen === 'builder') body = builder(l);
      else if (open.screen === 'iteration') body = iteration(l, open.n);
      else if (open.screen === 'ask') body = ask(l);
      else if (l.run && l.run.state === 'asking') body = ask(l);
      else if (l.run && (l.run.state === 'done' || (l.run.state === 'stopped' && l.run.stop === 'exit'))) body = report(l);
      else body = console_(l);
    }

    root.appendChild(el('div.lp-wrap',
      state.loops.length ? el('aside.lp-side', list) : null,
      el('main.lp-main', note_, body)));
  }

  /* ---------- вход ---------- */

  window.initLoops = (mount) => {
    root = mount;
    root.classList.add('lp');
    render();
    pull();
    if (!window.__loopsBound) {
      window.__loopsBound = true;
      window.jarvis.onLoopsState((s) => apply(s));
    }
  };
})();
