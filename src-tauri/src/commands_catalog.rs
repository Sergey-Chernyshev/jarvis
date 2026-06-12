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

use crate::util::{claude_dir, ellipsize, now_ms};

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
    ("context", "Показать, чем занят контекст (токены по компонентам)", ""),
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

/// Простой frontmatter: --- key: value --- в начале файла.
fn parse_frontmatter(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if !text.starts_with("---") {
        return out;
    }
    let Some(end) = text[3..].find("\n---") else { return out };
    let re = regex::Regex::new(r"^([a-zA-Z0-9_-]+):\s*(.*)$").unwrap();
    for line in text[3..3 + end].lines() {
        if let Some(c) = re.captures(line) {
            let v = c[2].trim().trim_matches(|ch| ch == '"' || ch == '\'').to_string();
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

/// Рекурсивный обход каталога команд: подкаталоги → namespace через ':'.
fn scan_dir(dir: &Path, source: &'static str, prefix: &str) -> Vec<Command> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else { return out };
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
    let Ok(repos) = fs::read_dir(plugins_dir()) else { return out };
    for repo in repos.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
        let Ok(plugins) = fs::read_dir(repo.path()) else { continue };
        for plugin in plugins.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
            let plugin_name = plugin.file_name().to_string_lossy().into_owned();
            let Ok(versions) = fs::read_dir(plugin.path()) else { continue };
            for v in versions.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
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
        let mtime = fs::metadata(user_cmds_dir()).and_then(|m| m.modified()).ok();
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
            list.extend(scan_dir(&Path::new(cwd).join(".claude/commands"), "project", ""));
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
        let fm = parse_frontmatter("---\ndescription: \"Запустить тесты\"\nargument-hint: '<файл>'\n---\nтело");
        assert_eq!(fm.get("description").unwrap(), "Запустить тесты");
        assert_eq!(fm.get("argument-hint").unwrap(), "<файл>");
        assert!(parse_frontmatter("без фронтматтера").is_empty());
    }

    #[test]
    fn dedupe_prefers_described() {
        let list = vec![
            Command { name: "x".into(), description: String::new(), hint: String::new(), source: "init" },
            Command { name: "x".into(), description: "понятно".into(), hint: String::new(), source: "user" },
        ];
        let out = dedupe(list);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].description, "понятно");
    }
}
