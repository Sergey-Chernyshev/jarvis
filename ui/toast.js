/* Стек тостов (Wispr-стиль): дот + заголовок + ×, ниже сжатый вывод модели.
 * Карточка кликабельна целиком (открыть чат), × закрывает без открытия. */

const stackEl = document.getElementById('stack');
const TTL = 8000;
const MAX_CARDS = 4;
const cards = new Map(); // id → {el, timer}

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
  restartRing(c.el);
  c.timer = setTimeout(() => removeCard(id), TTL);
}

window.toast.onAdd((d) => {
  if (cards.size >= MAX_CARDS) {
    removeCard(cards.keys().next().value, true); // самая старая — мгновенно
  }

  const card = document.createElement('div');
  card.className = 'card';
  card.style.setProperty('--ttl', `${TTL}ms`);

  const crow = document.createElement('div');
  crow.className = 'crow';

  const dot = document.createElement('span');
  dot.className = `dot${d.kind === 'waiting' ? ' waiting' : ''}`;

  const title = document.createElement('div');
  title.className = 'title';
  title.textContent = d.title || '';

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

  card.addEventListener('click', () => {
    window.toast.click(d.sessionId || null);
    removeCard(d.id);
  });
  card.addEventListener('mouseenter', () => {
    const c = cards.get(d.id);
    if (c) clearTimeout(c.timer); // читаешь — не исчезает
  });
  card.addEventListener('mouseleave', () => armTimer(d.id));

  stackEl.appendChild(card); // новые — снизу, старые поднимаются
  cards.set(d.id, { el: card, timer: null });
  armTimer(d.id);

  reportHeight();
  requestAnimationFrame(() => requestAnimationFrame(() => card.classList.add('in')));
});

// ИИ-выжимка догнала тост: меняем тело и даём время дочитать
window.toast.onUpdate((d) => {
  const c = cards.get(d.id);
  if (!c) return;
  const body = c.el.querySelector('.body');
  if (body) body.textContent = d.body || '';
  armTimer(d.id); // таймер заново — текст обновился
  reportHeight();
});
