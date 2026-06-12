/**
 * keep-awake: движок грантов. Чистый модуль без Electron — блокер, часы
 * и проверка процессов инжектятся (тестируется голым node).
 *
 * Инвариант — как у самих IOPMAssertions: assertion активна ⇔ грантов > 0.
 * Грантов максимум два:
 *   auto   — триггер «агенты работают» (аналог Trigger-сессии Amphetamine);
 *   manual — ручной слот (manual | timer | process), новый старт заменяет
 *            предыдущий: kill-then-start, как у Raycast Coffee.
 */

function createEngine(opts) {
  const blocker = opts.blocker;            // { start(type) → id, stop(id) }
  const blockerType = opts.blockerType;    // () → тип assertion (живой preference)
  const lingerMs = opts.lingerMs ?? 60 * 1000;
  const pulseMs = opts.pulseMs ?? 15 * 1000;
  const pidAlive = opts.pidAlive || ((pid) => {
    try { process.kill(pid, 0); return true; } catch { return false; }
  });
  const onChange = opts.onChange || (() => {});
  const onTimerEnd = opts.onTimerEnd || (() => {});
  const onProcessDied = opts.onProcessDied || (() => {});

  let autoEnabled = opts.autoEnabled ?? true;
  let autoHeld = false;      // auto-грант взят
  let workingCount = 0;      // последнее известное число working-сессий
  let lingerTimer = null;    // отложенное снятие auto-гранта
  let manual = null;         // { kind, label, until?, pid?, timer?, pulse? }
  let held = false;          // assertion реально взята
  let heldId = null;         // id от блокера (бывает 0 — не проверять truthiness!)
  let heldType = null;       // тип, с которым взята (для restartBlocker)

  function clearLinger() {
    if (lingerTimer) { clearTimeout(lingerTimer); lingerTimer = null; }
  }

  function clearManual() {
    if (!manual) return;
    if (manual.timer) clearTimeout(manual.timer);
    if (manual.pulse) clearInterval(manual.pulse);
    manual = null;
  }

  function evaluate() {
    const need = autoHeld || !!manual;
    if (need && !held) {
      heldType = blockerType();
      heldId = blocker.start(heldType);
      held = true;
    } else if (!need && held) {
      blocker.stop(heldId);
      held = false;
      heldId = null;
      heldType = null;
    }
    onChange(state());
  }

  function state() {
    return {
      active: held,
      auto: autoHeld,
      autoEnabled,
      working: workingCount,
      lingering: !!lingerTimer,
      manual: manual
        ? { kind: manual.kind, label: manual.label, until: manual.until, pid: manual.pid }
        : null,
    };
  }

  return {
    /** триггер: сколько сессий сейчас working */
    setWorking(n) {
      workingCount = n;
      if (!autoEnabled) return;
      if (n > 0) {
        clearLinger();
        if (!autoHeld) { autoHeld = true; evaluate(); }
        else onChange(state()); // число working для лейбла могло смениться
      } else if (autoHeld && !lingerTimer) {
        // линджер гасит дребезг working→done→working между ходами и держит
        // мост для авто-циклов, где следующий промпт приходит через секунды
        lingerTimer = setTimeout(() => {
          lingerTimer = null;
          autoHeld = false;
          evaluate();
        }, lingerMs);
        onChange(state());
      }
    },

    setAuto(enabled) {
      autoEnabled = !!enabled;
      if (!autoEnabled) {
        clearLinger();
        if (autoHeld) { autoHeld = false; evaluate(); }
      } else if (workingCount > 0) {
        this.setWorking(workingCount);
      }
    },

    startManual(label) {
      clearManual();
      manual = { kind: 'manual', label: label || 'бессрочно' };
      evaluate();
    },

    startTimer(ms, label) {
      clearManual();
      manual = { kind: 'timer', label, until: Date.now() + ms };
      manual.timer = setTimeout(() => {
        const g = manual;
        manual = null;
        evaluate();
        onTimerEnd(g);
      }, ms);
      evaluate();
    },

    startProcess(pid, label) {
      clearManual();
      manual = { kind: 'process', pid, label };
      manual.pulse = setInterval(() => {
        if (pidAlive(pid)) return;
        const g = manual;
        clearManual();
        evaluate();
        onProcessDied(g);
      }, pulseMs);
      evaluate();
    },

    stopManual() {
      clearManual();
      evaluate();
    },

    /** preference типа assertion сменился (тумблер «не гасить экран») */
    restartBlocker() {
      if (!held) return;
      const next = blockerType();
      if (next === heldType) return;
      blocker.stop(heldId);
      heldType = next;
      heldId = blocker.start(next);
      onChange(state());
    },

    active: () => held,
    state,

    dispose() {
      clearLinger();
      clearManual();
      autoHeld = false;
      if (held) { blocker.stop(heldId); held = false; heldId = null; }
    },
  };
}

module.exports = { createEngine };
