/* Стек тостов (Wispr-стиль): дот + заголовок + ×, ниже сжатый вывод модели.
 * Карточка кликабельна целиком (открыть чат), × закрывает без открытия. */

const stackEl = document.getElementById('stack');
const TTL = 8000;
const MAX_CARDS = 4;
const cards = new Map(); // id → {el, timer}
let hovering = false; // курсор над окном тостов (из нативного poll'а)

// Native geometry must be committed before motion begins. Keep the transparent
// canvas large enough for both shapes while a pill grows into its result card.
const retiringCards = new Map();
const cardMotions = new WeakMap();
let lastReportedHeight = -1;
let lastResize = Promise.resolve();
const reducedMotion = () => !!window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
const canAnimate = el => typeof el.animate === 'function' && typeof window.getComputedStyle === 'function';
const stackHeight = () => stackEl.children.length ? Math.min(480, Math.ceil(stackEl.scrollHeight)) : 0;

function reportHeight(reserved = 0) {
  const height = Math.max(reserved, stackHeight());
  if (height === lastReportedHeight) return lastResize;
  lastReportedHeight = height;
  // All deduplicated callers share the handled promise, including failures.
  lastResize = lastResize.catch(() => {}).then(() => window.toast.resize(height)).catch(() => {
    if (lastReportedHeight === height) lastReportedHeight = -1;
  });
  // IPC failure must never leave an otherwise usable card permanently invisible.
  return lastResize;
}

function stopMotion(card) {
  const motion = cardMotions.get(card);
  if (!motion) return;
  cardMotions.delete(card);
  clearTimeout(motion.fallback);
  for (const animation of motion.animations) animation.cancel();
  card.querySelector('.card-ghost')?.remove();
  card.style.width = ''; card.style.height = ''; card.style.overflow = '';
  const content = card.querySelector('.card-content');
  if (content) content.style.opacity = '';
}

function captureCard(card) {
  if (!canAnimate(card)) return null;
  const rect = card.getBoundingClientRect();
  const style = window.getComputedStyle(card);
  const content = card.querySelector('.card-content')?.cloneNode(true);
  if (content) content.style.opacity = '';
  const before = { width: rect.width, height: rect.height, radius: style.borderRadius, content,
    contentWidth: rect.width - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight) - 2 };
  stopMotion(card);
  return before;
}

function liveContent(card) {
  let content = card.querySelector('.card-content');
  if (content) return content;
  content = document.createElement('div'); content.className = 'card-content';
  while (card.firstChild) content.appendChild(card.removeChild(card.firstChild));
  card.appendChild(content);
  return content;
}

function presentCard(card, before = null) {
  if (!canAnimate(card)) { card.classList.add('in'); reportHeight(); return; }
  const content = liveContent(card);
  const after = card.getBoundingClientRect();
  const radius = window.getComputedStyle(card).borderRadius;
  const motion = { animations: [], fallback: null };
  cardMotions.set(card, motion);
  const morph = before && before.width > 0 && before.height > 0 && !reducedMotion();
  const reserved = Math.min(480, stackHeight() + (morph ? Math.max(0, before.height - after.height) : 0));
  let ghost;
  if (morph) {
    card.style.width = `${before.width}px`; card.style.height = `${before.height}px`;
    card.style.overflow = 'hidden'; content.style.opacity = '0';
    if (before.content) {
      ghost = document.createElement('div'); ghost.className = 'card-ghost';
      ghost.setAttribute('aria-hidden', 'true'); ghost.setAttribute('inert', '');
      before.content.style.width = `${before.contentWidth}px`;
      ghost.appendChild(before.content); card.appendChild(ghost);
    }
  }
  reportHeight(reserved).then(() => {
    if (cardMotions.get(card) !== motion || !card.isConnected) return;
    const firstTime = !card.classList.contains('in');
    card.classList.remove('out'); card.classList.add('in');
    const finish = () => {
      if (cardMotions.get(card) !== motion) return;
      stopMotion(card); reportHeight();
    };
    if (reducedMotion()) { finish(); return; }
    const animate = (el, frames, options) => {
      const animation = el.animate(frames, options);
      animation.finished.catch(() => {}); // interruption is expected for every crossfade layer
      motion.animations.push(animation); return animation;
    };
    const ease = 'cubic-bezier(.22, 1, .36, 1)';
    let main;
    if (morph) {
      main = animate(card, [
        { width: `${before.width}px`, height: `${before.height}px`, borderRadius: before.radius },
        { width: `${after.width}px`, height: `${after.height}px`, borderRadius: radius },
      ], { duration: 340, easing: ease, fill: 'forwards' });
      animate(content, [{ opacity: 0, transform: 'translateY(3px)' }, { opacity: 1, transform: 'translateY(0)' }],
        { duration: 220, delay: 70, easing: 'ease-out', fill: 'forwards' });
      if (ghost) animate(ghost, [{ opacity: 1 }, { opacity: 0 }], { duration: 130, easing: 'ease-out', fill: 'forwards' });
    } else if (firstTime) {
      main = animate(card, [{ opacity: 0, transform: 'translateY(8px) scale(.985)' }, { opacity: 1, transform: 'translateY(0) scale(1)' }],
        { duration: 280, easing: ease });
    }
    if (!main) { finish(); return; }
    main.finished.then(finish, () => {});
    // A non-key WKWebView can pause its compositor after a Space change. The
    // settled state must remain usable even if animation.finished is delayed.
    motion.fallback = setTimeout(finish, morph ? 430 : 370);
  });
}

