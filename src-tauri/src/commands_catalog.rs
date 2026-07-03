//! Каталог слэш-команд Claude Code для палитры в панели.
//!
//! Источники (все без затрат квоты):
//!  - встроенные — статическая карта (машинных описаний для них нет);
//!  - кастомные .md в ~/.claude/commands и <cwd>/.claude/commands (frontmatter);
//!  - плагинные .md в ~/.claude/plugins/cache (имя namespace'ится по плагину).

use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::util::{claude_dir, codex_dir, ellipsize, now_ms};

#[derive(Debug, Clone, Serialize)]
pub struct Command {
    pub name: String,
    pub description: String,
    pub hint: String,
    pub source: &'static str,
}

/// Встроенные команды, полезные при управлении живой сессией.
/// model/effort с подсказкой — Jarvis открывает свой пикер значений вместо
/// интерактивного слайдера TUI (см. палитру в renderer).
const BUILTINS: [(&str, &str, &str); 16] = [
    ("model", "Сменить модель сессии", "‹выбрать›"),
    ("effort", "Уровень рассуждения", "‹выбрать›"),
    ("compact", "Сжать историю разговора, освободив контекст", ""),
    (
        "context",
        "Показать, чем занят контекст (токены по компонентам)",
        "",
    ),
    ("clear", "Очистить историю и начать заново", ""),
    ("usage", "Показать использование плана и лимиты", ""),
    ("cost", "Показать стоимость текущей сессии", ""),
    ("init", "Создать CLAUDE.md с описанием проекта", ""),
    ("review", "Code-review текущих изменений", ""),
    ("security-review", "Проверить изменения на уязвимости", ""),
    ("agents", "Управление субагентами", ""),
    ("memory", "Редактировать файлы памяти (CLAUDE.md)", ""),
    ("resume", "Возобновить прошлую сессию", ""),
    ("export", "Экспортировать разговор", ""),
    ("doctor", "Диагностика установки Claude Code", ""),
    ("help", "Список команд", ""),
];

/// Встроенные команды для интерактивной Codex-сессии.
const CODEX_BUILTINS: [(&str, &str, &str); 49] = [
    (
        "permissions",
        "Настроить, что Codex может делать без подтверждения",
        "",
    ),
    ("ide", "Добавить IDE-контекст в следующий запрос", "‹текст›"),
    ("keymap", "Настроить горячие клавиши TUI", ""),
    ("vim", "Переключить Vim-режим composer", ""),
    (
        "sandbox-add-read-dir",
        "Дать sandbox доступ на чтение к каталогу",
        "‹путь›",
    ),
    ("agent", "Переключить активный agent thread", ""),
    ("apps", "Просмотреть приложения и добавить их в prompt", ""),
    ("plugins", "Просмотреть и управлять Codex plugins", ""),
    ("hooks", "Просмотреть и управлять lifecycle hooks", ""),
    ("clear", "Очистить терминал и начать свежий чат", ""),
    ("archive", "Заархивировать текущую сессию и выйти", ""),
    ("delete", "Удалить текущую сессию и выйти", ""),
    (
        "compact",
        "Сжать видимый разговор и освободить контекст",
        "",
    ),
    ("copy", "Скопировать последний завершённый ответ Codex", ""),
    ("diff", "Показать Git diff, включая untracked файлы", ""),
    ("exit", "Выйти из Codex", ""),
    (
        "experimental",
        "Переключить экспериментальные возможности",
        "",
    ),
    (
        "approve",
        "Разрешить один повтор после auto-review отказа",
        "",
    ),
    ("memories", "Настроить использование и генерацию memory", ""),
    ("skills", "Просмотреть и использовать Codex skills", ""),
    (
        "import",
        "Импортировать Claude Code setup и недавние чаты",
        "",
    ),
    (
        "feedback",
        "Отправить диагностические логи команде Codex",
        "",
    ),
    ("init", "Создать AGENTS.md с инструкциями проекта", ""),
    ("logout", "Выйти из аккаунта Codex", ""),
    ("mcp", "Показать настроенные MCP tools", "‹verbose›"),
    ("mention", "Прикрепить файл или папку к разговору", "‹путь›"),
    ("model", "Сменить модель Codex", "‹выбрать›"),
    (
        "fast",
        "Переключить Fast tier для текущей модели",
        "on|off|status",
    ),
    (
        "plan",
        "Перейти в plan mode и опционально отправить prompt",
        "‹prompt›",
    ),
    (
        "goal",
        "Поставить, посмотреть, поставить на паузу или очистить goal",
        "‹objective|pause|resume|clear›",
    ),
    ("personality", "Выбрать стиль общения Codex", ""),
    ("ps", "Показать background terminals и свежий output", ""),
    ("stop", "Остановить background terminals текущей сессии", ""),
    ("fork", "Разветвить текущий разговор в новый thread", ""),
    ("side", "Начать короткий side-разговор", "‹prompt›"),
    ("btw", "Alias для side-разговора", "‹prompt›"),
    ("raw", "Переключить raw scrollback mode", ""),
    ("resume", "Возобновить прошлую сессию", "‹id›"),
    ("new", "Начать новый разговор в той же CLI-сессии", ""),
    ("quit", "Выйти из Codex", ""),
    ("review", "Запустить code review текущих изменений", ""),
    ("status", "Показать конфигурацию сессии и token usage", ""),
    (
        "usage",
        "Показать account token usage",
        "daily|weekly|cumulative",
    ),
    ("debug-config", "Показать диагностику config layers", ""),
    ("statusline", "Настроить поля status line", ""),
    ("title", "Настроить заголовок terminal/tab", ""),
    ("theme", "Выбрать syntax highlighting theme", ""),
    ("doctor", "Диагностика установки Codex", ""),
    ("help", "Список команд Codex", ""),
];

