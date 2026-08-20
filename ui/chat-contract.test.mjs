import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const html = readFileSync(new URL('./index.html', import.meta.url), 'utf8');
const renderer = readFileSync(new URL('./renderer.js', import.meta.url), 'utf8');

// Сводки ходов (chatsum v1, возврат по спеке 2026-07-18): чат — одна лента,
// поверх которой живёт режим «Сводка» — тумблер в шапке, карточки .turnsum
// и подписка на chat:summary. Тест стережёт НАЛИЧИЕ этой поверхности.
test('session chat has the turn-summary surface on top of the transcript feed', () => {
  // тумблер «Сводка/Лента» в шапке чата + стили карточек и режима
  assert.match(html, /id="sumToggle"/);
  assert.match(html, /\.turnsum\b/);
  assert.match(html, /#chatlog\.sum \.turn\.done \.turnraw/);

  // рендерер: группировка ленты в ходы, карточки, тумблер, событие демона
  assert.match(renderer, /\bfunction startTurn\(/);
  assert.match(renderer, /\bfunction applyCard\(/);
  assert.match(renderer, /\bsummaryModeOn\b/);
  assert.match(renderer, /getElementById\('sumToggle'\)/);
  assert.match(renderer, /onChatSummary\(/);
});

// Слой «Документы», инкремент 2 (спека 2026-07-18 §3.1/§3.3): вьюер документа
// в панели — слайд-овер, безопасный markdown-рендер, открытие с файл-чипа.
test('doc viewer surface exists on top of the summary cards', () => {
  assert.match(html, /id="docWrap"/);
  assert.match(html, /id="docBody"/);
  assert.match(html, /markdown\.js/);

  assert.match(renderer, /\bfunction openDocViewer\(/);
  assert.match(renderer, /JarvisMarkdown\.render\(/);
  assert.match(renderer, /JarvisMarkdown\.isDocPath\(/); // doc-чипы первыми + CTA
});

// Рендерер реплик один на всех: чат сессии, вкладка «Джарвис» и окно из трея
// зовут markdown.js. Своя копия в renderer.js оставляла два языка разметки —
// вкладке и окну она была недоступна, и ответы агента шли сырым текстом.
test('assistant markdown lives in markdown.js only', () => {
  const markdown = readFileSync(new URL('./markdown.js', import.meta.url), 'utf8');
  assert.match(markdown, /function renderChat\(root, text\)/);
  assert.match(renderer, /JarvisMarkdown\.renderChat\(/);
  // признаки собственной копии: разбор Insight и склонение «N заметок»
  assert.doesNotMatch(renderer, /callout-body/);
  assert.doesNotMatch(renderer, /\bfunction notesWord\(/);
});

/* Рендерер один, а стили реплики лежат в двух файлах — и разъезжаются молча.
 * Заголовок, неотличимый от жирного, и код без переноса — это про чтение: во
 * вкладке и в окне из трея человек читает один и тот же ответ. */
test('assistant markdown styles agree between panel and agent window', () => {
  const win = readFileSync(new URL('./agent-chat.html', import.meta.url), 'utf8');
  for (const [where, css] of [['index.html', html], ['agent-chat.html', win]]) {
    const h = css.match(/\.bubble \.md-h[^{]*\{([^}]*)\}/);
    assert.ok(h, where + ': заголовок ответа не стилизован вовсе');
    assert.match(h[1], /font-size:\s*15\.5px/, where + ': заголовок неотличим от жирного текста');
    assert.match(h[1], /margin:\s*16px 0 6px/, where + ': заголовку не дали воздуха сверху');

    const pre = css.match(/\.bubble pre[^{]*\{([^}]*)\}/);
    assert.ok(pre, where + ': блок кода не стилизован');
    assert.match(pre[1], /white-space:\s*pre-wrap/, where + ': длинная строка кода уезжает вправо в никуда');
    assert.doesNotMatch(pre[1], /max-height/, where + ': блок кода — ловушка скролла при погашенных полосах');
  }
});

/* История разговоров живёт в двух документах, и стили у неё две копии — они
 * разъезжаются молча. Строка с диска, отличимая только в одном из окон, — это
 * снова недостижимый разговор, просто с другой стороны. */
test('история разговоров одинаково размечена во вкладке и в окне', () => {
  const win = readFileSync(new URL('./agent-chat.html', import.meta.url), 'utf8');
  for (const [where, css] of [['index.html', html], ['agent-chat.html', win]]) {
    for (const sel of ['.aghead', '.aglist', '.agchat.on', '.agdisk', '.agmeta', '.agedit', '.agempty']) {
      assert.ok(css.includes(sel), where + ': ' + sel + ' не стилизован — история разъехалась');
    }
    /* Убрать и забыть: у строки с диска не было кнопок вовсе. Крестик прячет,
     * слово рядом с метаданными — стирает файл, и «скрыто N · вернуть» держит
     * скрытие обратимым. Пропадёт в одном из окон — вернётся та же беда. */
    for (const sel of ['.aghide', '.agforget', '.aghidden', '.agask', '.agbtn.danger']) {
      assert.ok(css.includes(sel), where + ': ' + sel + ' не стилизован — убрать разговор снова нечем');
    }
    // Необратимому — единственная краска, которой это можно сказать; обратимому
    // скрытию она не положена: красный крестик обещал бы потерю там, где её нет.
    assert.match(css.match(/\.agforget:hover\s*\{([^}]*)\}/)[1], /var\(--danger\)/, where + ': забвение ничем не выделено');
    assert.doesNotMatch(css.match(/\.aghide:hover\s*\{([^}]*)\}/)[1], /--danger/, where + ': обратимое скрытие выкрашено как потеря');
    /* Занятость разговора — ровно то, ради чего сняли запрет на переключение:
     * уходя во второй чат, надо видеть, что первый ещё пишет. Отличаем формой и
     * весом, как состояния циклов, а не второй краской в списке. */
    for (const sel of ['.agchat.busy .agname', '.agcur.busy::before', '.agbusy']) {
      assert.ok(css.includes(sel), where + ': ' + sel + ' не стилизован — занятость чата не видна');
    }
    const dot = css.match(/\.agchat\.busy \.agname::before[^{]*\{([^}]*)\}/);
    assert.match(dot[1], /background: var\(--accent\)/, where + ': занятость раскрашена мимо единственной краски');
    // колонка не должна выдавливать переписку: потолок высоты со скроллом
    const list = css.match(/\.aglist\s*\{([^}]*)\}/);
    assert.match(list[1], /max-height/, where + ': у истории нет потолка — двадцать разговоров съедят экран');
    assert.match(list[1], /overflow-y:\s*auto/, where + ': историю без потолка нельзя прокрутить');
  }
  // полоски чипов больше нет: у строки истории есть заголовок, время и размер
  assert.doesNotMatch(html, /\.agchats\s*\{[^}]*overflow-x/, 'история снова свернулась в полоску чипов');
});

/* Мост зовёт демона по имени: разъехавшееся имя команды — тихий отказ, который
 * видно только на живом маке. Особенно agent_chat_open — он раньше значил
 * «открыть окно чата», а теперь «привязать разговор с диска». */
test('каждая команда чата, которую зовёт UI, зарегистрирована у демона', () => {
  const bridge = readFileSync(new URL('./bridge.js', import.meta.url), 'utf8');
  const chat = readFileSync(new URL('./agent-chat.js', import.meta.url), 'utf8');
  const main = readFileSync(new URL('../src-tauri/src/main.rs', import.meta.url), 'utf8');
  const used = new Set(
    [...(bridge + chat).matchAll(/invoke\('(agent_[a-z_]+)'/g)].map((m) => m[1]),
  );
  assert.ok(used.has('agent_chat_open'), 'разговор с диска открывать нечем');
  for (const cmd of used) {
    assert.match(main, new RegExp('ipc::' + cmd + '\\b'), `UI зовёт ${cmd}, а демон такой команды не знает`);
  }
  // старый смысл «открыть окно чата» переехал в agent_chat_window и в UI не зовётся
  assert.ok(!used.has('agent_chat_window'), 'agent_chat_open перепутан с окном чата');
});

// Легаси-заготовка «саммари сессии» (chatModeSeg/chatSummaryEl/setChatMode)
// не возвращается: v1 её заменил тумблером сводок, а не воскресил.
test('legacy summary-mode stub stays absent', () => {
  assert.doesNotMatch(html, /\bchatModeSeg\b/);
  assert.doesNotMatch(html, /\.chatmode-(?:seg|btn)\b/);
  assert.doesNotMatch(html, /Саммари сессии — заготовка дизайна/);

  assert.doesNotMatch(renderer, /\bchatModeSeg\b/);
  assert.doesNotMatch(renderer, /\bchatSummaryEl\b/);
  assert.doesNotMatch(renderer, /\bsetChatMode\b/);
});

// Голоса диалога разведены формой и стороной (без второй краски): человек —
// клеверный пузырь у правого края, агент — текст на бумаге слева. Обе стороны
// одинаковым текстом уже были — диалог читался как монолог.
test('user and agent messages are visually distinct voices', () => {
  assert.match(html, /\.msg\.user \.bubble \{[^}]*background: var\(--accent-soft\)/s);
  assert.match(html, /\.msg\.user \{[^}]*align-self: flex-end/s);
  // у агента подложки нет — его голос остаётся текстом на бумаге
  assert.doesNotMatch(html, /\.msg\.assistant \.bubble \{[^}]*background: var\(--accent/s);
});
