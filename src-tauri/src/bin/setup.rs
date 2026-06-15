//! Установщик интеграции Jarvis ⇄ Claude Code.
//!
//!   jarvis-setup install     — вшить хуки в ~/.claude/settings.json
//!   jarvis-setup uninstall   — вычистить свои записи
//!   jarvis-setup status      — показать, что установлено
//!
//! Принципы: merge, не overwrite; идемпотентно; бэкап перед записью;
//! атомарная запись (tmp + rename); битый JSON не трогаем.
//!
//! Шимы (jarvis-hook, claude-shim, tmux.conf) вшиты в бинарь include_str! —
//! установщик самодостаточен и не зависит от расположения исходников.

use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const HOOK_SRC: &str = include_str!("../../../bin/jarvis-hook");
const SHIM_SRC: &str = include_str!("../../../bin/claude-shim");
const TMUX_CONF_SRC: &str = include_str!("../../../bin/jarvis-tmux.conf");

/// Признак «это наша запись» — путь шима в команде.
/// Ловит и абсолютный путь, и вариант с $HOME.
const MARKER: &str = ".jarvis/bin/jarvis-hook";

/// Событие Claude Code → аргумент шима.
const EVENTS: [(&str, &str); 8] = [
    ("SessionStart", "session-start"),
    ("UserPromptSubmit", "prompt"),
    ("PreToolUse", "pre-tool"),
    ("PostToolUse", "post-tool"),
    ("Notification", "notification"),
    ("Stop", "stop"),
    ("StopFailure", "stop-failure"),
    ("SessionEnd", "session-end"),
];

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("нет $HOME"))
}

fn jarvis_dir() -> PathBuf {
    home().join(".jarvis")
}

fn hook_dst() -> PathBuf {
    jarvis_dir().join("bin/jarvis-hook")
}

fn shims_dir() -> PathBuf {
    jarvis_dir().join("shims")
}

fn shim_dst() -> PathBuf {
    shims_dir().join("claude")
}

fn tmux_conf_dst() -> PathBuf {
    jarvis_dir().join("tmux.conf")
}

fn settings_path() -> PathBuf {
    home().join(".claude/settings.json")
}

/* ================= Piper: бинарь + русский голос ================= */

/// Релиз rhasspy/piper, ассет macOS arm64. Проверено: gzip-архив, HTTP 200.
const PIPER_ARCHIVE_URL: &str =
    "https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_macos_aarch64.tar.gz";
/// Русский голос ru_RU-irina-medium из rhasspy/piper-voices.
const PIPER_VOICE_ONNX_URL: &str = "https://huggingface.co/rhasspy/piper-voices/resolve/main/ru/ru_RU/irina/medium/ru_RU-irina-medium.onnx";
const PIPER_VOICE_JSON_URL: &str = "https://huggingface.co/rhasspy/piper-voices/resolve/main/ru/ru_RU/irina/medium/ru_RU-irina-medium.onnx.json";

fn piper_dir() -> PathBuf {
    jarvis_dir().join("piper")
}

fn piper_bin() -> PathBuf {
    piper_dir().join("piper")
}

fn voices_dir() -> PathBuf {
    jarvis_dir().join("voices")
}

fn voice_onnx() -> PathBuf {
    voices_dir().join("ru.onnx")
}

fn voice_json() -> PathBuf {
    voices_dir().join("ru.onnx.json")
}

fn jarvis_settings_path() -> PathBuf {
    jarvis_dir().join("settings.json")
}

