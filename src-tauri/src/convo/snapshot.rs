//! Компактный снапшот мира для промпта планировщика. Чистый: на вход — уже
//! снятое состояние (сессии, строка времени, флаги), на выход — компактный текст.
//! Цель — чтобы Haiku отвечал на большинство read-вопросов за один вызов.

use crate::model::{Session, Status};

fn ellip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

/// Собрать снапшот: время, флаги (mute/keep-awake), список живых сессий + счётчики.
pub fn build_snapshot(sessions: &[Session], now: &str, muted: bool, keep_awake: bool) -> String {
    let live: Vec<&Session> = sessions.iter().filter(|s| s.renamed_to.is_none()).collect();
    let waiting = live.iter().filter(|s| s.status == Status::Waiting).count();
    let working = live.iter().filter(|s| s.status == Status::Working).count();

    let mut out = format!("Время: {now}\n");
    if muted {
        out.push_str("Состояние: звук выключен\n");
    }
    if keep_awake {
        out.push_str("Состояние: режим «не спать» активен\n");
    }

    if live.is_empty() {
        out.push_str("Сессии: нет активных сессий.\n");
        return out;
    }
    out.push_str(&format!("Сессии (ждут: {waiting}, работают: {working}):\n"));
    for s in live {
        let id = s.id.chars().take(8).collect::<String>();
        let project = s.project.as_deref().unwrap_or("?");
        let task = s.task.as_deref().unwrap_or("");
        let status = match s.status {
            Status::Waiting => "ждёт",
            Status::Working => "работает",
            Status::Done => "готово",
            Status::Limit => "лимит",
            Status::Idle => "простаивает",
        };
        let lp = s.last_prompt.as_deref().map(|p| ellip(p, 40)).unwrap_or_default();
        out.push_str(&format!("- [{id}] {project} · {task} · {status}"));
        if !lp.is_empty() {
            out.push_str(&format!(" · послед.: «{lp}»"));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Session, Status};

    fn sess(id: &str, project: &str, task: &str, st: Status) -> Session {
        let mut s = Session::new(id.into(), 1);
        s.project = Some(project.into());
        s.task = Some(task.into());
        s.status = st;
        s.tmux_pane = Some("%1".into());
        s
    }

    #[test]
    fn lists_sessions_and_counts() {
        let sessions = vec![
            sess("aaaaaaaa1", "frontend", "fix build", Status::Waiting),
            sess("bbbbbbbb2", "backend", "migrate", Status::Working),
        ];
        let snap = build_snapshot(&sessions, "2026-06-27 14:05", false, false);
        assert!(snap.contains("frontend"));
        assert!(snap.contains("backend"));
        assert!(snap.contains("14:05"));
        assert!(snap.contains("ждут: 1"));
        assert!(snap.contains("работают: 1"));
    }

    #[test]
    fn empty_sessions_says_none() {
        let snap = build_snapshot(&[], "2026-06-27 14:05", false, false);
        assert!(snap.to_lowercase().contains("нет активных"));
    }

    #[test]
    fn shows_mute_and_keepawake_flags() {
        let snap = build_snapshot(&[], "t", true, true);
        assert!(snap.contains("звук выключен"));
        assert!(snap.contains("не спать"));
    }

    #[test]
    fn excludes_renamed_sessions() {
        let mut s = sess("aaaaaaaa1", "frontend", "fix", Status::Idle);
        s.renamed_to = Some("new".into());
        let snap = build_snapshot(&[s], "t", false, false);
        assert!(snap.to_lowercase().contains("нет активных"));
    }
}
