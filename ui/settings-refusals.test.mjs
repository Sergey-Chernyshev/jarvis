/* Отказы бэкенда в настройках — в настоящем DOM.
 *
 * Бэкенд отвечает конкретной причиной («модель не скачана», «нужна нативная
 * сборка», «недоступно в этой сборке»), а панель эти ответы выбрасывала: селект
 * молча возвращался к прежнему движку, тумблер wake-word молча щёлкал обратно,
 * а вкладка «Интеграция» рисовала один захардкоженный Claude Code вместо трёх
 * посчитанных CLI. Тесты кликают по живой разметке — подставного рендера мало.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const HERE = new URL('./', import.meta.url);
const read = (name) => readFileSync(new URL(name, HERE), 'utf8');

// рендереры панелей асинхронны (await *Get()) — даём промисам доиграть
const settle = async () => { for (let i = 0; i < 12; i++) await new Promise((r) => setTimeout(r, 0)); };

/* Мост-заглушка: неизвестный метод отвечает {ok:true}, on*(cb) — подписка. */
async function boot(api) {
  const { window, document } = parseHTML(
    '<!doctype html><html><head></head><body><div id="root"></div></body></html>',
  );
  const calls = [];
  window.jarvis = new Proxy({}, {
    get(_t, prop) {
      if (typeof prop !== 'string') return undefined;
      if (/^on[A-Z]/.test(prop)) return () => () => {};
      return (...args) => {
        calls.push([prop, ...args]);
        const v = api[prop];
        return Promise.resolve(typeof v === 'function' ? v(...args) : (v === undefined ? { ok: true } : v));
      };
    },
  });
  window.requestAnimationFrame = (cb) => setTimeout(() => cb(0), 0);
  for (const name of ['keys.js', 'settings2.js']) {
    const fn = new Function(
      'window', 'document', 'globalThis', 'navigator', 'setTimeout', 'clearTimeout', 'requestAnimationFrame',
      read(name),
    );
    fn(window, document, window, { userAgent: 'Macintosh; Mac OS X' },
      setTimeout, clearTimeout, window.requestAnimationFrame);
  }
  window.initSettings2(document.getElementById('root'));
  await settle();
  return { window, doc: document, calls };
}

function click(doc, node) {
  node.dispatchEvent(new doc.defaultView.Event('click', { bubbles: true }));
}

async function openPane(doc, pane) {
  click(doc, doc.querySelector('.snav .item[data-pane="' + pane + '"]'));
  await settle();
  return doc.querySelector('#s2-pane-' + pane);
}

const STT = {
  sttGet: { engine: 'whisper-turbo', engines: ['whisper-turbo', 'qwen3-1.7b'], noiseGate: false },
  sttInputDevices: { devices: [], current: null },
  hotkeyBindings: { ok: true, bindings: [] },
  modelsGet: { models: [] },
};

test('отказ смены движка виден человеку, а селект не врёт про новый движок', async () => {
  const reason = 'qwen3-1.7b: модель не скачана — сначала скачай её в разделе «Модели»';
  const { doc } = await boot({ ...STT, sttSetEngine: { ok: false, error: reason } });
  const pane = await openPane(doc, 'stt');

  click(doc, pane.querySelector('.copt[data-value="qwen3-1.7b"]'));
  await settle();

  assert.ok(pane.textContent.includes(reason), 'причина отказа не показана: ' + pane.textContent.slice(0, 300));
  assert.equal(pane.querySelectorAll('.loadcap.err').length, 1, 'ошибка не помечена как ошибка');
  // подпись селекта обязана остаться на действующем движке
  assert.equal(pane.querySelector('.cstrigger .cval').textContent, 'whisper-turbo');
});

test('отказ «Сделать активной» называет причину прямо в строке модели', async () => {
  const reason = 'whisper-turbo: модель скачана, но нужна нативная сборка';
  const { doc } = await boot({
    ...STT,
    sttGet: { engine: 'qwen3-1.7b', engines: ['whisper-turbo', 'qwen3-1.7b'] },
    modelsGet: { models: [{ id: 'whisper-turbo', kind: 'stt', label: 'Whisper', present: true, active: false, bytes: 6e8 }] },
    sttSetEngine: { ok: false, error: reason },
  });
  const pane = await openPane(doc, 'stt');

  const btn = [...pane.querySelectorAll('button')].find((b) => b.textContent === 'Сделать активной');
  assert.ok(btn, 'кнопки «Сделать активной» нет');
  click(doc, btn);
  await settle();

  assert.ok(pane.textContent.includes(reason), 'причина отказа не показана');
  assert.equal(btn.disabled, false, 'кнопка осталась заблокированной');
  // повторный клик не должен копить одинаковые плашки
  click(doc, btn);
  await settle();
  assert.equal(pane.querySelectorAll('.loadcap.err').length, 1, 'плашки ошибок копятся');
});