function existingCard(id) {
  const active = cards.get(id);
  if (active) return active;
  const retiring = retiringCards.get(id);
  if (!retiring) return null;
  retiringCards.delete(id); stopMotion(retiring.el);
  retiring.el.classList.remove('out'); retiring.el.classList.add('in');
  cards.set(id, retiring);
  return retiring;
}

/* ── тело карточки: хвост не теряется молча ───────────────────────────────
 * `.body` в toast.html обрезан клампом на шести строках, и длинный ответ
 * заканчивался ничем — как обрезанный транскрипт, только без предупреждения.
 * Ширина окна тостов фиксирована (440px), так что верхнюю границу знаков в
 * строке назвать можно; берём её с запасом, чтобы пометка означала «хвост
 * точно не влез», а не «наверное». */
const BODY_LINES = 6;
const BODY_COLS = 60;
const MORE_NOTE = 'показан не весь текст — нажми, чтобы дочитать';

function bodyClipped(text) {
  let lines = 0;
  for (const src of String(text || '').split('\n')) {
    lines += Math.max(1, Math.ceil(src.length / BODY_COLS));
    if (lines > BODY_LINES) return true;
  }
  return false;
}

// Проставить текст тела и пометку про обрезанный хвост (создав узлы, если надо).
function setBody(card, text) {
  let body = card.querySelector('.body');
  if (!body) {
    body = document.createElement('div');
    body.className = 'body';
    card.appendChild(body);
  }
  body.textContent = text || '';
  const more = card.querySelector('.bmore');
  if (!bodyClipped(text)) {
    if (more) more.remove();
    return;
  }
  if (more) return;
  const note = document.createElement('div');
  note.className = 'bmore';
  note.textContent = MORE_NOTE;
  // сразу под телом; при первой сборке карточки тело ещё последнее — в конец
  if (body.parentNode && body.nextSibling) body.parentNode.insertBefore(note, body.nextSibling);
  else card.appendChild(note);
}

function removeCard(id, instant) {
  const c = cards.get(id);
  if (!c) return;
  cards.delete(id); clearTimeout(c.timer); stopMotion(c.el);
  const finish = () => {
    if (retiringCards.get(id) !== c) return;
    retiringCards.delete(id); stopMotion(c.el); c.el.remove(); reportHeight();
  };
  retiringCards.set(id, c);
  if (instant || reducedMotion()) { finish(); return; }
  c.el.classList.add('out');
  if (!canAnimate(c.el)) { setTimeout(finish, 190); return; }
  const animation = c.el.animate([
    { opacity: 1, transform: 'translateY(0) scale(1)' },
    { opacity: 0, transform: 'translateY(5px) scale(.99)' },
  ], { duration: 180, easing: 'cubic-bezier(.4, 0, 1, 1)', fill: 'forwards' });
  const motion = { animations: [animation], fallback: setTimeout(finish, 260) };
  cardMotions.set(c.el, motion);
  animation.finished.then(finish, () => {});
}

// кольцо стартует заново вместе с таймером — они всегда в фазе
function restartRing(el) {
  const fg = el.querySelector('.ring .fg');
  if (!fg) return;
  fg.style.animation = 'none';
  void fg.getBoundingClientRect(); // reflow — сбросить анимацию
  fg.style.animation = '';
}

function armTimer(id) {
  const c = cards.get(id);
  if (!c) return;
  clearTimeout(c.timer);
  // вопрос — «липкая» карточка: ждёт твой выбор, по таймеру не исчезает
  if (c.sticky) return;
  // читаешь (курсор над стеком) — карточка замирает, кольцо на паузе
  if (hovering) {
    c.el.classList.add('paused');
    return;
  }
  c.el.classList.remove('paused');
  restartRing(c.el);
  // `0` — явный пользовательский выбор «Не прятать», а не отсутствие TTL.
  if (c.ttl === 0) return;
  c.timer = setTimeout(() => removeCard(id), c.ttl);
}

/* ── доставка ответа: отказ виден человеку ────────────────────────────────
 * Ответ на вопрос и «Продолжить» уходят в пану tmux, а она к этому моменту
 * часто мертва (ноут проснулся, терминал закрыли) — ровно ради этого случая
 * кнопка и существует. Раньше карточка исчезала одинаково при успехе и при
 * {ok:false}, и человек был уверен, что ответил. Теперь отказ остаётся на
 * экране причиной, а карточка становится липкой — уйдёт только по ✕. */
function sendFailText(res) {
  if (!res) return 'Не удалось отправить — Jarvis не ответил';
  if (res.error) return String(res.error);
  if (res.needsTmux) {
    return 'Сессия вне tmux — ответь в терминале' + (res.resumeCmd ? `: ${res.resumeCmd}` : '');
  }
  return 'Не удалось отправить';
}

function showSendError(id, text) {
  const c = cards.get(id);
  if (!c) return;
  clearTimeout(c.timer);
  c.sticky = true; // причину нельзя прятать по таймеру: её ещё не прочитали
  c.el.classList.add('sticky');
  let err = c.el.querySelector('.derr');
  if (!err) {
    err = document.createElement('div');
    err.className = 'derr';
    c.el.appendChild(err);
  }
  err.textContent = text;
  reportHeight();
}