pub fn codex_builtins() -> Vec<Command> {
    CODEX_BUILTINS
        .iter()
        .map(|(name, description, hint)| Command {
            name: (*name).into(),
            description: (*description).into(),
            hint: (*hint).into(),
            source: "codex",
        })
        .collect()
}

pub fn codex_commands() -> Vec<Command> {
    let mut list = codex_builtins();
    list.extend(scan_codex_prompts_dir(&codex_prompts_dir()));
    dedupe(list)
}

pub struct Catalog {
    plugin_cache: Mutex<Option<Vec<Command>>>, // плагины меняются редко
    cwd_cache: Mutex<HashMap<String, (Vec<Command>, i64)>>, // cwd → (list, at)
    /// mtime ~/.claude/commands — замена fs.watch: правка команд сбрасывает кэши.
    user_cmds_mtime: Mutex<Option<std::time::SystemTime>>,
}

fn user_cmds_dir() -> PathBuf {
    claude_dir().join("commands")
}

fn plugins_dir() -> PathBuf {
    claude_dir().join("plugins").join("cache")
}

fn codex_prompts_dir() -> PathBuf {
    codex_dir().join("prompts")
}

/// Простой frontmatter: --- key: value --- в начале файла.
fn parse_frontmatter(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if !text.starts_with("---") {
        return out;
    }
    let Some(end) = text[3..].find("\n---") else {
        return out;
    };
    let re = regex::Regex::new(r"^([a-zA-Z0-9_-]+):\s*(.*)$").unwrap();
    for line in text[3..3 + end].lines() {
        if let Some(c) = re.captures(line) {
            let v = c[2]
                .trim()
                .trim_matches(|ch| ch == '"' || ch == '\'')
                .to_string();
            out.insert(c[1].to_string(), v);
        }
    }
    out
}

fn read_command_file(file: &Path, name: String, source: &'static str) -> Command {
    let fm = fs::read_to_string(file)
        .map(|t| parse_frontmatter(&t))
        .unwrap_or_default();
    Command {
        name,
        description: ellipsize(fm.get("description").map(String::as_str).unwrap_or(""), 200),
        hint: ellipsize(
            fm.get("argument-hint")
                .or_else(|| fm.get("argumentHint"))
                .map(String::as_str)
                .unwrap_or(""),
            80,
        ),
        source,
    }
}

