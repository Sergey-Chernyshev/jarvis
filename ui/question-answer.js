(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.JarvisQuestionAnswer = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  // Capability comes from the observed picker or provider schema. Old payloads
  // keep the conservative Claude-only fallback until a screen is identified.
  function customAllowed(agent, item, question) {
    if (typeof item?.customAllowed === 'boolean') return item.customAllowed;
    if (question?.fromScreen) return false;
    const catalog = item?.get ? item : globalThis.JarvisAgents;
    if (catalog?.get && catalog.get(agent)?.customAnswer === false) return false;
    return agent !== 'codex' && agent !== 'kimi';
  }

  // Нормализация поля «Свой ответ…»: пробельный ввод — не ответ.
  function normalizeText(raw) {
    const text = (raw || '').trim();
    return text || null;
  }

  // Выбор по текущему вопросу. multiSelect — тогглы, кастом добавляется к ним;
  // single-select — кастом приоритетен (это и есть выбор строки «Other»).
  // null = отправлять нечего (ни варианта, ни текста).
  function commitRow({ multiSelect, chosen, sel, text, customMode, optionCount = Infinity }) {
    const custom = normalizeText(text);
    const row = multiSelect
      ? [...chosen].sort((a, b) => a - b)
      : custom && customMode !== 'notes' ? [] : (sel >= 0 && sel < optionCount ? [sel + 1] : []);
    if (!row.length && !custom) return null;
    return { row, text: custom };
  }

  // Every production submission binds choices to provider identities. The
  // legacy helper shape is retained only for callers without a Question model.
  function identity(question) {
    return { requestId: question.requestId || `legacy-${question.at}`, revision: question.revision || 0 };
  }

  let sequence = 0;
  function submissionId() {
    return globalThis.crypto?.randomUUID?.() || `answer-${Date.now()}-${++sequence}`;
  }

  function buildPayload(answers, texts, question, id = submissionId()) {
    if (question) return {
      ...identity(question), submissionId: id,
      answers: question.questions.map((item, i) => ({
        questionId: item.id || `q${i + 1}`,
        optionIds: (answers[i] || []).map(n => item.options[n - 1]?.id || `o${n}`),
        text: normalizeText(texts?.[i]),
      })),
    };
    const payload = { answers };
    if ((texts || []).some(Boolean)) payload.texts = texts.map((t) => t || null);
    return payload;
  }

  function draftKey(sessionId, question) {
    const { requestId, revision } = identity(question);
    return JSON.stringify([sessionId, requestId, revision]);
  }

  function createDraftStore() {
    const drafts = new Map();
    return {
      get(sessionId, question) {
        const key = draftKey(sessionId, question);
        if (!drafts.has(key)) {
          if (drafts.size >= 64) drafts.delete(drafts.keys().next().value);
          drafts.set(key, { answers: question.questions.map(() => []), texts: question.questions.map(() => ''),
            selections: question.questions.map(() => 0), index: 0, review: false,
            submissionId: submissionId(), message: '', unknown: false });
        }
        return drafts.get(key);
      },
      delete(sessionId, question) { drafts.delete(draftKey(sessionId, question)); },
    };
  }

  return Object.freeze({ customAllowed, normalizeText, commitRow, buildPayload, identity, submissionId, draftKey, createDraftStore });
});
