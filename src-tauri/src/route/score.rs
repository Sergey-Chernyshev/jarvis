//! Детерминированный скорер маршрутизации: реплика + живые сессии → порядок
//! кандидатов. Чистый (без I/O), ранжирует по ДОВЕРЕННЫМ структурным полям
//! `Session` (project / task / branch / cwd / last_prompt + свежесть). Узкий
//! LLM-tie-break (route::classify) подключается только на близких кандидатах.

use crate::model::Session;

/// Один кандидат с баллом и человекочитаемой меткой для HUD/пикера.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub session_id: String,
    pub score: f32,
    pub label: String, // «project · task»
    /// Есть ли СИЛЬНЫЙ сигнал (совпадение по project/task), а не только слабые
    /// поля (cwd/branch/last_prompt). Уверенный роут требует сильного сигнала —
    /// иначе эхо прошлого промпта могло бы авто-роутить (VR-LOGIC-3).
    pub strong: bool,
}

/// Решение скорера.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Уверенный лидер — можно стейджить и слать.
    Route(String),
    /// Несколько близких — в tie-break / пикер (top-K session_id).
    Ambiguous(Vec<String>),
    /// Нет сигнала / нет живых сессий.
    Unknown,
}

/// Минимальный абсолютный балл лидера, чтобы вообще считать роут «уверенным».
/// 1.5 = одно полное совпадение по имени проекта (вес 1.5). Слабее (cwd/branch
/// в одиночку, last_prompt) — не «уверенно» → tie-break/пикер.
const MIN_LEAD: f32 = 1.5;
/// Лидер должен опережать второго хотя бы в этот раз.
const GAP_RATIO: f32 = 1.6;
/// Сколько кандидатов отдаём в tie-break/пикер.
const TOPK: usize = 4;

fn norm(s: &str) -> String {
    s.to_lowercase()
}

/// Токены реплики/поля: разбиваем по не-алфанумерике, отбрасываем короткие.
fn tokens(s: &str) -> Vec<String> {
    norm(s)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3)
        .map(|t| t.to_string())
        .collect()
}

/// Балл совпадения слов реплики с одним полем сессии (substring + общий токен).
fn field_score(words: &[String], field: &Option<String>, weight: f32) -> f32 {
    let Some(f) = field else { return 0.0 };
    let fl = norm(f);
    let ftoks = tokens(f);
    let mut sc = 0.0;
    for w in words {
        if fl.contains(w.as_str()) {
            sc += weight;
        } else if ftoks.iter().any(|ft| ft == w) {
            sc += weight * 0.8;
        }
    }
    sc
}

/// Имя последнего компонента пути (frontend из /Users/x/code/frontend).
fn cwd_leaf(cwd: &Option<String>) -> Option<String> {
    cwd.as_ref()
        .and_then(|c| c.trim_end_matches('/').rsplit('/').next())
        .map(String::from)
}

fn label_for(s: &Session) -> String {
    match (&s.project, &s.task) {
        (Some(p), Some(t)) if !t.is_empty() => format!("{p} · {t}"),
        (Some(p), _) => p.clone(),
        _ => s.id.chars().take(8).collect(),
    }
}

/// Ранжировать живые (не остановленные) сессии по совпадению с репликой.
/// Выше балл — выше в списке; при равенстве баллов — свежее (updated_at).
pub fn rank(transcript: &str, sessions: &[Session]) -> Vec<Scored> {
    let words = tokens(transcript);
    let mut out: Vec<(Scored, i64)> = sessions
        .iter()
        // Пропускаем переименованные/вытесненные сессии (renamed_to задан) и те,
        // что НЕ в tmux (tmux_pane = None): вставить промпт туда нельзя, роут в
        // них — тупик (VR-LOGIC-2). Нет таких → decide вернёт Unknown → «нет сессий».
        .filter(|s| s.renamed_to.is_none() && s.tmux_pane.is_some())
        .map(|s| {
            // сильные поля (project/task) отдельно — по ним решаем «уверенность»
            let strong_score = field_score(&words, &s.project, 1.5) + field_score(&words, &s.task, 1.2);
            let mut score = strong_score;
            score += field_score(&words, &s.branch, 1.0);
            score += field_score(&words, &cwd_leaf(&s.cwd), 1.0);
            score += field_score(&words, &s.last_prompt, 0.6);
            (
                Scored {
                    session_id: s.id.clone(),
                    score,
                    label: label_for(s),
                    strong: strong_score > 0.0,
                },
                s.updated_at,
            )
        })
        .collect();
    out.sort_by(|a, b| {
        b.0.score
            .partial_cmp(&a.0.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.1.cmp(&a.1))
    });
    out.into_iter().map(|(s, _)| s).collect()
}