fn read_codex_prompt_file(file: &Path, name: String) -> Command {
    let text = fs::read_to_string(file).unwrap_or_default();
    let fm = parse_frontmatter(&text);
    let description = fm
        .get("description")
        .cloned()
        .or_else(|| markdown_summary(&text))
        .unwrap_or_default();
    Command {
        name,
        description: ellipsize(&description, 200),
        hint: ellipsize(
            fm.get("argument-hint")
                .or_else(|| fm.get("argumentHint"))
                .map(String::as_str)
                .unwrap_or(""),
            80,
        ),
        source: "user",
    }
}

/// Рекурсивный обход каталога команд: подкаталоги → namespace через ':'.
fn scan_dir(dir: &Path, source: &'static str, prefix: &str) -> Vec<Command> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for e in entries.filter_map(|e| e.ok()) {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            out.extend(scan_dir(&path, source, &format!("{prefix}{name}:")));
        } else if let Some(base) = name.strip_suffix(".md") {
            out.push(read_command_file(&path, format!("{prefix}{base}"), source));
        }
    }
    out
}

/// Плагинные команды: <cache>/<repo>/<plugin>/<version>/commands/<cmd>.md → plugin:cmd
fn scan_plugins() -> Vec<Command> {
    let mut out = Vec::new();
    let Ok(repos) = fs::read_dir(plugins_dir()) else {
        return out;
    };
    for repo in repos.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
        let Ok(plugins) = fs::read_dir(repo.path()) else {
            continue;
        };
        for plugin in plugins.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
            let plugin_name = plugin.file_name().to_string_lossy().into_owned();
            let Ok(versions) = fs::read_dir(plugin.path()) else {
                continue;
            };
            for v in versions
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
            {
                let cmds = v.path().join("commands");
                if cmds.exists() {
                    for c in scan_dir(&cmds, "plugin", "") {
                        out.push(Command {
                            name: format!("{plugin_name}:{}", c.name),
                            ..c
                        });
                    }
                }
            }
        }
    }
    out
}

/// Codex custom prompts: ~/.codex/prompts/foo.md → /prompts:foo.
fn scan_codex_prompts_dir(dir: &Path) -> Vec<Command> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for e in entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
    {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(base) = name.strip_suffix(".md") {
            out.push(read_codex_prompt_file(&e.path(), format!("prompts:{base}")));
        }
    }
    out
}

