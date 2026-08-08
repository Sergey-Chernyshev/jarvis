/* Контракт панели «Циклы» с демоном.
 *
 * Модуль рисует расписание руками, и вид `Wake` на проводе — договор, а не
 * деталь. Он уже разъезжался: панель ждала `Daily`, serde слал `daily`, и
 * конструктор молча терял расписание при сохранении. Тест держит обе стороны
 * договора; парная проверка со стороны Rust — в loops::model::tests. */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const src = readFileSync(new URL('./loops.js', import.meta.url), 'utf8');

test('расписание читается и пишется в том же виде, в каком его шлёт демон', () => {
  for (const key of ["'manual'", "'daily'", "'every'"]) {
    assert.ok(src.includes(key), `модуль не знает про ${key}`);
  }
  // Варианты с большой буквы означали бы возврат прежней ошибки.
  assert.ok(!/wake\.(Daily|Every)/.test(src), 'варианты расписания снова с большой буквы');
});

test('состояния запуска и вердикты названы так же, как в модели', () => {
  for (const name of ['running', 'asking', 'stopped', 'done']) {
    assert.ok(src.includes(`'${name}'`), `нет состояния ${name}`);
  }
  for (const name of ['passed', 'returned', 'gateFailed', 'failed']) {
    assert.ok(src.includes(name), `нет вердикта ${name}`);
  }
});

test('каждая команда моста имеет пару в панели', () => {
  const bridge = readFileSync(new URL('./bridge.js', import.meta.url), 'utf8');
  const used = [...src.matchAll(/window\.jarvis\.(loops\w+)\(/g)].map((m) => m[1]);
  assert.ok(used.length >= 8, 'панель почти ничего не зовёт — похоже на обрыв');
  for (const fn of new Set(used)) {
    assert.ok(bridge.includes(`${fn}:`), `моста для ${fn} нет — вызов упадёт в рантайме`);
  }
});

test('ошибка команды показывается, а не глотается', () => {
  // Цикл не запустился — человек обязан узнать почему, иначе он будет ждать
  // результата всю ночь.
  assert.ok(/res\.ok === false/.test(src), 'нет ветки отказа');
  assert.ok(/note\(res\.error/.test(src), 'причина отказа не доходит до экрана');
});

test('вход в конструктор есть всегда, а не только на пустом экране', () => {
  // Раньше конструктор прятался: библиотека исчезала, стоило открыть цикл.
  assert.ok(/\+ Новый цикл/.test(src), 'нет постоянной кнопки нового цикла');
  const render = src.slice(src.indexOf('function render()'));
  assert.ok(/const side = el\('aside\.lp-side'/.test(render), 'колонка собирается не всегда');
  assert.ok(!/state\.loops\.length \? el\('aside/.test(render), 'колонка снова зависит от наличия циклов');
});

test('новый цикл не пишется на диск, пока его не создали', () => {
  // Передумавший на первом же поле не должен оставлять «без имени» навсегда.
  assert.ok(src.includes('loopsDraft'), 'создание не через заготовку');
  assert.ok(!src.includes('loopsCreate'), 'осталось старое создание сразу на диск');
  assert.ok(/text: isNew \? 'Создать цикл'/.test(src), 'у нового цикла нет своей кнопки');
});
