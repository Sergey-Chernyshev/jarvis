/* Режим «Связка» — несколько агентов «в 10 рук» над одним проектом.
 *
 * Два экрана. Старт: пачка задач, каждая станет обычным чатом — ветка и
 * worktree создадутся сами. Пульт: карточки рук, внимание — на исключения
 * (конфликт, «ждёт твоего „влить“»), очередь слияний, горячие файлы и лента.
 * Кнопка «Влить в main» — единственное решение, оставленное человеку;
 * всё остальное связка готовит сама.
 *
 * Все числа и состояния приходят от демона: git, гейты, сессии рук. */

(() => {
  const el = (tag, attrs, ...kids) => {
    const [name, ...cls] = tag.split('.');
    const n = document.createElement(name || 'div');
    if (cls.length) n.className = cls.join(' ');
    const isProps = attrs != null && typeof attrs === 'object' && !Array.isArray(attrs) && !attrs.nodeType;
    if (isProps) {
      for (const [k, v] of Object.entries(attrs)) {
        if (v == null || v === false) continue;
        if (k === 'text') n.textContent = v;
        else if (k.startsWith('on')) n.addEventListener(k.slice(2), async e => {
          if (n.getAttribute('aria-busy') === 'true') return;
          try {
            const result = v(e);
            if (result && typeof result.then === 'function') { n.setAttribute('aria-busy', 'true'); await result; }
          } catch (error) { note(String(error), true); }
          finally { n.removeAttribute('aria-busy'); }
        });
        else n.setAttribute(k, v === true ? '' : v);
      }
    } else if (attrs != null) {
      kids.unshift(attrs);
    }
    for (const kid of kids.flat(Infinity)) if (kid) n.appendChild(kid);
    if (name === 'div' && isProps && typeof attrs.onclick === 'function') {
      n.setAttribute('role', 'button'); n.setAttribute('tabindex', '0');
      n.addEventListener('keydown', e => {
        if (e.target === n && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); n.click(); }
      });
    }
    return n;
  };

  let state = { bundles: [] };
  /* Машины — эта плюс узлы из настроек. Грузятся один раз: старт-экран должен
   * предлагать выбор, а не поле с именем узла по памяти. */
  let machines = [{ id: 'local', name: 'Эта машина', kind: 'local' }];
  async function loadMachines() {
    try {
      const r = typeof window.jarvis.machinesList === 'function' ? await window.jarvis.machinesList() : null;
      if (Array.isArray(r) && r.length) machines = r;
    } catch (e) { /* останется локальная */ }
  }
  const machineName = (id) =>
    (machines.find((m) => m.id === id) || {}).name || (id === 'local' || !id ? 'Эта машина' : id);
  let root = null;
  /* Черновик старта: живёт в панели, на диск уезжает только по «Запустить». */
  let draft = null;
  /* Черновик, созданный сами́м пустым экраном: его можно молча выбросить,
   * когда приехали настоящие связки. Черновик, открытый человеком через
   * «Новая связка», выбрасывать нельзя. */
  let draftAuto = false;
  /* Какую связку смотрим; null — последнюю живую. */
  let openId = null;
  let message = null, dismissDialog = null, draftLoading = false, draftError = null;
  const additionalTasks = new Map();
  window.jarvisModuleBack = window.jarvisModuleBack || {};
  window.jarvisModuleBack.bundle = () => {
    if (dismissDialog) { dismissDialog(); return true; }
    return false; // Leaving the module retains its unfinished starter.
  };

  const fmtTokens = (n) => Math.max(0, Math.round(Number(n) || 0)).toLocaleString('ru-RU');
  const fmtTime = (ms) => (ms ? new Date(ms).toLocaleTimeString('ru', { hour: '2-digit', minute: '2-digit' }) : '');

  const chats = (n) => {
    const d10 = n % 10, d100 = n % 100;
    if (d10 === 1 && d100 !== 11) return 'чат';
    if (d10 >= 2 && d10 <= 4 && (d100 < 12 || d100 > 14)) return 'чата';
    return 'чатов';
  };

  const STATE_WORD = {
    new: 'ждёт запуска',
    working: 'работает',
    ready: 'готов к слиянию',
    conflict: 'конфликт',
    merged: 'объединён',
    failed: 'ошибка запуска',
  };

  /* ---------- обмен ---------- */

  async function pull() {
    try {
      const res = await window.jarvis.bundleGet();
      if (!res?.ok) throw new Error(res?.error || 'Не удалось загрузить команды агентов.');
      state = res; render(); return true;
    } catch (error) { note(String(error), true); return false; }
  }

  function note(msg, bad) {
    message = msg ? { text: msg, bad: !!bad } : null;
    const bar = root && root.querySelector('.bd-note');
    if (!bar) return;
    bar.textContent = msg || '';
    bar.hidden = !msg;
    bar.classList.toggle('bad', !!bad);
  }

  async function call(fn, okMsg) {
    try {
      const res = await fn();
      if (!res || res.ok === false) throw new Error(res?.error || 'Не удалось выполнить действие.');
      await pull(); note(okMsg || '', false); return true;
    } catch (error) { note(String(error), true); return false; }
  }

  const current = () => {
    if (!state.bundles.length) return null;
    if (openId) {
      const hit = state.bundles.find((b) => b.id === openId);
      if (hit) return hit;
    }
    return state.bundles.find((b) => b.active) || state.bundles[state.bundles.length - 1];
  };

  /* Рука по сессии — для бейджа в чате: «связка · team/billing». */
  window.bundleHandOf = (sessionId) => {
    for (const b of state.bundles) {
      const hand = (b.hands || []).find((h) => h.sessionId === sessionId);
      if (hand) return { bundle: b.name, branch: hand.branch, state: hand.state, worktree: hand.worktree };
    }
    return null;
  };

  /* ---------- выбор директории: обзор вместо памяти ---------- */

  /**
   * Оверлей: известные проекты машины одним кликом плюс обзор её файловой
   * системы. Работает одинаково для этой машины и узла — разница живёт в
   * бэкенде. Поле ввода никуда не девается: обзор — это способ не набирать
   * путь по памяти, а не запрет его набрать.
   */
  function openDirPicker(machine, startPath, onPick) {
    const previousFocus = document.activeElement;
    let browseSequence = 0, closed = false, browsing = true;
    const overlay = el('div.lp-shade');
    const close = () => {
      closed = true; overlay.remove(); if (dismissDialog === close) dismissDialog = null;
      if (previousFocus?.isConnected) previousFocus.focus();
    };
    dismissDialog = close;
    overlay.addEventListener('click', (e) => { if (e.target === overlay) close(); });
    overlay.addEventListener('keydown', e => {
      if (e.key !== 'Tab') return;
      const items = [...overlay.querySelectorAll('button:not(:disabled),input,[tabindex="0"]')]
        .filter(item => !item.disabled && item.getAttribute('tabindex') !== '-1' && !item.closest('[hidden],[aria-hidden="true"]'));
      const index = items.indexOf(document.activeElement);
      if (!items.length) return;
      if (e.shiftKey && index <= 0) { e.preventDefault(); items[items.length - 1].focus(); }
      else if (!e.shiftKey && index === items.length - 1) { e.preventDefault(); items[0].focus(); }
    });

    let path = startPath || '';
    const crumb = el('div.bd-dir-path', { text: '…' });
    const list = el('div.bd-dir-list');
    const newName = el('input.lp-input', { placeholder: 'новая папка — создастся при запуске' });
    const box = el('div.lp-cat',
      el('div.lp-cat-head',
        el('span.lp-cat-title', { text: 'Где работать' }),
        el('button.j-btn.lp-ghost', { text: '×', title: 'закрыть', onclick: close }),
      ),
      crumb,
      el('div.lp-actions.bd-dir-act',
        el('button.j-btn.is-primary', {
          text: 'Выбрать эту директорию',
          onclick: () => { if (!browsing) { onPick(path); close(); } },
        }),
        el('div.lp-withrow.bd-dir-new',
          newName,
          el('button.j-btn.lp-ghost', {
            text: '+ сюда',
            onclick: () => {
              if (browsing) return;
              const name = newName.value.trim().replace(/\/+/g, '');
              if (!name) return;
              onPick(`${path}/${name}`);
              close();
            },
          }),
        ),
      ),
      list,
    );
    overlay.appendChild(box);
    box.setAttribute('role', 'dialog'); box.setAttribute('aria-modal', 'true'); box.setAttribute('aria-label', 'Где работать');
    root.appendChild(overlay);
    newName.focus?.();

    const go = async (next) => {
      const request = ++browseSequence;
      browsing = true;
      const choosing = [...(box.querySelectorAll?.('.bd-dir-act button') || [])];
      for (const button of choosing) button.disabled = true;
      crumb.textContent = 'смотрю…';
      let res;
      try { res = await window.jarvis.bundleBrowse(machine, next || ''); }
      catch (e) { res = { ok: false, error: String(e) }; }
      if (closed || request !== browseSequence) return;
      if (!res || !res.ok) {
        crumb.textContent = (res && res.path) || next || '';
        list.textContent = '';
        list.appendChild(el('div.lp-empty', { text: (res && res.error) || 'не дотянулся до машины' }));
        return;
      }
      path = res.path;
      browsing = false;
      for (const button of choosing) button.disabled = false;
      crumb.textContent = path;
      list.textContent = '';
      if (res.parent && res.parent !== path) {
        list.appendChild(el('div.bd-dir-row.up', { text: '‹ вверх', onclick: () => go(res.parent) }));
      }
      if (!res.dirs.length) {
        list.appendChild(el('div.lp-empty', { text: 'подкаталогов нет — можно выбрать эту или завести новую' }));
      }
      res.dirs.forEach((name) => list.appendChild(
        el('div.bd-dir-row', { text: name, onclick: () => go(`${path}/${name}`) })));
    };

    // Известные проекты — прежде обзора: чаще всего нужный каталог уже там.
    window.jarvis.bundlePlaces(machine).then((res) => {
      if (closed || !res || !res.ok || !(res.known || []).length) return;
      const known = el('div.bd-dir-known',
        el('div.lp-sub', { text: 'известные проекты' }),
        res.known.map((cwd) => el('button.lp-chip', {
          text: cwd.split('/').pop() || cwd,
          title: cwd,
          onclick: () => { onPick(cwd); close(); },
        })),
      );
      box.appendChild(known);
    }).catch(() => {});

    go(startPath || '');
  }

  /* ---------- старт: несколько чатов разом ---------- */

  const field = (label, value, oninput, hint) =>
    el('label.bd-field',
      el('span.bd-label', { text: label }),
      el('input.lp-input', { value: value == null ? '' : String(value), oninput: (e) => oninput(e.target.value) }),
      hint ? el('span.lp-hint', { text: hint }) : null,
    );

  function starter() {
    const d = draft;
    d.hands = d.hands && d.hands.length ? d.hands : [{ task: '' }];
    d.gates = d.gates || [];
    const hands = el('div.bd-hands');
    const countLaunchable = () => d.hands.filter((h) => (h.task || '').trim()).length;
    const startBtn = el('button.j-btn.is-primary', {
      text: 'Запустить 0 чатов',
      onclick: async () => {
        if (!d.dir?.trim()) {
          note(`Выберите директорию на машине «${machineName(d.machine)}».`, true);
          directoryInput?.focus?.();
          return;
        }
        const saved = await window.jarvis.bundleSave(d);
        if (!saved || !saved.ok) { note((saved && saved.error) || 'не сохранилось', true); return; }
        if (saved.problems && saved.problems.length) {
          note('не хватает: ' + saved.problems.join('; '), true);
          return;
        }
        if (await call(() => window.jarvis.bundleStart(saved.id), 'Исполнители запускаются — у каждого своя ветка и worktree.')) {
          draft = null;
          draftAuto = false;
          openId = saved.id;
          render();
        }
      },
    });
    const syncBtn = () => {
      const n = countLaunchable();
      startBtn.textContent = `Запустить ${n} ${chats(n)}`;
      startBtn.disabled = n === 0;
    };

    const paintHands = () => {
      hands.textContent = '';
      d.hands.forEach((h, i) => {
        const ta = el('textarea.lp-input.bd-task', {
          rows: 2,
          placeholder: 'Задача исполнителя — первое сообщение в его чате. Например: «Добавь вход по ссылке из письма и тесты для новой формы.»',
          oninput: (e) => { h.task = e.target.value; syncBtn(); },
        });
        ta.value = h.task || '';
        hands.appendChild(el('div.bd-hand-row',
          el('div.bd-hand-head',
            el('span.bd-hand-n', { text: String(i + 1) }),
            el('span.lp-hint', { text: 'своя ветка и worktree — сами' }),
            d.hands.length > 1
              ? el('button.j-btn.lp-ghost', { text: '×', title: 'Убрать исполнителя', onclick: () => { d.hands.splice(i, 1); paintHands(); } })
              : null,
          ),
          ta,
        ));
      });
      hands.appendChild(el('button.j-btn.lp-ghost', {
        text: '+ добавить исполнителя',
        onclick: () => { d.hands.push({ task: '' }); paintHands(); },
      }));
      syncBtn();
    };
    paintHands();

    const gates = el('div.lp-gates');
    const paintGates = () => {
      gates.textContent = '';
      d.gates.forEach((g, i) => {
        gates.appendChild(el('div.lp-gate',
          el('input.lp-input.narrow', { value: g.name, placeholder: 'имя', oninput: (e) => { g.name = e.target.value; } }),
          el('input.lp-input', { value: g.command, placeholder: 'команда', oninput: (e) => { g.command = e.target.value; } }),
          el('button.j-btn.lp-ghost', { text: '×', onclick: () => { d.gates.splice(i, 1); paintGates(); } }),
        ));
      });
      gates.appendChild(el('button.j-btn.lp-ghost', {
        text: '+ добавить проверку',
        onclick: () => { d.gates.push({ name: '', command: '' }); paintGates(); },
      }));
    };
    paintGates();

    let directoryInput;
    d.machine = d.machine || 'local';
    d.agent = d.agent || 'claude';
    const agentSel = el('select.lp-input', { 'aria-label': 'Агент' },
      el('option', { value: 'claude', text: 'Claude Code' }),
      el('option', { value: 'codex', text: 'Codex' }));
    agentSel.value = d.agent;
    agentSel.addEventListener('change', () => { d.agent = agentSel.value; });
    // Машина — выбор, как в «Проектах»: эта или любой узел из настроек.
    const machineSel = el('select.lp-input',
      machines.map((m) => el('option', { value: m.id, text: m.name + (m.kind === 'remote' && m.sshHost ? ` · ${m.sshHost}` : '') })));
    if (!machines.some((m) => m.id === d.machine)) {
      machineSel.appendChild(el('option', { value: d.machine, text: d.machine }));
    }
    machineSel.value = d.machine;
    machineSel.addEventListener('change', () => {
      if (d.machine === machineSel.value) return;
      d.machine = machineSel.value;
      // Paths belong to one host. Keep the rest of the unfinished form intact.
      d.dir = '';
      if (directoryInput) directoryInput.value = '';
      dismissDialog?.();
      note(`Машина изменена. Выберите директорию на «${machineName(d.machine)}».`);
    });

    return el('div.bd-start',
      el('div.lp-h1', { text: 'Команда — несколько чатов разом' }),
      el('div.lp-h2', { text: 'Каждый исполнитель работает в отдельном чате: ветка и worktree создаются сами. Изменения проходят проверки и попадают в очередь. Слияние подтверждаете вы.' }),
      el('div.bd-start-grid',
        field('Название команды', d.name, (v) => { d.name = v; }, 'например: клевер-релиз'),
        el('label.bd-field',
          el('span.bd-label', { text: 'машина' }),
          machineSel,
          el('span.lp-hint', { text: 'Этот компьютер или удалённая машина' }),
        ),
        (() => {
          const input = directoryInput = el('input.lp-input', {
            value: d.dir || '',
            oninput: (e) => { d.dir = e.target.value; },
          });
          return el('label.bd-field',
            el('span.bd-label', { text: 'директория' }),
            el('div.lp-withrow', input,
              el('button.j-btn.lp-ghost', {
                text: 'выбрать…',
                onclick: () => {
                  const pickedMachine = d.machine;
                  openDirPicker(pickedMachine, d.dir, (picked) => {
                    if (d.machine !== pickedMachine) return;
                    d.dir = picked;
                    input.value = picked;
                  });
                },
              })),
            el('span.lp-hint', { text: 'Создадим папку и Git-репозиторий при запуске, если их нет.' }),
          );
        })(),
        el('label.bd-field',
          el('span.bd-label', { text: 'Агент' }),
          agentSel,
          el('span.lp-hint', { text: 'Перед запуском проверим CLI на выбранной машине. Вход в аккаунт — через CLI.' }),
        ),
        field('Ориентир расхода', d.budgetTokens, (v) => { d.budgetTokens = Math.max(0, Math.round(Number(v) || 0)); }, 'Токенов на исполнителя. Не ограничивает работу. 0 — без ориентира.'),
      ),
      el('div.lp-sub', { text: 'Исполнители' }),
      hands,
      el('div.lp-sub', { text: 'Проверки перед слиянием' }),
      gates,
      el('div.lp-actions',
        startBtn,
        state.bundles.length
          ? el('button.j-btn.lp-ghost', { text: 'Отменить', onclick: () => { draft = null; render(); } })
          : null,
      ),
    );
  }

  /* ---------- пульт: карточки рук ---------- */

  function statusLine(h) {
    if (h.state === 'ready') return `готов к слиянию · очередь #${h.queuePos || '?'} · проверки пройдены`;
    if (h.state === 'conflict') {
      return `конфликт при ребейзе: ${(h.conflictFiles || []).join(', ') || '…'} · чинит сам, попытка ${h.attempt} · очередь ждёт`;
    }
    if (h.state === 'merged') return `объединён · ${fmtTime(h.mergedAt) || 'изменения в основной ветке'}`;
    if (h.state === 'failed') return 'ошибка запуска — причина в ленте событий';
    if (h.state === 'new') return 'ждёт запуска';
    if (h.status === 'waiting') return 'спрашивает — зайди в чат';
    return h.detail || 'работает';
  }

  function handCard(b, h) {
    const card = el('div.bd-card', { 'data-state': h.state },
      el('div.bd-card-head',
        el('span.bd-card-name', { text: h.name || 'исполнитель' }),
        el('span.bd-card-state', { text: STATE_WORD[h.state] || h.state }),
      ),
      el('div.bd-card-line', { text: statusLine(h) }),
      el('div.bd-card-meta', {
        title: 'Расход включает входные, выходные и кэшированные токены. Ориентир не ограничивает работу.',
        text: [
          h.branch || null,
          h.tokens > 0 ? `Расход: ${fmtTokens(h.tokens)} токенов` : null,
          b.budgetTokens > 0 ? `Ориентир: ${fmtTokens(b.budgetTokens)} токенов` : null,
        ].filter(Boolean).join(' · '),
      }),
    );
    if (h.sessionId) {
      card.classList.add('open');
      card.addEventListener('click', () => {
        if (window.openSessionById) window.openSessionById(h.sessionId);
      });
      card.title = 'Открыть чат исполнителя';
    }
    return card;
  }

  function queueBlock(b) {
    const ready = b.hands.filter((h) => h.state === 'ready').sort((x, y) => (x.queuePos || 9) - (y.queuePos || 9));
    const conflicts = b.hands.filter((h) => h.state === 'conflict');
    const working = b.hands.filter((h) => h.state === 'working');
    const box = el('div.bd-queue',
      el('div.lp-sub', { text: 'очередь слияний' }),
      el('div.lp-hint', {
        text: b.lastMergeAt
          ? `Последнее слияние ${fmtTime(b.lastMergeAt)}. Остальные ветки обновляются, проверки запускаются заново.`
          : 'После каждого слияния остальные ветки обновляются, проверки запускаются заново.',
      }),
    );
    if (!ready.length && !conflicts.length) {
      box.appendChild(el('div.lp-empty', { text: 'Пока пусто — исполнители встанут в очередь сами, когда завершат задачи и пройдут проверки.' }));
    }
    ready.forEach((h, i) => {
      const row = el('div.bd-qrow',
        el('span.bd-qpos', { text: `#${h.queuePos || i + 1}` }),
        el('span.bd-qname', { text: `${h.name} · ${h.branch}` }),
      );
      if (i === 0) {
        row.appendChild(el('button.j-btn.is-primary.bd-merge', {
          text: `Влить в ${b.base || 'main'}`,
          title: h.canMerge ? 'Проверки пройдены, ветка обновлена' : 'Ожидаются успешные проверки обновлённой ветки',
          disabled: !h.canMerge,
          onclick: (e) => {
            e.stopPropagation();
            call(() => window.jarvis.bundleMerge(b.id, h.id), 'Изменения объединены. Остальные ветки обновляются.');
          },
        }));
      } else {
        row.appendChild(el('span.lp-hint', { text: `После #${i} — обновление ветки и проверки` }));
      }
      box.appendChild(row);
    });
    conflicts.forEach((h) => {
      box.appendChild(el('div.bd-qrow.out',
        el('span.bd-qpos', { text: '⚠' }),
        el('span.bd-qname', { text: `${h.name} — конфликт · чинит сам, попытка ${h.attempt}` }),
        el('span.lp-hint', { text: 'Вернётся в конец очереди после устранения конфликта и успешных проверок' }),
      ));
    });
    if (working.length) {
      box.appendChild(el('div.lp-hint.bd-qtail', {
        text: `ещё в работе: ${working.map((h) => h.name).join(', ')} — встанут в очередь сами`,
      }));
    }
    return box;
  }

  function sidePanel(b) {
    const box = el('div.bd-side');
    if ((b.hotFiles || []).length) {
      box.appendChild(el('div.lp-sub', { text: 'Файлы нескольких исполнителей' }));
      b.hotFiles.forEach((f) => box.appendChild(
        el('div.bd-hot',
          el('span.bd-hot-file', { text: f.file }),
          el('span.lp-hint', { text: (f.hands || []).join(' + ') }),
        )));
    }
    const events = (b.events || []).slice(-12).reverse();
    if (events.length) {
      box.appendChild(el('div.lp-sub', { text: 'лента' }));
      events.forEach((ev) => box.appendChild(
        el('div.bd-ev',
          el('span.bd-ev-t', { text: fmtTime(ev.at) }),
          el('span.bd-ev-x', { text: ev.text }),
        )));
    }
    return box;
  }

  function console_(b) {
    const addTask = el('textarea.lp-input.bd-task', { rows: 2, placeholder: 'Задача нового исполнителя — первое сообщение в его чате',
      oninput: e => additionalTasks.set(b.id, e.target.value) });
    addTask.value = additionalTasks.get(b.id) || '';
    return el('div.bd-console',
      el('div.bd-head',
        el('div',
          el('div.lp-h1', { text: `команда · ${b.name}` }),
          el('div.lp-h2', {
            text: `${b.agent === 'codex' ? 'Codex' : 'Claude Code'} · ${machineName(b.machine)} · ${b.dir} → ${b.base || 'main'}${b.paused ? ' · на паузе' : ''}`,
          }),
        ),
        el('div.lp-actions',
          el('button.j-btn', {
            text: b.paused ? 'Продолжить' : 'Пауза всем',
            onclick: () => call(() => window.jarvis.bundlePause(b.id, !b.paused)),
          }),
          el('button.j-btn.lp-ghost', {
            text: 'Новая команда',
            onclick: async () => {
              const res = await window.jarvis.bundleDraft();
              if (!res?.ok || !res.item) throw new Error(res?.error || 'Не удалось подготовить форму.');
              draft = res.item; draftAuto = false; render();
            },
          }),
          el('button.j-btn.lp-ghost', {
            text: 'Убрать команду',
            onclick: () => {
              // Ветки остаются — в них работа; уходят worktree и карточки.
              return call(() => window.jarvis.bundleRemove(b.id), 'Команда убрана. Ветки с изменениями сохранены.');
            },
          }),
        ),
      ),
      el('div.bd-grid', b.hands.map((h) => handCard(b, h))),
      el('div.bd-add',
        addTask,
        el('button.j-btn.lp-ghost', {
          text: '+ исполнитель',
          onclick: async () => {
            const task = addTask.value.trim();
            if (!task) { note('Опишите задачу для нового исполнителя.', true); return; }
            if (await call(() => window.jarvis.bundleAddHand(b.id, task, null), 'Исполнитель запускается.')) {
              additionalTasks.delete(b.id); render();
            }
          },
        }),
      ),
      el('div.bd-bottom', queueBlock(b), sidePanel(b)),
    );
  }

  /* ---------- сборка ---------- */

  function render() {
    if (!root) return;
    root.textContent = '';
    // Гонка первого входа: пустой экран заводит черновик, а через мгновение
    // приезжают настоящие связки. Автозаведённый черновик им уступает — иначе
    // старт-форма победила бы пульт навсегда. Черновик человека не трогаем.
    if (draft && draftAuto && state.bundles.length) { draft = null; draftAuto = false; }
    const body = draft ? starter() : (() => {
      const b = current();
      if (!b) {
        // Первый вход: черновик сразу — незачем показывать пустоту с кнопкой.
        if (draftError) return el('div.lp-empty',
          el('p', { text: draftError }),
          el('button.j-btn', { text: 'Повторить', onclick: () => { draftError = null; render(); } }));
        if (!draftLoading) {
          draftLoading = true;
          Promise.resolve().then(() => window.jarvis.bundleDraft()).then(res => {
            if (!res?.ok || !res.item) throw new Error(res?.error || 'Не удалось подготовить форму.');
            if (!draft && !state.bundles.length) { draft = res.item; draftAuto = true; }
          }).catch(error => { draftError = String(error); })
            .finally(() => { draftLoading = false; render(); });
        }
        return el('div.lp-empty', { text: 'готовлю форму…' });
      }
      return console_(b);
    })();
    const note_ = el('div.bd-note', { hidden: true });
    root.appendChild(el('div.bd-wrap', note_, body));
    if (message) note(message.text, message.bad);
  }

  window.initBundle = (mount) => {
    root = mount;
    render();
    pull();
    loadMachines().then(render);
    if (!window.__bundleBound) {
      window.__bundleBound = true;
      if (window.jarvis.onBundleState) {
        window.jarvis.onBundleState((s) => {
          if (s && s.ok) {
            state = s;
            if (!(draft && !draftAuto || root?.contains(document.activeElement) && document.activeElement?.matches('input,textarea,select,[role="combobox"]') || root?.querySelector('[role="combobox"][aria-expanded="true"]'))) render();
          }
        });
      }
    }
  };
})();
