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
  c.timer = setTimeout(() => removeCard(id), c.ttl || TTL);
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
    removeCard(cards.keys().next().value, true); // самая старая — мгновенно
  }

  // карточка смены режима: компактная, по центру, с «поп»-анимацией, живёт недолго
  const isMode = d.kind === 'mode';
  const ttl = isMode ? 1900 : TTL;
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

    if (d.body) {
      const body = document.createElement('div');
      body.className = 'body';
      body.textContent = d.body;
      card.appendChild(body);
    }

    // варианты вопроса (AskUserQuestion): номер ⌘⌥N + label + описание.
    // Выбор — глобальным хоткеем ⌘⌥1..9 (через демон) или кликом по варианту.
    const opts = d.question && Array.isArray(d.question.options) ? d.question.options : null;
    if (opts && opts.length) {
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
          window.toast.answerQuestion(d.sessionId, [i + 1], !!d.question.multiSelect);
          if (!d.question.multiSelect) removeCard(d.id);
        });
        list.appendChild(opt);
      });
      card.appendChild(list);
    }

    // действие «Продолжить» — только для застрявших сессий (ждёт / лимит /
    // оборвалась, напр. сном), но НЕ для нормально завершённых (done) и НЕ для
    // вопросов (там действие — выбрать вариант, «Продолжить» не к месту).
    if (d.sessionId && d.kind !== 'done' && !(opts && opts.length)) {
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
  const ms = d.ms || 3500;
  c.el.style.setProperty('--ttl', `${ms}ms`);
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
