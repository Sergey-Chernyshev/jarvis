//! Режим «Циклы»: рутина, которую агент крутит сам.
//!
//! Отличие цикла от сессии — в двух вещах, и обе обязательны. У цикла есть
//! КОНЕЦ: условие выхода, по которому он сам поймёт, что работа сделана. И у
//! него есть СТЕНЫ: ограничители, за которыми он остановится, чем бы ни был
//! занят. Без первого он не завершится, без вторых съест лимит аккаунта за ночь.
//!
//! Всё остальное — приоткрытая дверь: выборочная проверка, вопрос человеку на
//! спорном решении, вмешательство на ходу. Автономность без двери — это не
//! доверие, а надежда.

pub mod engine;
pub mod ipc;
pub mod model;
pub mod runner;
pub mod schedule;
pub mod store;
pub mod templates;

use model::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use store::Store;

/// Всё про циклы, что живёт в демоне.
pub struct Loops {
    pub store: Arc<Store>,
    /// Крутится ли прямо сейчас какой-нибудь запуск.
    ///
    /// Один за раз: два цикла разом — это два агента, жгущих один лимит
    /// аккаунта, и ночь, после которой не сделано ни то, ни другое.
    busy: AtomicBool,
}

impl Default for Loops {
    fn default() -> Self {
        Self::new()
    }
}

impl Loops {
    pub fn new() -> Self {
        Self { store: Arc::new(Store::load()), busy: AtomicBool::new(false) }
    }

    pub fn idle(&self) -> bool {
        !self.busy.load(Ordering::SeqCst)
    }

    /// Занять место под запуск. `false` — уже занято.
    pub fn claim(&self) -> bool {
        !self.busy.swap(true, Ordering::SeqCst)
    }

    pub fn release(&self) {
        self.busy.store(false, Ordering::SeqCst);
    }
}

/// Снимок для панели: цикл вместе с состоянием его последнего запуска.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopView {
    #[serde(flatten)]
    pub item: Loop,
    pub run: Option<Run>,
    /// Подпись расписания — панель не должна разбирать `Wake` сама.
    pub wake_label: String,
    pub next_wake: Option<i64>,
    /// Сколько итераций ждут человеческого взгляда.
    pub pending_review: usize,
    /// Чего не хватает для запуска.
    pub problems: Vec<String>,
}

pub fn view(item: &Loop, run: Option<Run>, now: i64) -> LoopView {
    LoopView {
        wake_label: schedule::wake_label(item),
        next_wake: schedule::next_wake(item, now),
        pending_review: run.as_ref().map(|r| r.pending_review()).unwrap_or(0),
        problems: item.problems(),
        item: item.clone(),
        run,
    }
}

/// Шаблоны для библиотеки — в том виде, в каком их рисует панель.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateView {
    pub id: String,
    pub name: String,
    pub hint: String,
}

pub fn template_views() -> Vec<TemplateView> {
    templates::all()
        .into_iter()
        .map(|t| TemplateView { id: t.id.into(), name: t.name.into(), hint: t.hint.into() })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_run_at_a_time() {
        let loops = Loops::new();
        assert!(loops.idle());
        assert!(loops.claim(), "первый запуск занимает место");
        assert!(!loops.claim(), "второй не должен стартовать поверх первого");
        assert!(!loops.idle());
        loops.release();
        assert!(loops.claim(), "после освобождения место снова свободно");
    }

    #[test]
    fn view_carries_everything_the_panel_needs() {
        let mut item = Loop { id: "a".into(), name: "ночной test-fix".into(), ..Default::default() };
        item.schedule.wake = Wake::Daily { at: "02:00".into() };
        let run = Run {
            loop_id: "a".into(),
            iterations: vec![Iteration { n: 1, sampled: true, ..Default::default() }],
            ..Default::default()
        };
        let v = view(&item, Some(run), crate::util::now_ms());
        assert_eq!(v.wake_label, "каждый день в 02:00");
        assert!(v.next_wake.is_some());
        assert_eq!(v.pending_review, 1);
        // Незаполненный цикл честно рассказывает, чего ему не хватает.
        assert!(!v.problems.is_empty());
    }

    #[test]
    fn templates_reach_the_panel_whole() {
        let views = template_views();
        assert_eq!(views.len(), templates::all().len());
        assert!(views.iter().all(|t| !t.name.is_empty() && !t.hint.is_empty()));
    }
}
