//! Живые агенты машины: что здесь работает прямо сейчас.
//!
//! Зачем это узлу, если сессии заводятся из хуков. Две причины, обе вскрылись
//! вживую:
//!
//! 1. **Сверка живости.** У локальной сессии ноут судит по процессу агента
//!    (pid = `$PPID` хука) и только потом по пане. У удалённой процесса в его
//!    таблице нет, оставалась пана — а `$TMUX_PANE` хук берёт из ЛЮБОГО
//!    tmux-сервера, не только из `-L jarvis`. Агент, поднятый человеком в его
//!    обычном tmux, получал пану `%3`, которой в `-L jarvis` нет, и сверка
//!    выселяла живую сессию раз в полминуты. Отсюда `alive`: спросить ту
//!    машину, жив ли pid, — единственный честный способ.
//!
//! 2. **Сессии без хуков.** Хуки берутся снапшотом на старте сессии: агент,
//!    запущенный до установки узла, не пришлёт ни одного события и в списке не
//!    появится никогда, хотя работает. Найти его на чужой машине кроме узла
//!    некому — ровно как транскрипты (`projects`).
//!
//! Граница дизайна не двигается: узел ищет ФАКТЫ на диске и в таблице
//! процессов, а статусы, ходы и уведомления по-прежнему считает ноут.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Насколько свежим должен быть транскрипт, чтобы связать его с процессом по
/// догадке (когда точный путь через открытые дескрипторы не нашёлся).
///
/// Час — компромисс, и он выбран в сторону молчания: ошибиться здесь значит
/// показать чужой чат, а не пропустить строку. Промах самолечится — первый же
/// хук из этой паны выселит подобранную сессию (инвариант «одна пана — одна
/// сессия» в редьюсере ноута).
const GUESS_WINDOW: Duration = Duration::from_secs(3600);

const PS_TIMEOUT: Duration = Duration::from_secs(5);

/// Строка таблицы процессов.
struct Proc {
    pid: i64,
    ppid: i64,
    args: String,
}

/// Снимок живого: агенты машины и живость спрошенных pid.
///
/// Одна таблица процессов на оба ответа — их и спрашивают вместе. `ps -eo`
/// вместо чтения `/proc`: узел живёт и на macOS-станции.
///
/// `ps` не ответил вовсе — считаем спрошенные pid ЖИВЫМИ: «не смог спросить» и
/// «мертвы» разные вещи, и путать их значит выселить живые сессии ноута.
pub async fn snapshot(home: &Path, pids: &[i64]) -> Snapshot {
    let wanted: Vec<i64> = pids.iter().copied().filter(|p| *p > 0).collect();
    // Паны спрашиваем всегда: ноуту они нужны и сами по себе (инвариант «одна
    // пана — одна сессия»), а `error` отличает «tmux не установлен» от «пан нет».
    let (panes, error) = match super::tmux::list_panes().await {
        Ok(p) => (p, String::new()),
        Err(msg) => (Vec::new(), msg),
    };
    let Some(out) = run(&["-eo", "pid=,ppid=,args="]).await else {
        return Snapshot { agents: Vec::new(), alive: wanted, panes, error };
    };
    let procs = parse_table(&out);
    if procs.is_empty() {
        // пустая таблица процессов — тоже «не смог спросить», а не «все мертвы»
        return Snapshot { agents: Vec::new(), alive: wanted, panes, error };
    }
    let live: std::collections::HashSet<i64> = procs.iter().map(|p| p.pid).collect();
    let alive = wanted.into_iter().filter(|p| live.contains(p)).collect();
    Snapshot { agents: agents(home, &procs, &panes), alive, panes, error }
}

/// Снимок «что живо» — ответ `/agents` целиком.
pub struct Snapshot {
    pub agents: Vec<Value>,
    pub alive: Vec<i64>,
    pub panes: Vec<super::tmux::Pane>,
    /// tmux не установлен или сервер не поднят.
    pub error: String,
}