/* Веса скачаны, а движка в сборке нет: кнопка обещала переключение, которого
 * бэкенд не сделает. Как у wake-word — сказали про сборку и не предлагаем. */
test('движок без сборки не предлагают «сделать активной»', async () => {
  const { doc, calls } = await boot({
    ...STT,
    sttGet: { engine: 'qwen3-1.7b', engines: ['whisper-turbo', 'qwen3-1.7b'] },
    modelsGet: {
      models: [
        { id: 'whisper-turbo', kind: 'stt', label: 'Whisper', present: true, active: false, usable: false, bytes: 6e8 },
        { id: 'qwen3-0.6b', kind: 'stt', label: 'Qwen 0.6B', present: true, active: false, usable: true, bytes: 1e9 },
      ],
    },
  });
  const pane = await openPane(doc, 'stt');

  const offer = [...pane.querySelectorAll('button')].filter((b) => b.textContent === 'Сделать активной');
  assert.equal(offer.length, 1, 'кнопку предложили и недоступному движку');
  assert.ok(/[Нн]едоступно.*сборке/.test(pane.textContent), 'про сборку не сказано');
  // отняли ровно у недоступного: уцелевшая кнопка — в строке рабочего движка
  const rows = [...pane.querySelectorAll('.drow')].filter((r) => r.textContent.includes('Qwen 0.6B'));
  assert.equal(rows.length, 1, 'строки рабочего движка нет');
  assert.equal([...rows[0].querySelectorAll('button')].filter((b) => b.textContent === 'Сделать активной').length, 1,
    'у рабочего движка кнопку тоже сняли');
  assert.equal(calls.filter(([m]) => m === 'sttSetEngine').length, 0);
});

test('wake-word без движка в сборке не предлагают включить и не зовут качать веса', async () => {
  const { doc, calls } = await boot({
    wakeGet: { enabled: false, model_present: true, threshold: 0.5, muted: false, ort_built: false },
  });
  const pane = await openPane(doc, 'wake');

  assert.ok(/[Нн]едоступно.*сборке/.test(pane.textContent), 'про сборку не сказано: ' + pane.textContent.slice(0, 300));
  const toggles = pane.querySelectorAll('input.toggle');
  assert.equal(toggles[0].disabled, true, 'тумблер активации доступен на сборке без детектора');
  const dl = [...pane.querySelectorAll('button')].filter((b) => /Скачать|Повторить/.test(b.textContent));
  assert.equal(dl.length, 0, 'веса всё ещё предлагают скачать');
  assert.equal(calls.filter(([m]) => m === 'wakeInstallModels').length, 0);
});

/* Старый бэкенд поля не шлёт — по догадке возможность не отнимаем. */
test('без данных о сборке wake-word остаётся включаемым', async () => {
  const { doc } = await boot({ wakeGet: { enabled: false, model_present: true, threshold: 0.5 } });
  const pane = await openPane(doc, 'wake');
  assert.equal(pane.querySelectorAll('input.toggle')[0].disabled, false);
  assert.equal(/[Нн]едоступно.*сборке/.test(pane.textContent), false);
});

test('«Интеграция» рисует все три CLI из readiness.agents, а не один Claude Code', async () => {
  const { doc } = await boot({
    integrationGet: {
      status: { hooks: true, shim: true, tmux_conf: false, path_block: true },
      foreign_hooks: 0,
      models: [],
      quiet: false,
      readiness: {
        coreReady: false,
        agents: [
          { id: 'claude', label: 'Claude Code', ready: true, available: true, detail: 'События и lifecycle hooks' },
          { id: 'codex', label: 'Codex', ready: false, available: true, detail: 'Codex может запросить доверие', action: 'Подтвердить доверие hooks' },
          { id: 'kimi', label: 'Kimi Code', ready: false, available: false, detail: 'CLI не найден в PATH', action: 'Установить Kimi Code CLI или обновить PATH' },
        ],
        warnings: ['Kimi hooks требуют восстановления.'],
      },
    },
  });
  const pane = await openPane(doc, 'integration');
  const text = pane.textContent;

  for (const label of ['Claude Code', 'Codex', 'Kimi Code']) {
    assert.ok(text.includes(label), 'нет строки агента ' + label);
  }
  assert.ok(text.includes('подключён'), 'готовый агент не отмечен');
  assert.ok(text.includes('CLI не найден'), 'ненайденный CLI не назван');
  assert.ok(text.includes('Подтвердить доверие hooks'), 'что делать с Codex — не сказано');
  assert.ok(text.includes('Kimi hooks требуют восстановления.'), 'предупреждение бэкенда потеряно');
  // глобальный флаг больше не выдаёт себя за состояние одного агента
  assert.equal(text.includes('Claude Code · не подключено'), false);
});

/* Разделов 14, и половина названий о содержимом не говорит — поле поиска было
 * визуальным no-op'ом, то есть хуже отсутствующего. */