// общий путь «кликнул вариант / Продолжить»: снимаем карточку только на успехе
function sendThen(p, id, opts) {
  const o = opts || {};
  Promise.resolve(p).then((res) => {
    if (res && res.ok === false) {
      showSendError(id, sendFailText(res));
      if (o.onFail) o.onFail();
      return;
    }
    if (!o.keep) removeCard(id);
  }).catch((e) => {
    showSendError(id, sendFailText({ error: (e && e.message) || String(e) }));
    if (o.onFail) o.onFail();
  });
}

// карточка под курсором по y (DOM-координата из нативного поллинга) → .hot
function markHot(y) {
  for (const [, c] of cards) {
    const r = c.el.getBoundingClientRect();
    c.el.classList.toggle('hot', y >= r.top && y < r.bottom);
  }
}

// нативный hover: курсор над стеком ставит на паузу ВЕСЬ стек (ничего не
// исчезнет, пока читаешь), но подсветку и ✕ держим только на карточке под
// курсором — иначе непонятно, на какую именно наведено.
window.toast.onHover((h) => {
  const over = !!(h && h.over);
  hovering = over;
  if (!over) {
    for (const [id, c] of cards) {
      c.el.classList.remove('hot');
      armTimer(id); // заново с полного TTL — кольцо стартует с нуля
    }
    return;
  }
  for (const [, c] of cards) {
    clearTimeout(c.timer);
    c.el.classList.add('paused');
  }
  markHot(h.y);
});

window.toast.onAdd((d) => {
  // дедуп: карточка с таким id уже на экране (стабильный id «done-<sid>» и т.п.)
  // — обновляем её на месте, а не плодим вторую «одна за другой»
  const existing = existingCard(d.id);
  if (existing) {
    const before = captureCard(existing.el);
    const t = existing.el.querySelector('.title');
    if (t) t.textContent = d.title || '';
    updateBody(existing.el, d.body);
    armTimer(d.id); // таймер заново — карточка «обновилась»
    presentCard(existing.el, before);
    return;
  }

  if (cards.size >= MAX_CARDS) {
    evictForRoom(); // самая старая НЕ-липкая — мгновенно (не сносим пикер/стейдж/мик)
  }

  // карточка смены режима: компактная, по центру, с «поп»-анимацией, живёт недолго
  const isMode = d.kind === 'mode';
  const configuredTtl = Number.isFinite(d.ttlMs) && d.ttlMs >= 0 ? d.ttlMs : TTL;
  const ttl = isMode ? 1900 : configuredTtl;
  let sticky = false; // вопрос — «липкая» карточка (не исчезает по таймеру)

  const card = document.createElement('div');
  card.className = `card${isMode ? ' mode' : ''}`;
  card.style.setProperty('--ttl', `${ttl}ms`);

  const crow = document.createElement('div');
  crow.className = 'crow';

  const title = document.createElement('div');
  title.className = 'title';
  title.textContent = d.title || '';

  if (isMode) {
    crow.append(title);
    card.appendChild(crow);
  } else {
    const dot = document.createElement('span');
    dot.className = `dot${d.kind === 'waiting' ? ' waiting' : ''}`;

    const close = document.createElement('button');
    close.className = 'close';
    close.title = 'Скрыть';
    close.setAttribute('aria-label', 'Скрыть уведомление');
    // кольцо-таймер вокруг ✕ (SVG: подложка + стекающая дуга)
    const SVG = 'http://www.w3.org/2000/svg';
    const ring = document.createElementNS(SVG, 'svg');
    ring.setAttribute('class', 'ring');
    ring.setAttribute('viewBox', '0 0 26 26');
    for (const cls of ['track', 'fg']) {
      const c = document.createElementNS(SVG, 'circle');
      c.setAttribute('class', cls);
      c.setAttribute('cx', '13');
      c.setAttribute('cy', '13');
      c.setAttribute('r', '11.5');
      ring.appendChild(c);
    }
    const x = document.createElement('span');
    x.textContent = '✕';
    close.append(ring, x);
    close.addEventListener('click', (e) => {
      e.stopPropagation();
      removeCard(d.id);
    });

    crow.append(dot, title, close);
    card.appendChild(crow);

    // мета-строка (ветка/модель/усилие/время) — состав задаётся настройками,
    // демон присылает готовые сегменты d.meta = [{kind,text}, …]
    if (Array.isArray(d.meta) && d.meta.length) {
      const meta = document.createElement('div');
      meta.className = 'meta';
      d.meta.forEach((seg, i) => {
        if (i > 0) {
          const sp = document.createElement('span');
          sp.className = 'msp';
          sp.textContent = '·';
          meta.appendChild(sp);
        }
        const s = document.createElement('span');
        s.className = 'mseg ' + (seg && seg.kind ? seg.kind : 'plain');
        s.textContent = (seg && seg.text) || '';
        meta.appendChild(s);
      });
      card.appendChild(meta);
    }

    if (d.body) setBody(card, d.body);

    // варианты вопроса (AskUserQuestion). Payload плоский: первый вопрос +
    // count. Инлайн-чипы — только для одиночного вопроса; мульти-вопрос
    // отвечается в приложении (визард).
    const qq = d.question || null;
    const count = qq && typeof qq.count === 'number' ? qq.count : (qq && qq.options ? 1 : 0);
    const opts = qq && Array.isArray(qq.options) ? qq.options : null;
    if (count > 1 || (count > 0 && !opts?.length) || qq?.multiSelect || ['external', 'codex-rpc'].includes(qq?.transport)) {
      sticky = true;
      card.classList.add('sticky');
      const note = document.createElement('div');
      note.className = 'body';
      note.textContent = count > 1 ? `Вопросов: ${count} · открой, чтобы выбрать и проверить ответы` : 'Открой вопрос, чтобы выбрать варианты и написать ответ';
      card.appendChild(note);
    } else if (opts && opts.length) {
      sticky = true; // ждём выбор — карточка не тикает по TTL
      card.classList.add('sticky');
      const list = document.createElement('div');
      list.className = 'opts';
      opts.slice(0, 9).forEach((o, i) => {
        const opt = document.createElement('div');
        opt.className = 'opt';
        const num = document.createElement('span');
        num.className = 'num';
        const key = document.createElement('span');
        key.className = 'key';
        // подпись модификаторов — под ОС (⌘⌥ на маке, Ctrl+Alt на Linux)
        key.textContent = window.jarvisKeys
          ? window.jarvisKeys.MOD + window.jarvisKeys.SEP + window.jarvisKeys.ALT + window.jarvisKeys.SEP
          : '';
        num.append(key, document.createTextNode(String(i + 1)));
        const otext = document.createElement('div');
        otext.className = 'otext';
        const ol = document.createElement('div');
        ol.className = 'olabel';
        ol.textContent = o.label || '';
        otext.appendChild(ol);
        if (o.description) {
          const od = document.createElement('div');
          od.className = 'odesc';
          od.textContent = o.description;
          otext.appendChild(od);
        }
        opt.append(num, otext);
        let pendingAnswer = false;
        opt.addEventListener('click', async (e) => {
          e.stopPropagation();
          if (pendingAnswer || list.dataset.sending === 'true') return;
          pendingAnswer = true; list.dataset.sending = 'true';
          const status = document.createElement('div'); status.className = 'body'; status.setAttribute('role', 'status');
          status.textContent = 'Отправляю · жду подтверждения…'; list.appendChild(status);
          const submissionId = globalThis.crypto?.randomUUID?.() || `toast-${Date.now()}-${i}`;
          try {
            const result = await window.toast.answerQuestion(d.sessionId, {
              requestId: qq.requestId, revision: qq.revision, submissionId, answers: [[i + 1]],
            });
            if (result.ok && result.delivery === 'confirmed') removeCard(d.id);
            else {
              status.textContent = result.error || 'Проверь ответ в приложении агента';
              if (result.delivery !== 'unknown' && result.delivery !== 'sending' && !result.ok) {
                pendingAnswer = false; delete list.dataset.sending;
              }
            }
          } catch (error) { status.textContent = 'Связь прервалась. Ответ мог дойти — проверь терминал.'; }
        });
        list.appendChild(opt);
      });
      card.appendChild(list);
    }

    // действие «Продолжить» — только для застрявших сессий (ждёт / лимит /
    // оборвалась, напр. сном), но НЕ для нормально завершённых (done) и НЕ для
    // вопросов (там действие — выбрать вариант, «Продолжить» не к месту).
    if (d.sessionId && d.kind !== 'done' && !(count > 0)) {
      const cont = document.createElement('button');
      cont.className = 'cont';
      cont.textContent = 'Продолжить';
      cont.addEventListener('click', (e) => {
        e.stopPropagation();
        cont.disabled = true;
        cont.textContent = 'Отправляю…';
        sendThen(window.toast.continueSession(d.sessionId), d.id, {
          onFail: () => { cont.disabled = false; cont.textContent = 'Повторить'; },
        });
      });
      card.appendChild(cont);
    }

    card.addEventListener('click', () => {
      window.toast.click(d.sessionId || null);
      removeCard(d.id);
    });
  }

  stackEl.appendChild(card); // новые — снизу, старые поднимаются
  cards.set(d.id, { el: card, timer: null, ttl, sticky });
  armTimer(d.id);

  presentCard(card);
});

