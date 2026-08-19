(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.JarvisQuestionAnswer = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  // Свой текст доступен не каждому агенту: у пикера codex строки «Other» нет,
  // доставить текст туда некуда. Кто умеет — говорит каталог агентов
  // (app_meta.agents), список передают снаружи: модуль остаётся чистым.
  // Незнакомый агент и сессия без метки — разрешаем, как было до меток.
  function customAllowed(agent, agents) {
    const a = (agents || []).find((x) => x && x.id === (agent || 'claude'));
    return a ? a.supportsCustomAnswer !== false : true;
  }

  // Нормализация поля «Свой ответ…»: пробельный ввод — не ответ.
  function normalizeText(raw) {
    const text = (raw || '').trim();
    return text || null;
  }

  // Выбор по текущему вопросу. multiSelect — тогглы, кастом добавляется к ним;
  // single-select — кастом приоритетен (это и есть выбор строки «Other»).
  // null = отправлять нечего (ни варианта, ни текста).
  function commitRow({ multiSelect, chosen, sel, text }) {
    const custom = normalizeText(text);
    const row = multiSelect
      ? [...chosen].sort((a, b) => a - b)
      : custom ? [] : [sel + 1];
    if (!row.length && !custom) return null;
    return { row, text: custom };
  }

  // Payload для question_answer. texts кладём только когда есть хоть один
  // кастом — без него контракт байт-в-байт старый `{answers}`.
  function buildPayload(answers, texts) {
    const payload = { answers };
    if ((texts || []).some(Boolean)) payload.texts = texts.map((t) => t || null);
    return payload;
  }

  return Object.freeze({ customAllowed, normalizeText, commitRow, buildPayload });
});