test('поиск настроек ищет по названию и по содержимому раздела', async () => {
  const { doc, window } = await boot({});
  const input = doc.querySelector('.ssearch input');
  const visible = () => [...doc.querySelectorAll('.snav .item')]
    .filter((i) => !i.hidden)
    .map((i) => i.textContent);

  const type = async (value) => {
    input.value = value;
    input.dispatchEvent(new window.Event('input', { bubbles: true }));
    await settle();
  };

  await type('прокси');
  const proxied = visible();
  assert.ok(proxied.includes('Под капотом'), 'egress-прокси не находится: ' + proxied.join(', '));
  assert.ok(proxied.includes('Локальный запуск'), 'команда прокси не находится');
  assert.ok(!proxied.includes('Уведомления'), 'фильтр не отсекает лишнее');

  await type('kimi');
  assert.deepEqual(visible().sort(), ['Агенты', 'Интеграция']);

  await type('шчшч');
  assert.equal(visible().length, 0);
  assert.equal(doc.querySelector('.settings-search-empty').hidden, false);

  await type('');
  // 15, а не 14: плагинное ядро добавило раздел «Плагины».
  assert.equal(visible().length, doc.querySelectorAll('.snav .item').length, 'после сброса вернулись не все разделы');
});

/* Тяжёлая загрузка идёт минутами, а вкладку за это время перерисовывают. */
test('идущая загрузка переживает перерисовку вкладки', async () => {
  const models = { models: [{ id: 'silero', kind: 'voice', label: 'Silero', present: false, usable: true }] };
  const { doc } = await boot({ ...STT, modelsGet: models });
  const pane = await openPane(doc, 'stt');

  const btn = [...pane.querySelectorAll('button')].find((b) => /Установить голос/.test(b.textContent));
  assert.ok(btn, 'кнопки установки голоса нет');
  click(doc, btn);
  await settle();

  // уход на другую вкладку и обратно = полная перерисовка панели
  await openPane(doc, 'wake');
  const again = await openPane(doc, 'stt');
  const back = [...again.querySelectorAll('button')].filter((b) => /Установить голос/.test(b.textContent));
  assert.equal(back.length, 0, 'кнопка «Установить» вернулась поверх идущей загрузки');
  assert.ok(again.textContent.includes('Качаю…'), 'о идущей загрузке не сказано');
});

/* Личная дописка к преамбуле правится в панели, а не в JSON.
 *
 * Ключ agentPreamble закрыт для агентов навсегда (allowlist в grant.rs
 * deny-by-default): агент, правящий собственные инструкции, — та же дыра, что
 * нажатие собственной карточки подтверждения. Значит написать туда может только
 * человек, и поле обязано быть в интерфейсе. */
test('дописка к преамбуле: значение читается, сохраняется и подписана как ДОБАВКА', async () => {
  const { doc, calls } = await boot({
    getSettings: () => ({ agentPreamble: 'Отчитывайся таблицей.' }),
    agentsList: { ok: true, agents: [], presets: [] },
  });
  const pane = await openPane(doc, 'agents');
  const area = pane.querySelector('textarea.s2preamble');
  assert.ok(area, 'поля дописки нет в настройках');
  assert.equal(area.value, 'Отчитывайся таблицей.', 'уже написанное не показано — человек затрёт его вслепую');

  // Подпись обязана говорить, что дописка ДОБАВЛЯЕТСЯ. Иначе человек напишет
  // сюда «инструкцию целиком» и будет считать, что базовой больше нет.
  const row = area.closest('.drow');
  assert.match(row.textContent, /добавляется к базовым инструкциям/i);
  assert.match(row.textContent, /не заменяет/i);

  area.value = '  Не трогай infra/  ';
  click(doc, [...row.querySelectorAll('button')].find((b) => b.textContent === 'Сохранить'));
  await settle();
  const saved = calls.filter((c) => c[0] === 'setSettings' && c[1] && 'agentPreamble' in c[1]).pop();
  assert.ok(saved, 'сохранение не ушло в бэкенд');
  assert.equal(saved[1].agentPreamble, 'Не трогай infra/', 'края не обрезаны');
});

test('дописка к преамбуле: отказ бэкенда виден, а не проглочен', async () => {
  const { doc } = await boot({
    getSettings: () => ({ agentPreamble: '' }),
    agentsList: { ok: true, agents: [], presets: [] },
    setSettings: () => ({ ok: false, error: 'настройки не записались' }),
  });
  const pane = await openPane(doc, 'agents');
  const row = pane.querySelector('textarea.s2preamble').closest('.drow');
  click(doc, [...row.querySelectorAll('button')].find((b) => b.textContent === 'Сохранить'));
  await settle();
  assert.match(row.textContent, /настройки не записались/, 'отказ проглочен — поле врёт про сохранённое');
  assert.equal(row.querySelectorAll('.loadcap.err').length, 1, 'отказ не помечен как ошибка');
});