// вопрос ответили (хоткеем/панелью/в терминале) → снять «липкую» карточку
window.toast.onRemove((d) => removeCard(d.id));

// голос говорит эту карточку → держим её (не закрываем по TTL, пока речь идёт)
window.toast.onHold((d) => {
  const c = cards.get(d.id);
  if (c) clearTimeout(c.timer);
});

// речь закончилась → карточка живёт ещё d.ms (≈3.5с), кольцо стекает за это время
window.toast.onExtend((d) => {
  const c = cards.get(d.id);
  if (!c) return;
  clearTimeout(c.timer);
  if (hovering) { c.el.classList.add('paused'); return; } // под курсором не тикаем
  c.el.classList.remove('paused');
  // После озвучки отсчёт начинается заново, но длительность остаётся той,
  // которую пользователь выбрал в notify.ttlSec (включая `0`).
  const ms = Number.isFinite(c.ttl)
    ? c.ttl
    : (Number.isFinite(d.ms) && d.ms >= 0 ? d.ms : 3500);
  c.el.style.setProperty('--ttl', `${ms}ms`);
  if (ms === 0) return;
  restartRing(c.el);
  c.timer = setTimeout(() => removeCard(d.id), ms);
});

// A delayed body may arrive after a title-only notification.
function updateBody(card, text) {
  let body = card.querySelector('.body');
  if (!body && text) {
    body = document.createElement('div'); body.className = 'body';
    (card.querySelector('.card-content') || card).appendChild(body);
  }
  if (body) { body.textContent = text || ''; if (!text) body.remove(); }
}
window.toast.onUpdate((d) => {
  const c = cards.get(d.id);
  if (!c) return;
  const before = captureCard(c.el);
  updateBody(c.el, d.body); armTimer(d.id); presentCard(c.el, before);
});

