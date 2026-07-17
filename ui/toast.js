/* Стек тостов (Wispr-стиль): дот + заголовок + ×, ниже сжатый вывод модели.
 * Карточка кликабельна целиком (открыть чат), × закрывает без открытия. */

const stackEl = document.getElementById('stack');
const TTL = 8000;
const MAX_CARDS = 4;
const cards = new Map(); // id → {el, timer}
let hovering = false; // курсор над окном тостов (из нативного poll'а)

function reportHeight() {
  if (!cards.size) { window.toast.resize(0); return; }
  window.toast.resize(Math.min(480, stackEl.scrollHeight + 4));
}

function removeCard(id, instant) {
  const c = cards.get(id);
  if (!c) return;
  cards.delete(id);
  clearTimeout(c.timer);
  if (instant) {
    c.el.remove();
    reportHeight();
    return;
  }
  c.el.classList.remove('in');
  c.el.classList.add('out');
  setTimeout(() => { c.el.remove(); reportHeight(); }, 190);
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
  const existing = cards.get(d.id);
  if (existing) {
    const t = existing.el.querySelector('.title');
    if (t) t.textContent = d.title || '';
    const b = existing.el.querySelector('.body');
    if (b) b.textContent = d.body || '';
    armTimer(d.id); // таймер заново — карточка «обновилась»
    reportHeight();
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

    if (d.body) {
      const body = document.createElement('div');
      body.className = 'body';
      body.textContent = d.body;
      card.appendChild(body);
    }

    // варианты вопроса (AskUserQuestion). Payload плоский: первый вопрос +
    // count. Инлайн-чипы — только для одиночного вопроса; мульти-вопрос
    // отвечается в приложении (визард).
    const qq = d.question || null;
    const count = qq && typeof qq.count === 'number' ? qq.count : (qq && qq.options ? 1 : 0);
    const opts = qq && Array.isArray(qq.options) ? qq.options : null;
    if (count > 1) {
      sticky = true;
      card.classList.add('sticky');
      const note = document.createElement('div');
      note.className = 'body';
      note.textContent = `Несколько вопросов (${count}) — ответь в приложении`;
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
        key.textContent = '⌘⌥';
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
        opt.addEventListener('click', (e) => {
          e.stopPropagation();
          window.toast.answerQuestion(d.sessionId, { answers: [[i + 1]] });
          if (!qq.multiSelect) removeCard(d.id);
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
        window.toast.continueSession(d.sessionId);
        removeCard(d.id);
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

  reportHeight();
  requestAnimationFrame(() => requestAnimationFrame(() => card.classList.add('in')));
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

// текст приходит готовым в onAdd; onUpdate оставлен как безопасный no-op-путь
// на случай отложенного обновления тела существующей карточки
window.toast.onUpdate((d) => {
  const c = cards.get(d.id);
  if (!c) return;
  const body = c.el.querySelector('.body');
  if (body) body.textContent = d.body || '';
  armTimer(d.id);
  reportHeight();
});

/* ===================== голосовая маршрутизация (HUD) ===================== */

// Терминальные фазы исчезают по TTL; промежуточные/интерактивные — «липкие».
// 'heard' (итог диктовки) — терминальная: уходит сама по TTL (как остальные
// уведомления), а не висит вечно до крестика. Под курсором пауза — успеть прочесть/кликнуть.
const VOICE_TERMINAL = new Set(['sent', 'cancelled', 'empty', 'nosessions', 'error', 'reply', 'heard']);
// Фазы, где разговор УЖЕ завершён — × просто закрывает карточку, без abort и без
// «Отмена». ВАЖНО: 'reply' тут НЕТ — пока Джарвис ОЗВУЧИВАЕТ ответ, крестик должен
// оборвать речь и завершить разговор (RC1), а не молча спрятать карточку.
const VOICE_FINISHED = new Set(['sent', 'cancelled', 'empty', 'nosessions', 'error', 'dismiss']);

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
  close.title = 'Стоп';
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
  if (p.phase === 'dismiss') { removeCard(p.id, true); return; }
  const id = p.id;
  const terminal = VOICE_TERMINAL.has(p.phase);
  // «Услышал»: вставка подтверждена (текст уже в поле) — короткие 2с;
  // не подтверждена — 5с, успеть прочитать/скопировать/кликнуть в историю.
  const ttl = p.phase === 'heard' ? (p.inserted ? 2000 : 5000) : 4200;

  const existing = cards.get(id);
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
  crow.append(dot, title, voiceClose(p));
  card.appendChild(crow);

  if (p.body) {
    const body = document.createElement('div');
    body.className = 'body';
    body.textContent = p.body;
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
    // Надиктовка завершена. Кнопка ручного копирования — на случай, если
    // автоматическая вставка/копия не сработала (полный текст из p.full).
    const copy = document.createElement('button');
    copy.className = 'cont';
    copy.textContent = 'Копировать';
    copy.addEventListener('click', (e) => {
      e.stopPropagation();
      try { window.toast.copy(p.full || p.body || ''); } catch {}
      copy.textContent = 'Скопировано';
      setTimeout(() => { copy.textContent = 'Копировать'; }, 1200);
    });
    card.appendChild(copy);
    // Клик по карточке → открыть «Историю голоса» (× и кнопка копирования
    // гасят всплытие, так что не конфликтуют).
    card.style.cursor = 'pointer';
    card.title = 'Открыть историю голоса';
    card.onclick = () => { try { window.toast.openVoiceHistory(); } catch {} };
  }

  if (firstTime) {
    if (cards.size >= MAX_CARDS) evictForRoom();
    stackEl.appendChild(card);
    requestAnimationFrame(() => requestAnimationFrame(() => card.classList.add('in')));
  }
  cards.set(id, { el: card, timer: null, ttl, sticky: !terminal });
  armTimer(id); // терминальные — тикают к TTL; липкие — ждут следующую фазу
  reportHeight();
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
  const silent = !!s.mic_silent;
  if (!denied && !noDevice && !silent) {
    removeCard(MIC_ID, true);
    return;
  }
  const existing = cards.get(MIC_ID);
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
  title.textContent = denied
    ? 'Нет доступа к микрофону'
    : noDevice
      ? 'Микрофон не найден'
      : 'Микрофон молчит — говори громче';
  const close = document.createElement('button');
  close.className = 'close';
  close.title = 'Скрыть';
  const x = document.createElement('span');
  x.textContent = '✕';
  close.appendChild(x);
  close.addEventListener('click', (e) => { e.stopPropagation(); removeCard(MIC_ID); });
  crow.append(dot, title, close);
  card.appendChild(crow);
  if (firstTime) {
    if (cards.size >= MAX_CARDS) evictForRoom();
    stackEl.appendChild(card);
    requestAnimationFrame(() => requestAnimationFrame(() => card.classList.add('in')));
  }
  cards.set(MIC_ID, { el: card, timer: null, ttl: TTL, sticky: true });
  reportHeight();
}

window.toast.onAudioState(renderMicState);
// дотянуть текущее состояние на загрузке: audio_state эмитится лишь на изменении,
// ранний denied/«нет устройства» мог уйти до регистрации слушателя (VR-3)
if (window.toast.audioState) window.toast.audioState().then(renderMicState).catch(() => {});
