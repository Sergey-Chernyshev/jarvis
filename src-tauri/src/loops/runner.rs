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

/// Each invocation owns a fresh process group. Dropping a timed out or
/// cancelled future terminates the shell AND its agent/tool descendants.
struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}

async fn output(
    cmd: &mut tokio::process::Command,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    cmd.process_group(0).kill_on_drop(true);
    let child = cmd
        .spawn()
        .map_err(|e| format!("не запустилась команда: {e}"))?;
    let _group = ProcessGroup(child.id().ok_or("у процесса нет PID")?);
    tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("не уложилось в {} с", timeout.as_secs()))?
        .map_err(|e| format!("не прочитал ответ команды: {e}"))
}

/// Запустить команду и получить (код, вывод).
///
/// stderr сливаем в stdout: у гейта диагностика почти всегда именно там, а
/// человеку в экране итерации нужен весь вывод, а не половина.
pub async fn shell(cwd: &Path, command: &str, timeout: Duration) -> (i32, String) {
    let mut cmd = tokio::process::Command::new("/bin/sh");
    // Inherit the daemon PATH; login profiles can print diagnostics or mutate it.
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .env("JARVIS_IGNORE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = match output(&mut cmd, timeout).await {
        Ok(out) => out,
        Err(why) => return (-1, why),
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
pub async fn make_sandbox(
    root: &Path,
    item: &Loop,
    run_n: u32,
) -> Result<(PathBuf, String), String> {
    let repo = PathBuf::from(&item.sandbox.repo);
    if !repo.join(".git").exists() {
        return Err(format!("{} — не репозиторий git", repo.display()));
    }
    let branch = item.branch_for(run_n);
    if !item.sandbox.worktree {
        return Ok((repo, branch));
    }
    let identity: String = item
        .id
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let dir = root.join("worktrees").join(format!(
        "{}-{identity}-{run_n}",
        super::model::slug(&item.name)
    ));
    if dir.exists() {
        validate_sandbox(item, &dir, &branch).await?;
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

/// Refuse unrelated/replaced folders, including an old worktree from another loop.
pub async fn validate_sandbox(item: &Loop, dir: &Path, branch: &str) -> Result<(), String> {
    let common = "git rev-parse --path-format=absolute --git-common-dir";
    let (a, repo) = shell(
        Path::new(&item.sandbox.repo),
        common,
        Duration::from_secs(30),
    )
    .await;
    let (b, wt) = shell(dir, common, Duration::from_secs(30)).await;
    let (c, actual) = shell(dir, "git branch --show-current", Duration::from_secs(30)).await;
    if a != 0
        || b != 0
        || c != 0
        || repo.trim() != wt.trim()
        || (item.sandbox.worktree && actual.trim() != branch)
    {
        return Err("песочница больше не принадлежит этому репозиторию и ветке".into());
    }
    Ok(())
}

/// Убрать песочницу. Ветку НЕ трогаем: в ней работа, и «убрал за собой» здесь
/// означало бы «стёр результат ночи».
pub async fn drop_sandbox(item: &Loop, dir: &Path) {
    if !item.sandbox.worktree || dir == Path::new(&item.sandbox.repo) {
        return;
    }
    let cmd = format!(
        "git worktree remove --force {}",
        crate::util::shell_quote(&dir.to_string_lossy())
    );
    let _ = shell(Path::new(&item.sandbox.repo), &cmd, Duration::from_secs(60)).await;
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
        return run_codex(cwd, prompt, model, timeout).await;
    }
    let Some(bin) = crate::claude_bin::resolve_claude_bin() else {
        return AgentOut {
            failed: true,
            text: "claude не найден".into(),
            ..Default::default()
        };
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
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::claude_bin::apply_claude_auth(&mut cmd);
    let out = match output(&mut cmd, timeout).await {
        Ok(out) => out,
        Err(why) => {
            return AgentOut {
                failed: true,
                text: why,
                ..Default::default()
            }
        }
    };
    if !out.status.success() {
        let mut parsed = parse_agent_json(&String::from_utf8_lossy(&out.stdout));
        parsed.failed = true;
        if parsed.text.is_empty() {
            parsed.text = tail(&String::from_utf8_lossy(&out.stderr), 8);
        }
        if parsed.text.is_empty() {
            parsed.text = "агент завершился с ошибкой".into();
        }
        return parsed;
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
        return AgentOut {
            text: stdout.trim().to_string(),
            failed: stdout.trim().is_empty(),
            ..Default::default()
        };
    };
    let text = v
        .get("result")
        .and_then(|r| r.as_str())
        .unwrap_or_default()
        .to_string();
    let usage = v.get("usage");
    let field = |name: &str| -> u64 {
        usage
            .and_then(|u| u.get(name))
            .and_then(|n| n.as_u64())
            .unwrap_or(0)
    };
    // Кэш считаем наравне: лимит аккаунта расходуется и им.
    let tokens = field("input_tokens")
        + field("output_tokens")
        + field("cache_creation_input_tokens")
        + field("cache_read_input_tokens");
    let cost = v
        .get("total_cost_usd")
        .and_then(|c| c.as_f64())
        .unwrap_or(0.0);
    let failed = v.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false)
        || v.get("subtype")
            .and_then(|s| s.as_str())
            .is_some_and(|s| s.starts_with("error"))
        || text.trim().is_empty();
    AgentOut {
        text,
        tokens,
        cost_usd: cost,
        failed,
    }
}

async fn run_codex(cwd: &Path, prompt: &str, model: Option<&str>, timeout: Duration) -> AgentOut {
    let Some(bin) = crate::backend::codex::resolve_codex_bin() else {
        return AgentOut {
            failed: true,
            text: "codex не найден".into(),
            ..Default::default()
        };
    };
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args([
        "exec",
        "--json",
        "--dangerously-bypass-approvals-and-sandbox",
    ]);
    if let Some(model) = model {
        cmd.args(["--model", model]);
    }
    cmd.arg("--")
        .arg(prompt)
        .current_dir(cwd)
        .env("JARVIS_IGNORE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = match output(&mut cmd, timeout).await {
        Ok(out) => out,
        Err(why) => {
            return AgentOut {
                failed: true,
                text: why,
                ..Default::default()
            }
        }
    };
    let mut parsed = parse_codex_jsonl(&String::from_utf8_lossy(&out.stdout));
    parsed.failed |= !out.status.success();
    if parsed.failed && parsed.text.is_empty() {
        parsed.text = tail(&String::from_utf8_lossy(&out.stderr), 8);
    }
    parsed
}

fn parse_codex_jsonl(stdout: &str) -> AgentOut {
    let mut out = AgentOut::default();
    let mut completed = false;
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match v["type"].as_str().unwrap_or_default() {
            "item.completed" if v["item"]["type"] == "agent_message" => {
                if let Some(text) = v["item"]["text"].as_str() {
                    if !out.text.is_empty() {
                        out.text.push('\n');
                    }
                    out.text.push_str(text);
                }
            }
            "turn.completed" => {
                completed = true;
                // cached_input_tokens is a subset of input_tokens, not extra usage.
                out.tokens += v["usage"]["input_tokens"].as_u64().unwrap_or(0)
                    + v["usage"]["output_tokens"].as_u64().unwrap_or(0);
            }
            "turn.failed" | "error" => {
                out.failed = true;
                if let Some(text) = v["error"]["message"]
                    .as_str()
                    .or_else(|| v["message"].as_str())
                {
                    if !out.text.is_empty() {
                        out.text.push('\n');
                    }
                    out.text.push_str(text);
                }
            }
            _ => {}
        }
    }
    out.failed |= !completed || out.text.trim().is_empty();
    out
}

/// Прогнать гейты по порядку. Первый красный останавливает: гонять остальные
/// нечего, итерация всё равно вернётся на доработку.
pub async fn run_gates(gates: &[Gate], cwd: &Path) -> Vec<GateRun> {
    let mut out = Vec::new();
    for g in gates {
        let (code, text) = shell(cwd, &g.command, Duration::from_secs(1800)).await;
        let ok = code == 0;
        out.push(GateRun {
            name: g.name.clone(),
            ok,
            output: tail(&text, 40),
        });
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
    let cut = out
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|i| *i <= max_bytes)
        .last()
        .unwrap_or(0);
    format!("{}\n… дифф обрезан", &out[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_stream_counts_usage_and_requires_a_successful_terminal_event() {
        let lines = r#"{"type":"item.completed","item":{"type":"agent_message","text":"done"}}
{"type":"turn.completed","usage":{"input_tokens":9,"cached_input_tokens":4,"output_tokens":3}}"#;
        let out = parse_codex_jsonl(lines);
        assert!(!out.failed);
        assert_eq!(out.tokens, 12);
        assert_eq!(out.text, "done");
        assert!(
            parse_codex_jsonl(
                r#"{"type":"item.completed","item":{"type":"agent_message","text":"partial"}}"#
            )
            .failed
        );
        assert!(parse_agent_json("").failed);
        assert!(parse_agent_json(r#"{"subtype":"error_max_turns","result":"incomplete"}"#).failed);
    }

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
        let text = (1..=100)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(tail(&text, 3), "98\n99\n100");
        assert_eq!(tail("одна строка", 5), "одна строка");
    }

    #[tokio::test]
    async fn shell_reports_exit_code_and_merges_stderr() {
        let dir = std::env::temp_dir();
        let (code, out) = shell(
            &dir,
            "echo привет; echo беда >&2; exit 3",
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(code, 3);
        assert!(out.contains("привет") && out.contains("беда"), "{out}");
    }

    #[tokio::test]
    async fn gates_stop_at_the_first_red_one() {
        let dir = std::env::temp_dir();
        let gates = vec![
            Gate {
                name: "первый".into(),
                command: "true".into(),
            },
            Gate {
                name: "второй".into(),
                command: "false".into(),
            },
            Gate {
                name: "третий".into(),
                command: "true".into(),
            },
        ];
        let runs = run_gates(&gates, &dir).await;
        assert_eq!(runs.len(), 2, "после красного гонять остальные нечего");
        assert!(runs[0].ok && !runs[1].ok);
    }
}