/* ===================== голосовая маршрутизация (HUD) ===================== */

// Терминальные фазы исчезают по TTL; промежуточные/интерактивные — «липкие».
// 'heard' (итог диктовки) — терминальная: уходит сама по TTL (как остальные
// уведомления), а не висит вечно до крестика. Под курсором пауза — успеть прочесть/кликнуть.
const VOICE_TERMINAL = new Set(['sent', 'cancelled', 'empty', 'nosessions', 'error', 'reply', 'heard']);
// Фазы, где разговор УЖЕ завершён — × просто закрывает карточку, без abort и без
// «Отмена». ВАЖНО: 'reply' тут НЕТ — пока Джарвис ОЗВУЧИВАЕТ ответ, крестик должен
// оборвать речь и завершить разговор (RC1), а не молча спрятать карточку.
const VOICE_FINISHED = new Set(['sent', 'cancelled', 'empty', 'nosessions', 'error', 'dismiss', 'heard']);

// Освободить место под новую карточку, НЕ трогая «липкие» (пикер/стейдж/мик/
// вопрос — интерактивные, должны выжить). Если все липкие — не вытесняем: стек
// кратко превысит MAX_CARDS, высота всё равно ограничена reportHeight (F3).
function evictForRoom() {
  for (const [cid, c] of cards) { // порядок Map = старые первыми
    if (!c.sticky) { removeCard(cid, true); return; }
  }
}

// Кнопка ✕ для HUD = «стоп всё»: снять конкретное действие (staged/picker/
// confirm), оборвать озвучку и ЗАВЕРШИТЬ разговор (перестать слушать).
function voiceClose(p) {
  const close = document.createElement('button');
  close.className = 'close';
  close.title = VOICE_FINISHED.has(p.phase) ? 'Скрыть' : 'Стоп';
  close.setAttribute('aria-label', VOICE_FINISHED.has(p.phase) ? 'Скрыть уведомление' : 'Остановить голосовой ввод');
  const x = document.createElement('span');
  x.textContent = '✕';
  close.appendChild(x);
  close.addEventListener('click', (e) => {
    e.stopPropagation();
    if (p.phase === 'staged') window.toast.voiceCancel(p.nonce);
    else if (p.phase === 'picker') window.toast.voicePick(p.nonce, null);
    else if (p.phase === 'confirm') window.toast.voiceConfirm(p.nonce, false);
    // На ЗАВЕРШЁННЫХ фазах (Отменено/Отправлено/Ошибка/…) абортить НЕЧЕГО —
    // разговор уже окончен. voiceAbort там СНОВА эмитит Cancelled → новый тост →
    // бесконечный «Отменено». Закрываем карточку локально. НО на активных фазах
    // (Слушаю/Думаю/Reply/staged/picker/confirm) × = «стоп всё»: рвём речь и
    // завершаем разговор — в т.ч. пока Джарвис ГОВОРИТ ответ (RC1).
    if (!VOICE_FINISHED.has(p.phase)) {
      window.toast.voiceAbort(); // оборвать речь + закончить разговор/слушание
    }
    removeCard(p.id);
  });
  return close;
}

