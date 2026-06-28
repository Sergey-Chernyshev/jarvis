//! Координация голосового взаимодействия.
//!
//! Пока пользователь ДИКТУЕТ (push-to-talk) или ведёт РАЗГОВОР с Jarvis:
//!  - голосовые уведомления (done/waiting/limit) НЕ озвучиваются поверх речи —
//!    они буферизуются и проигрываются ПОСЛЕ завершения взаимодействия;
//!  - wake-детектор подавляется (не реагирует на собственную речь / эхо TTS).
//!
//! Глубина (взаимодействия вкладываются: разговор содержит под-захваты) И буфер
//! отложенных уведомлений живут под ОДНИМ мьютексом. Это критично: проверка
//! «идёт ли взаимодействие» + постановка в буфер (`defer_if_active`) и
//! декремент + слив буфера (`leave`) линеаризованы, поэтому уведомление, пришедшее
//! ровно на границе завершения, не может «провалиться» (отложиться уже после слива).

use std::sync::Mutex;

/// Отложенное голосовое уведомление — озвучить после завершения взаимодействия.
#[derive(Debug, Clone, PartialEq)]
pub struct DeferredVoiced {
    pub id: String,
    pub title: String,
    pub speak: String,
    pub kind: String,
}

#[derive(Default)]
struct State {
    depth: i32,
    pending: Vec<DeferredVoiced>,
}

/// Состояние «пользователь занят голосом» + буфер отложенных уведомлений под
/// одним локом (см. модульный комментарий — это закрывает гонку потери уведомления).
#[derive(Default)]
pub struct Interaction {
    state: Mutex<State>,
}

impl Interaction {
    /// Войти во взаимодействие (диктовка/разговор начались).
    pub fn enter(&self) {
        self.state.lock().unwrap().depth += 1;
    }

    /// Выйти из взаимодействия. Возвращает `(стало_idle, отложенные_для_слива)`.
    /// Слив буфера происходит ПОД тем же локом, что и `defer_if_active`, поэтому
    /// поздняя постановка в буфер уже после слива невозможна (нет гонки).
    pub fn leave(&self) -> (bool, Vec<DeferredVoiced>) {
        let mut s = self.state.lock().unwrap();
        if s.depth <= 1 {
            // Рассинхрон (лишний leave) не уводит счётчик в минус: фиксируем 0.
            s.depth = 0;
            (true, std::mem::take(&mut s.pending))
        } else {
            s.depth -= 1;
            (false, Vec::new())
        }
    }

    /// Идёт ли взаимодействие прямо сейчас (для гейта wake-детектора).
    pub fn is_active(&self) -> bool {
        self.state.lock().unwrap().depth > 0
    }

    /// Если идёт взаимодействие — отложить уведомление (вернуть `true`). Иначе НЕ
    /// откладывать (вернуть `false`) — вызывающий озвучивает сразу. Проверка и
    /// постановка атомарны под локом `leave`, поэтому уведомление не потеряется на
    /// границе завершения. Дедуп по id: свежий статус заменяет старый.
    pub fn defer_if_active(&self, v: DeferredVoiced) -> bool {
        let mut s = self.state.lock().unwrap();
        if s.depth > 0 {
            s.pending.retain(|p| p.id != v.id);
            s.pending.push(v);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dv(id: &str, kind: &str) -> DeferredVoiced {
        DeferredVoiced { id: id.into(), title: "T".into(), speak: "S".into(), kind: kind.into() }
    }

    #[test]
    fn idle_by_default() {
        let i = Interaction::default();
        assert!(!i.is_active());
    }

    #[test]
    fn enter_active_leave_drains_and_clears() {
        let i = Interaction::default();
        i.enter();
        assert!(i.is_active());
        let (idle, drained) = i.leave();
        assert!(idle, "leave с глубины 1 → стало idle");
        assert!(drained.is_empty(), "нечего сливать");
        assert!(!i.is_active());
    }

    #[test]
    fn nested_interactions_count() {
        let i = Interaction::default();
        i.enter();
        i.enter();
        let (idle1, _) = i.leave();
        assert!(!idle1, "внутренний leave: ещё активно");
        assert!(i.is_active());
        let (idle2, _) = i.leave();
        assert!(idle2, "внешний leave: завершилось");
        assert!(!i.is_active());
    }

    #[test]
    fn leave_clamps_at_zero() {
        let i = Interaction::default();
        let (idle, drained) = i.leave(); // лишний leave с нуля
        assert!(idle);
        assert!(drained.is_empty());
        assert!(!i.is_active());
        i.enter();
        assert!(i.is_active(), "счётчик не застрял в минусе");
    }

    #[test]
    fn defer_if_active_buffers_when_active_then_leave_drains() {
        let i = Interaction::default();
        i.enter();
        assert!(i.defer_if_active(dv("a", "done")), "активно → отложено (true)");
        assert!(i.defer_if_active(dv("b", "waiting")));
        let (idle, drained) = i.leave();
        assert!(idle);
        assert_eq!(drained.len(), 2, "оба отложенных слиты под локом leave");
    }

    // КЛЮЧЕВОЙ инвариант, закрывающий гонку потери уведомления: когда взаимодействия
    // НЕТ, defer_if_active НЕ буферизует и возвращает false → вызывающий озвучивает
    // сразу, а не кладёт в буфер, из которого слив уже ушёл.
    #[test]
    fn defer_if_active_returns_false_when_idle_and_does_not_buffer() {
        let i = Interaction::default();
        assert!(!i.defer_if_active(dv("a", "done")), "idle → не откладываем (false)");
        // следующее взаимодействие не должно «всплыть» потерянным уведомлением
        i.enter();
        let (_idle, drained) = i.leave();
        assert!(drained.is_empty(), "ничего не осело в буфере, пока было idle");
    }

    #[test]
    fn defer_if_active_dedups_by_id_latest_wins() {
        let i = Interaction::default();
        i.enter();
        i.defer_if_active(DeferredVoiced { id: "s1".into(), title: "old".into(), speak: "old".into(), kind: "waiting".into() });
        i.defer_if_active(DeferredVoiced { id: "s1".into(), title: "new".into(), speak: "new".into(), kind: "done".into() });
        let (_idle, drained) = i.leave();
        assert_eq!(drained.len(), 1, "одинаковый id не дублируется");
        assert_eq!(drained[0].title, "new", "свежий статус заменяет старый");
    }
}
