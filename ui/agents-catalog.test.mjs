/* Каталог агентов: третий агент должен быть ровно таким же жителем панели.
 *
 * Раньше «двух агентов» знал каждый экран по-своему, и каждый новый бэкенд
 * молча выпадал из моделей, уровней усилия и кнопок запуска. Здесь две части:
 * поведение самого каталога и источниковые проверки, что экраны спрашивают
 * его, а не сравнивают id со строкой. */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const A = require('./agents.js');
const renderer = readFileSync(new URL('./renderer.js', import.meta.url), 'utf8');
const loops = readFileSync(new URL('./loops.js', import.meta.url), 'utf8');

/* Ответ app_meta — как его отдаёт бэкенд. */
const META_AGENTS = [
  {
    id: 'claude', title: 'Claude',
    models: [{ id: 'opus', name: 'Opus' }, { id: 'haiku', name: 'Haiku' }],
    effortLevels: ['low', 'medium', 'high', 'xhigh', 'max'],
    hasSeparateEffort: true, supportsCustomAnswer: true, present: true,
  },
  {
    id: 'codex', title: 'Codex',
    models: [{ id: 'gpt-5.5', name: 'GPT-5.5' }],
    effortLevels: [], hasSeparateEffort: false, supportsCustomAnswer: false, present: false,
  },
  {
    id: 'kimi', title: 'Kimi',
    models: [{ id: 'kimi-code/k3', name: 'K3' }, { id: 'kimi-code/k3-256k', name: 'K3-256k' }],
    effortLevels: ['low', 'high', 'max'],
    hasSeparateEffort: true, supportsCustomAnswer: false, present: true,
  },
];

test('до ответа демона каталог знает всех троих встроенных', () => {
  const ids = A.all().map((a) => a.id);
  for (const id of ['claude', 'codex', 'kimi']) {
    assert.ok(ids.includes(id), `встроенного агента ${id} нет в каталоге`);
  }
  assert.ok(A.models('kimi').length, 'у kimi нет моделей до прихода app_meta');
});

test('модели, усилия и умения берутся из app_meta для каждого агента', () => {
  A.setAll(META_AGENTS);

  assert.deepEqual(A.models('claude').map((m) => m.id), ['opus', 'haiku']);
  assert.deepEqual(A.models('codex').map((m) => m.id), ['gpt-5.5']);
  assert.deepEqual(A.models('kimi').map((m) => m.id), ['kimi-code/k3', 'kimi-code/k3-256k']);

  assert.deepEqual(A.efforts('kimi'), ['low', 'high', 'max']);
  assert.deepEqual(A.efforts('codex'), []);
  assert.equal(A.hasSeparateEffort('claude'), true);
  assert.equal(A.hasSeparateEffort('kimi'), true);
  assert.equal(A.hasSeparateEffort('codex'), false, 'у codex своя ручка усилия — палитра тут лишняя');

  assert.equal(A.supportsCustomAnswer('claude'), true);
  assert.equal(A.supportsCustomAnswer('kimi'), false);
  assert.equal(A.supportsCustomAnswer('codex'), false);

  assert.equal(A.title('kimi'), 'Kimi');
});

test('незнакомый агент получает поведение claude, а не пустоту', () => {
  A.setAll(META_AGENTS);
  assert.deepEqual(A.models('qwen').map((m) => m.id), A.models('claude').map((m) => m.id));
  assert.deepEqual(A.efforts('qwen'), A.efforts('claude'));
  // Умения незнакомца не запрещаем: запрет должен быть сказан бэкендом явно.
  assert.equal(A.supportsCustomAnswer('qwen'), true);
  assert.equal(A.hasSeparateEffort('qwen'), true);
  // Сессия без метки агента — это claude, как писали до появления второго CLI.
  assert.deepEqual(A.models(undefined).map((m) => m.id), A.models('claude').map((m) => m.id));
  assert.equal(A.title('qwen'), 'qwen', 'подпись незнакомца — он сам, а не «claude»');
});

test('кнопки запуска — только найденные в системе', () => {
  A.setAll(META_AGENTS);
  assert.deepEqual(A.present().map((a) => a.id), ['claude', 'kimi'], 'codex не найден — кнопка была бы обманом');
});

test('пустой или чужой ответ демона не оставляет панель без агентов', () => {
  A.setAll(META_AGENTS);
  A.setAll([]);
  assert.ok(A.all().length >= 3, 'пустой список стёр каталог');
  A.setAll(undefined);
  assert.ok(A.all().length >= 3, 'отсутствие поля agents стёрло каталог');
  // Старый бэкенд шлёт описание без части полей — добираем встроенным знанием.
  A.setAll([{ id: 'kimi' }]);
  assert.ok(A.models('kimi').length, 'модели kimi потерялись на скупом ответе');
  assert.equal(A.title('kimi'), 'Kimi');
  A.setAll(META_AGENTS);
});

/* --- источниковые: экраны спрашивают каталог, а не сравнивают id --- */

test('панель не выбирает модели и усилие тернарником по id', () => {
  assert.ok(!/agent === 'codex' \? MODELS_CODEX/.test(renderer), 'модели всё ещё выбираются по id');
  assert.ok(renderer.includes('AGENTS.models(agent)'), 'модели берутся не из каталога');
  assert.ok(renderer.includes('AGENTS.hasSeparateEffort('), 'палитра усилия скрывается не по умению агента');
  assert.ok(!/s\.agent === 'codex'/.test(renderer), 'в палитре остался хардкод codex');
  assert.ok(renderer.includes('AGENTS.efforts(agent)'), 'уровни усилия не берутся у агента');
});

test('кнопки запуска собираются из каталога и в одном месте', () => {
  assert.ok(renderer.includes('function launchAgents('), 'общей точки списка агентов нет');
  assert.ok(renderer.includes('AGENTS.present()'), 'в кнопки попадают агенты, которых нет в системе');
  // Дубля «две кнопки руками» быть не должно ни в одной из двух форм истории.
  assert.ok(!/btn\('claude', 'Claude'\)/.test(renderer), 'кнопка Claude всё ещё вписана руками');
  assert.ok(!/'\+ Codex'/.test(renderer), 'кнопка + Codex всё ещё вписана руками');
  assert.equal(renderer.split('function launchAgents(').length - 1, 1, 'точек сборки списка больше одной');
  assert.ok(renderer.includes('const agents = launchAgents(remote)'), '«Новый проект» собирает список сам');
  assert.ok(renderer.includes('for (const a of launchAgents(remote))'), 'экран проекта собирает список сам');
});

test('возобновление знает флаг каждого встроенного CLI', () => {
  assert.ok(/kimi: \(sid\) => `kimi -S \$\{sid\}`/.test(renderer), 'kimi нечем возобновить');
  assert.ok(renderer.includes('const builtin = BUILTIN_RESUME[id]'), 'резюме собирается лесенкой if-ов');
});

test('конструктор циклов берёт агентов и модели критика по выбранному агенту', () => {
  assert.ok(!/\['claude', 'codex'\]\.map/.test(loops), 'сегмент агентов всё ещё пара строк');
  assert.ok(loops.includes('agentIds().map('), 'сегмент агентов собирается не из каталога');
  assert.ok(loops.includes('modelsFor(criticAgent)'), 'критик получает модели чужого агента');
});
