/* Trace analytics for local profiles and connected nodes. Missing evidence is not zero.
 * No prompt text or tool output is inserted as HTML. The Rust report owns metrics.
 */
(() => {
  'use strict';
  const states = new WeakMap();
  const num = value => typeof value === 'number' && Number.isFinite(value) ? value : null;
  const list = value => Array.isArray(value) ? value : [];
  const fmt = value => num(value) == null ? 'Нет данных' : value.toLocaleString('ru-RU', { maximumFractionDigits: 1 });
  const pct = value => num(value) == null ? 'Неизвестно' : `${fmt(value)}%`;
  const duration = ms => num(ms) == null ? 'Нет данных' : ms < 60000 ? `${fmt(ms / 1000)} с` : ms < 3600000 ? `${fmt(ms / 60000)} мин` : `${fmt(ms / 3600000)} ч`;
  const cash = (n, currency) => num(n) == null ? 'Нет данных' : `${fmt(n)} ${currency || 'USD'}`;
  const selectValue = (select, value) => {
    for (const option of select.options) option.selected = false;
    const selected = [...select.options].find(option => option.value === value);
    if (selected) selected.selected = true;
  };
  const errorText = error => String(error?.message || error || 'Неизвестная ошибка');
  const node = (tag, cls, text) => {
    const item = document.createElement(tag);
    if (cls) item.className = cls;
    if (text != null) item.textContent = text;
    return item;
  };
  const button = (text, action, cls = '') => {
    const b = node('button', `ai-button ${cls}`.trim(), text); b.type = 'button';
    b.addEventListener('click', action); return b;
  };
  const note = text => node('p', 'ai-note', text);
  const empty = (title, text) => { const box = node('div', 'ai-empty'); box.append(node('strong', '', title), note(text)); return box; };
  const metric = (label, value, explanation) => {
    const box = node('div', 'ai-metric'); box.append(node('dt', '', label), node('dd', '', value));
    if (explanation) box.append(note(explanation)); return box;
  };
  const facts = rows => {
    const dl = node('dl', 'ai-facts');
    rows.forEach(([label, value]) => { const row = node('div'); row.append(node('dt', '', label), node('dd', '', value)); dl.append(row); });
    return dl;
  };
  const section = (title, explanation) => {
    const box = node('section', 'ai-section'); box.append(node('h2', '', title));
    if (explanation) box.append(note(explanation)); return box;
  };
  const disclosure = (title, subtitle, open = false) => {
    const d = node('details', 'ai-disclosure'); d.open = open;
    const summary = node('summary'); summary.append(node('span', 'ai-disclosure-title', title));
    if (subtitle) summary.append(node('span', 'ai-disclosure-meta', subtitle));
    const content = node('div', 'ai-disclosure-body'); d.append(summary, content); return { root: d, content };
  };
  function evidence(value) {
    if (value == null) return null;
    const data = typeof value === 'string' ? value : JSON.stringify(value, null, 2);
    const d = disclosure('Основание сигнала'); d.content.append(node('pre', 'ai-evidence', data.slice(0, 6000)));
    return d.root;
  }
  function table(headers, rows, label) {
    const wrap = node('div', 'ai-table-wrap'); wrap.tabIndex = 0; wrap.setAttribute('role', 'region'); wrap.setAttribute('aria-label', label);
    const t = node('table', 'ai-table'); const head = node('thead'); const tr = node('tr');
    headers.forEach(text => { const th = node('th', '', text); th.scope = 'col'; tr.append(th); }); head.append(tr);
    const body = node('tbody'); rows.forEach(cells => { const row = node('tr'); cells.forEach(value => row.append(node('td', '', value))); body.append(row); });
    t.append(head, body); wrap.append(t); return wrap;
  }
  function sessionName(session) { return `${session.sourceLabel || session.instanceLabel || session.agent || 'Агент'} · ${session.cwd?.split(/[\\/]/).filter(Boolean).pop() || 'Без проекта'} · ${session.providerSessionId || session.id}`; }

  function renderCoverage(report) {
    const c = report.coverage || {};
    const bar = node('div', `ai-coverage${c.limited || list(c.errors).length ? ' ai-coverage-partial' : ''}`);
    bar.append(node('strong', '', c.limited ? 'Отчёт неполный' : 'Прочитанные данные'), node('span', '', `Файлов: ${fmt(c.filesScanned)} · сессий: ${fmt(c.sessions ?? report.summary?.sessions)}`));
    if (c.caveat) { const caveat = note(c.caveat); caveat.classList.add('ai-coverage-caveat'); bar.append(caveat); }
    const reasons = list(c.errors);
    if (c.limited || reasons.length || list(c.roots).length) {
      const d = disclosure('Источники и покрытие');
      list(c.roots).forEach(root => d.content.append(node('p', 'ai-path', `${root.label || root.agent || 'Агент'}${root.machine && root.machine !== 'local' ? ` · ${root.machine}` : ''}: ${root.path || 'Неизвестно'}`)));
      list(c.remote?.nodes).forEach(machine => d.content.append(note(`${machine.machine}: ${machine.connected ? 'подключён' : 'недоступен'}`)));
      if (c.remote?.caveat && list(c.remote?.nodes).length) d.content.append(note(c.remote.caveat));
      d.content.append(note('Метрики относятся к прочитанным трейсам. Недоступные события не считаются успешными.'));
      reasons.forEach(error => d.content.append(note(typeof error === 'string' ? error : JSON.stringify(error))));
      if (c.limited) d.content.append(note('Сработал лимит чтения. Выбери нужный проект, чтобы приоритетно прочитать его трейсы. Ограничения размера файлов сохраняются; смена периода не гарантирует полное покрытие.'));
      bar.append(d.root);
    }
    return bar;
  }
  function renderOverview(report) {
    const s = report.summary || {};
    const box = section('Работа с агентами', 'Наблюдения по подключённым трейсам агентов. Результат задачи фиксируется отдельно.');
    const metrics = node('dl', 'ai-metric-strip');
    metrics.append(metric('Сессии', fmt(s.sessions), `Запросов пользователя: ${fmt(s.prompts)}`), metric('Инструменты', fmt(s.toolCalls), `Ошибок: ${fmt(s.toolErrors)} · статус неизвестен: ${fmt(s.toolUnknown)}`), metric('Активность агентов', duration(s.activeMs), 'Прокси по событиям; сессии могут идти параллельно.'), metric('Интервал наблюдения', duration(s.wallMs), 'От первого до последнего события; включает паузы.'));
    box.append(metrics);
    if (num(s.wrapperCalls) > 0) box.append(note(`Непрозрачных оболочек exec/wait: ${fmt(s.wrapperCalls)}. Их завершение не подтверждает успех вложенных инструментов; в score участвуют прямые вызовы: ${fmt(s.eligibleCalls)}.`));
    return box;
  }
  function renderHarness(report) {
    const sessions = list(report.sessions);
    const box = section('Качество harness', 'Harness — инструкции, контекст, инструменты и проверки вокруг модели. Score отражает наблюдаемый процесс, а не качество кода или программиста.');
    const head = node('div', 'ai-harness-head');
    const score = num(report.summary?.harnessScore);
    head.append(node('strong', 'ai-score', score == null ? '—' : fmt(score)), node('div', 'ai-score-copy', score == null ? 'Недостаточно событий для оценки' : 'из 100 · операционный score'));
    box.append(head);
    if (score != null) box.append(note(`Среднее по сессиям с оценкой: ${fmt(report.summary?.harnessScoredSessions)} из ${fmt(report.summary?.sessions)}. Сессии без оценки исключены.`));
    const issues = sessions.flatMap(session => list(session.harness?.issues).map(issue => ({ session, issue })));
    const grouped = new Map();
    issues.forEach(item => {
      const key = `${item.issue.code || ''}\0${item.issue.message || ''}`;
      if (!grouped.has(key)) grouped.set(key, []); grouped.get(key).push(item);
    });
    if (!issues.length) box.append(note(sessions.length ? 'Диагностические правила не нашли проблем в доступных событиях. Это не подтверждает корректность результата.' : 'Диагностика появится после чтения сессий.'));
    [...grouped.values()].slice(0, 12).forEach(items => {
      const { issue } = items[0]; const row = node('div', `ai-issue ai-issue-${issue.severity === 'high' || issue.severity === 'error' ? 'high' : 'notice'}`);
      row.append(node('strong', '', issue.message || issue.code || 'Сигнал'), note(`Сессий с сигналом: ${new Set(items.map(item => item.session.id)).size}`));
      const d = disclosure('Показать события');
      items.slice(0, 12).forEach(({ session, issue: entry }) => {
        d.content.append(node('p', 'ai-session-ref', sessionName(session)));
        const e = evidence(entry.evidence); if (e) d.content.append(e);
      }); row.append(d.root); box.append(row);
    });
    if (grouped.size > 12) box.append(note('Остальные сигналы доступны в подробностях сессий ниже.'));
    return box;
  }
  function renderProjects(report) {
    const box = section('Код ИИ и ручные изменения', 'Git показывает изменение кода, но сам по себе не знает автора каждой строки. Несопоставленные строки остаются неизвестными.');
    if (!list(report.projects).length) box.append(empty('Нет проектов', 'Проекты появятся из рабочих папок прочитанных сессий. Для атрибуции кода нужен доступный локальный Git-репозиторий.'));
    list(report.projects).forEach(project => {
      const g = project.git || {}; const a = g.attribution || {};
      const d = disclosure(project.name || project.path || 'Проект', g.ok ? `${g.branch || 'HEAD'} · +${fmt(g.addedLines)} / −${fmt(g.removedLines)}${g.limited ? ' · неполное чтение' : ''}` : 'Git недоступен');
      d.content.append(node('p', 'ai-path', project.path));
      if (!g.ok) { d.content.append(note(g.error || 'Репозиторий не прочитан.')); box.append(d.root); return; }
      if (g.limited) { const warning = node('p', 'ai-repository-partial', 'Репозиторий прочитан частично: сработал лимит чтения или часть файлов недоступна. Строки и проценты относятся только к прочитанным исходникам.'); warning.setAttribute('role', 'note'); d.content.append(warning); }
      d.content.append(facts([['Изменённых файлов', fmt(g.changedFiles)], ['Исключённых файлов', fmt(g.excludedFiles)], ['Совпадает с успешными правками ИИ', pct(a.aiPct)], ['Неизвестное происхождение', pct(a.unknownPct)], ['Подтверждённый ручной код', pct(a.humanPct)]]));
      d.content.append(note(a.caveat || 'Точное совпадение добавленных строк рабочего дерева с успешными правками агента — эвристика, а не доказательство авторства.'));
      d.content.append(note(`Основание: ${fmt(a.aiMatchedLines)} совпавших и ${fmt(a.unknownLines)} неизвестных добавленных строк. Область: текущее рабочее дерево; коммиты не входят в этот процент.`));
      const tracked = g.gitAi;
      if (tracked?.available) {
        d.content.append(node('h3', '', 'Git AI · HEAD'), facts([['ИИ по provenance', pct(tracked.aiPct)], ['Человек по provenance', pct(tracked.humanPct)], ['Неизвестно по provenance', pct(tracked.unknownPct)]]));
        if (tracked.caveat) d.content.append(note(tracked.caveat));
      } else d.content.append(note(tracked?.caveat || 'Provenance Git AI для HEAD недоступен. Без отслеживания правок процент ручного кода не восстанавливается.'));
      box.append(d.root);
    }); return box;
  }
  function tokenCoverage(model) {
    if (model.tokenRecords === 0) return `Нет usage · запросов без usage: ${fmt(model.missingUsageRequests)}`;
    if (num(model.tokenRecords) == null) return 'Покрытие неизвестно';
    return `${model.tokenCoverage === 'partial' || num(model.missingUsageRequests) > 0 ? 'Частично · ' : ''}записей usage: ${fmt(model.tokenRecords)} · запросов без usage: ${fmt(model.missingUsageRequests)}${model.tokenCoverage === 'observed-records-only' ? ' · полный объём запросов неизвестен' : ''}`;
  }
  function renderModels(report) {
    const d = disclosure('Модели и выбор модели', `Моделей в трейсах: ${list(report.models).length}`);
    d.content.append(note('Название модели не доказывает её пригодность. Сравнивай одинаковые типы задач с одинаковым harness по принятому результату, времени и фактическим затратам.'));
    if (list(report.models).length) d.content.append(table(['Модель', 'Запросы', 'Вход', 'Выход', 'Чтение кеша', 'Ошибки / вызовы', 'Учёт токенов'], list(report.models).map(m => [m.model || m.id || 'Неизвестно', fmt(m.requests), fmt(m.inputTokens), fmt(m.outputTokens), fmt(m.cacheReadTokens), `${fmt(m.toolErrors)} / ${fmt(m.toolCalls)}`, tokenCoverage(m)]), 'Использование моделей'));
    if (list(report.models).some(m => m.tokenCoverage === 'partial' || m.tokenCoverage === 'partial' || num(m.missingUsageRequests) > 0 || num(m.knownMissingUsageRequests) > 0 || num(m.missingUsageUnknownSessions) > 0 || m.tokenRecords === 0)) d.content.append(note('Учёт токенов неполный. Показаны только события с usage; запросы без usage не считаются запросами с нулевым расходом.'));
    const comparisons = list(report.economics?.comparisons);
    comparisons.forEach(c => {
      const row = disclosure(`${c.taskType} · ${c.model}`, `${fmt(c.samples)} результатов · ${c.harnessVersion}`);
      row.content.append(note(c.project || 'Без проекта'));
      row.content.append(facts([['Принято', `${fmt(c.accepted)} / ${fmt(c.samples)} (${pct(c.acceptancePct)})`], ['Среднее время с ревью и переделками', num(c.meanTotalMinutes) == null ? 'Нет данных' : `${fmt(c.meanTotalMinutes)} мин`], ['Затраты на принятый результат', cash(c.costPerAccepted, c.currency)]]));
      row.content.append(note(c.status === 'insufficient-evidence' ? 'Недостаточно наблюдений для выбора модели.' : 'Наблюдательное сравнение: задачи могут отличаться по сложности.'));
      if (c.recommendation) row.content.append(note(c.recommendation)); d.content.append(row.root);
    });
    if (!comparisons.length) d.content.append(note('Добавь результаты задач ниже, чтобы сравнивать модели. Токены без результатов не дают оценку эффективности.'));
    return d.root;
  }
  function renderEconomics(report, state) {
    const economics = report.economics || {};
    const box = section('Время и экономический эффект', 'Денежный эквивалент времени, а не прибыль на счёте. Оценка опирается на записанные результаты. Пустые поля означают «неизвестно», а ноль — отсутствие затрат.');
    if (!list(economics.groups).length) box.append(empty('Эффект ещё не измерен', 'Запиши время задачи без ИИ, время с ИИ, ревью, переделки и фактические затраты. Из длительности трейса нельзя вывести экономию человека.'));
    list(economics.spending).forEach(spend => {
      box.append(facts([['Известные затраты на ИИ', cash(spend.knownActualCost, spend.currency)], ['Задач с указанной стоимостью', `${fmt(spend.costRecordedTasks)} / ${fmt(spend.tasks)}`], ['Задач без стоимости', fmt(spend.missingCostTasks)]]));
    });
    list(economics.groups).forEach(group => {
      const d = disclosure(`${group.currency} · ${fmt(group.taskCount)} полных записей`, `${fmt(group.measuredBaselines)} измеренных baseline`, true);
      const strip = node('dl', 'ai-metric-strip'); strip.append(metric('Расчётный эффект', cash(group.netValue, group.currency)), metric('Сэкономленное время', num(group.savedMinutes) == null ? 'Нет данных' : `${fmt(group.savedMinutes)} мин`), metric('Ускорение', num(group.speedup) == null ? 'Нет данных' : `${fmt(group.speedup)}×`), metric('ROI полных затрат', pct(group.roiPct)));
      d.content.append(strip, note(`Фактические затраты: ${cash(group.totalCost, group.currency)}. Неполных записей исключено: ${fmt(group.excludedIncompleteTasks)}. Оценённые baseline дают оценку эффекта, а не причинно доказанный профит.`)); box.append(d.root);
    });
    box.append(note(`Записей: ${fmt(economics.recorded)}. Полных для расчёта: ${fmt(economics.complete)}. Неполных: ${fmt(num(economics.recorded) != null && num(economics.complete) != null ? economics.recorded - economics.complete : null)}. Принятых: ${fmt(economics.accepted)}.`));
    const formula = disclosure('Как считается эффект');
    formula.content.append(note('Время с ИИ = работа с ИИ + ревью + переделки. Экономия = baseline без ИИ − время с ИИ. Чистый эффект = экономия / 60 × ставка − фактические затраты. Ускорение = baseline / время с ИИ. ROI = чистый эффект / (стоимость времени с ИИ + затраты на ИИ) × 100%. Для отклонённых задач и задач с переделками выгода не начисляется, затраты учитываются. Нулевой или неизвестный знаменатель не превращается в бесконечный ROI. Валюты считаются отдельно.'));
    box.append(formula.root, renderModels(report));
    const form = disclosure('Записать результат задачи', 'Результат, время, фактические затраты');
    form.content.append(outcomeForm(report, state)); box.append(form.root);
    if (list(report.outcomes).length) {
      const ledger = disclosure('Записанные результаты', `${report.outcomes.length} записей`);
      report.outcomes.forEach(outcome => {
        const row = node('div', 'ai-ledger-row'); row.append(node('strong', '', `${outcome.taskType} · ${outcome.model}`), note(`${outcome.harnessVersion} · ${ { accepted: 'Принято', rework: 'Нужны переделки', rejected: 'Отклонено' }[outcome.outcome] || outcome.outcome }`));
        row.append(button('Изменить', () => {
          form.root.open = true; form.content.replaceChildren(outcomeForm(report, state, outcome));
          form.content.querySelector('input')?.focus();
        })); ledger.content.append(row);
      }); box.append(ledger.root);
    }
    return box;
  }
  function outcomeForm(report, state, initial = {}) {
    const preferences = !initial.id ? report.config?.economics || {} : {};
    initial = { currency: preferences.currency ?? 'USD', hourlyRate: preferences.hourlyRate ?? null, ...initial };
    const form = node('form', 'ai-outcome-form'); const controls = {};
    const fields = node('div', 'ai-form-grid');
    function field(key, label, options = {}) {
      const wrap = node('label', 'ai-field'); wrap.append(node('span', '', label));
      let input;
      if (options.choices) {
        input = node('select'); options.choices.forEach(([value, text]) => { const option = node('option', '', text); option.value = value; input.append(option); }); selectValue(input, options.choices[0]?.[0] || '');
      } else { input = node('input'); input.type = options.number ? 'number' : 'text'; }
      input.name = key;
      if (options.required) input.required = true;
      if (options.number) { input.min = '0'; input.step = 'any'; input.placeholder = 'Неизвестно'; }
      else if (options.placeholder) input.placeholder = options.placeholder;
      if (initial[key] != null) { if (options.choices) selectValue(input, String(initial[key])); else input.value = String(initial[key]); }
      if (options.hint) wrap.append(note(options.hint));
      input.setAttribute('aria-label', label); wrap.append(input); fields.append(wrap); controls[key] = input; return input;
    }
    const sessions = list(report.sessions);
    const session = field('sessionId', 'Сессия', { required: true, choices: [['', 'Выбрать сессию'], ...sessions.map(s => [s.id, sessionName(s)])] });
    if (initial.sessionId && !sessions.some(s => s.id === initial.sessionId)) {
      const option = node('option', '', initial.sessionId); option.value = initial.sessionId; session.append(option); selectValue(session, initial.sessionId);
    }
    field('taskType', 'Тип задачи', { required: true, placeholder: 'Например, исправление ошибки' });
    field('model', 'Модель', { required: true, placeholder: 'Точный ID из трейса' });
    field('harnessVersion', 'Версия harness', { required: true, placeholder: 'Например, git SHA инструкций' });
    field('outcome', 'Результат', { required: true, choices: [['accepted', 'Принято'], ['rework', 'Нужны переделки'], ['rejected', 'Отклонено']] });
    field('baselineSource', 'Источник baseline', { choices: [['estimate', 'Оценка без ИИ'], ['measured', 'Измерено без ИИ']] });
    field('baselineMinutes', 'Без ИИ, минуты', { number: true });
    field('aiMinutes', 'Работа с ИИ, минуты', { number: true, hint: 'Активное время человека без ревью и переделок; не длительность трейса.' });
    field('reviewMinutes', 'Ревью, минуты', { number: true });
    field('reworkMinutes', 'Переделки, минуты', { number: true });
    field('hourlyRate', 'Стоимость часа', { number: true });
    field('actualCost', 'Фактические затраты на ИИ', { number: true, hint: 'API или доля подписки для этой задачи. Не стоимость токенов по прайсу.' });
    const currency = field('currency', 'Валюта', { required: true, placeholder: 'USD, RUB, EUR, GBP…' });
    currency.value = initial.currency || 'USD'; currency.maxLength = 3; currency.pattern = '[A-Za-z]{3}';
    currency.addEventListener('change', () => { currency.value = currency.value.trim().toUpperCase(); });
    field('reviewerEvidence', 'Основание принятия', { placeholder: 'Тесты, ревью, принятый PR' });
    session.addEventListener('change', () => {
      const found = sessions.find(s => s.id === session.value);
      const models = [...new Set(list(found?.models).map(m => m.model).filter(Boolean))];
      controls.model.value = models.length === 1 && models[0].toLowerCase() !== 'unknown' ? models[0] : '';
      controls.model.placeholder = models.length > 1 ? 'Укажи состав моделей вручную' : 'Точный ID из трейса';
    });
    const status = node('p', 'ai-form-status'); status.setAttribute('role', 'status'); status.setAttribute('aria-live', 'polite');
    const submit = node('button', 'ai-button ai-button-primary', initial.id ? 'Сохранить изменения' : 'Сохранить результат'); submit.type = 'submit';
    form.append(fields, note('Используй один тип задачи и одну версию harness для сопоставимых наблюдений. При участии нескольких моделей укажи состав; такая запись не доказывает эффект одной модели.'), status, submit);
    if (!sessions.length && !initial.sessionId) { submit.disabled = true; status.textContent = 'Сначала нужен доступный трейс сессии.'; }
    form.addEventListener('submit', async event => {
      event.preventDefault(); if (submit.disabled) return;
      const payload = {};
      for (const [key, control] of Object.entries(controls)) {
        const value = (control.value || '').trim();
        if (control.required && !value) { status.textContent = `Заполни поле «${control.getAttribute('aria-label')}».`; control.focus(); return; }
        if (control.type === 'number') {
          payload[key] = value === '' ? null : Number(value);
          if (payload[key] != null && (!Number.isFinite(payload[key]) || payload[key] < 0)) { status.textContent = 'Время и затраты должны быть неотрицательными числами.'; control.focus(); return; }
        } else payload[key] = value;
      }
      payload.currency = payload.currency.toUpperCase();
      if (!/^[A-Z]{3}$/.test(payload.currency)) { status.textContent = 'Укажи код валюты из трёх латинских букв, например USD или GBP.'; controls.currency.focus(); return; }
      if (initial.id) payload.id = initial.id;
      const selectedSession = sessions.find(s => s.id === payload.sessionId);
      payload.project = selectedSession?.project || selectedSession?.cwd || initial.project || '';
      submit.disabled = true; status.textContent = 'Сохраняем…'; status.setAttribute('role', 'status');
      try {
        if (!window.jarvis?.analyticsSaveOutcome) throw new Error('Сохранение результатов недоступно в этой версии Jarvis.');
        const result = await window.jarvis.analyticsSaveOutcome(payload);
        if (!result || result.ok === false || result.error) throw new Error(result?.error || 'Нет подтверждения сохранения.');
        status.textContent = 'Результат сохранён.';
        state.success = 'Результат сохранён. Метрики обновлены.';
        await loadReport(state, true);
      } catch (error) {
        status.setAttribute('role', 'alert'); status.textContent = `Не сохранено: ${errorText(error)}`;
        submit.disabled = false;
      }
    });
    return form;
  }
  function renderSessions(report) {
    const box = section('Сессии и доказательства', 'Открой сессию, чтобы увидеть основания score и различить ошибки, неизвестные результаты и фактические проверки.');
    list(report.sessions).forEach(session => {
      const d = disclosure(sessionName(session), session.harness?.score == null ? 'Score неизвестен' : `Score ${fmt(session.harness.score)} / 100`);
      const t = session.tools || {}; const time = session.timing || {};
      d.content.append(facts([['ID сессии', session.id], ['Проект', session.cwd || 'Неизвестно'], ['Запросы пользователя', fmt(session.prompts?.count)], ['Инструменты', fmt(t.calls)], ['Успешно / ошибка / неизвестно', `${fmt(t.success)} / ${fmt(t.errors)} / ${fmt(t.unknown)}`], ['Повторные вызовы', fmt(t.repeatedCalls)], ['Активность по событиям', duration(time.activeMs)], ['Длительность с паузами', duration(time.wallMs)], ['Инструменты p50 / p95', `${duration(time.toolP50Ms)} / ${duration(time.toolP95Ms)}`]]));
      if (session.sourceFile) d.content.append(facts([['Исходный трейс', session.sourceFile]]));
      if (num(t.wrapperCalls) > 0) d.content.append(note(`Оболочек: ${fmt(t.wrapperCalls)}; прямых вызовов для score: ${fmt(t.eligibleCalls)}. Статус оболочки не описывает её внутренние инструменты.`));
      if (list(t.byName).length) d.content.append(table(['Инструмент', 'Вызовы', 'Успех', 'Ошибка', 'Неизвестно', 'Повторы', 'p95', 'Замеров'], t.byName.map(tool => [`${tool.name || tool.tool}${tool.opaqueWrapper ? ' (оболочка)' : ''}`, fmt(tool.calls), fmt(tool.success), fmt(tool.errors), fmt(tool.unknown), fmt(tool.repeatedCalls), duration(tool.p95Ms), fmt(tool.latencySamples)]), 'Результаты инструментов сессии'));
      list(session.harness?.dimensions).forEach(dim => {
        const dimension = node('div', 'ai-dimension'); const title = node('div', 'ai-dimension-heading');
        title.append(node('strong', '', dim.label || dim.id), node('span', '', num(dim.score) == null ? 'Нет данных' : `${fmt(dim.score)} / 100`)); dimension.append(title);
        if (num(dim.score) != null) { const bar = node('progress'); bar.max = 100; bar.value = Math.max(0, Math.min(100, dim.score)); bar.setAttribute('aria-label', dim.label || dim.id); dimension.append(bar); }
        if (num(dim.observed) != null || num(dim.total) != null) dimension.append(note(`Наблюдения: ${fmt(dim.observed)} / ${fmt(dim.total)}`));
        if (dim.explanation) dimension.append(note(dim.explanation)); d.content.append(dimension);
      });
      list(session.harness?.issues).forEach(issue => { const row = node('div', 'ai-issue'); row.append(node('strong', '', issue.message || issue.code)); const e = evidence(issue.evidence); if (e) row.append(e); d.content.append(row); });
      if (session.prompts) { const p = disclosure('Сигналы промптинга', 'Наличие признака не доказывает качество запроса'); p.content.append(promptFacts(session.prompts)); d.content.append(p.root); }
      if (session.verification) { const v = disclosure('Проверки результата'); v.content.append(verificationFacts(session.verification)); d.content.append(v.root); }
      if (session.context) { const context = disclosure('Контекст и сжатие'); context.content.append(contextFacts(session.context)); d.content.append(context.root); }
      if (session.coverage) { const c = disclosure('Покрытие сессии'); c.content.append(node('pre', 'ai-evidence', JSON.stringify(session.coverage, null, 2))); d.content.append(c.root); }
      box.append(d.root);
    }); return box;
  }
  function renderPrompting(report) {
    const box = section('Промптинг', 'Наблюдаемые признаки запроса помогают найти пропущенные условия. Они не оценивают ясность намерения и корректность решения.');
    const signals = new Map();
    list(report.sessions).forEach(session => list(session.prompts?.signals).forEach(signal => {
      if (num(signal.observed) == null || num(signal.total) == null) return;
      const current = signals.get(signal.id) || { label: signal.label || signal.id, observed: 0, total: 0 };
      current.observed += signal.observed; current.total += signal.total; signals.set(signal.id, current);
    }));
    if (signals.size) box.append(table(['Признак', 'Запросов', 'Доля'], [...signals.values()].map(s => [s.label, `${fmt(s.observed)} / ${fmt(s.total)}`, s.total > 0 ? pct(100 * s.observed / s.total) : 'Нет данных']), 'Наблюдаемые признаки промптов'));
    else box.append(note('Сигналы появятся после чтения запросов пользователя.'));
    box.append(note('Продолжение разговора может опираться на предыдущий контекст. Отсутствие пути или критериев в одном сообщении не доказывает проблему.'));
    return box;
  }
  function promptFacts(prompts) {
    const box = node('div');
    box.append(facts([['Запросы пользователя', fmt(prompts.count)], ['Исключённые блоки контекста', fmt(prompts.excludedContextBlocks)]]));
    if (list(prompts.signals).length) box.append(table(['Признак', 'Наблюдения', 'Доля'], prompts.signals.map(s => [s.label || s.id, `${fmt(s.observed)} / ${fmt(s.total)}`, pct(s.pct)]), 'Признаки промптов сессии'));
    list(prompts.caveats).forEach(caveat => box.append(note(caveat)));
    box.append(note('Это эвристические признаки структуры запроса. Короткий запрос может быть достаточным; корректность намерения и результата требует ревью.')); return box;
  }
  function verificationFacts(v) {
    const box = node('div');
    box.append(facts([['Успешные правки', fmt(v.successfulEdits)], ['Распознанные вызовы проверок', fmt(v.checkCalls)], ['Проверки после последней правки', fmt(v.checksAfterLatestEdit)], ['Успешные после правки', fmt(v.passedAfterLatestEdit)], ['Ошибки после правки', fmt(v.failedAfterLatestEdit)], ['Последняя проверка успешна', v.afterLatestEditPassed == null ? 'Неизвестно' : v.afterLatestEditPassed ? 'Да' : 'Нет'], ['Семантическое качество', 'Нужна оценка результата']]));
    box.append(note('Вызов проверки отличается от успешного результата. Успешный тест не заменяет ревью и оценку полноты требований. Распознаются простые команды с подтверждённым результатом.')); return box;
  }
  function contextFacts(context) {
    const box = node('div');
    box.append(facts([['Явные события сжатия', fmt(context.compactionEvents)], ['Сбросы счётчика токенов', fmt(context.tokenCounterResets)], ['Замеры заполнения окна', fmt(context.windowSamples)], ['Пиковое заполнение входом', pct(context.peakInputWindowPct)], ['Последний размер окна, токены', fmt(context.lastWindowTokens)], ['Последний вход, токены', fmt(context.lastInputTokens)]]));
    box.append(note(context.caveat || 'Заполнение окна и события сжатия описывают объём контекста. Они не доказывают потери информации или ухудшение качества. Сброс счётчика не равен сжатию.')); return box;
  }
  function renderReport(state, report) {
    const body = state.body; body.replaceChildren();
    if (state.success) { const saved = node('p', 'ai-success', state.success); saved.setAttribute('role', 'status'); body.append(saved); state.success = ''; }
    body.append(renderCoverage(report), renderOverview(report));
    if (!list(report.sessions).length) body.append(empty('В этом периоде нет сессий', 'Выбери другой период или подключи источник в настройках аналитики: Claude Code, Codex или универсальный JSONL. Новые сессии появятся после обновления.'));
    body.append(renderHarness(report), renderPrompting(report), renderProjects(report), renderEconomics(report, state), renderSessions(report));
    const footer = disclosure('О метриках и границах анализа');
    footer.content.append(note('Данные обрабатываются локально. Сырые запросы не отправляются внешней модели для оценки. Harness score — диагностическая эвристика по доступным событиям. Процент кода, скорость и стоимость имеют разные источники и не складываются в рейтинг программиста.'), note('Активность по событиям не равна рабочему времени человека. Смена модели обоснована только сопоставимыми задачами, результатами и затратами. Изменения рабочего дерева могут включать работу других сессий и более ранних периодов.'));
    body.append(footer.root);
  }
  async function loadReport(state, refresh = false) {
    const request = ++state.request; state.refresh.disabled = true; state.export.disabled = true; state.report = null; state.status.textContent = 'Читаем трейсы профилей, подключённых узлов и локальный Git…'; state.status.setAttribute('role', 'status'); state.body.setAttribute('aria-busy', 'true');
    try {
      if (!window.jarvis?.analyticsReport) throw new Error('Аналитика недоступна в этой версии Jarvis.');
      const report = await window.jarvis.analyticsReport({ period: state.period, project: state.project || null, refresh });
      if (request !== state.request) return;
      if (report?.schemaVersion !== 1 || !report.summary || !Array.isArray(report.sessions)) throw new Error(report?.error || 'Сервер вернул отчёт неизвестного формата.');
      state.report = report; state.export.disabled = false;
      [...list(report.discoveredProjects), ...list(report.projects)].forEach(project => { if (project.path) state.projects.set(project.path, project.name || project.path); });
      if (state.project && !state.projects.has(state.project)) state.projects.set(state.project, state.project);
      state.projectSelect.replaceChildren();
      [['', 'Все проекты'], ...state.projects].forEach(([path, name]) => { const option = node('option', '', name); option.value = path; state.projectSelect.append(option); }); selectValue(state.projectSelect, state.project);
      renderReport(state, report);
      const date = new Date(report.generatedAt); state.status.textContent = Number.isNaN(date.getTime()) ? 'Отчёт обновлён.' : `Обновлено ${date.toLocaleString('ru-RU')}`;
    } catch (error) {
      if (request !== state.request) return;
      state.status.setAttribute('role', 'alert'); state.status.textContent = `Не удалось обновить аналитику: ${errorText(error)}`;
      state.body.replaceChildren(empty('Отчёт недоступен', 'Нажми «Обновить», чтобы повторить чтение. Непрочитанные данные не считаются нулевыми.'));
      if (state.success) { const saved = node('p', 'ai-success', 'Результат сохранён. Обновление отчёта не удалось.'); saved.setAttribute('role', 'status'); state.body.prepend(saved); state.success = ''; }
    } finally {
      if (request === state.request) { state.refresh.disabled = false; state.body.setAttribute('aria-busy', 'false'); }
    }
  }
  function configValue(value) {
    if (!value || value.version !== 1 || !Array.isArray(value.sources) || !value.rules || !value.limits || !value.git) throw new Error('Настройки имеют неизвестный формат.');
    return value;
  }
  async function loadConfig(state) {
    if (state.configLoading) return;
    const body = state.configBody; state.configLoading = true; body.replaceChildren(note('Читаем настройки аналитики…'));
    try {
      if (!window.jarvis?.analyticsConfig) throw new Error('Настройки аналитики недоступны в этой версии Jarvis.');
      const config = configValue(await window.jarvis.analyticsConfig());
      if (state.configBody !== body) return;
      state.configLoaded = true; renderConfigEditor(state, config);
    } catch (error) {
      if (state.configBody !== body) return;
      const message = node('p', 'ai-form-status', `Не удалось загрузить настройки: ${errorText(error)}`); message.setAttribute('role', 'alert');
      const defaults = button('Загрузить значения по умолчанию', async () => {
        defaults.disabled = true;
        try {
          if (!window.jarvis?.analyticsDefaults) throw new Error('Значения по умолчанию недоступны.');
          const config = configValue(await window.jarvis.analyticsDefaults());
          if (state.configBody === body) { state.configLoaded = true; renderConfigEditor(state, config, 'Загружены значения по умолчанию. Нажми «Сохранить настройки», чтобы заменить повреждённую конфигурацию.'); }
        } catch (error) { message.textContent = `Не удалось загрузить значения по умолчанию: ${errorText(error)}`; defaults.disabled = false; }
      });
      body.replaceChildren(message, button('Повторить загрузку настроек', () => loadConfig(state)), defaults);
    } finally { if (state.configBody === body) state.configLoading = false; }
  }
  function renderConfigEditor(state, config, message = '') {
    const body = state.configBody; const form = node('form', 'ai-config-form');
    const simple = node('fieldset', 'ai-config-simple'); const controls = {};
    const status = node('p', 'ai-form-status', message); status.setAttribute('role', 'status'); status.setAttribute('aria-live', 'polite');
    const fail = error => { status.setAttribute('role', 'alert'); status.textContent = errorText(error); };
    const announce = text => { status.setAttribute('role', 'status'); status.textContent = text; };
    let advancedDirty = false;
    const discoveryLabel = node('label', 'ai-check-field'); const discovery = node('input'); discovery.type = 'checkbox'; discovery.name = 'config-autoDiscover'; discovery.checked = !!config.autoDiscover;
    discoveryLabel.append(discovery, node('span', '', 'Автоматически искать трейсы профилей и подключённых узлов')); simple.append(discoveryLabel);
    simple.append(note('Автопоиск использует стандартные каталоги агентов и переменные окружения этого пользователя. Дополнительные источники добавляются ниже.'));
    const sourceHeading = node('h3', '', 'Источники трейсов'); const sources = node('div', 'ai-config-sources');
    const rows = [];
    function sourceField(row, key, label, value, choices) {
      const wrap = node('label', 'ai-field'); wrap.append(node('span', '', label));
      const input = node(choices ? 'select' : 'input'); input.setAttribute('data-field', key); input.setAttribute('aria-label', label);
      if (choices) { choices.forEach(([id, text]) => { const option = node('option', '', text); option.value = id; input.append(option); }); selectValue(input, value); }
      else { input.type = 'text'; input.value = value || ''; }
      wrap.append(input); row.controls[key] = input; row.element.append(wrap); return input;
    }
    function addSource(source) {
      const row = { element: node('div', 'ai-config-source'), controls: {} }; row.element.setAttribute('data-config-source', '');
      const enabledLabel = node('label', 'ai-check-field'); const enabled = node('input'); enabled.type = 'checkbox'; enabled.checked = source.enabled !== false; enabled.setAttribute('data-field', 'enabled');
      enabledLabel.append(enabled, node('span', '', 'Читать источник')); row.controls.enabled = enabled; row.element.append(enabledLabel);
      sourceField(row, 'id', 'ID источника', source.id).maxLength = 64;
      sourceField(row, 'format', 'Формат трейсов', source.format || 'normalized', [['claude', 'Claude Code'], ['codex', 'Codex'], ['normalized', 'Универсальный JSONL']]);
      const path = sourceField(row, 'path', 'Путь к файлу или каталогу', source.path); path.placeholder = '~/traces или /путь/сессия.jsonl'; path.closest('label').classList.add('ai-config-source-path');
      row.element.append(button('Удалить источник', () => { rows.splice(rows.indexOf(row), 1); row.element.remove(); syncJson(); }));
      rows.push(row); sources.append(row.element); return row;
    }
    list(config.sources).forEach(addSource);
    const add = button('Добавить источник', () => {
      if (rows.length >= 32) { fail('Можно добавить до 32 источников.'); return; }
      const used = new Set(rows.map(row => row.controls.id.value)); let id = 1; while (used.has(`source-${id}`)) id++;
      const row = addSource({ id: `source-${id}`, format: 'normalized', path: '', enabled: true }); row.controls.path.focus(); syncJson();
    });
    simple.append(sourceHeading, note('Каталог или JSONL-файл. Поддерживаются абсолютные пути, ~/ и ${HOME}/; команды в путях не исполняются. ID: латинские буквы, цифры, дефис или подчёркивание.'), sources, add);
    const rules = node('div', 'ai-form-grid');
    [['idleCapMinutes', 'Предел паузы для активности, минуты', 0.1, 60, 'any'], ['contextWarningPct', 'Сигнал заполнения контекста, %', 1, 100, 'any'], ['minModelSamples', 'Минимум задач для сравнения моделей', 1, 1000, '1']].forEach(([key, label, min, max, step]) => {
      const wrap = node('label', 'ai-field'); const input = node('input'); input.type = 'number'; input.name = `config-${key}`; input.min = String(min); input.max = String(max); input.step = step; input.value = String(config.rules[key]); input.required = true; input.setAttribute('aria-label', label);
      controls[key] = input; wrap.append(node('span', '', label), input); rules.append(wrap);
    });
    simple.append(node('h3', '', 'Правила измерения'), rules, note('Изменение правил меняет сравнимость score и времени. Используй одну версию настроек для сопоставимых задач.'));
    const economicFields = node('div', 'ai-form-grid');
    const preferredCurrencyLabel = node('label', 'ai-field'); const preferredCurrency = node('input'); preferredCurrency.type = 'text'; preferredCurrency.name = 'config-currency'; preferredCurrency.maxLength = 3; preferredCurrency.value = config.economics?.currency || 'USD'; preferredCurrency.placeholder = 'USD, JPY, GBP…'; preferredCurrency.setAttribute('aria-label', 'Валюта новых результатов');
    preferredCurrencyLabel.append(node('span', '', 'Валюта новых результатов'), preferredCurrency);
    const preferredRateLabel = node('label', 'ai-field'); const preferredRate = node('input'); preferredRate.type = 'number'; preferredRate.name = 'config-hourlyRate'; preferredRate.min = '0'; preferredRate.step = 'any'; preferredRate.value = num(config.economics?.hourlyRate) == null ? '' : String(config.economics.hourlyRate); preferredRate.placeholder = 'Неизвестно'; preferredRate.setAttribute('aria-label', 'Стоимость часа для новых результатов');
    preferredRateLabel.append(node('span', '', 'Стоимость часа для новых результатов'), preferredRate); economicFields.append(preferredCurrencyLabel, preferredRateLabel);
    simple.append(node('h3', '', 'Значения для новых результатов'), economicFields, note('Подставляются в новые записи. Сохранённые результаты не изменяются. Пустая ставка означает неизвестную стоимость часа.'));
    const advanced = disclosure('Все параметры в JSON', 'Лимиты, веса score, инструменты и правила файлов Git');
    const json = node('textarea', 'ai-config-json'); json.rows = 16; json.spellcheck = false; json.setAttribute('aria-label', 'Настройки аналитики JSON'); json.value = JSON.stringify(config, null, 2);
    const undoJson = button('Отменить изменения JSON', () => { advancedDirty = false; simple.disabled = false; undoJson.disabled = true; syncJson(); announce('JSON восстановлен из полей формы.'); }); undoJson.disabled = true;
    function fromSimple() {
      const draft = JSON.parse(JSON.stringify(config)); draft.autoDiscover = discovery.checked;
      draft.sources = rows.map(row => ({ id: row.controls.id.value.trim(), format: row.controls.format.value, path: row.controls.path.value.trim(), enabled: row.controls.enabled.checked }));
      for (const [key, input] of Object.entries(controls)) {
        if (!input.value.trim() || !Number.isFinite(Number(input.value))) throw new Error(`Укажи число в поле «${input.getAttribute('aria-label')}».`);
        draft.rules[key] = Number(input.value);
      }
      const currency = preferredCurrency.value.trim().toUpperCase();
      if (!/^[A-Z]{3}$/.test(currency)) throw new Error('Валюта новых результатов: укажи код из трёх латинских букв.');
      const rateText = preferredRate.value.trim(); const hourlyRate = rateText === '' ? null : Number(rateText);
      if (hourlyRate != null && (!Number.isFinite(hourlyRate) || hourlyRate < 0)) throw new Error('Стоимость часа должна быть неотрицательным числом или оставаться пустой.');
      draft.economics = { ...draft.economics, currency, hourlyRate };
      return draft;
    }
    function current() {
      if (!advancedDirty) return fromSimple();
      const value = JSON.parse(json.value);
      if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Настройки JSON должны быть объектом.');
      return value;
    }
    function syncJson() { if (!advancedDirty) { try { json.value = JSON.stringify(fromSimple(), null, 2); } catch (error) { fail(error); } } }
    simple.addEventListener('input', syncJson); simple.addEventListener('change', syncJson);
    json.addEventListener('input', () => { advancedDirty = true; simple.disabled = true; undoJson.disabled = false; announce('Редактируется JSON. Изменения применятся только после сохранения; обычные поля временно недоступны.'); });
    advanced.content.append(note('Для обычного подключения достаточно полей выше. Здесь можно настроить все параметры или вставить переносимую конфигурацию. Неизвестные параметры и неверные значения отклоняются при сохранении.'), json, undoJson);
    const actions = node('div', 'ai-config-actions');
    const save = node('button', 'ai-button ai-button-primary', 'Сохранить настройки'); save.type = 'submit';
    const defaults = button('Загрузить значения по умолчанию', async () => {
      if (defaults.disabled) return; defaults.disabled = true; save.disabled = true; simple.disabled = true; json.disabled = true;
      try {
        if (!window.jarvis?.analyticsDefaults) throw new Error('Значения по умолчанию недоступны.');
        const value = configValue(await window.jarvis.analyticsDefaults());
        if (state.configBody === body) renderConfigEditor(state, value, 'Загружены значения по умолчанию. Нажми «Сохранить настройки», чтобы применить.');
      } catch (error) { fail(error); defaults.disabled = false; save.disabled = false; simple.disabled = advancedDirty; json.disabled = false; }
    });
    const copy = button('Копировать настройки', async () => {
      try {
        const value = current();
        if (!window.jarvis?.copyText) throw new Error('Буфер обмена недоступен.');
        const result = await window.jarvis.copyText(JSON.stringify(value, null, 2));
        if (result?.ok === false || result?.error) throw new Error(result.error || 'Буфер обмена недоступен.');
        announce('Настройки скопированы. Несохранённые изменения также включены.');
      } catch (error) { fail(error); }
    });
    actions.append(save, copy, defaults);
    form.append(simple, advanced.root, status, actions);
    form.addEventListener('submit', async event => {
      event.preventDefault(); if (save.disabled) return;
      let draft; try { draft = current(); } catch (error) { fail(`Не сохранено: ${errorText(error)}`); return; }
      save.disabled = true; defaults.disabled = true; simple.disabled = true; json.disabled = true; undoJson.disabled = true; announce('Сохраняем настройки…');
      try {
        if (!window.jarvis?.analyticsSaveConfig) throw new Error('Сохранение настроек недоступно.');
        const result = await window.jarvis.analyticsSaveConfig(draft);
        if (!result?.ok) throw new Error(result?.error || 'Нет подтверждения сохранения.');
        const saved = configValue(result.config);
        if (state.configBody !== body) return;
        renderConfigEditor(state, saved, 'Настройки сохранены.'); await loadReport(state, true);
      } catch (error) {
        fail(`Не сохранено: ${errorText(error)}`); save.disabled = false; defaults.disabled = false; simple.disabled = advancedDirty; json.disabled = false; undoJson.disabled = !advancedDirty;
      }
    });
    body.replaceChildren(form);
  }

  async function render(host, options = {}) {
    let state = states.get(host);
    if (!state) { state = { host, period: 'week', project: '', projects: new Map(), request: 0, success: '' }; states.set(host, state); }
    // Invalidate pending callbacks before replacing a previous mount.
    ++state.request; host.replaceChildren(); host.classList.add('ai-analytics-host');
    const root = node('div', 'ai-analytics'); const header = node('header', 'ai-header');
    header.append(node('h1', '', 'Аналитика ИИ'));
    const actions = node('div', 'ai-header-actions');
    state.export = button('Копировать JSON', async () => {
      if (!state.report) return;
      const request = state.request; const current = state.report; state.export.disabled = true;
      try {
        if (!window.jarvis?.copyText) throw new Error('Копирование недоступно в этой версии Jarvis.');
        const result = await window.jarvis.copyText(JSON.stringify(current, null, 2));
        if (result?.ok === false || result?.error) throw new Error(result.error || 'Буфер обмена недоступен.');
        if (request === state.request) { state.status.setAttribute('role', 'status'); state.status.textContent = 'JSON отчёта скопирован.'; }
      } catch (error) {
        if (request === state.request) { state.status.setAttribute('role', 'alert'); state.status.textContent = `Не удалось скопировать отчёт: ${errorText(error)}`; }
      } finally { if (request === state.request) state.export.disabled = !state.report; }
    });
    state.export.disabled = true; actions.append(state.export); header.append(actions);
    if (options.onUsage) actions.append(button('Расход и лимиты', () => { ++state.request; options.onUsage(); }));
    const controls = node('div', 'ai-controls');
    const periods = node('div', 'ai-periods'); periods.setAttribute('role', 'group'); periods.setAttribute('aria-label', 'Период аналитики');
    [['today', 'Сегодня'], ['week', '7 дней'], ['month', '30 дней'], ['all', 'Всё']].forEach(([period, label]) => {
      const b = button(label, () => {
        state.period = period;
        [...periods.children].forEach(item => item.setAttribute('aria-pressed', String(item === b)));
        loadReport(state);
      }); b.setAttribute('aria-pressed', String(period === state.period)); periods.append(b);
    });
    state.projectSelect = node('select', 'ai-project-select'); state.projectSelect.setAttribute('aria-label', 'Проект аналитики');
    const all = node('option', '', 'Все проекты'); all.value = ''; state.projectSelect.append(all);
    state.projectSelect.addEventListener('change', () => { state.project = state.projectSelect.value; state.manualProject.value = ''; loadReport(state); });
    state.refresh = button('Обновить', () => loadReport(state, true));
    controls.append(periods, state.projectSelect, state.refresh);
    const manual = node('form', 'ai-manual-project');
    state.manualProject = node('input'); state.manualProject.type = 'text'; state.manualProject.placeholder = 'Другой репозиторий: /путь или ~/путь'; state.manualProject.setAttribute('aria-label', 'Путь к проекту аналитики');
    const openProject = node('button', 'ai-button', 'Открыть проект'); openProject.type = 'submit';
    manual.append(state.manualProject, openProject);
    manual.addEventListener('submit', event => {
      event.preventDefault(); const path = state.manualProject.value.trim();
      if (!path || !(path.startsWith('/') || path.startsWith('~/') || path.startsWith('${HOME}/'))) { state.status.setAttribute('role', 'alert'); state.status.textContent = 'Укажи абсолютный путь к репозиторию или путь от ~/.'; state.manualProject.focus(); return; }
      state.project = path; loadReport(state);
    });
    state.status = node('p', 'ai-load-status'); state.status.setAttribute('aria-live', 'polite'); state.body = node('div', 'ai-report');
    const configBox = disclosure('Настройки аналитики'); state.configBody = configBox.content; state.configLoaded = false; state.configLoading = false;
    const settingsButton = button('Настройки аналитики', () => {
      configBox.root.open = !configBox.root.open; settingsButton.setAttribute('aria-expanded', String(configBox.root.open));
      if (configBox.root.open && !state.configLoaded) loadConfig(state);
    });
    settingsButton.setAttribute('aria-expanded', 'false'); actions.prepend(settingsButton);
    configBox.root.addEventListener('toggle', () => { settingsButton.setAttribute('aria-expanded', String(configBox.root.open)); if (configBox.root.open && !state.configLoaded) loadConfig(state); });
    root.append(header, controls, manual, state.status, configBox.root, state.body); host.append(root);
    await loadReport(state);
  }
  window.jarvisAiAnalytics = { render };
})();
