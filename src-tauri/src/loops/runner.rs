//! Как цикл на самом деле что-то делает: песочница, вызов агента, гейты.
//!
//! Здесь нет ничего игрушечного — это настоящие подпроцессы: `git worktree`,
//! `claude -p`, команды гейтов. Всё, что возвращается наверх, получено от них.

use super::model::*;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// Что вернул headless-вызов агента.
#[derive(Debug, Default, Clone)]
pub struct AgentOut {
    pub text: String,
    pub tokens: u64,
    pub cost_usd: f64,
    /// Агент не отработал: не нашёлся бинарь, таймаут, ненулевой код.
    pub failed: bool,
}

/// Запустить команду и получить (код, вывод).
///
/// stderr сливаем в stdout: у гейта диагностика почти всегда именно там, а
/// человеку в экране итерации нужен весь вывод, а не половина.
pub async fn shell(cwd: &Path, command: &str, timeout: Duration) -> (i32, String) {
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .env("JARVIS_IGNORE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let Ok(Ok(out)) = tokio::time::timeout(timeout, cmd.output()).await else {
        return (-1, format!("не уложилось в {} с", timeout.as_secs()));
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        text.push_str(&err);
    }
    (out.status.code().unwrap_or(-1), text)
}

/// Хвост вывода: экрану итерации нужен конец, там и диагностика.
pub fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let from = all.len().saturating_sub(lines);
    all[from..].join("\n")
}

/// Поднять песочницу: отдельный worktree на своей ветке.
///
/// Радиус поражения — ветка: агент правит файлы часами без надзора, и делать
/// это в рабочем дереве человека значит однажды застать его посреди своей же
/// несохранённой правки.
pub async fn make_sandbox(item: &Loop, run_n: u32) -> Result<(PathBuf, String), String> {
    let repo = PathBuf::from(&item.sandbox.repo);
    if !repo.join(".git").exists() {
        return Err(format!("{} — не репозиторий git", repo.display()));
    }
    let branch = item.branch_for(run_n);
    if !item.sandbox.worktree {
        return Ok((repo, branch));
    }
    let dir = crate::util::jarvis_dir()
        .join("worktrees")
        .join(format!("{}-{}", super::model::slug(&item.name), run_n));
    if dir.exists() {
        // Тот же запуск поднимают второй раз — переиспользуем, а не падаем:
        // после перезапуска приложения продолжить работу важнее чистоты.
        return Ok((dir, branch));
    }
    std::fs::create_dir_all(dir.parent().unwrap_or(&dir)).map_err(|e| e.to_string())?;
    let cmd = format!(
        "git worktree add -b {} {} HEAD",
        crate::util::shell_quote(&branch),
        crate::util::shell_quote(&dir.to_string_lossy())
    );
    let (code, out) = shell(&repo, &cmd, Duration::from_secs(120)).await;
    if code != 0 {
        return Err(format!("git worktree: {}", tail(&out, 4)));
    }
    Ok((dir, branch))
}

/// Убрать песочницу. Ветку НЕ трогаем: в ней работа, и «убрал за собой» здесь
/// означало бы «стёр результат ночи».
pub async fn drop_sandbox(item: &Loop, dir: &Path) {
    if !item.sandbox.worktree || dir == Path::new(&item.sandbox.repo) {
        return;
    }
    let cmd = format!("git worktree remove --force {}", crate::util::shell_quote(&dir.to_string_lossy()));
    let _ = shell(Path::new(&item.sandbox.repo), &cmd, Duration::from_secs(60)).await;
}

/* ======================= ветки параллели ======================= */

