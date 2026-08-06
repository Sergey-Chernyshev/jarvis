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
new vm.Script(`${extract("histTime")}`).runInContext(context);
const { histTime } = context;

const at = (y, m, d, h = 12, min = 0) => new Date(y, m - 1, d, h, min).getTime();

test("сегодняшний чат подписан временем суток", () => {
  const now = new Date();
  const today = at(
    now.getFullYear(),
    now.getMonth() + 1,
    now.getDate(),
    17,
    15,
  );
  assert.equal(histTime(today), "17:15");
});

test("вчерашний чат читается словом, а не датой", () => {
  const yesterday = new Date();
  yesterday.setDate(yesterday.getDate() - 1);
  yesterday.setHours(16, 57, 0, 0);
  assert.equal(histTime(yesterday.getTime()), "вчера");
});

// «вчера» должно выживать на переходе через месяц и год: 1 января подпись
// обязана указывать на 31 декабря, а не на «31.12» прошлого года.
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
    assert.equal(histTime(at(2026, 12, 31, 23, 30)), "вчера");
    context.Date = pinned(2026, 3, 1);
    assert.equal(histTime(at(2026, 2, 28, 9, 0)), "вчера");
  } finally {
    context.Date = realDate;
  }
});

test("давние чаты — дата, а прошлогодние — с годом", () => {
  const now = new Date();
  const thisYear = at(now.getFullYear(), 1, 5, 9, 0);
  // 5 января этого года: если сегодня рядом с этой датой, тест сравнивал бы
  // «вчера» — поэтому берём заведомо далёкий день того же года.
  if (Math.abs(now.getTime() - thisYear) > 3 * 24 * 60 * 60 * 1000) {
    assert.equal(histTime(thisYear), "05.01");
  }
  assert.equal(histTime(at(now.getFullYear() - 2, 8, 3, 9, 0)), `03.08.${now.getFullYear() - 2}`);
});