fn markdown_summary(text: &str) -> Option<String> {
    let body = if text.starts_with("---") {
        match text[3..].find("\n---") {
            Some(end) => &text[3 + end + 4..],
            None => text,
        }
    } else {
        text
    };
    body.lines()
        .map(|l| l.trim().trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .map(|l| l.to_string())
}

/// Дедуп по имени: описание из файла важнее статической заглушки.
fn dedupe(list: Vec<Command>) -> Vec<Command> {
    let mut by_name: HashMap<String, Command> = HashMap::new();
    for c in list {
        match by_name.get(&c.name) {
            Some(prev) if !(prev.description.is_empty() && !c.description.is_empty()) => {}
            _ => {
                by_name.insert(c.name.clone(), c);
            }
        }
    }
    let mut out: Vec<Command> = by_name.into_values().collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

impl Catalog {
    pub fn new() -> Self {
        Self {
            plugin_cache: Mutex::new(None),
            cwd_cache: Mutex::new(HashMap::new()),
            user_cmds_mtime: Mutex::new(None),
        }
    }

    /// Аналог fs.watch(USER_CMDS) → invalidate(): дешёвый stat при каждом
    /// запросе каталога; mtime каталога сменился — сбросить оба кэша.
    fn invalidate_if_user_cmds_changed(&self) {
        let mtime = fs::metadata(user_cmds_dir())
            .and_then(|m| m.modified())
            .ok();
        let mut last = self.user_cmds_mtime.lock().unwrap();
        if *last != mtime {
            *last = mtime;
            *self.plugin_cache.lock().unwrap() = None;
            self.cwd_cache.lock().unwrap().clear();
        }
    }

    fn build(&self, cwd: Option<&str>) -> Vec<Command> {
        let plugins = {
            let mut cache = self.plugin_cache.lock().unwrap();
            cache.get_or_insert_with(scan_plugins).clone()
        };
        let mut list: Vec<Command> = BUILTINS
            .iter()
            .map(|(name, description, hint)| Command {
                name: (*name).into(),
                description: (*description).into(),
                hint: (*hint).into(),
                source: "builtin",
            })
            .collect();
        list.extend(scan_dir(&user_cmds_dir(), "user", ""));
        if let Some(cwd) = cwd {
            list.extend(scan_dir(
                &Path::new(cwd).join(".claude/commands"),
                "project",
                "",
            ));
        }
        list.extend(plugins);
        dedupe(list)
    }

    pub fn get_for_cwd(&self, cwd: Option<&str>) -> Vec<Command> {
        self.invalidate_if_user_cmds_changed();
        let key = cwd.unwrap_or("~").to_string();
        {
            let cache = self.cwd_cache.lock().unwrap();
            if let Some((list, at)) = cache.get(&key) {
                if now_ms() - at < 30_000 {
                    return list.clone();
                }
            }
        }
        let list = self.build(cwd);
        self.cwd_cache
            .lock()
            .unwrap()
            .insert(key, (list.clone(), now_ms()));
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_parses_quoted_values() {
        let fm = parse_frontmatter(
            "---\ndescription: \"Запустить тесты\"\nargument-hint: '<файл>'\n---\nтело",
        );
        assert_eq!(fm.get("description").unwrap(), "Запустить тесты");
        assert_eq!(fm.get("argument-hint").unwrap(), "<файл>");
        assert!(parse_frontmatter("без фронтматтера").is_empty());
    }

    #[test]
    fn dedupe_prefers_described() {
        let list = vec![
            Command {
                name: "x".into(),
                description: String::new(),
                hint: String::new(),
                source: "init",
            },
            Command {
                name: "x".into(),
                description: "понятно".into(),
                hint: String::new(),
                source: "user",
            },
        ];
        let out = dedupe(list);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].description, "понятно");
    }

    #[test]
    fn codex_builtins_have_core_commands_without_effort() {
        let out = codex_builtins();
        assert!(out.iter().any(|c| c.name == "model" && c.source == "codex"));
        assert!(out
            .iter()
            .any(|c| c.name == "review" && c.source == "codex"));
        assert!(out
            .iter()
            .any(|c| c.name == "resume" && c.source == "codex"));
        assert!(
            !out.iter().any(|c| c.name == "effort"),
            "Codex не показывает Claude-only /effort"
        );
    }

    #[test]
    fn codex_prompts_become_prompt_commands() {
        let dir = std::env::temp_dir().join(format!(
            "jarvis-codex-prompts-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("ship.md"),
            "---\ndescription: \"Подготовить релиз\"\nargument-hint: '<version>'\n---\nТело prompt",
        )
        .unwrap();
        fs::write(
            dir.join("plain.md"),
            "# Разобрать падение теста\n\nПроверь логи",
        )
        .unwrap();

        let out = scan_codex_prompts_dir(&dir);
        fs::remove_dir_all(&dir).ok();

        let ship = out.iter().find(|c| c.name == "prompts:ship").unwrap();
        assert_eq!(ship.description, "Подготовить релиз");
        assert_eq!(ship.hint, "<version>");
        assert_eq!(ship.source, "user");

        let plain = out.iter().find(|c| c.name == "prompts:plain").unwrap();
        assert_eq!(plain.description, "Разобрать падение теста");
    }
}
