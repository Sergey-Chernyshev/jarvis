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
  assert.equal(pane.querySelectorAll('.s2err').length, 1, 'плашки ошибок копятся');
});

test('wake-word без движка в сборке не предлагают включить и не зовут качать веса', async () => {
  const { doc, calls } = await boot({
    wakeGet: { enabled: false, model_present: true, threshold: 0.5, muted: false, ort_built: false },
  });
  const pane = await openPane(doc, 'wake');

  assert.ok(pane.textContent.includes('недоступно в этой сборке'), 'про сборку не сказано: ' + pane.textContent.slice(0, 300));
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
  assert.equal(pane.textContent.includes('недоступно в этой сборке'), false);
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
    .filter((i) => i.style.display !== 'none')
    .map((i) => i.textContent);

  const type = async (value) => {
    input.value = value;
    input.dispatchEvent(new window.Event('input', { bubbles: true }));
    await settle();
  };

  await type('прокси');
  const proxied = visible();
  assert.ok(proxied.includes('Под капотом'), 'egress-прокси не находится: ' + proxied.join(', '));
  assert.ok(proxied.includes('Запуск'), 'команда прокси не находится');
  assert.ok(!proxied.includes('Уведомления'), 'фильтр не отсекает лишнее');

  await type('kimi');
  assert.deepEqual(visible().sort(), ['Агенты', 'Интеграция']);

  await type('шчшч');
  assert.equal(visible().length, 0);
  assert.equal(doc.querySelector('.snav-none').style.display, '');

  await type('');
  assert.equal(visible().length, 14, 'после сброса вернулись не все разделы');
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
