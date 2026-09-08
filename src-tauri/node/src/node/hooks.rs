//! Exact Codex trust repair for registered provider homes. The caller cannot
//! select a filesystem root or command. Provider configuration stays owned by
//! Codex, and disabled/foreign hooks are never automatically enabled/trusted.
use super::sources::{self, Source};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

static REPAIR: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn codex_program() -> Result<(PathBuf, String), String> {
    // Only PATH and the selected executable leave this process. Never print
    // the shell environment or any provider credentials.
    let script = super::tmux::with_agent_path("printf '\\036%s\\n' \"$(command -v codex)\" \"$PATH\"");
    let output = tokio::time::timeout(Duration::from_secs(10), tokio::process::Command::new("bash")
        .args(["-lc", &script]).env("JARVIS_IGNORE", "1")
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).output())
        .await.map_err(|_| "Не удалось найти Codex CLI за 10 секунд")?
        .map_err(|e| format!("Не удалось найти Codex CLI: {e}"))?;
    if !output.status.success() || output.stdout.len() > 128 * 1024 { return Err("Не удалось получить путь Codex CLI".into()); }
    let text = String::from_utf8(output.stdout).map_err(|_| "Некорректный путь Codex CLI")?;
    let mut values = text.lines().filter_map(|line| line.strip_prefix('\u{001e}'));
    let program = values.next().map(PathBuf::from).ok_or("Codex CLI не найден в PATH пользователя узла")?;
    let path = values.next().filter(|path| !path.is_empty()).ok_or("PATH пользователя узла пуст")?.to_string();
    if !program.is_absolute() || !program.is_file() { return Err("Codex CLI не найден в PATH пользователя узла".into()); }
    Ok((program, path))
}

async fn repair(source: &Source) -> Result<Value, String> {
    if source.agent != "codex" { return Err("Доверие через RPC применяется только к Codex".into()); }
    if !source.home.join("hooks.json").is_file() { return Err("Хуки Jarvis ещё не установлены для этого источника".into()); }
    let hook_bin = super::jarvis_dir().join("bin/jarvis-hook");
    if !hook_bin.is_file() { return Err("Установленный jarvis-hook не найден".into()); }
    let (program, path) = codex_program().await?;
    crate::codex_hooks::reconcile_with_path(&program, &source.home, &hook_bin, true, Some(path.as_ref())).await?;
    Ok(json!({"ok":true,"sourceId":source.id,"instanceId":source.id,"providerHome":source.home,"trusted":true}))
}

pub async fn repair_source(id: &str) -> Result<Value, String> {
    let _guard = REPAIR.try_lock().map_err(|_| "Настройка хуков уже выполняется")?;
    let source = sources::discover(&super::home_dir(), &super::jarvis_dir()).into_iter()
        .find(|source| source.id == id).ok_or("Источник агента не найден на узле")?;
    repair(&source).await
}

pub async fn repair_all() -> Value {
    let _guard = REPAIR.lock().await;
    let mut reports = vec![];
    for source in sources::discover(&super::home_dir(), &super::jarvis_dir()) {
        if source.agent != "codex" || !source.home.join("hooks.json").is_file() { continue; }
        reports.push(match repair(&source).await {
            Ok(value) => value,
            Err(error) => json!({"ok":false,"sourceId":source.id,"providerHome":source.home,"error":error}),
        });
    }
    json!({"ok":reports.iter().all(|row| row["ok"] == true),"sources":reports})
}