/// Живые агенты машины: pid, кто это, где работает, в какой пане и какой у него
/// транскрипт.
fn agents(home: &Path, procs: &[Proc], panes: &[super::tmux::Pane]) -> Vec<Value> {
    let found: Vec<(&Proc, &'static str)> =
        procs.iter().filter_map(|p| classify(&p.args).map(|a| (p, a))).collect();
    if found.is_empty() {
        return Vec::new(); // оглавление проектов спрашивать незачем
    }
    // pid → пана `-L jarvis`. Агент запускается из-под shell'а паны, поэтому
    // сопоставляем не напрямую, а поднимаясь по родителям.
    let by_pane_pid: HashMap<i64, &super::tmux::Pane> = panes.iter().map(|p| (p.pid, p)).collect();
    let parent: HashMap<i64, i64> = procs.iter().map(|p| (p.pid, p.ppid)).collect();

    // Оглавление проектов — единственная дорогая часть (обход каталогов), и
    // нужна она только для догадки. Читаем лениво и один раз на весь обход.
    let mut projects: Option<Vec<Value>> = None;
    let mut out = Vec::new();
    for (p, agent) in found {
        let pane = ancestor_pane(p.pid, &parent, &by_pane_pid);
        let cwd = proc_cwd(p.pid).or_else(|| pane.map(|x| PathBuf::from(&x.cwd)));
        let cwd = cwd.map(|c| c.to_string_lossy().into_owned()).unwrap_or_default();
        let transcript = match open_transcript(p.pid, home) {
            Some(t) => Some(t),
            None => {
                let projects = projects.get_or_insert_with(|| super::projects::list(home));
                guess_transcript(&cwd, projects)
            }
        };
        out.push(json!({
            "pid": p.pid,
            "agent": agent,
            "cwd": cwd,
            "pane": pane.map(|x| x.pane.clone()).unwrap_or_default(),
            "session": pane.map(|x| x.session.clone()).unwrap_or_default(),
            "sessionId": transcript
                .as_ref()
                .and_then(|t| t.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            "transcript": transcript
                .as_ref()
                .map(|t| t.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }));
    }
    out
}

/// Разбор таблицы процессов. Формат без заголовка (`=` у каждого поля) —
/// одинаково понимают и GNU, и BSD `ps`.
fn parse_table(out: &str) -> Vec<Proc> {
    out.lines()
        .filter_map(|line| {
            // Колонки `ps` выровнены пробелами, а argv в третьей содержит свои —
            // отсюда ручное деление на «два числа и всё остальное», а не splitn
            // по одиночному пробелу.
            let (pid, rest) = first_word(line.trim_start())?;
            let (ppid, args) = first_word(rest.trim_start())?;
            Some(Proc {
                pid: pid.parse::<i64>().ok()?,
                ppid: ppid.parse::<i64>().ok()?,
                args: args.trim().to_string(),
            })
        })
        .collect()
}

fn first_word(s: &str) -> Option<(&str, &str)> {
    let i = s.find(char::is_whitespace)?;
    Some((&s[..i], &s[i..]))
}

async fn run(args: &[&str]) -> Option<String> {
    let mut cmd = tokio::process::Command::new("ps");
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(PS_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    // `ps -p` с полностью мёртвым списком выходит ненулевым кодом и пустым
    // stdout — это валидный ответ «никого нет», а не сбой.
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Агент ли это и какой. Смотрим на ИМЯ исполняемого файла в argv[0] и на
/// первое слово после интерпретатора: `claude` ставят и нативно, и через npm
/// (`node …/cli.js`), и через bun.
///
/// Строгость здесь важнее полноты: лишнее совпадение («grep claude», редактор с
/// файлом `codex.md` в аргументах) завело бы в списке сессию из воздуха.
fn classify(args: &str) -> Option<&'static str> {
    let mut words = args.split_whitespace();
    let exe = words.next()?;
    let name = exe.rsplit('/').next().unwrap_or(exe);
    for (agent, marker) in [("claude", "claude"), ("codex", "codex")] {
        if name == marker {
            return Some(agent);
        }
        // node/bun/deno + путь до cli агента. Так выглядит npm-установка:
        // `node …/@anthropic-ai/claude-code/cli.js`, `node …/bin/claude`.
        // Сегмент целиком или с дефисом — `claude-code` считается, `claudius`
        // нет: иначе чужой скрипт с похожим именем завёл бы сессию из воздуха.
        if matches!(name, "node" | "bun" | "deno") {
            if let Some(script) = words.clone().find(|w| !w.starts_with('-')) {
                let hit = script.split('/').any(|seg| {
                    seg == marker || seg.strip_prefix(marker).is_some_and(|r| r.starts_with('-'))
                });
                if hit {
                    return Some(agent);
                }
            }
        }
    }
    None
}

/// Пана `-L jarvis`, из которой растёт процесс. Поднимаемся по родителям, а не
/// сравниваем напрямую: между паной и агентом стоит как минимум shell.
fn ancestor_pane<'a>(
    pid: i64,
    parent: &HashMap<i64, i64>,
    by_pane_pid: &HashMap<i64, &'a super::tmux::Pane>,
) -> Option<&'a super::tmux::Pane> {
    let mut cur = pid;
    // Потолок обхода: цикл в таблице процессов невозможен, но она снята
    // неатомарно, и зацикливаться на чужой гонке узел не должен.
    for _ in 0..32 {
        if let Some(p) = by_pane_pid.get(&cur) {
            return Some(p);
        }
        match parent.get(&cur) {
            Some(&next) if next > 1 => cur = next,
            _ => return None,
        }
    }
    None
}

/// Рабочий каталог процесса. Только Linux: `/proc/<pid>/cwd`. На macOS его
/// пришлось бы спрашивать у `lsof`, а нужен он лишь как уточнение к каталогу
/// паны — не та цена.
fn proc_cwd(pid: i64) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Транскрипт агента по догадке: свежайший транскрипт проекта с этим рабочим
/// каталогом, и только пока он свежий.
///
/// Дорога второго сорта — её берут, когда точный путь (открытый дескриптор
/// процесса) не нашёлся: нет `/proc` или файл держится открытым не всё время.
/// Окно свежести здесь намеренно: молчание лучше чужого чата.
fn guess_transcript(cwd: &str, projects: &[Value]) -> Option<PathBuf> {
    if cwd.is_empty() {
        return None;
    }
    let cwd = cwd.trim_end_matches('/');
    let project = projects.iter().find(|p| {
        p.get("cwd")
            .and_then(Value::as_str)
            .map(|c| c.trim_end_matches('/') == cwd)
            .unwrap_or(false)
    })?;
    let newest = project.get("sessions").and_then(Value::as_array)?.first()?;
    let at = newest.get("at").and_then(Value::as_i64).unwrap_or(0);
    if now_ms() - at > GUESS_WINDOW.as_millis() as i64 {
        return None; // давно не писали — это чужая, прошлая сессия
    }
    newest.get("path").and_then(Value::as_str).map(PathBuf::from)
}

/// Открытые файлы процесса, попадающие в каталог транскриптов Claude.
fn open_transcript(pid: i64, home: &Path) -> Option<PathBuf> {
    let root = home.join(".claude").join("projects");
    let dir = std::fs::read_dir(format!("/proc/{pid}/fd")).ok()?;
    for e in dir.filter_map(Result::ok) {
        let Ok(target) = std::fs::read_link(e.path()) else { continue };
        if target.extension().is_some_and(|x| x == "jsonl") && target.starts_with(&root) {
            return Some(target);
        }
    }
    None
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_table_survives_arguments_with_spaces() {
        let t = parse_table("  123     1 /usr/bin/claude --model opus\n 7 123 bash -lc echo\nмусор\n");
        assert_eq!(t.len(), 2, "строка без числового pid не должна ронять разбор");
        assert_eq!(t[0].pid, 123);
        assert_eq!(t[0].ppid, 1);
        assert_eq!(t[0].args, "/usr/bin/claude --model opus");
    }

    #[test]
    fn agents_are_recognised_by_the_binary_not_by_a_mention() {
        assert_eq!(classify("/home/u/.local/bin/claude"), Some("claude"));
        assert_eq!(classify("claude --dangerously-skip-permissions"), Some("claude"));
        assert_eq!(classify("/usr/bin/codex exec"), Some("codex"));
        // npm-установка: настоящий бинарь — интерпретатор, агент в аргументе
        assert_eq!(
            classify("node /home/u/.npm-global/lib/node_modules/@anthropic-ai/claude-code/cli.js"),
            Some("claude")
        );
        assert_eq!(classify("node /home/u/.nvm/versions/node/v22/bin/claude"), Some("claude"));
        // упоминание — не запуск
        assert_eq!(classify("grep -r claude ."), None);
        assert_eq!(classify("vim codex.md"), None);
        assert_eq!(classify("node /srv/claudius/server.js"), None, "похожее имя — не агент");
        assert_eq!(classify(""), None);
    }

    #[test]
    fn pane_is_found_through_the_shell_between_it_and_the_agent() {
        let pane = super::super::tmux::Pane {
            pane: "%4".into(),
            session: "work".into(),
            pid: 100,
            cwd: "/srv/app".into(),
        };
        let by_pane_pid: HashMap<i64, &super::super::tmux::Pane> = [(100, &pane)].into();
        // 300 (агент) → 200 (bash) → 100 (пана)
        let parent: HashMap<i64, i64> = [(300, 200), (200, 100), (100, 1)].into();
        assert_eq!(ancestor_pane(300, &parent, &by_pane_pid).map(|p| p.pane.clone()), Some("%4".into()));
        // процесс не из паны — пусто, а не «первая попавшаяся»
        let orphan: HashMap<i64, i64> = [(900, 1)].into();
        assert!(ancestor_pane(900, &orphan, &by_pane_pid).is_none());
    }

    #[test]
    fn a_stale_transcript_is_not_attributed_to_a_live_agent() {
        let old = now_ms() - 6 * 3600 * 1000;
        let stale = vec![json!({
            "cwd": "/srv/app",
            "sessions": [{ "id": "s1", "at": old, "path": "/h/.claude/projects/-srv-app/s1.jsonl" }],
        })];
        assert!(guess_transcript("/srv/app", &stale).is_none(), "час прошёл — это прошлая сессия");

        let fresh = vec![json!({
            "cwd": "/srv/app",
            "sessions": [{ "id": "s1", "at": now_ms(), "path": "/h/.claude/projects/-srv-app/s1.jsonl" }],
        })];
        assert_eq!(
            guess_transcript("/srv/app", &fresh),
            Some(PathBuf::from("/h/.claude/projects/-srv-app/s1.jsonl"))
        );
        // чужой проект не подставляем, каталога не знаем — молчим
        assert!(guess_transcript("/srv/other", &fresh).is_none());
        assert!(guess_transcript("", &fresh).is_none());
    }

    #[tokio::test]
    async fn snapshot_finds_this_very_process_alive() {
        let me = std::process::id() as i64;
        let home = std::env::temp_dir().join(format!("jarvis-live-{me}"));
        let s = snapshot(&home, &[me, 0, -1]).await;
        assert_eq!(s.alive, vec![me], "нули и отрицательные не спрашиваем вовсе");

        // пустой список — пустой ответ, а не «все живы»
        assert!(snapshot(&home, &[]).await.alive.is_empty());
    }
}