// Единственная HUD-карточка (стабильный id «voice-hud»). Контент перестраивается
// на каждую фазу (свежие обработчики), но УЗЕЛ переиспользуется — без мигания и
// повторной slide-in анимации между фазами (F2).
function renderVoiceHud(p) {
  if (!p || !p.id) return;
  // «dismiss» — естественный конец разговора: тихо убрать карточку, без «Отмена» (RC3).
  if (p.phase === 'dismiss') { removeCard(p.id); return; }
  const id = p.id;
  const permissionBlocked = p.phase === 'heard' && p.insertionBlocked === 'accessibility'
    && !p.inserted && !p.pasteSent && !p.insertionCancelled;
  const terminal = VOICE_TERMINAL.has(p.phase) && !permissionBlocked;
  // «Услышал»: вставка подтверждена (текст уже в поле) — короткие 2с;
  // не подтверждена — 5с, успеть прочитать/скопировать/кликнуть в историю.
  const ttl = p.phase === 'heard' ? (p.inserted ? 2000 : 5000) : 4200;

  const existing = existingCard(id);
  // Listening sends elapsed-time updates. Keep its waveform node and phase
  // clock running instead of crossfading the whole HUD every second.
  if (existing && existing.el.dataset.phase === p.phase
      && ['listening', 'thinking', 'analyzing', 'transcribing'].includes(p.phase)
      && (existing.el.querySelector('.body')?.textContent || '') === (p.body || '')) {
    const title = existing.el.querySelector('.title');
    if (title) title.textContent = p.title || '';
    return;
  }
  const before = existing ? captureCard(existing.el) : null;
  const firstTime = !existing;
  let card;
  if (existing) {
    card = existing.el;
    clearTimeout(existing.timer);
    while (card.firstChild) card.removeChild(card.firstChild); // сброс контента/обработчиков
  } else {
    card = document.createElement('div');
    card.className = 'card voice';
  }
  card.style.setProperty('--ttl', `${ttl}ms`);
  card.dataset.phase = p.phase;
  card.dataset.attemptId = String(p.insertionAttemptId || '');
  card.setAttribute('role', 'status');
  card.setAttribute('aria-live', 'polite');
  // узел переиспользуется между фазами — сбрасываем клик/курсор/подсказку прошлой фазы
  card.onclick = null;
  card.style.cursor = '';
  card.title = '';

  const crow = document.createElement('div');
  crow.className = 'crow';
  const dot = document.createElement('span');
  dot.className = 'dot' + (['staged', 'picker', 'confirm'].includes(p.phase) ? ' waiting' : '');
  const title = document.createElement('div');
  title.className = 'title';
  // staged: показываем КУДА уйдёт промпт — это и есть смысл окна отмены (VR-2)
  if (p.phase === 'staged' && p.label) title.textContent = `Отправлю → ${p.label}`;
  else title.textContent = p.title || '';
  if (['listening', 'thinking', 'analyzing', 'transcribing'].includes(p.phase)) {
    const wave = document.createElement('span'); wave.className = 'hud-wave'; wave.setAttribute('aria-hidden', 'true');
    for (let i = 0; i < 5; i++) wave.appendChild(document.createElement('i'));
    crow.append(wave, title, voiceClose(p));
  } else crow.append(dot, title, voiceClose(p));
  card.appendChild(crow);

  if (permissionBlocked) {
    const permission = document.createElement('section'); permission.className = 'hud-permission';
    permission.setAttribute('aria-label', 'Разрешение автоматической вставки');
    const heading = document.createElement('div'); heading.className = 'hud-permission-title';
    heading.textContent = 'Для вставки нужен Универсальный доступ';
    const help = document.createElement('div'); help.className = 'hud-permission-help';
    help.textContent = 'Текст готов. Разреши Jarvis доступ в настройках macOS или скопируй текст вручную.';
    const actions = document.createElement('div'); actions.className = 'hud-permission-actions';
    const allow = document.createElement('button'); allow.className = 'cont hud-permission-allow';
    allow.textContent = 'Разрешить вставку';
    allow.addEventListener('click', async event => {
      event.stopPropagation(); if (allow.disabled) return; allow.disabled = true;
      try {
        if (typeof window.toast.systemAccessibilitySettings !== 'function') throw new Error();
        const result = await window.toast.systemAccessibilitySettings();
        if (result?.ok === false) throw new Error();
        if (card.dataset.attemptId !== String(p.insertionAttemptId || '')) return;
        help.textContent = 'Включи Jarvis в «Универсальном доступе». Этот текст остаётся здесь: скопируй и вставь его в нужное поле.';
      } catch (_) { help.textContent = 'Не удалось открыть настройки. Открой macOS → Конфиденциальность и безопасность → Универсальный доступ.'; }
      finally { allow.disabled = false; }
    });
    const cancel = document.createElement('button'); cancel.className = 'cont hud-permission-cancel';
    cancel.textContent = 'Отменить автовставку';
    cancel.addEventListener('click', async event => {
      event.stopPropagation(); if (cancel.disabled) return; cancel.disabled = true;
      try {
        if (!Number.isSafeInteger(p.insertionAttemptId) || typeof window.toast.dictationCancelInsertion !== 'function') throw new Error();
        const result = await window.toast.dictationCancelInsertion(p.insertionAttemptId);
        if (result?.ok !== true || result.cancelled !== true) throw new Error();
        if (card.dataset.attemptId !== String(p.insertionAttemptId)) return;
        heading.textContent = 'Автовставка отменена';
        help.textContent = 'Текст сохранён в этой карточке. Его можно скопировать; следующая диктовка работает как обычно.';
        actions.remove(); permission.dataset.cancelled = 'true';
      } catch (_) { help.textContent = 'Не удалось подтвердить отмену. Текст остаётся доступен для копирования.'; cancel.disabled = false; }
    });
    actions.append(allow, cancel); permission.append(heading, help, actions); card.appendChild(permission);
  }

  if (p.body) {
    if (p.phase === 'heard') {
      const label = document.createElement('div'); label.className = 'hud-transcript-label';
      label.textContent = p.formatted ? 'Отформатировано' : 'Распознано'; card.appendChild(label);
    }
    const body = document.createElement('div');
    body.className = p.phase === 'heard' ? 'body hud-transcript' : 'body';
    body.textContent = p.phase === 'heard' && typeof p.full === 'string' ? p.full : p.body;
    card.appendChild(body);
  }

  if (p.phase === 'staged') {
    const cancel = document.createElement('button');
    cancel.className = 'cont';
    cancel.textContent = p.secs ? `Отменить (${p.secs}с)` : 'Отменить';
    cancel.addEventListener('click', (e) => {
      e.stopPropagation();
      window.toast.voiceCancel(p.nonce);
    });
    card.appendChild(cancel);
  } else if (p.phase === 'picker') {
    const opts = Array.isArray(p.options) ? p.options : [];
    const list = document.createElement('div');
    list.className = 'opts';
    opts.slice(0, 9).forEach((o, i) => {
      const opt = document.createElement('div');
      opt.className = 'opt';
      const num = document.createElement('span');
      num.className = 'num';
      num.textContent = String(i + 1);
      const otext = document.createElement('div');
      otext.className = 'otext';
      const ol = document.createElement('div');
      ol.className = 'olabel';
      ol.textContent = o.label || o.sessionId || '';
      otext.appendChild(ol);
      opt.append(num, otext);
      opt.addEventListener('click', (e) => {
        e.stopPropagation();
        window.toast.voicePick(p.nonce, o.sessionId);
      });
      list.appendChild(opt);
    });
    card.appendChild(list);
    const cancel = document.createElement('button');
    cancel.className = 'cont';
    cancel.textContent = 'Отмена';
    cancel.addEventListener('click', (e) => {
      e.stopPropagation();
      window.toast.voicePick(p.nonce, null);
    });
    card.appendChild(cancel);
  } else if (p.phase === 'confirm') {
    const yes = document.createElement('button');
    yes.className = 'cont';
    yes.textContent = 'Да';
    yes.addEventListener('click', (e) => {
      e.stopPropagation();
      window.toast.voiceConfirm(p.nonce, true);
    });
    const no = document.createElement('button');
    no.className = 'cont';
    no.textContent = 'Отмена';
    no.addEventListener('click', (e) => {
      e.stopPropagation();
      window.toast.voiceConfirm(p.nonce, false);
    });
    card.append(yes, no);
  } else if (p.phase === 'heard') {
    const recovery = document.createElement('div'); recovery.className = 'hud-recovery';
    recovery.setAttribute('aria-label', 'Состояние доставки текста');
    recovery.textContent = p.insertionCancelled ? 'Автовставка отменена. Текст доступен для копирования.' : p.inserted ? 'Текст вставлен' : p.pasteSent ? 'Команда вставки отправлена. Если текст не появился, скопируй его.' : p.copied ? 'Текст скопирован в буфер обмена.' : p.saved ? 'Текст сохранён в истории диктовки.' : 'Скопируй текст перед закрытием уведомления.';
    if (p.insertionError && !permissionBlocked && !p.pasteSent && !p.inserted) recovery.textContent += ' ' + p.insertionError;
    card.appendChild(recovery);
    // Надиктовка завершена. Кнопка ручного копирования — на случай, если
    // автоматическая вставка/копия не сработала (полный текст из p.full).
    const copy = document.createElement('button');
    copy.className = 'cont';
    copy.textContent = 'Копировать';
    copy.addEventListener('click', async (e) => {
      e.stopPropagation(); copy.disabled = true;
      try { await window.toast.copy(p.full || p.body || ''); copy.textContent = 'Скопировано'; }
      catch (_) { copy.textContent = 'Повторить копирование'; }
      finally { copy.disabled = false; }
    });
    card.appendChild(copy);
    if (typeof p.rawText === 'string' && p.rawText !== (p.full || p.body || '')) {
      const raw = document.createElement('details'); raw.className = 'hud-raw';
      const summary = document.createElement('summary'); summary.textContent = 'Исходное распознавание';
      const original = document.createElement('div'); original.className = 'hud-raw-text'; original.textContent = p.rawText;
      const rawCopy = document.createElement('button'); rawCopy.className = 'hud-raw-copy'; rawCopy.textContent = 'Копировать исходный текст';
      rawCopy.addEventListener('click', async event => { event.stopPropagation();
        try { await window.toast.copy(p.rawText); rawCopy.textContent = 'Скопировано'; }
        catch (_) { rawCopy.textContent = 'Повторить копирование'; }
      });
      raw.addEventListener('click', event => event.stopPropagation());
      raw.addEventListener('toggle', () => reportHeight());
      raw.append(summary, original, rawCopy); card.appendChild(raw);
    }
    // Клик по карточке → открыть «Историю голоса» (× и кнопка копирования
    // гасят всплытие, так что не конфликтуют).
    card.style.cursor = 'pointer';
    card.title = 'Открыть историю голоса';
    card.onclick = event => { if (event.target.closest?.('button, details, .hud-permission, .hud-transcript')) return; try { window.toast.openVoiceHistory(); } catch {} };
  }

  if (firstTime) {
    if (cards.size >= MAX_CARDS) evictForRoom();
    stackEl.appendChild(card);
  }
  cards.set(id, { el: card, timer: null, ttl, sticky: !terminal });
  armTimer(id); // терминальные — тикают к TTL; липкие — ждут следующую фазу
  presentCard(card, before);
}