/// Превратить ранжированный список в решение.
pub fn decide(scored: &[Scored]) -> Decision {
    let Some(first) = scored.first() else { return Decision::Unknown };
    let topk = || -> Vec<String> { scored.iter().take(TOPK).map(|s| s.session_id.clone()).collect() };

    // Нет уверенного сигнала: есть кандидаты → пусть человек/LLM выберет.
    if first.score < MIN_LEAD {
        let cands = topk();
        return if cands.is_empty() { Decision::Unknown } else { Decision::Ambiguous(cands) };
    }
    let second = scored.get(1).map(|s| s.score).unwrap_or(0.0);
    let decisive = second <= 0.0 || first.score >= second * GAP_RATIO;
    // Уверенно ТОЛЬКО при сильном сигнале (project/task), иначе слабый лидер
    // (cwd/branch/last_prompt-эхо) идёт в пикер/tie-break (VR-LOGIC-3).
    if decisive && first.strong {
        Decision::Route(first.session_id.clone())
    } else {
        Decision::Ambiguous(topk())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Session, Status};

    fn sess(id: &str, project: &str, task: &str, updated: i64) -> Session {
        let mut s = Session::new(id.into(), updated);
        s.project = Some(project.into());
        s.task = Some(task.into());
        s.updated_at = updated;
        s.status = Status::Idle;
        s.tmux_pane = Some("%1".into()); // маршрутизируемая (в tmux) сессия
        s
    }

    #[test]
    fn obvious_project_match_wins_decisively() {
        let sessions = vec![
            sess("a", "frontend", "fix build", 100),
            sess("b", "backend", "db migration", 200),
        ];
        let scored = rank("почини билд во frontend", &sessions);
        assert_eq!(scored[0].session_id, "a");
        assert!(matches!(decide(&scored), Decision::Route(ref id) if id == "a"));
    }

    #[test]
    fn no_signal_is_ambiguous_not_route() {
        let sessions = vec![
            sess("a", "frontend", "fix build", 100),
            sess("b", "backend", "db migration", 100),
        ];
        let scored = rank("сделай хорошо пожалуйста", &sessions);
        assert!(matches!(decide(&scored), Decision::Ambiguous(_) | Decision::Unknown));
    }

    #[test]
    fn empty_sessions_is_unknown() {
        assert!(matches!(decide(&rank("что угодно", &[])), Decision::Unknown));
    }

    #[test]
    fn renamed_sessions_excluded() {
        let mut s = sess("a", "frontend", "fix build", 100);
        s.renamed_to = Some("new-id".into());
        let scored = rank("frontend build", &[s]);
        assert!(scored.is_empty());
    }

    #[test]
    fn close_competitors_are_ambiguous() {
        // оба матчат слово «api» одинаково → неоднозначно, не уверенный роут
        let sessions = vec![
            sess("a", "api gateway", "api gateway", 100),
            sess("b", "api worker", "api worker", 100),
        ];
        let scored = rank("посмотри api", &sessions);
        assert!(matches!(decide(&scored), Decision::Ambiguous(_)));
    }

    #[test]
    fn label_falls_back_to_project_then_id() {
        let mut s = Session::new("abcdefgh1234".into(), 1);
        s.project = None;
        s.task = None;
        s.tmux_pane = Some("%1".into());
        let scored = rank("x", &[s]);
        assert_eq!(scored[0].label, "abcdefgh");
    }

    #[test]
    fn sessions_without_tmux_pane_excluded() {
        // нет tmux_pane → вставить нельзя → не кандидат (VR-LOGIC-2)
        let mut s = sess("a", "frontend", "fix build", 100);
        s.tmux_pane = None;
        let scored = rank("frontend build", &[s]);
        assert!(scored.is_empty());
        assert!(matches!(decide(&scored), Decision::Unknown));
    }

    #[test]
    fn last_prompt_only_match_is_ambiguous_not_route() {
        // совпадение ТОЛЬКО по last_prompt (эхо прошлого промпта) набирает балл,
        // но без сильного сигнала (project/task) — не уверенный роут (VR-LOGIC-3)
        let mut s = sess("a", "zzz", "yyy", 100);
        s.last_prompt = Some("деплой стейджинг прод релиз".into());
        let scored = rank("деплой стейджинг прод релиз", &[s]);
        assert!(scored[0].score >= 1.5, "балл выше порога, но сигнал слабый");
        assert!(!scored[0].strong, "нет совпадения по project/task");
        assert!(matches!(decide(&scored), Decision::Ambiguous(_)));
    }
}