/// Поднять рабочее место ветки: свой worktree на своей ветке от ЭТОГО состояния.
///
/// От `base` (а не от HEAD репозитория) принципиально: ветка отпочковывается от
/// того, что уже сделали шаги до ветвления. Иначе параллельная ветка начинала
/// бы работу с чистого листа и потом вливала бы поверх чужой работы конфликт
/// на конфликте.
pub async fn add_lane(repo: &Path, dir: &Path, branch: &str, base: &str) -> Result<(), String> {
    if dir.exists() {
        // Перезапуск приложения посреди прогона — переиспользуем. Но только
        // убедившись, что это ТА ЖЕ ветка: каталог, занятый чужой дорожкой,
        // молча принял бы чужую работу за свою.
        let (_, head) = shell(dir, "git rev-parse --abbrev-ref HEAD", Duration::from_secs(30)).await;
        if head.trim() == branch {
            return Ok(());
        }
        return Err(format!(
            "{} занят веткой «{}», а нужна «{branch}»",
            dir.display(),
            head.trim()
        ));
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let cmd = format!(
        "git worktree add -b {} {} {}",
        crate::util::shell_quote(branch),
        crate::util::shell_quote(&dir.to_string_lossy()),
        crate::util::shell_quote(base)
    );
    let (code, out) = shell(repo, &cmd, Duration::from_secs(120)).await;
    if code != 0 {
        return Err(format!("git worktree: {}", tail(&out, 4)));
    }
    Ok(())
}

/// Где сейчас голова этого дерева.
pub async fn head_sha(dir: &Path) -> Option<String> {
    let (code, out) = shell(dir, "git rev-parse HEAD", Duration::from_secs(30)).await;
    (code == 0).then(|| out.trim().to_string()).filter(|s| !s.is_empty())
}

/// Закоммитить всё, что агент наделал. `false` — коммитить было нечего.
///
/// Без коммита слияние веток невозможно в принципе: `git merge` сводит
/// КОММИТЫ, а работа агента до этого момента живёт незакоммиченными файлами.
pub async fn commit_all(dir: &Path, message: &str) -> Result<bool, String> {
    let (_, status) = shell(dir, "git status --porcelain", Duration::from_secs(60)).await;
    if status.trim().is_empty() {
        return Ok(false);
    }
    let cmd = format!(
        "git add -A && git -c user.name=Jarvis -c user.email=jarvis@local commit -q -m {}",
        crate::util::shell_quote(message)
    );
    let (code, out) = shell(dir, &cmd, Duration::from_secs(120)).await;
    if code != 0 {
        return Err(format!("git commit: {}", tail(&out, 4)));
    }
    Ok(true)
}

/// Чем кончилось слияние ветки.
#[derive(Debug, Clone, PartialEq)]
pub enum Merge {
    /// Влилось.
    Done,
    /// Вливать было нечего — ветка не ушла вперёд.
    Nothing,
    /// Конфликт: вот файлы. Слияние ОТКАЧЕНО — дерево осталось рабочим.
    Conflict(Vec<String>),
    /// Git отказал по другой причине.
    Failed(String),
}

/// Влить ветку в это дерево.
///
/// `strategy` — `ours` | `theirs` для `-X`: это не «выбрать сторону целиком», а
/// «при конфликте В ФАЙЛЕ взять эту сторону». Разница важна: чужие
/// неконфликтующие правки всё равно приезжают, и ветка не пропадает зря.
///
/// Конфликт НЕ откатывается здесь: дерево остаётся в состоянии merge с
/// маркерами в файлах — ровно то, что нужно увидеть агенту, если разбирать
/// конфликт поручено ему. Решает вызывающий: разобрать или откатить
/// ([`abort_merge`]). Оставлять дерево так навсегда нельзя ни в коем случае —
/// прогон в незавершённом слиянии не сможет сделать больше ничего.
pub async fn merge_lane(dir: &Path, branch: &str, strategy: Option<&str>) -> Merge {
    let x = match strategy {
        Some(s) => format!("-X {s} "),
        None => String::new(),
    };
    let cmd = format!(
        "git -c user.name=Jarvis -c user.email=jarvis@local merge --no-ff {x}-m {} {}",
        crate::util::shell_quote(&format!("слияние ветки {branch}")),
        crate::util::shell_quote(branch)
    );
    let (code, out) = shell(dir, &cmd, Duration::from_secs(300)).await;
    if code == 0 {
        return if out.contains("Already up to date") || out.contains("Already up-to-date") {
            Merge::Nothing
        } else {
            Merge::Done
        };
    }
    let (_, conflicted) = shell(dir, "git diff --name-only --diff-filter=U", Duration::from_secs(60)).await;
    let files: Vec<String> = conflicted.lines().map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
    if files.is_empty() {
        // Не конфликт, а отказ git по другой причине — дерево чинить нечем,
        // но и бросать его в полусостоянии нельзя.
        abort_merge(dir).await;
        return Merge::Failed(tail(&out, 6));
    }
    Merge::Conflict(files)
}

/// Откатить незавершённое слияние — дерево возвращается рабочим.
pub async fn abort_merge(dir: &Path) {
    let _ = shell(dir, "git merge --abort", Duration::from_secs(60)).await;
}

/// Дозакрыть слияние после того, как конфликт разрешён (агентом или стратегией).
pub async fn finish_merge(dir: &Path, message: &str) -> Result<(), String> {
    let cmd = format!(
        "git add -A && git -c user.name=Jarvis -c user.email=jarvis@local commit -q --no-edit -m {}",
        crate::util::shell_quote(message)
    );
    let (code, out) = shell(dir, &cmd, Duration::from_secs(120)).await;
    if code != 0 {
        return Err(tail(&out, 4));
    }
    Ok(())
}

/// Убрать рабочее место ветки. Ветку НЕ трогаем: в ней работа, и «убрал за
/// собой» здесь означало бы «стёр результат ночи».
pub async fn remove_lane(repo: &Path, dir: &Path) {
    let cmd = format!("git worktree remove --force {}", crate::util::shell_quote(&dir.to_string_lossy()));
    let _ = shell(repo, &cmd, Duration::from_secs(60)).await;
}

/// Вызвать агента headless и разобрать ответ.
///
/// `--output-format json` даёт не только текст, но и расход — без него
/// ограничитель по токенам было бы нечем кормить, а он половина смысла цикла.
pub async fn run_agent(
    agent: &str,
    cwd: &Path,
    prompt: &str,
    model: Option<&str>,
    timeout: Duration,
) -> AgentOut {
    if agent == "codex" {
        return run_codex(cwd, prompt, timeout).await;
    }
    let Some(bin) = crate::claude_bin::resolve_claude_bin() else {
        return AgentOut { failed: true, text: "claude не найден".into(), ..Default::default() };
    };
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("-p")
        .arg(prompt)
        .arg("--output-format")
        .arg("json")
        // Цикл работает в своей песочнице и без человека за спиной: спрашивать
        // разрешения не у кого, а остановка на вопросе означала бы зависший до
        // утра запуск.
        .arg("--dangerously-skip-permissions");
    if let Some(m) = model {
        cmd.arg("--model").arg(m);
    }
    cmd.current_dir(cwd)
        .env("JARVIS_IGNORE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    crate::claude_bin::apply_claude_auth(&mut cmd);
    let Ok(Ok(out)) = tokio::time::timeout(timeout, cmd.output()).await else {
        return AgentOut { failed: true, text: "агент не уложился в отведённое время".into(), ..Default::default() };
    };
    if !out.status.success() {
        return AgentOut { failed: true, text: "агент завершился с ошибкой".into(), ..Default::default() };
    }
    parse_agent_json(&String::from_utf8_lossy(&out.stdout))
}

/// Разбор `--output-format json`.
///
/// Формат внутренний и дрейфует, поэтому читаем defensive: нет расхода —
/// значит ноль, а не отказ от всей итерации.
pub fn parse_agent_json(stdout: &str) -> AgentOut {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        // Не json — значит агент печатал обычным текстом. Работа сделана, а
        // расход просто неизвестен.
        return AgentOut { text: stdout.trim().to_string(), ..Default::default() };
    };
    let text = v
        .get("result")
        .and_then(|r| r.as_str())
        .unwrap_or_default()
        .to_string();
    let usage = v.get("usage");
    let field = |name: &str| -> u64 {
        usage.and_then(|u| u.get(name)).and_then(|n| n.as_u64()).unwrap_or(0)
    };
    // Кэш считаем наравне: лимит аккаунта расходуется и им.
    let tokens = field("input_tokens")
        + field("output_tokens")
        + field("cache_creation_input_tokens")
        + field("cache_read_input_tokens");
    let cost = v.get("total_cost_usd").and_then(|c| c.as_f64()).unwrap_or(0.0);
    let failed = v.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
    AgentOut { text, tokens, cost_usd: cost, failed }
}

async fn run_codex(cwd: &Path, prompt: &str, timeout: Duration) -> AgentOut {
    // У Codex нет разбора расхода: его headless-вывод — обычный текст.
    // Ограничитель по токенам для него не работает, и это честнее, чем
    // подставить выдуманное число.
    let cmd = format!(
        "codex exec --dangerously-bypass-approvals-and-sandbox {}",
        crate::util::shell_quote(prompt)
    );
    let (code, out) = shell(cwd, &cmd, timeout).await;
    AgentOut { text: out, failed: code != 0, ..Default::default() }
}

/// Прогнать гейты по порядку. Первый красный останавливает: гонять остальные
/// нечего, итерация всё равно вернётся на доработку.
pub async fn run_gates(gates: &[Gate], cwd: &Path) -> Vec<GateRun> {
    let mut out = Vec::new();
    for g in gates {
        let (code, text) = shell(cwd, &g.command, Duration::from_secs(1800)).await;
        let ok = code == 0;
        out.push(GateRun { name: g.name.clone(), ok, output: tail(&text, 40) });
        if !ok {
            break;
        }
    }
    out
}

/// Файлы, которых итерация коснулась, — по самому git, а не по словам агента.
pub async fn touched_files(cwd: &Path) -> Vec<String> {
    let (code, out) = shell(cwd, "git status --porcelain", Duration::from_secs(30)).await;
    if code != 0 {
        return Vec::new();
    }
    out.lines()
        .filter_map(|l| l.get(3..).map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .take(50)
        .collect()
}

/// Дифф итерации — его и читает критик, и показывает экран итерации.
pub async fn diff(cwd: &Path, max_bytes: usize) -> String {
    let (_, out) = shell(cwd, "git --no-pager diff HEAD", Duration::from_secs(60)).await;
    if out.len() <= max_bytes {
        return out;
    }
    let cut = out.char_indices().map(|(i, _)| i).take_while(|i| *i <= max_bytes).last().unwrap_or(0);
    format!("{}\n… дифф обрезан", &out[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_json_gives_text_and_the_real_spend() {
        let out = parse_agent_json(
            r#"{"type":"result","result":"Починил флаки-тест","total_cost_usd":0.42,
                "usage":{"input_tokens":100,"output_tokens":20,
                         "cache_creation_input_tokens":3,"cache_read_input_tokens":7}}"#,
        );
        assert_eq!(out.text, "Починил флаки-тест");
        // Кэш входит в расход: лимит аккаунта тратится и им.
        assert_eq!(out.tokens, 130);
        assert!((out.cost_usd - 0.42).abs() < 1e-9);
        assert!(!out.failed);
    }

    #[test]
    fn plain_text_output_is_not_a_failure() {
        let out = parse_agent_json("просто текст без json");
        assert_eq!(out.text, "просто текст без json");
        assert_eq!(out.tokens, 0, "расход неизвестен — это ноль, а не отказ");
        assert!(!out.failed);
    }

    #[test]
    fn error_result_is_marked_failed() {
        let out = parse_agent_json(r#"{"type":"result","is_error":true,"result":"квота"}"#);
        assert!(out.failed);
    }

    #[test]
    fn missing_usage_does_not_break_the_run() {
        let out = parse_agent_json(r#"{"result":"готово"}"#);
        assert_eq!(out.tokens, 0);
        assert_eq!(out.cost_usd, 0.0);
        assert_eq!(out.text, "готово");
    }

    #[test]
    fn tail_keeps_the_end_where_the_diagnosis_is() {
        let text = (1..=100).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        assert_eq!(tail(&text, 3), "98\n99\n100");
        assert_eq!(tail("одна строка", 5), "одна строка");
    }

    #[tokio::test]
    async fn shell_reports_exit_code_and_merges_stderr() {
        let dir = std::env::temp_dir();
        let (code, out) = shell(&dir, "echo привет; echo беда >&2; exit 3", Duration::from_secs(10)).await;
        assert_eq!(code, 3);
        assert!(out.contains("привет") && out.contains("беда"), "{out}");
    }

    #[tokio::test]
    async fn gates_stop_at_the_first_red_one() {
        let dir = std::env::temp_dir();
        let gates = vec![
            Gate { name: "первый".into(), command: "true".into() },
            Gate { name: "второй".into(), command: "false".into() },
            Gate { name: "третий".into(), command: "true".into() },
        ];
        let runs = run_gates(&gates, &dir).await;
        assert_eq!(runs.len(), 2, "после красного гонять остальные нечего");
        assert!(runs[0].ok && !runs[1].ok);
    }
}