window.toast.onVoiceHud(renderVoiceHud);

/* ============ индикатор «слышу тебя» / «тихо» (фикс «ничего не вижу») ============ */

// Стабильная карточка состояния микрофона. Появляется ТОЛЬКО когда есть что
// сказать (мик молчит / нет доступа / нет устройства) — иначе снимается. Дешёвый
// фикс UX-1: детект уже есть в hub.rs, его просто не показывали.
const MIC_ID = 'voice-mic';
function renderMicState(s) {
  if (!s) return;
  const denied = s.state === 'denied';
  const noDevice = s.state === 'no-device';
  const pending = s.state === 'permission-pending';
  const starting = s.state === 'starting';
  const silent = !!s.mic_silent && !s.muted && s.state === 'listening';
  if (s.muted || (!denied && !noDevice && !pending && !starting && !silent)) {
    removeCard(MIC_ID, true);
    return;
  }
  const existing = existingCard(MIC_ID);
  const before = existing ? captureCard(existing.el) : null;
  const firstTime = !existing;
  let card;
  if (existing) {
    card = existing.el;
    while (card.firstChild) card.removeChild(card.firstChild);
  } else {
    card = document.createElement('div');
    card.className = 'card voice mic';
  }
  const crow = document.createElement('div');
  crow.className = 'crow';
  const dot = document.createElement('span');
  dot.className = 'dot waiting';
  const title = document.createElement('div');
  title.className = 'title';
  title.textContent = pending ? 'Ожидаем разрешения на микрофон'
    : starting ? 'Подключаем микрофон…'
    : denied ? 'Нет доступа к микрофону'
    : noDevice
      ? 'Микрофон не найден'
      : 'С микрофона не поступает звук';
  const close = document.createElement('button');
  close.className = 'close';
  close.title = 'Скрыть';
  close.setAttribute('aria-label', 'Скрыть уведомление');
  const x = document.createElement('span');
  x.textContent = '✕';
  close.appendChild(x);
  close.addEventListener('click', (e) => { e.stopPropagation(); removeCard(MIC_ID); });
  crow.append(dot, title, close);
  card.appendChild(crow);
  if (pending || denied || silent) {
    const hint = document.createElement('div'); hint.className = 'subtitle';
    hint.textContent = pending ? 'Ответьте на системный запрос macOS. Окно Jarvis остаётся доступным.'
      : denied ? 'Разрешите Jarvis доступ: Системные настройки → Конфиденциальность → Микрофон.'
      : 'Проверьте выбранное устройство и его выключатель звука в настройках голосового ввода.';
    card.appendChild(hint);
  }
  if (firstTime) {
    if (cards.size >= MAX_CARDS) evictForRoom();
    stackEl.appendChild(card);
  }
  cards.set(MIC_ID, { el: card, timer: null, ttl: TTL, sticky: true });
  presentCard(card, before);
}

