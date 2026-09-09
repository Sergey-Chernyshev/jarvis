/* Конструктор пайплайна: цикл как граф узлов.
 *
 * Экран отвечает на два вопроса: что делается по шагам и куда ход идёт дальше.
 * Полотна со стрелками здесь нет намеренно — и это не отказ от схемы, а
 * разделение труда. На пяти узлах связи читаются списком быстрее, чем ищутся
 * глазами по холсту; на двадцати пяти список бесполезен, но там и нужен не наш
 * самописный холст, а настоящий модельер. Поэтому граф выгружается в BPMN 2.0,
 * правится в Camunda Modeler и забирается обратно (кнопки внизу) — писать
 * третий по счёту редактор диаграмм ради этого незачем.
 *
 * Правки идут в черновик (чистые функции ниже), а на диск он уезжает только по
 * «Сохранить» — как и остальной конструктор циклов.
 */

(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.JarvisPipeline = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  const KINDS = [
    ['agent', 'агент', 'позвать агента с этим промтом'],
    ['shell', 'команда', 'тесты, сборка, линт — всё, что возвращает код'],
    ['review', 'ревью', 'агент читает дифф и отвечает вердиктом'],
    ['human', 'человек', 'спросить и дождаться ответа'],
    ['wait', 'пауза', 'подождать чужую сборку или деплой'],
    ['choice', 'развилка', 'уйдёт ОДИН переход — первый подходящий'],
    ['fork', 'ветвление', 'уйдут ВСЕ ветки разом, каждая в своём worktree'],
    ['join', 'слияние', 'дождаться все ветки и свести их обратно'],
  ];

  /* Шлюзы ничего не делают — они направляют ход. Отсюда и разница в карточке:
   * ни промта, ни попыток, ни модели у них нет и быть не может. */
  const GATEWAYS = ['choice', 'fork', 'join'];
  const isGate = (kind) => GATEWAYS.includes(kind);

  const CONDS = [
    ['always', 'всегда'],
    ['ok', 'если получилось'],
    ['fail', 'если не вышло'],
    ['verdict', 'если вердикт'],
    ['contains', 'если в выводе есть'],
    ['expr', 'выражением'],
  ];

  /* Что делать с конфликтом слияния. Умолчание — остановиться: молча выбрать
   * сторону значит выбросить чью-то ночь работы, ничего об этом не сказав. */
  const CONFLICTS = [
    ['stop', 'остановиться'],
    ['agent', 'отдать агенту'],
    ['ours', 'оставить свою'],
    ['theirs', 'взять чужую'],
  ];

  /* Уникальный id шага: человек его увидит в переменных (${тесты.вывод}),
   * поэтому это имя, а не случайные буквы. */
  function makeId(p, base) {
    const clean = String(base || 'шаг').trim().replace(/\s+/g, '-').slice(0, 24) || 'шаг';
    const taken = new Set((p.steps || []).map((s) => s.id));
    if (!taken.has(clean)) return clean;
    for (let i = 2; i < 99; i += 1) if (!taken.has(`${clean}-${i}`)) return `${clean}-${i}`;
    return `${clean}-${Date.now() % 1000}`;
  }

  function blank(kind) {
    if (kind === 'shell') return { kind: 'shell', command: '' };
    if (kind === 'review') return { kind: 'review', prompt: '', model: '' };
    if (kind === 'human') return { kind: 'human', question: '' };
    if (kind === 'wait') return { kind: 'wait', minutes: 5 };
    if (kind === 'choice') return { kind: 'choice' };
    if (kind === 'fork') return { kind: 'fork' };
    if (kind === 'join') return { kind: 'join' };
    return { kind: 'agent', prompt: '', model: '' };
  }

  /* Новый шаг встаёт В КОНЕЦ и получает переход «всегда → конец»: пайплайн
   * обязан оставаться рабочим после каждой правки, а не только когда человек
   * закончил всю сборку. */
  function addStep(p, kind, name) {
    const steps = p.steps || (p.steps = []);
    const id = makeId(p, name || kindWord(kind));
    const step = Object.assign({ id, name: name || '', retries: 0, next: [{ to: '', cond: 'always' }] }, blank(kind));
    const last = steps[steps.length - 1];
    // Прошлый последний шаг больше не конец: ведём его в новый, иначе новый
    // остался бы недостижимым — самая частая ошибка сборки.
    if (last && (last.next || []).every((f) => !f.to)) {
      last.next = [{ to: id, cond: 'always' }];
    }
    steps.push(step);
    return step;
  }

  /* Параллель одной кнопкой: ветвление, ветки и слияние сразу связанными.
   *
   * По одному узлу это собирается за шесть действий, и на третьем человек
   * забывает завести слияние — а ветвление без слияния означает брошенные
   * worktree'ы, то есть работу, которая никуда не вольётся.
   */
  function addParallel(p, count) {
    const steps = p.steps || (p.steps = []);
    const n = Math.min(6, Math.max(2, Number(count) || 2));
    const last = steps[steps.length - 1];
    const fork = Object.assign({ id: makeId(p, 'ветвление'), name: 'ветвление', retries: 0, next: [] }, blank('fork'));
    steps.push(fork);
    const join = Object.assign({ id: makeId(p, 'слияние'), name: 'слияние', retries: 0, onConflict: 'stop', next: [{ to: '', cond: 'always' }] }, blank('join'));
    const branches = [];
    for (let i = 1; i <= n; i += 1) {
      const b = Object.assign({ id: makeId(p, `ветка-${i}`), name: `ветка ${i}`, retries: 0, next: [] }, blank('agent'));
      steps.push(b);
      branches.push(b);
    }
    steps.push(join);
    fork.next = branches.map((b) => ({ to: b.id, cond: 'always' }));
    for (const b of branches) b.next = [{ to: join.id, cond: 'always' }];
    // Прошлый последний узел больше не конец: ведём его в ветвление.
    if (last && (last.next || []).every((f) => !f.to)) last.next = [{ to: fork.id, cond: 'always' }];
    return fork;
  }

  function removeStep(p, id) {
    p.steps = (p.steps || []).filter((s) => s.id !== id);
    // Переходы в удалённый шаг превратились бы в ссылку в никуда — вместо
    // этого они становятся концом пайплайна, и это видно.
    for (const s of p.steps) s.next = (s.next || []).map((f) => (f.to === id ? { ...f, to: '' } : f));
    if (p.start === id) p.start = (p.steps[0] || {}).id || '';
    return p;
  }

  function moveStep(p, id, dir) {
    const steps = p.steps || [];
    const i = steps.findIndex((s) => s.id === id);
    const j = i + (dir < 0 ? -1 : 1);
    if (i < 0 || j < 0 || j >= steps.length) return p;
    [steps[i], steps[j]] = [steps[j], steps[i]];
    return p;
  }

  function addFlow(step) {
    (step.next || (step.next = [])).push({ to: '', cond: 'always' });
    return step;
  }

  function removeFlow(step, i) {
    step.next = (step.next || []).filter((_, k) => k !== i);
    return step;
  }

  function kindWord(kind) {
    const hit = KINDS.find((k) => k[0] === kind);
    return hit ? hit[1] : kind;
  }

  function condWord(cond) {
    const hit = CONDS.find((c) => c[0] === cond);
    return hit ? hit[1] : cond;
  }

  function conflictWord(id) {
    const hit = CONFLICTS.find((c) => c[0] === (id || 'stop'));
    return hit ? hit[1] : id;
  }

  /* Переход человеческой строкой — то, что человек читает вместо стрелок. */
  function flowLine(p, f) {
    const to = f.to ? (stepTitle(p, f.to) || f.to) : 'конец';
    const cond = f.cond === 'verdict' ? `если вердикт «${f.verdict || '—'}»`
      : f.cond === 'contains' ? `если в выводе есть «${f.text || '—'}»`
        : f.cond === 'expr' ? `если ${f.text || '—'}`
          : condWord(f.cond);
    return `${cond} → ${to}`;
  }

  function stepTitle(p, id) {
    const s = (p.steps || []).find((x) => x.id === id);
    if (!s) return '';
    return s.name && s.name.trim() ? s.name.trim() : s.id;
  }

  /* Заготовки: пайплайн из головы не собирают. Это те же сценарии, которые
   * люди и описывают словами, когда объясняют, чего хотят от агента. */
  function presets() {
    return [
      {
        id: 'разведка-правка-тесты',
        name: 'разведка → правка → тесты',
        what: 'сначала понять, потом чинить, красные тесты возвращают к правке',
        pipeline: {
          start: 'разведка',
          steps: [
            {
              id: 'разведка', name: 'разведка', kind: 'agent', model: '', retries: 0,
              prompt: 'Разберись в задаче и опиши план. Файлов не трогай.',
              next: [{ to: 'правка', cond: 'always' }],
            },
            {
              id: 'правка', name: 'правка', kind: 'agent', model: '', retries: 0,
              prompt: 'Сделай по плану: ${разведка.вывод}',
              next: [{ to: 'тесты', cond: 'always' }],
            },
            {
              id: 'тесты', name: 'тесты', kind: 'shell', command: 'cargo test', retries: 1,
              next: [{ to: 'починить', cond: 'fail' }, { to: '', cond: 'always' }],
            },
            {
              id: 'починить', name: 'починить', kind: 'agent', model: '', retries: 0,
              prompt: 'Тесты красные, вот вывод:\n${тесты.вывод}\nПочини причину, а не симптом.',
              next: [{ to: 'тесты', cond: 'always' }],
            },
          ],
        },
      },
      {
        id: 'параллель',
        name: 'параллель: фронт ∥ бэк → слияние',
        what: 'две ветки работают одновременно в своих worktree, потом сводятся',
        pipeline: {
          start: 'план',
          steps: [
            {
              id: 'план', name: 'план', kind: 'agent', model: '', retries: 0,
              prompt: 'Разбей задачу на две независимые части: фронт и бэк. Файлов не трогай.',
              next: [{ to: 'разойтись', cond: 'always' }],
            },
            {
              id: 'разойтись', name: 'разойтись', kind: 'fork', retries: 0,
              next: [{ to: 'фронт', cond: 'always' }, { to: 'бэк', cond: 'always' }],
            },
            {
              id: 'фронт', name: 'фронт', kind: 'agent', model: '', retries: 0,
              prompt: 'Сделай фронтовую часть по плану: ${план.вывод}. Бэк не трогай.',
              next: [{ to: 'свести', cond: 'always' }],
            },
            {
              id: 'бэк', name: 'бэк', kind: 'agent', model: '', retries: 0,
              prompt: 'Сделай серверную часть по плану: ${план.вывод}. Фронт не трогай.',
              next: [{ to: 'свести', cond: 'always' }],
            },
            {
              id: 'свести', name: 'свести', kind: 'join', onConflict: 'agent', retries: 0,
              next: [{ to: 'слилось', cond: 'always' }],
            },
            // Выбор после слияния — отдельной развилкой: параллельный шлюз в
            // BPMN ничего не выбирает, и условия на его стрелках читались бы в
            // модельере как ещё одно ветвление.
            {
              id: 'слилось', name: 'слилось', kind: 'choice', retries: 0,
              next: [{ to: 'тесты', cond: 'ok' }, { to: '', cond: 'always' }],
            },
            {
              id: 'тесты', name: 'тесты', kind: 'shell', command: 'cargo test', retries: 1,
              next: [{ to: 'починить', cond: 'fail' }, { to: '', cond: 'always' }],
            },
            {
              id: 'починить', name: 'починить', kind: 'agent', model: '', retries: 0,
              prompt: 'После слияния веток тесты красные:\n${тесты.вывод}\nПочини.',
              next: [{ to: 'тесты', cond: 'always' }],
            },
          ],
        },
      },
      {
        id: 'для-каждого',
        name: 'для каждого упавшего теста — своя ветка',
        what: 'список берётся на ходу, на каждый элемент — свой worktree, потом всё сливается',
        pipeline: {
          start: 'найти',
          steps: [
            {
              id: 'найти', name: 'найти упавшие', kind: 'shell', retries: 0,
              command: "cargo test 2>&1 | grep -oE '^test [^ ]+' | sed 's/^test //' || true",
              next: [{ to: 'починить', cond: 'always' }],
            },
            {
              id: 'починить', name: 'починить', kind: 'agent', model: '', retries: 0,
              over: '${найти.вывод}', onConflict: 'agent',
              prompt: 'Почини тест ${элемент}. Причину, а не симптом; других тестов не трогай.',
              next: [{ to: 'тесты', cond: 'always' }],
            },
            {
              id: 'тесты', name: 'тесты', kind: 'shell', command: 'cargo test', retries: 1,
              next: [{ to: 'найти', cond: 'fail' }, { to: '', cond: 'always' }],
            },
          ],
        },
      },
      {
        id: 'правка-ревью',
        name: 'правка → ревью → человек',
        what: 'агент делает, второй агент ревьюит, спорное уходит человеку',
        pipeline: {
          start: 'правка',
          steps: [
            {
              id: 'правка', name: 'правка', kind: 'agent', model: '', retries: 0,
              prompt: 'Сделай задачу целиком.',
              next: [{ to: 'ревью', cond: 'always' }],
            },
            {
              id: 'ревью', name: 'ревью', kind: 'review', prompt: '', model: '', retries: 0,
              next: [
                { to: 'правка', cond: 'verdict', verdict: 'return' },
                { to: 'спросить', cond: 'verdict', verdict: 'ask' },
                { to: '', cond: 'always' },
              ],
            },
            {
              id: 'спросить', name: 'спросить', kind: 'human', retries: 0,
              question: 'Ревьюер не уверен: ${ревью.вывод}\nЧто делаем?',
              next: [{ to: 'правка', cond: 'always' }],
            },
          ],
        },
      },
    ];
  }

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

  function textField(label, value, hint, on) {
    const input = el('input.lp-input', { type: 'text', value: value == null ? '' : String(value) });
    input.addEventListener('input', () => on(input.value));
    return el('label.lp-field', el('span.lp-field-label', { text: label }), input,
      hint ? el('span.lp-hint', { text: hint }) : null);
  }

  function select(label, options, value, on) {
    const sel = el('select.lp-input');
    for (const [id, word] of options) {
      const o = el('option', { value: id, text: word });
      if (id === value) o.setAttribute('selected', '');
      sel.appendChild(o);
    }
    sel.value = value;
    sel.addEventListener('change', () => on(sel.value));
    return el('label.lp-field', el('span.lp-field-label', { text: label }), sel);
  }

  /* Отрисовать редактор в контейнер. onChange зовётся после каждой правки —
   * конструктор пересобирает своё «что из этого выйдет».
   *
   * `actions` — обмен с модельером: {export, import, unlink}. Их даёт хозяин
   * экрана, а не мы: этот модуль ничего не знает про мост в демон и грузится в
   * тестах без него. */
  function renderTo(host, p, onChange, actions) {
    const change = () => { renderTo(host, p, onChange, actions); if (onChange) onChange(p); };
    host.textContent = '';
    const steps = p.steps || [];
    const targets = [['', 'конец']].concat(steps.map((s) => [s.id, stepTitle(p, s.id)]));

    if (!steps.length) {
      host.appendChild(el('div.lp-hint', { text: 'Пайплайн пуст. Возьми заготовку или добавь первый шаг.' }));
    }

    steps.forEach((s, i) => {
      const body = [];
      if (s.kind === 'agent' || s.kind === 'review') {
        body.push(textField(s.kind === 'agent' ? 'что делать' : 'на что смотреть', s.prompt, 'можно вставлять ${шаг.вывод}', (v) => { s.prompt = v; }));
      }
      if (s.kind === 'shell') body.push(textField('команда', s.command, 'её код возврата и решает, куда идти дальше', (v) => { s.command = v; }));
      if (s.kind === 'human') body.push(textField('вопрос', s.question, 'прогон замрёт, пока не ответишь', (v) => { s.question = v; }));
      if (s.kind === 'wait') body.push(textField('минут', s.minutes, '', (v) => { s.minutes = Number(v) || 0; }));
      if (s.kind === 'fork') {
        body.push(el('div.lp-hint', { text: 'каждая ветка получит свой git worktree и свою ветку — файлы не смешаются' }));
      }
      if (s.kind === 'choice') {
        body.push(el('div.lp-hint', { text: 'уйдёт первый переход, чьё условие сошлось на результате предыдущего шага' }));
      }
      if (s.kind === 'join') {
        body.push(el('div.lp-hint', { text: 'ждёт все входящие ветки и сводит их worktree обратно в один' }));
      }
      // Попытки шлюзу не нужны: он ничего не делает, повторять нечего.
      if (!isGate(s.kind)) {
        body.push(textField('попыток', s.retries || 0, 'сорвалось из-за сети — повторит само', (v) => { s.retries = Number(v) || 0; }));
        // «Для каждого»: список известен только на ходу, поэтому ветвлением
        // его не нарисуешь — веток столько, сколько окажется строк.
        body.push(textField('для каждого', s.over || '',
          'выражение со списком, по строке на элемент — например ${тесты.вывод}; в шаге элемент виден как ${элемент}',
          (v) => { s.over = v; change(); }));
      }
      // Правило конфликта нужно тем, кто СЛИВАЕТ ветки: слиянию и узлу «для
      // каждого». Показывать его остальным значило бы обещать выбор, которого
      // в их жизни не случится.
      if (s.kind === 'join' || (s.over || '').trim()) {
        body.push(select('конфликт веток', CONFLICTS, s.onConflict || 'stop', (v) => { s.onConflict = v; }));
      }
      if ((s.over || '').trim() && !isGate(s.kind)) {
        body.push(el('div.lp-hint', { text: 'шаг выполнится по разу на элемент, каждый — в своём worktree; потом ветки сольются' }));
      }

      // У ветвления условий нет: уходят ВСЕ ветки. Показывать там селект
      // «если получилось» значило бы обещать выбор, которого не будет.
      const flows = (s.next || []).map((f, k) => el('div.pl-flow',
        s.kind === 'fork' ? null : select('', CONDS, f.cond, (v) => { f.cond = v; change(); }),
        (s.kind !== 'fork' && f.cond === 'verdict') ? select('', [['ok', 'принято'], ['return', 'возврат'], ['ask', 'нужен человек']], f.verdict || 'ok', (v) => { f.verdict = v; }) : null,
        (s.kind !== 'fork' && (f.cond === 'contains' || f.cond === 'expr')) ? textField('', f.text || '', f.cond === 'expr' ? 'как в Camunda: ${verdict == \'return\'}' : '', (v) => { f.text = v; }) : null,
        select('', targets, f.to || '', (v) => { f.to = v; change(); }),
        el('button.j-btn.small', { text: '−', title: 'убрать переход', onclick: () => { removeFlow(s, k); change(); } }),
      ));

      host.appendChild(el('section.pl-step' + (isGate(s.kind) ? '.pl-gate' : ''),
        el('div.pl-step-h',
          el('span.pl-step-kind', { text: kindWord(s.kind) }),
          textField('', s.name || s.id, '', (v) => { s.name = v; }),
          el('span.spacer'),
          el('button.j-btn.small', { text: '↑', title: 'выше', onclick: () => { moveStep(p, s.id, -1); change(); } }),
          el('button.j-btn.small', { text: '↓', title: 'ниже', onclick: () => { moveStep(p, s.id, 1); change(); } }),
          el('button.j-btn.small', { text: '×', title: 'убрать шаг', onclick: () => { removeStep(p, s.id); change(); } }),
        ),
        el('div.pl-step-b', body),
        el('div.pl-flows',
          el('span.lp-field-label', { text: s.kind === 'fork' ? 'ветки' : 'дальше' }),
          flows,
          el('button.j-btn.small', { text: s.kind === 'fork' ? '+ ветка' : '+ переход', onclick: () => { addFlow(s); change(); } }),
        ),
        i === 0 ? el('span.pl-start', { text: 'старт' }) : null,
      ));
    });

    const add = el('div.pl-add', el('span.lp-field-label', { text: 'добавить шаг' }));
    for (const [kind, word, hint] of KINDS) {
      add.appendChild(el('button.j-btn.small', { text: word, title: hint, onclick: () => { addStep(p, kind); change(); } }));
    }
    // Отдельной кнопкой, потому что руками это шесть действий, и на третьем
    // забывается слияние — а ветвление без слияния теряет работу веток.
    add.appendChild(el('button.j-btn.small', {
      text: '∥ параллель',
      title: 'ветвление + две ветки + слияние, уже связанные',
      onclick: () => { addParallel(p, 2); change(); },
    }));
    host.appendChild(add);

    const ready = el('div.pl-presets', el('span.lp-field-label', { text: 'заготовки' }));
    for (const pr of presets()) {
      ready.appendChild(el('button.j-btn.small', {
        text: pr.name,
        title: pr.what,
        onclick: () => {
          p.start = pr.pipeline.start;
          p.steps = JSON.parse(JSON.stringify(pr.pipeline.steps));
          change();
        },
      }));
    }
    host.appendChild(ready);

    if (actions) host.appendChild(bpmnRow(p, actions));
  }

  /* Обмен с модельером.
   *
   * Показываем ровно то, что человеку надо знать: связан ли граф с файлом и с
   * каким. Пока связи нет — одна кнопка; когда есть — путь и две дороги
   * (забрать правку, отвязаться). Автоматическое втягивание правок делает
   * демон, и об этом сказано прямо: иначе «я же сохранил в Camunda, а тут
   * старое» — вопрос, на который экран обязан отвечать сам. */
  function bpmnRow(p, actions) {
    const row = el('div.pl-bpmn', el('span.lp-field-label', { text: 'BPMN' }));
    const linked = (p.bpmnFile || '').trim();
    row.appendChild(el('button.j-btn.small', {
      text: linked ? 'Открыть в модельере' : 'Выгрузить в .bpmn',
      title: 'Camunda Modeler, bpmn.io — чем открывается .bpmn на этой машине',
      onclick: () => actions.exportBpmn && actions.exportBpmn(),
    }));
    if (linked) {
      row.appendChild(el('button.j-btn.small', {
        text: 'Забрать из файла',
        title: 'перечитать .bpmn прямо сейчас',
        onclick: () => actions.importBpmn && actions.importBpmn(),
      }));
      row.appendChild(el('button.j-btn.small', {
        text: 'Отвязать',
        title: 'больше не следить за файлом',
        onclick: () => actions.unlinkBpmn && actions.unlinkBpmn(),
      }));
      row.appendChild(el('div.lp-hint', {
        text: `${linked} · правки из модельера подхватываются сами, как только ты сохранишь файл`,
      }));
    } else {
      row.appendChild(el('div.lp-hint', {
        text: 'граф уедет файлом BPMN 2.0: ветвления ромбами, петли стрелками. Сохранишь в модельере — вернётся сюда сам',
      }));
    }
    return row;
  }

  return {
    KINDS, CONDS, CONFLICTS, GATEWAYS, isGate,
    addStep, addParallel, removeStep, moveStep, addFlow, removeFlow,
    makeId, kindWord, condWord, conflictWord, flowLine, stepTitle, presets, renderTo,
  };
});
