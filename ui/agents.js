/* Каталог агентов панели: один источник правды вместо тернарников по id,
 * пары кнопок и копий списка моделей, разбросанных по экранам.
 * Описания приезжают из app_meta; до ответа демона работает встроенная копия.
 * Незнакомый агент ведёт себя как claude, а не как пустота. */
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.JarvisAgents = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  // Сессия без метки агента — claude: так писали до появления второго бэкенда.
  const DEFAULT_ID = 'claude';

  /* Зеркало backend/*.rs. present:true у всех: до ответа app_meta мы не знаем,
   * что стоит на машине, а лишняя кнопка на полсекунды лучше спрятанной. */
  const BUILTIN = [
    {
      id: 'claude', title: 'Claude',
      // Порядок зеркалит backend/mod.rs: Fable последней — по умолчанию её не
      // берут, а первая строка списка однажды будет нажата не глядя.
      models: [{ id: 'opus', name: 'Opus' }, { id: 'sonnet', name: 'Sonnet' },
        { id: 'haiku', name: 'Haiku' }, { id: 'fable', name: 'Fable' }],
      effortLevels: ['low', 'medium', 'high', 'xhigh', 'max'],
      hasSeparateEffort: true, supportsCustomAnswer: true, present: true,
    },
    {
      id: 'codex', title: 'Codex',
      models: [{ id: 'gpt-5.5', name: 'GPT-5.5' }, { id: 'gpt-5-codex', name: 'Codex' },
        { id: 'gpt-5', name: 'GPT-5' }],
      effortLevels: [], hasSeparateEffort: false, supportsCustomAnswer: false, present: true,
    },
    {
      id: 'kimi', title: 'Kimi',
      models: [{ id: 'kimi-code/k3', name: 'K3' }, { id: 'kimi-code/k3-256k', name: 'K3-256k' },
        { id: 'kimi-code/kimi-for-coding', name: 'K2.7 Coding' },
        { id: 'kimi-code/kimi-for-coding-highspeed', name: 'K2.7 Coding Highspeed' }],
      effortLevels: ['low', 'high', 'max'],
      hasSeparateEffort: true, supportsCustomAnswer: false, present: true,
    },
  ];

  const byId = (list, id) => list.find((a) => a && a.id === id) || null;

  /* Недостающие поля добираем из встроенной копии. Умения по умолчанию
   * разрешены — запрет должен быть сказан явно. */
  function normalize(a) {
    const b = byId(BUILTIN, a.id);
    const models = Array.isArray(a.models) && a.models.length
      ? a.models.filter((m) => m && m.id).map((m) => ({ id: m.id, name: m.name || m.id }))
      : (b ? b.models.slice() : []);
    const efforts = Array.isArray(a.effortLevels) ? a.effortLevels.slice() : (b ? b.effortLevels.slice() : []);
    return {
      id: a.id,
      title: a.title || (b && b.title) || a.id,
      models,
      effortLevels: efforts,
      hasSeparateEffort: a.hasSeparateEffort !== false,
      supportsCustomAnswer: a.supportsCustomAnswer !== false,
      present: a.present !== false,
    };
  }

  let catalog = BUILTIN.map(normalize);

  // Пустое/чужое не принимаем: встроенный список лучше, чем ни одного агента.
  function setAll(list) {
    if (!Array.isArray(list)) return all();
    const next = list.filter((a) => a && a.id).map(normalize);
    if (next.length) catalog = next;
    return all();
  }

  function all() { return catalog.slice(); }
  function get(id) { return byId(catalog, id || DEFAULT_ID); }
  // Неизвестный id получает поведение claude.
  function safe(id) { return get(id) || byId(catalog, DEFAULT_ID) || catalog[0] || null; }

  // Только те, кого нашли в системе, — кнопка запуска отсутствующего CLI обман.
  function present() { return catalog.filter((a) => a.present); }

  function title(id) { const a = get(id); return a ? a.title : (id || DEFAULT_ID); }
  function models(id) { const a = safe(id); return a ? a.models.slice() : []; }
  function efforts(id) { const a = safe(id); return a ? a.effortLevels.slice() : []; }
  function hasSeparateEffort(id) { const a = get(id); return a ? a.hasSeparateEffort : true; }
  function supportsCustomAnswer(id) { const a = get(id); return a ? a.supportsCustomAnswer : true; }

  return Object.freeze({
    DEFAULT_ID, BUILTIN, setAll, all, get, present,
    title, models, efforts, hasSeparateEffort, supportsCustomAnswer,
  });
});