window.toast.onAudioState(renderMicState);
// дотянуть текущее состояние на загрузке: audio_state эмитится лишь на изменении,
// ранний denied/«нет устройства» мог уйти до регистрации слушателя (VR-3)
if (window.toast.audioState) window.toast.audioState().then(renderMicState).catch(() => {});


// A recording remains visible outside the main window. Its Stop button affects
// only the meeting; it never aborts a CLI session or clears dictation text.
const MEETING_HUD = 'meeting-recording';
let recordingMeeting = null;
let meetingEventGeneration = 0;
let stoppingMeetingId = null;
function renderMeetingHud(meeting) {
  if (meeting && recordingMeeting && meeting.id !== recordingMeeting.id && meeting.status !== 'recording') return;
  if (!meeting || !['recording', 'transcribing'].includes(meeting.status)) {
    recordingMeeting = null;
    removeCard(MEETING_HUD);
    return;
  }
  recordingMeeting = meeting;
  const prior = existingCard(MEETING_HUD);
  const before = prior ? captureCard(prior.el) : null;
  const card = prior?.el || document.createElement('div');
  if (!prior) card.className = 'card meeting-hud';
  card.setAttribute('role', 'status');
  card.textContent = '';
  const row = document.createElement('div'); row.className = 'crow';
  row.appendChild(window.jarvisIcons.create('record', 18));
  const title = document.createElement('div'); title.className = 'title';
  title.textContent = meeting.status === 'recording' ? 'Запись встречи' : 'Готовим расшифровку';
  const time = document.createElement('span'); time.className = 'meeting-hud-time';
  row.append(title, time); card.appendChild(row);
  const body = document.createElement('div'); body.className = 'body'; body.textContent = meeting.title; card.appendChild(body);
  if (meeting.status === 'recording') {
    const stop = document.createElement('button'); stop.className = 'cont'; stop.textContent = stoppingMeetingId === meeting.id ? 'Сохраняем…' : 'Остановить запись';
    stop.disabled = stoppingMeetingId === meeting.id;
    stop.addEventListener('click', async e => {
      e.stopPropagation();
      if (stoppingMeetingId === meeting.id) return;
      stoppingMeetingId = meeting.id;
      const generation = meetingEventGeneration;
      stop.disabled = true; stop.textContent = 'Сохраняем…';
      try {
        const result = await window.toast.meetingStop();
        if (generation === meetingEventGeneration) renderMeetingHud(result);
      } catch (error) {
        const current = cards.get(MEETING_HUD)?.el;
        if (recordingMeeting?.id === meeting.id && current) {
          current.querySelector('.body').textContent = String(error);
          current.querySelector('.cont').textContent = 'Повторить остановку';
        }
      } finally {
        if (stoppingMeetingId === meeting.id) stoppingMeetingId = null;
        const button = cards.get(MEETING_HUD)?.el.querySelector('.cont');
        if (button) button.disabled = false;
      }
    });
    card.appendChild(stop);
  }
  if (!prior) stackEl.appendChild(card);
  cards.set(MEETING_HUD, { el: card, sticky: true, timer: null, ttl: 0 });
  updateMeetingClock(); presentCard(card, before);
}
function updateMeetingClock() {
  const meeting = recordingMeeting;
  if (!meeting) return;
  const ms = meeting.status === 'recording' ? Date.now() - meeting.startedAt : meeting.durationMs;
  const seconds = Math.max(0, Math.floor(ms / 1000));
  const clock = cards.get(MEETING_HUD)?.el.querySelector('.meeting-hud-time');
  if (clock) clock.textContent = `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
}
if (window.toast.onMeetingChanged) {
  let receivedMeeting = false;
  window.toast.onMeetingChanged(meeting => { receivedMeeting = true; meetingEventGeneration++; renderMeetingHud(meeting); });
  window.toast.meetingStatus().then(meeting => { if (!receivedMeeting) renderMeetingHud(meeting); }).catch(() => {});
  setInterval(updateMeetingClock, 1000);
}