/// Скачать `url` в `dst` атомарно: curl в `.tmp`, потом rename.
fn download_to(url: &str, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst.parent().unwrap()).map_err(|e| format!("mkdir: {e}"))?;
    let tmp = dst.with_file_name(format!(
        ".{}.tmp-{}",
        dst.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let status = Command::new("curl")
        .args(["-fSL", "-o"])
        .arg(&tmp)
        .arg(url)
        .status()
        .map_err(|e| format!("запуск curl: {e}"))?;
    if !status.success() {
        let _ = fs::remove_file(&tmp);
        return Err(format!("curl не смог скачать {url}"));
    }
    fs::rename(&tmp, dst).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Прописать voice.voicePath в ~/.jarvis/settings.json — только если не задан.
/// Битый/отсутствующий файл → {}; чужие ключи не трогаем; атомарная запись + бэкап.
fn set_default_voice_path(onnx: &Path) -> Result<(), String> {
    let path = jarvis_settings_path();
    let mut json: Value = if path.exists() {
        match fs::read_to_string(&path) {
            Ok(raw) if !raw.trim().is_empty() => serde_json::from_str(&raw).unwrap_or_else(|_| json!({})),
            _ => json!({}),
        }
    } else {
        json!({})
    };
    if !json.is_object() {
        json = json!({});
    }
    // voice.voicePath уже задан непустой строкой — не клобберим.
    let already = json
        .pointer("/voice/voicePath")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if already {
        return Ok(());
    }
    if json.get("voice").map(|v| !v.is_object()).unwrap_or(true) {
        json["voice"] = json!({});
    }
    json["voice"]["voicePath"] = json!(onnx.display().to_string());

    if path.exists() {
        backup(&path);
    }
    fs::create_dir_all(jarvis_dir()).map_err(|e| format!("mkdir ~/.jarvis: {e}"))?;
    atomic_write(&path, &(serde_json::to_string_pretty(&json).unwrap() + "\n"));
    Ok(())
}

/// Установить Piper: бинарь + один русский голос. Идемпотентно, атомарно.
/// Любая ошибка → Err (вызывающий трактует как не-фатальную).
fn install_piper() -> Result<(), String> {
    fs::create_dir_all(piper_dir()).map_err(|e| format!("mkdir piper: {e}"))?;
    fs::create_dir_all(voices_dir()).map_err(|e| format!("mkdir voices: {e}"))?;

    // 1. Бинарь
    if !piper_bin().exists() {
        let tmp_arc = piper_dir().join(".piper.tar.gz.tmp");
        download_to(PIPER_ARCHIVE_URL, &tmp_arc)?;
        // Архив раскрывается в каталог `piper/` с бинарём внутри.
        // Распакуем во временный каталог и вытащим именно исполняемый `piper`.
        let extract_dir = piper_dir().join(".extract.tmp");
        let _ = fs::remove_dir_all(&extract_dir);
        fs::create_dir_all(&extract_dir).map_err(|e| format!("mkdir extract: {e}"))?;
        let status = Command::new("tar")
            .arg("-xzf")
            .arg(&tmp_arc)
            .arg("-C")
            .arg(&extract_dir)
            .status()
            .map_err(|e| format!("запуск tar: {e}"))?;
        let _ = fs::remove_file(&tmp_arc);
        if !status.success() {
            let _ = fs::remove_dir_all(&extract_dir);
            return Err("tar не смог распаковать архив piper".into());
        }
        // Бинарь лежит в piper/piper внутри архива.
        let src_bin = extract_dir.join("piper/piper");
        if !src_bin.exists() {
            let _ = fs::remove_dir_all(&extract_dir);
            return Err(format!("в архиве нет {}", src_bin.display()));
        }
        // Скопировать ВЕСЬ каталог piper/ (там лежат .dylib рядом с бинарём).
        for entry in fs::read_dir(extract_dir.join("piper"))
            .map_err(|e| format!("чтение распакованного: {e}"))?
        {
            let entry = entry.map_err(|e| format!("элемент: {e}"))?;
            let to = piper_dir().join(entry.file_name());
            fs::rename(entry.path(), &to).map_err(|e| format!("перенос {}: {e}", to.display()))?;
        }
        let _ = fs::remove_dir_all(&extract_dir);
        fs::set_permissions(piper_bin(), fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod piper: {e}"))?;
    }

    // 2. Голос (две части)
    if !voice_onnx().exists() {
        download_to(PIPER_VOICE_ONNX_URL, &voice_onnx())?;
    }
    if !voice_json().exists() {
        download_to(PIPER_VOICE_JSON_URL, &voice_json())?;
    }

    // 3. Прописать путь голоса по умолчанию (если не задан).
    set_default_voice_path(&voice_onnx())?;
    Ok(())
}

/* ================= managed-блок в rc-файлах ================= */
/* Блок живёт между маркерами и заменяется целиком — merge, не overwrite. */

pub const BEGIN: &str = "# >>> jarvis >>>";
pub const END: &str = "# <<< jarvis <<<";

fn block_body(shims_dir: &str) -> String {
    format!(
        "{BEGIN}\n# Управляется Jarvis (npm run setup/teardown) — не редактируй вручную\nexport PATH=\"{shims_dir}:$PATH\"\n{END}"
    )
}

fn has_block(content: &str) -> bool {
    content.contains(BEGIN) && content.contains(END)
}

/// Вставить или заменить блок. Идемпотентно: повторный вызов ничего не меняет.
fn merge_block(content: &str, shims_dir: &str) -> String {
    let block = block_body(shims_dir);
    if has_block(content) {
        let re = regex::Regex::new(&format!(
            "{}[\\s\\S]*?{}",
            regex::escape(BEGIN),
            regex::escape(END)
        ))
        .unwrap();
        // NoExpand: в блоке есть "$PATH" — без него regex счёл бы это группой
        return re.replace_all(content, regex::NoExpand(block.as_str())).into_owned();
    }
    let sep = if !content.is_empty() && !content.ends_with('\n') { "\n" } else { "" };
    format!("{content}{sep}\n{block}\n")
}

/// Убрать блок вместе с окружающими его пустыми строками.
fn remove_block(content: &str) -> String {
    if !has_block(content) {
        return content.to_string();
    }
    let re = regex::Regex::new(&format!(
        "\\n*{}[\\s\\S]*?{}\\n?",
        regex::escape(BEGIN),
        regex::escape(END)
    ))
    .unwrap();
    let out = re.replace_all(content, "\n").into_owned();
    regex::Regex::new("\n{3,}").unwrap().replace_all(&out, "\n\n").into_owned()
}

/* ================= helpers ================= */

fn read_settings() -> (bool, Value) {
    let path = settings_path();
    if !path.exists() {
        return (false, json!({}));
    }
    // нечитаемый файл (права, не-UTF-8) — отказ, а не тихий overwrite пустым
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => {
            eprintln!("✗ не смог прочитать {}: {err} — не трогаю.", path.display());
            std::process::exit(1);
        }
    };
    if raw.trim().is_empty() {
        return (true, json!({}));
    }
    match serde_json::from_str(&raw) {
        Ok(v) => (true, v),
        Err(_) => {
            eprintln!("✗ {} содержит невалидный JSON — не трогаю.", path.display());
            eprintln!("  Почини файл вручную и запусти setup ещё раз.");
            std::process::exit(1);
        }
    }
}

fn atomic_write(file: &Path, content: &str) {
    let tmp = file.with_file_name(format!(
        ".{}.tmp-{}",
        file.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    fs::write(&tmp, content).expect("запись tmp-файла");
    fs::rename(&tmp, file).expect("rename tmp-файла");
}

fn backup(file: &Path) -> Option<PathBuf> {
    if !file.exists() {
        return None;
    }
    let stamp = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S-%3fZ");
    let dst = PathBuf::from(format!("{}.bak-{stamp}", file.display()));
    fs::copy(file, &dst).ok()?;
    Some(dst)
}

fn is_ours(hook: &Value) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.contains(MARKER))
}

fn group_has_ours(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| hooks.iter().any(is_ours))
}

fn event_installed(json: &Value, event: &str) -> bool {
    json.pointer(&format!("/hooks/{event}"))
        .and_then(Value::as_array)
        .is_some_and(|arr| arr.iter().any(group_has_ours))
}

fn claude_found() -> bool {
    Command::new("/bin/sh")
        .args(["-c", "command -v claude"])
        .stdout(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn tmux_found() -> bool {
    Command::new("tmux")
        .arg("-V")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// rc-файлы, которые правим: zsh всегда, bash — если он login shell.
fn rc_files() -> Vec<PathBuf> {
    let mut files = vec![home().join(".zshrc")];
    if std::env::var("SHELL").unwrap_or_default().ends_with("bash") {
        files.push(home().join(".bashrc"));
        files.push(home().join(".bash_profile"));
    }
    files
}

fn live_tmux_sessions() -> Vec<String> {
    Command::new("tmux")
        .args(["-L", "jarvis", "list-sessions", "-F", "#{session_name}"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(String::from)
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn write_executable(dst: &Path, content: &str) {
    fs::create_dir_all(dst.parent().unwrap()).expect("mkdir");
    fs::write(dst, content).expect("запись шима");
    fs::set_permissions(dst, fs::Permissions::from_mode(0o755)).expect("chmod");
}

/* ================= commands ================= */

fn install() {
    // 1. Шим в ~/.jarvis/bin
    write_executable(&hook_dst(), HOOK_SRC);
    println!("✓ Шим установлен: {}", hook_dst().display());

    if !claude_found() {
        println!("⚠ Бинарь `claude` не найден в PATH — хуки всё равно пропишу,");
        println!("  они подхватятся, когда Claude Code появится.");
    }

    // 2. Merge в settings.json
    let (exists, mut json) = read_settings();
    if exists {
        if let Some(bak) = backup(&settings_path()) {
            println!("✓ Бэкап: {}", bak.display());
        }
    }
    fs::create_dir_all(home().join(".claude")).expect("mkdir ~/.claude");
    if !json.is_object() {
        json = serde_json::json!({});
    }
    if json.get("hooks").map(|h| !h.is_object()).unwrap_or(true) {
        json["hooks"] = serde_json::json!({});
    }

    let mut added = Vec::new();
    let mut present = Vec::new();
    for (event, arg) in EVENTS {
        if event_installed(&json, event) {
            present.push(event);
            continue;
        }
        let hooks = json["hooks"].as_object_mut().unwrap();
        let arr = hooks.entry(event).or_insert_with(|| serde_json::json!([]));
        if !arr.is_array() {
            *arr = serde_json::json!([]);
        }
        arr.as_array_mut().unwrap().push(serde_json::json!({
            "hooks": [{
                "type": "command",
                "command": format!("{} claude {arg}", hook_dst().display()),
                "timeout": 5,
            }],
        }));
        added.push(event);
    }

    if !added.is_empty() {
        atomic_write(
            &settings_path(),
            &(serde_json::to_string_pretty(&json).unwrap() + "\n"),
        );
        println!("✓ Добавлены хуки: {}", added.join(", "));
    }
    if !present.is_empty() {
        println!("• Уже стояли: {}", present.join(", "));
    }

    // 3. tmux-транспорт: шим claude + конфиг + PATH-блок в rc-файлах
    install_tmux_transport();

    // 4. Piper: бинарь + русский голос (не-фатально — демон не зависит от голоса)
    match install_piper() {
        Ok(()) => println!("✓ Piper установлен (~/.jarvis/piper, голос ~/.jarvis/voices)"),
        Err(e) => eprintln!("⚠ Piper не установлен ({e}); голос Piper недоступен, демон не затронут"),
    }

    println!("\nГотово. Активные сессии Claude Code нужно перезапустить —");
    println!("хуки снимаются снапшотом на старте сессии.");
    println!("Если Claude Code попросит подтвердить изменённые хуки (/hooks) — это наша запись.");
    println!("Чтобы шим подхватился в текущем шелле: exec zsh (или новая вкладка).");
}

fn install_tmux_transport() {
    if !tmux_found() {
        println!("⚠ tmux не найден — транспорт ввода пропускаю.");
        println!("  Поставь: brew install tmux — и запусти npm run setup ещё раз.");
        println!("  Уведомления и панель работают и без него.");
        return;
    }

    // Шим claude (паттерн pyenv)
    write_executable(&shim_dst(), SHIM_SRC);
    println!("✓ Шим claude: {}", shim_dst().display());

    // Конфиг отдельного tmux-сервера
    fs::write(tmux_conf_dst(), TMUX_CONF_SRC).expect("запись tmux.conf");
    println!("✓ tmux-конфиг: {}", tmux_conf_dst().display());

    // Managed-блок PATH в rc-файлах
    let shims = shims_dir().display().to_string();
    for rc in rc_files() {
        let existed = rc.exists();
        let content = if existed { fs::read_to_string(&rc).unwrap_or_default() } else { String::new() };
        let merged = merge_block(&content, &shims);
        if merged != content {
            if existed {
                if let Some(bak) = backup(&rc) {
                    println!("✓ Бэкап: {}", bak.display());
                }
            }
            atomic_write(&rc, &merged);
            println!("✓ PATH-блок в {}", rc.display());
        } else {
            println!("• PATH-блок уже стоит в {}", rc.display());
        }
    }
}

fn uninstall() {
    let (exists, mut json) = read_settings();
    if !exists || json.get("hooks").and_then(Value::as_object).is_none() {
        println!("• Записей Jarvis в settings.json нет.");
    } else {
        if let Some(bak) = backup(&settings_path()) {
            println!("✓ Бэкап: {}", bak.display());
        }

        let mut removed = Vec::new();
        let hooks = json["hooks"].as_object_mut().unwrap();
        let events: Vec<String> = hooks.keys().cloned().collect();
        for event in events {
            let Some(arr) = hooks.get_mut(&event).and_then(Value::as_array_mut) else { continue };
            let before = arr.len();
            // Выкидываем наши команды из групп, потом пустые группы
            for group in arr.iter_mut() {
                if let Some(group_hooks) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                    group_hooks.retain(|h| !is_ours(h));
                }
            }
            arr.retain(|g| {
                g.get("hooks").and_then(Value::as_array).is_some_and(|h| !h.is_empty())
            });
            if arr.len() != before {
                removed.push(event.clone());
            }
            if arr.is_empty() {
                hooks.remove(&event);
            }
        }
        let empty = hooks.is_empty();
        if empty {
            json.as_object_mut().unwrap().remove("hooks");
        }

        atomic_write(
            &settings_path(),
            &(serde_json::to_string_pretty(&json).unwrap() + "\n"),
        );
        if removed.is_empty() {
            println!("• Наших хуков не нашлось.");
        } else {
            println!("✓ Удалены хуки: {}", removed.join(", "));
        }
    }

    for f in [hook_dst(), jarvis_dir().join("run.sock"), shim_dst(), tmux_conf_dst()] {
        if fs::remove_file(&f).is_ok() {
            println!("✓ Удалён: {}", f.display());
        }
    }
    let _ = fs::remove_dir(shims_dir());

    // Де-мёрж PATH-блока из rc-файлов
    for rc in rc_files() {
        if !rc.exists() {
            continue;
        }
        let content = fs::read_to_string(&rc).unwrap_or_default();
        let cleaned = remove_block(&content);
        if cleaned != content {
            if let Some(bak) = backup(&rc) {
                println!("✓ Бэкап: {}", bak.display());
            }
            atomic_write(&rc, &cleaned);
            println!("✓ PATH-блок убран из {}", rc.display());
        }
    }

    let live = live_tmux_sessions();
    if !live.is_empty() {
        println!("⚠ Живые tmux-сессии Jarvis не тронуты: {}", live.join(", "));
        println!("  Подключиться: tmux -L jarvis attach -t <имя>; убить все: tmux -L jarvis kill-server");
    }
}

fn status() {
    let mark = |ok: bool| if ok { "✓" } else { "✗" };
    println!(
        "Шим:      {}",
        if hook_dst().exists() { format!("✓ {}", hook_dst().display()) } else { "✗ не установлен".into() }
    );
    println!(
        "Сокет:    {}",
        if jarvis_dir().join("run.sock").exists() { "✓ демон, похоже, запущен" } else { "✗ демон не запущен" }
    );
    println!("claude:   {}", if claude_found() { "✓ найден в PATH" } else { "✗ не найден" });

    println!("tmux-транспорт:");
    println!("  {}", if tmux_found() { "✓ tmux в PATH" } else { "✗ tmux не установлен (brew install tmux)" });
    println!("  {} шим claude ({})", mark(shim_dst().exists()), shim_dst().display());
    println!("  {} конфиг ({})", mark(tmux_conf_dst().exists()), tmux_conf_dst().display());
    for rc in rc_files() {
        let ok = rc.exists() && has_block(&fs::read_to_string(&rc).unwrap_or_default());
        println!("  {} PATH-блок в {}", mark(ok), rc.display());
    }
    let live = live_tmux_sessions();
    if !live.is_empty() {
        println!("  • живые сессии: {}", live.join(", "));
    }

    // Голос: Piper (бинарь + модель) и активный движок из ~/.jarvis/settings.json.
    let piper_installed = piper_bin().exists() && voice_onnx().exists() && voice_json().exists();
    let engine = voice_engine();
    let yn = |b: bool| if b { "да" } else { "нет" };
    println!("Голос:");
    println!(
        "  piper:  установлен={} (бинарь + модель на месте), активен={} (voice.engine={engine})",
        yn(piper_installed),
        yn(engine == "piper"),
    );
    println!("  silero: Фаза 2 — не установлен");

    let (exists, json) = read_settings();
    if !exists {
        println!("Settings: ✗ {} не существует", settings_path().display());
        return;
    }
    println!("Settings: {}", settings_path().display());
    for (event, _) in EVENTS {
        println!("  {} {event}", mark(event_installed(&json, event)));
    }
}

/// Активный голосовой движок из ~/.jarvis/settings.json (voice.engine), дефолт "piper".
/// Битый/отсутствующий файл — тоже "piper", без паники.
fn voice_engine() -> String {
    let path = jarvis_settings_path();
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(_) => return "piper".into(),
    };
    serde_json::from_str::<Value>(&raw)
        .ok()
        .and_then(|v| v.pointer("/voice/engine").and_then(Value::as_str).map(String::from))
        .unwrap_or_else(|| "piper".into())
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("install") => install(),
        Some("uninstall") => uninstall(),
        Some("status") => status(),
        _ => {
            println!("Использование: jarvis-setup <install|uninstall|status>");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = "/Users/test/.jarvis/shims";

    #[test]
    fn merge_into_empty() {
        let merged = merge_block("", DIR);
        assert!(has_block(&merged), "вставка в пустой файл");
        assert!(merged.contains(&format!("export PATH=\"{DIR}:$PATH\"")), "PATH правильный");
    }

    #[test]
    fn merge_preserves_existing_and_is_idempotent() {
        let existing = "# мой zshrc\nexport FOO=bar\n";
        let merged = merge_block(existing, DIR);
        assert!(merged.starts_with(existing), "существующее содержимое не тронуто");
        assert_eq!(merge_block(&merged, DIR), merged, "повторный merge идемпотентен");
    }

    #[test]
    fn merge_replaces_stale_block() {
        let merged = merge_block("export FOO=bar\n", DIR);
        let stale = merged.replace(DIR, "/old/path");
        let refreshed = merge_block(&stale, DIR);
        assert!(refreshed.contains(DIR) && !refreshed.contains("/old/path"), "устаревший блок заменяется");
        assert_eq!(refreshed.matches(BEGIN).count(), 1, "блок ровно один");
    }

    #[test]
    fn remove_block_keeps_foreign_content() {
        let merged = merge_block("# мой zshrc\nexport FOO=bar\n", DIR);
        let removed = remove_block(&merged);
        assert!(!has_block(&removed), "демёрж убирает блок");
        assert!(removed.contains("export FOO=bar"), "демёрж сохраняет чужое");
        assert_eq!(remove_block(&removed), removed, "повторный демёрж идемпотентен");
    }

    #[test]
    fn ours_detection() {
        let ours = json!({ "command": "/Users/x/.jarvis/bin/jarvis-hook claude stop" });
        let foreign = json!({ "command": "/usr/local/bin/other-hook" });
        assert!(is_ours(&ours));
        assert!(!is_ours(&foreign));
    }

    #[test]
    fn embedded_assets_look_sane() {
        assert!(HOOK_SRC.starts_with("#!/bin/sh"));
        assert!(HOOK_SRC.contains("JARVIS_IGNORE"));
        assert!(SHIM_SRC.contains("tmux"));
        assert!(TMUX_CONF_SRC.contains("status off"));
    }
}
