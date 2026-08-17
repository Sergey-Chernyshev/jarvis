/* Конструктор пайплайна: цикл как граф шагов.
 *
 * Экран отвечает на два вопроса: что делается по шагам и куда ход идёт дальше.
 * Никакого полотна со стрелками: рисовалка BPMN здесь была бы красивой и
 * бесполезной — в пайплайне из пяти шагов связи читаются списком быстрее, чем
 * ищутся глазами по холсту. Каждый шаг — карточка, под ней его переходы
 * строкой «если … → …».
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
  ];

  const CONDS = [
    ['always', 'всегда'],
    ['ok', 'если получилось'],
    ['fail', 'если не вышло'],
    ['verdict', 'если вердикт'],
    ['contains', 'если в выводе есть'],
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

  /* Переход человеческой строкой — то, что человек читает вместо стрелок. */
  function flowLine(p, f) {
    const to = f.to ? (stepTitle(p, f.to) || f.to) : 'конец';
    const cond = f.cond === 'verdict' ? `если вердикт «${f.verdict || '—'}»`
      : f.cond === 'contains' ? `если в выводе есть «${f.text || '—'}»`
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
   * конструктор пересобирает своё «что из этого выйдет». */
  function renderTo(host, p, onChange) {
    const change = () => { renderTo(host, p, onChange); if (onChange) onChange(p); };
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
      body.push(textField('попыток', s.retries || 0, 'сорвалось из-за сети — повторит само', (v) => { s.retries = Number(v) || 0; }));

      const flows = (s.next || []).map((f, k) => el('div.pl-flow',
        select('', CONDS, f.cond, (v) => { f.cond = v; change(); }),
        f.cond === 'verdict' ? select('', [['ok', 'принято'], ['return', 'возврат'], ['ask', 'нужен человек']], f.verdict || 'ok', (v) => { f.verdict = v; }) : null,
        f.cond === 'contains' ? textField('', f.text || '', '', (v) => { f.text = v; }) : null,
        select('', targets, f.to || '', (v) => { f.to = v; change(); }),
        el('button.j-btn.small', { text: '−', title: 'убрать переход', onclick: () => { removeFlow(s, k); change(); } }),
      ));

      host.appendChild(el('section.pl-step',
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
          el('span.lp-field-label', { text: 'дальше' }),
          flows,
          el('button.j-btn.small', { text: '+ переход', onclick: () => { addFlow(s); change(); } }),
        ),
        i === 0 ? el('span.pl-start', { text: 'старт' }) : null,
      ));
    });

    const add = el('div.pl-add', el('span.lp-field-label', { text: 'добавить шаг' }));
    for (const [kind, word, hint] of KINDS) {
      add.appendChild(el('button.j-btn.small', { text: word, title: hint, onclick: () => { addStep(p, kind); change(); } }));
    }
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
  }

  return {
    KINDS, CONDS, addStep, removeStep, moveStep, addFlow, removeFlow,
    makeId, kindWord, condWord, flowLine, stepTitle, presets, renderTo,
  };
});
