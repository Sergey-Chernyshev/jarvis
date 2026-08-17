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
        else if (k.startsWith('on')) n.addEventListener(k.slice(2), v);
        else n.setAttribute(k, v === true ? '' : v);
      }
    } else if (attrs != null) {
      kids.unshift(attrs);
    }
    for (const kid of kids.flat(Infinity)) if (kid) n.appendChild(kid);
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

  const fmtTokens = (n) => (n >= 1000 ? `${Math.round(n / 1000)}k` : String(Math.round(n) || 0));
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
    ready: 'готов к мержу',
    conflict: 'конфликт',
    merged: 'влита',
    failed: 'не поднялась',
  };

  /* ---------- обмен ---------- */

  async function pull() {
    try {
      const res = await window.jarvis.bundleGet();
      if (res && res.ok) { state = res; render(); }
    } catch (e) { /* старый бэкенд — экран скажет сам */ }
  }

  function note(msg, bad) {
    const bar = root && root.querySelector('.bd-note');
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
    const overlay = el('div.lp-shade');
    const close = () => overlay.remove();
    overlay.addEventListener('click', (e) => { if (e.target === overlay) close(); });

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
          onclick: () => { onPick(path); close(); },
        }),
        el('div.lp-withrow.bd-dir-new',
          newName,
          el('button.j-btn.lp-ghost', {
            text: '+ сюда',
            onclick: () => {
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
    root.appendChild(overlay);

    const go = async (next) => {
      crumb.textContent = 'смотрю…';
      let res;
      try { res = await window.jarvis.bundleBrowse(machine, next || ''); }
      catch (e) { res = null; }
      if (!res || !res.ok) {
        crumb.textContent = (res && res.path) || next || '';
        list.textContent = '';
        list.appendChild(el('div.lp-empty', { text: (res && res.error) || 'не дотянулся до машины' }));
        return;
      }
      path = res.path;
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
      if (!res || !res.ok || !(res.known || []).length) return;
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
        const saved = await window.jarvis.bundleSave(d);
        if (!saved || !saved.ok) { note((saved && saved.error) || 'не сохранилось', true); return; }
        if (saved.problems && saved.problems.length) {
          note('не хватает: ' + saved.problems.join('; '), true);
          return;
        }
        if (await call(() => window.jarvis.bundleStart(saved.id), 'руки поднимаются — ветка и worktree на каждую')) {
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
          placeholder: 'Задача руки — это её первое сообщение. Например: «Экран логина, magic-link, сессии в keychain. UI по theme.css, ничего нового не изобретать.»',
          oninput: (e) => { h.task = e.target.value; syncBtn(); },
        });
        ta.value = h.task || '';
        hands.appendChild(el('div.bd-hand-row',
          el('div.bd-hand-head',
            el('span.bd-hand-n', { text: String(i + 1) }),
            el('span.lp-hint', { text: 'своя ветка и worktree — сами' }),
            d.hands.length > 1
              ? el('button.j-btn.lp-ghost', { text: '×', title: 'убрать руку', onclick: () => { d.hands.splice(i, 1); paintHands(); } })
              : null,
          ),
          ta,
        ));
      });
      hands.appendChild(el('button.j-btn.lp-ghost', {
        text: '+ добавить руку',
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
        text: '+ гейт',
        onclick: () => { d.gates.push({ name: '', command: '' }); paintGates(); },
      }));
    };
    paintGates();

    d.machine = d.machine || 'local';
    // Машина — выбор, как в «Проектах»: эта или любой узел из настроек.
    const machineSel = el('select.lp-input',
      machines.map((m) => el('option', { value: m.id, text: m.name + (m.kind === 'remote' && m.sshHost ? ` · ${m.sshHost}` : '') })));
    if (!machines.some((m) => m.id === d.machine)) {
      machineSel.appendChild(el('option', { value: d.machine, text: d.machine }));
    }
    machineSel.value = d.machine;
    machineSel.addEventListener('change', () => { d.machine = machineSel.value; });

    return el('div.bd-start',
      el('div.lp-h1', { text: 'Связка — несколько чатов разом' }),
      el('div.lp-h2', { text: 'каждая рука — обычный чат: пишешь первое сообщение, ветка и worktree создаются сами. Очередь слияний: авторебейз и гейты — сами, вливаешь ты.' }),
      el('div.bd-start-grid',
        field('имя связки', d.name, (v) => { d.name = v; }, 'например: клевер-релиз'),
        el('label.bd-field',
          el('span.bd-label', { text: 'машина' }),
          machineSel,
          el('span.lp-hint', { text: 'эта или любой узел — как в «Проектах»' }),
        ),
        (() => {
          const input = el('input.lp-input', {
            value: d.dir || '',
            oninput: (e) => { d.dir = e.target.value; },
          });
          return el('label.bd-field',
            el('span.bd-label', { text: 'директория' }),
            el('div.lp-withrow', input,
              el('button.j-btn.lp-ghost', {
                text: 'выбрать…',
                onclick: () => openDirPicker(d.machine || 'local', d.dir, (picked) => {
                  d.dir = picked;
                  input.value = picked;
                }),
              })),
            el('span.lp-hint', { text: 'git не обязателен: нет .git или самого каталога — создам и инициализирую сам' }),
          );
        })(),
        field('бюджет на руку, токенов', d.budgetTokens, (v) => { d.budgetTokens = Number(v) || 0; }, 'ориентир на пульте, не ограничитель'),
      ),
      el('div.lp-sub', { text: 'руки' }),
      hands,
      el('div.lp-sub', { text: 'гейты перед очередью' }),
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
    if (h.state === 'ready') return `готов к мержу · очередь #${h.queuePos || '?'} · гейты и критик зелёные`;
    if (h.state === 'conflict') {
      return `конфликт при ребейзе: ${(h.conflictFiles || []).join(', ') || '…'} · чинит сам, попытка ${h.attempt} · очередь ждёт`;
    }
    if (h.state === 'merged') return `влита в ${fmtTime(h.mergedAt) || 'базу'}`;
    if (h.state === 'failed') return 'не поднялась — причина в ленте';
    if (h.state === 'new') return 'ждёт запуска';
    if (h.status === 'waiting') return 'спрашивает — зайди в чат';
    return h.detail || 'работает';
  }

  function handCard(b, h) {
    const card = el('div.bd-card', { 'data-state': h.state },
      el('div.bd-card-head',
        el('span.bd-card-name', { text: h.name || 'рука' }),
        el('span.bd-card-state', { text: STATE_WORD[h.state] || h.state }),
      ),
      el('div.bd-card-line', { text: statusLine(h) }),
      el('div.bd-card-meta', {
        text: [
          h.branch || null,
          h.tokens ? `${fmtTokens(h.tokens)}${b.budgetTokens ? ' / ' + fmtTokens(b.budgetTokens) : ''}` : null,
        ].filter(Boolean).join(' · '),
      }),
    );
    if (h.sessionId) {
      card.classList.add('open');
      card.addEventListener('click', () => {
        if (window.openSessionById) window.openSessionById(h.sessionId);
      });
      card.title = 'открыть чат руки';
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
          ? `последнее вливание ${fmtTime(b.lastMergeAt)} · после каждого — авторебейз хвоста и гейты заново`
          : 'после каждого вливания — авторебейз хвоста и прогон гейтов заново',
      }),
    );
    if (!ready.length && !conflicts.length) {
      box.appendChild(el('div.lp-empty', { text: 'пока пусто — руки встанут в очередь сами, когда закончат и гейты позеленеют' }));
    }
    ready.forEach((h, i) => {
      const row = el('div.bd-qrow',
        el('span.bd-qpos', { text: `#${h.queuePos || i + 1}` }),
        el('span.bd-qname', { text: `${h.name} · ${h.branch}` }),
      );
      if (i === 0) {
        row.appendChild(el('button.j-btn.is-primary.bd-merge', {
          text: `Влить в ${b.base || 'main'}`,
          title: h.canMerge ? 'гейты зелёные, ветка на свежей базе' : 'ждём зелёных гейтов на свежей базе',
          disabled: !h.canMerge,
          onclick: (e) => {
            e.stopPropagation();
            call(() => window.jarvis.bundleMerge(b.id, h.id), 'влито — хвост переребейзится сам');
          },
        }));
      } else {
        row.appendChild(el('span.lp-hint', { text: `после #${i} — авторебейз и гейты` }));
      }
      box.appendChild(row);
    });
    conflicts.forEach((h) => {
      box.appendChild(el('div.bd-qrow.out',
        el('span.bd-qpos', { text: '⚠' }),
        el('span.bd-qname', { text: `${h.name} — выпала: конфликт · чинит сам, попытка ${h.attempt}` }),
        el('span.lp-hint', { text: 'вернётся в хвост, когда ребейз пройдёт и гейты позеленеют' }),
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
      box.appendChild(el('div.lp-sub', { text: 'горячие файлы' }));
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
    const addTask = el('textarea.lp-input.bd-task', { rows: 2, placeholder: 'добавить руку: задача — первое сообщение нового чата' });
    return el('div.bd-console',
      el('div.bd-head',
        el('div',
          el('div.lp-h1', { text: `связка · ${b.name}` }),
          el('div.lp-h2', {
            text: `${machineName(b.machine)} · ${b.dir} → ${b.base || 'main'}${b.paused ? ' · на паузе' : ''}`,
          }),
        ),
        el('div.lp-actions',
          el('button.j-btn', {
            text: b.paused ? 'Продолжить' : 'Пауза всем',
            onclick: () => call(() => window.jarvis.bundlePause(b.id, !b.paused)),
          }),
          el('button.j-btn.lp-ghost', {
            text: 'Новая связка',
            onclick: async () => {
              const res = await window.jarvis.bundleDraft();
              if (res && res.ok) { draft = res.item; draftAuto = false; render(); }
            },
          }),
          el('button.j-btn.lp-ghost', {
            text: 'Убрать связку',
            onclick: () => {
              // Ветки остаются — в них работа; уходят worktree и карточки.
              call(() => window.jarvis.bundleRemove(b.id), 'связка убрана — ветки целы');
            },
          }),
        ),
      ),
      el('div.bd-note', { hidden: true }),
      el('div.bd-grid', b.hands.map((h) => handCard(b, h))),
      el('div.bd-add',
        addTask,
        el('button.j-btn.lp-ghost', {
          text: '+ рука',
          onclick: async () => {
            const task = addTask.value.trim();
            if (!task) { note('задача пустая — руке нечего делать', true); return; }
            addTask.value = '';
            await call(() => window.jarvis.bundleAddHand(b.id, task, null), 'рука поднимается');
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
        window.jarvis.bundleDraft().then((res) => {
          if (res && res.ok && !draft && !state.bundles.length) {
            draft = res.item;
            draftAuto = true;
            render();
          }
        });
        return el('div.lp-empty', { text: 'готовлю форму…' });
      }
      return console_(b);
    })();
    const note_ = draft ? el('div.bd-note', { hidden: true }) : null;
    root.appendChild(el('div.bd-wrap', note_, body));
  }

  window.initBundle = (mount) => {
    root = mount;
    render();
    pull();
    loadMachines().then(render);
    if (!window.__bundleBound) {
      window.__bundleBound = true;
      if (window.jarvis.onBundleState) {
        window.jarvis.onBundleState((s) => { if (s && s.ok) { state = s; render(); } });
      }
    }
  };
})();
