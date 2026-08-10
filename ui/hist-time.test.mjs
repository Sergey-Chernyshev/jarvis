import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

// renderer.js целиком не грузится в node (браузерные глобалы, нет экспортов),
// поэтому берём из него ровно две чистые функции — так же, как theme-accent
// прогоняет theme.js на стабе. Иначе подпись времени осталась бы без тестов, а
// в ней ровно та арифметика дат, где ошибки живут годами.
const source = readFileSync(new URL("./renderer.js", import.meta.url), "utf8");

function extract(name) {
  const start = source.indexOf(`function ${name}(`);
  assert.notEqual(start, -1, `${name} не найдена в renderer.js`);
  let depth = 0;
  for (let i = source.indexOf("{", start); i < source.length; i += 1) {
    if (source[i] === "{") depth += 1;
    else if (source[i] === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(start, i + 1);
    }
  }
  throw new Error(`не удалось выделить тело ${name}`);
}

const context = { pad2: (n) => String(n).padStart(2, "0") };
vm.createContext(context);
new vm.Script(`${extract("histTime")}\n${extract("histDay")}`).runInContext(context);
const { histTime, histDay } = context;

const at = (y, m, d, h = 12, min = 0) => new Date(y, m - 1, d, h, min).getTime();

test("строка чата подписана временем суток — дату несёт заголовок дня", () => {
  const now = new Date();
  const today = at(now.getFullYear(), now.getMonth() + 1, now.getDate(), 17, 15);
  assert.equal(histTime(today), "17:15");
  // Важное: у давнего чата тоже время, а не «04.08». Иначе под заголовком
  // «04 августа» девять строк повторяли бы эту же дату, а различает их время.
  assert.equal(histTime(at(2026, 8, 4, 3, 22)), "03:22");
  assert.equal(histTime(at(2024, 12, 31, 9, 5)), "09:05");
});

test("заголовок дня: сегодня и вчера — словами", () => {
  const now = new Date();
  assert.equal(histDay(now.getTime()), "Сегодня");
  const y = new Date();
  y.setDate(y.getDate() - 1);
  assert.equal(histDay(y.getTime()), "Вчера");
});

// «Вчера» должно выживать на переходе через месяц и год: 1 января заголовок
// обязан сказать «Вчера» про 31 декабря, а не показать дату.
test("вчера остаётся вчера на границе месяца и года", () => {
  const realDate = Date;
  const pinned = (y, m, d) =>
    class extends realDate {
      constructor(...args) {
        super(...(args.length ? args : [y, m - 1, d, 12, 0, 0, 0]));
      }
    };

  try {
    context.Date = pinned(2027, 1, 1);
    assert.equal(histDay(at(2026, 12, 31, 23, 30)), "Вчера");
    context.Date = pinned(2026, 3, 1);
    assert.equal(histDay(at(2026, 2, 28, 9, 0)), "Вчера");
  } finally {
    context.Date = realDate;
  }
});

// Год в заголовке дня нужен только у прошлогодних: иначе «04 августа» из 2024
// и из этого года выглядят одинаково, а список отсортирован по времени.
test("заголовок дня показывает год только у прошлогодних", () => {
  const now = new Date();
  assert.match(histDay(at(now.getFullYear() - 2, 8, 4)), new RegExp(String(now.getFullYear() - 2)));
  const thisYear = at(now.getFullYear(), 1, 15);
  if (Math.abs(now.getTime() - thisYear) > 3 * 24 * 60 * 60 * 1000) {
    assert.doesNotMatch(histDay(thisYear), /\d{4}/);
  }
});
