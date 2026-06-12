/**
 * clamshell: чистое ядро — парсеры системных выводов и решения.
 * Никаких exec/Electron: всё, что трогает систему, живёт в index.js.
 */

/** ioreg -r -k AppleClamshellState -d 4 → состояние крышки.
 * causesSleep учитывает и родной clamshell-режим, и disablesleep —
 * macOS сама говорит, уснёт ли мак от закрытия крышки прямо сейчас. */
function parseClamshellState(out) {
  const s = String(out || '');
  const st = s.match(/"AppleClamshellState"\s*=\s*(Yes|No)/i);
  const cs = s.match(/"AppleClamshellCausesSleep"\s*=\s*(Yes|No)/i);
  if (!st && !cs) return { present: false, closed: null, causesSleep: null };
  const yes = (m) => (m ? /yes/i.test(m[1]) : null);
  return { present: true, closed: yes(st), causesSleep: yes(cs) };
}

/** pmset -g → стоит ли сейчас флаг disablesleep (строка SleepDisabled) */
function parseSleepDisabled(out) {
  const m = String(out || '').match(/SleepDisabled\s+(\d)/);
  return m ? m[1] === '1' : null;
}

/** pmset -g batt → процент и источник питания (десктоп без батареи → null) */
function parseBattery(out) {
  const s = String(out || '');
  const pct = s.match(/(\d{1,3})%/);
  const src = s.match(/Now drawing from '([^']+)'/);
  return {
    pct: pct ? Math.min(100, parseInt(pct[1], 10)) : null,
    onBattery: src ? /battery/i.test(src[1]) : null,
    charging: /;\s*charging/i.test(s) ? true : /discharging/i.test(s) ? false : null,
  };
}

/** Проснулись после сна: предлагать ли closed-display?
 * kind 'arm' — предложить disablesleep; 'native' — есть внешний дисплей,
 * рассказать про родной clamshell-режим (root не нужен). */
function decideSuggest({ workingAtSleep, armed, externalDisplay, lastSuggestAt, now, minGapMs }) {
  if (!workingAtSleep || armed) return { suggest: false };
  if (now - (lastSuggestAt || 0) < minGapMs) return { suggest: false };
  return { suggest: true, kind: externalDisplay ? 'native' : 'arm' };
}

/** /etc/sudoers.d/jarvis-pmset: тихий доступ ровно к двум командам.
 * Имя юзера валидируем жёстко — содержимое уходит в sudoers. */
function sudoersContent(user) {
  const u = String(user || '');
  if (!/^[A-Za-z_][A-Za-z0-9_.-]*$/.test(u)) {
    throw new Error(`недопустимое имя пользователя для sudoers: ${JSON.stringify(u)}`);
  }
  return [
    '# Jarvis: тихое переключение closed-display mode (плагин clamshell).',
    '# Разрешает БЕЗ пароля ровно две команды — включить/выключить disablesleep.',
    `${u} ALL=(root) NOPASSWD: /usr/bin/pmset -a disablesleep 1, /usr/bin/pmset -a disablesleep 0`,
    '',
  ].join('\n');
}

module.exports = {
  parseClamshellState,
  parseSleepDisabled,
  parseBattery,
  decideSuggest,
  sudoersContent,
};
