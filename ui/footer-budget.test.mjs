/* Бюджет в футере панели.
 *
 * Панельный вызов лимитов долго отдавал одно состояние баннера — бюджета
 * человек в интерфейсе не видел вовсе. Теперь видит, и не голым процентом:
 * 44% при спокойном темпе это запас, при рывке — завтрашняя стена. Тесты
 * держат три свойства: строка говорит про ДНИ, процент стоит рядом мелким,
 * а две подписки живут двумя шкалами и не складываются. */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const src = readFileSync(new URL('./renderer.js', import.meta.url), 'utf8');
/* Константы и сама строка — без узлов DOM: строка чистая, и проверять её надо
 * именно так, а не через настоящую разметку. */
const body =
  src.slice(src.indexOf('const WEEKDAY = ['), src.indexOf('const footerBudgetEl')) +
  src.slice(src.indexOf('function budgetLine('), src.indexOf('function takeBudget('));

/** Настоящий код строки поверх подставного окружения. */
const line = new Function('name', 'p', 'now', `${body}; return budgetLine(name, p, now);`);

const DAY = 86400000;
/* Опора всех чисел: понедельник 17.08.2026, полдень по местному времени. */
const MON = new Date(2026, 7, 17, 12, 0, 0).getTime();

const claude = (over = {}) => ({
  rung: 'warn',
  reason: 'при темпе 12.0%/сут прогноз перестал дотягивать до сброса',
  weekLeftPct: 44.3,
  weekResetAt: MON + 2 * DAY, // среда
  daysToReset: 2,
  runwayDays: 1.2,            // вторник — до среды не дотянет
  ...over,
});

test('футер говорит про дни запаса, а не голый процент', () => {
  const l = line('claude', claude(), MON);
  assert.equal(l.text, 'хватит до вт, до сброса (ср) не дотянет');
  assert.equal(l.text.includes('%'), false, 'процент в тексте запаса делать нечего');
  assert.equal(l.pct, 44, 'процент отдан отдельно — он идёт рядом и мелким');
});

test('запас, дотягивающий до сброса, так и назван', () => {
  const l = line('claude', claude({ runwayDays: 5, rung: 'ok' }), MON);
  assert.equal(l.text, 'хватит до сб, до сброса (ср) дотянет');
});

test('две подписки — две шкалы: у каждой свой день сброса', () => {
  // у kimi сброс во вторник, у claude в среду; общей цифры быть не может
  const kimi = line('kimi', claude({ weekResetAt: MON + DAY, daysToReset: 1, runwayDays: 0.5, weekLeftPct: 45 }), MON);
  const cl = line('claude', claude(), MON);
  assert.equal(kimi.text, 'хватит до вт, до сброса (вт) не дотянет');
  assert.equal(cl.text, 'хватит до вт, до сброса (ср) не дотянет');
  assert.notEqual(kimi.pct, cl.pct, 'проценты у шкал свои');
});

test('прогноза нет — дня не обещаем, а говорим об этом', () => {
  const l = line('claude', claude({ runwayDays: null, rung: 'ok' }), MON);
  assert.equal(l.text, 'темпа пока нет, сброс ср');
  assert.equal(l.pct, 44);
});

test('чисел нет — молчания не будет: причина уходит в подсказку', () => {
  const l = line('claude', { rung: 'unknown', reason: 'токен kimi протух — нет свежих чисел' }, MON);
  assert.equal(l.text, 'чисел нет');
  assert.equal(l.pct, null);
  assert.equal(l.title, 'токен kimi протух — нет свежих чисел');
  assert.equal(line('claude', null, MON).text, 'чисел нет');
});

test('ступень бюджета доезжает до строки — футеру есть что покрасить', () => {
  assert.equal(line('claude', claude({ rung: 'stop' }), MON).rung, 'stop');
  const paint = src.slice(src.indexOf('function paintFooterBudget('), src.indexOf('function refreshBudget('));
  assert.ok(paint.includes("['claude', 'kimi']"), 'провайдеры перестали быть двумя отдельными шкалами');
  assert.ok(paint.includes("createElement('small')"), 'процент рядом мелким пропал');
  assert.ok(paint.includes("footerBottom !== 'limit'"), 'бюджет лезет в футер поверх расхода');
});

test('в футере действительно появляются две строки, а процент — мелким узлом', () => {
  const { document } = parseHTML('<footer><span id="footerBudget" hidden></span></footer>');
  const el = document.getElementById('footerBudget');
  const paint = new Function(
    'document', 'footerBudgetEl', 'budgetInfo', 'footerBottom', 'budgetLine',
    `${src.slice(src.indexOf('function paintFooterBudget('), src.indexOf('function refreshBudget('))}; paintFooterBudget();`,
  );
  const info = { providers: { claude: claude(), kimi: claude({ weekLeftPct: 45, rung: 'stop' }) } };

  paint(document, el, info, 'limit', (n, p, now) => line(n, p, now));
  assert.equal(el.hidden, false);
  assert.equal(el.children.length, 2, 'две подписки — две строки');
  assert.equal(el.querySelectorAll('small').length, 2, 'процент рядом мелким');
  assert.ok(el.textContent.includes('хватит до'), el.textContent);
  assert.ok(el.textContent.includes('44%') && el.textContent.includes('45%'), el.textContent);
  assert.equal(el.children[1].className.includes('is-crit'), true, 'ступень «стоп» не покрашена');

  // расход в футере — бюджету там места нет
  paint(document, el, info, 'spend', (n, p, now) => line(n, p, now));
  assert.equal(el.hidden, true);
});

test('бюджет берётся из ответа limit_get, а событие баннера его не затирает', () => {
  assert.ok(src.includes('takeBudget(l)'), 'ответ limit_get не кладётся в бюджет');
  const on = src.slice(src.indexOf('window.jarvis.onLimitState('), src.indexOf('setInterval(paintLimitBanner'));
  assert.equal(on.split('\n')[0].includes('takeBudget'), false,
    'событие limit-state бюджета не несёт — им нельзя затирать числа');
});
