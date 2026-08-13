//! Что именно тронул агент: объявления, попавшие под правку.
//!
//! В Air есть «символы» — навигация по объявлениям из языкового сервера. Своего
//! языкового сервера у панели нет и заводить его незачем: она надзиратель за
//! агентами, а не редактор. Но за символами в ревью ходят не ради навигации, а
//! ради вопроса «что именно он трогал» — и на него можно ответить честно и
//! дёшево: взять объявления файла и посмотреть, в какие из них попали
//! изменённые строки.
//!
//! Разбор лексический, а не семантический, и это сказано вслух: он не знает про
//! вложенность и макросы, но список «правки в `fn parse_status` и `fn collect`»
//! читается в сто раз быстрее, чем сорок номеров строк.

use serde::Serialize;

/// Объявление в файле.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Symbol {
    pub line: u32,
    pub name: String,
    /// «функция», «класс», «тип», «заголовок» — словом, а не значком.
    pub kind: String,
}

/// Объявления файла. Порядок — как в файле.
pub fn symbols_of(text: &str) -> Vec<Symbol> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = (i + 1) as u32;
        let s = raw.trim_start();
        if let Some(sym) = declaration(s) {
            out.push(Symbol {
                line,
                name: sym.0,
                kind: sym.1.to_string(),
            });
        }
    }
    out
}

/// Одно объявление из строки: (имя, чем является).
///
/// Языки перечислены те, на которых пишут в этом доме; неизвестный синтаксис
/// просто не даёт объявлений — пустой список честнее выдуманного.
fn declaration(s: &str) -> Option<(String, &'static str)> {
    // Заголовки markdown: для доков это и есть структура.
    if let Some(rest) = s.strip_prefix('#') {
        let title = rest.trim_start_matches('#').trim();
        if !title.is_empty() && s.starts_with("# ") || s.starts_with("## ") || s.starts_with("### ") {
            return Some((title.to_string(), "заголовок"));
        }
    }
    let strip = |p: &str| s.strip_prefix(p).map(str::trim_start);
    // Rust
    for p in ["pub async fn ", "pub fn ", "async fn ", "fn "] {
        if let Some(rest) = strip(p) {
            return Some((ident(rest), "функция"));
        }
    }
    for (p, kind) in [
        ("pub struct ", "тип"),
        ("struct ", "тип"),
        ("pub enum ", "тип"),
        ("enum ", "тип"),
        ("pub trait ", "тип"),
        ("trait ", "тип"),
        ("impl ", "блок impl"),
        // JS/TS
        ("export function ", "функция"),
        ("export async function ", "функция"),
        ("async function ", "функция"),
        ("function ", "функция"),
        ("export class ", "класс"),
        ("class ", "класс"),
        // Python
        ("async def ", "функция"),
        ("def ", "функция"),
        // Go
        ("func ", "функция"),
        // Swift / Kotlin
        ("struct ", "тип"),
    ] {
        if let Some(rest) = strip(p) {
            let name = ident(rest);
            if !name.is_empty() {
                return Some((name, kind));
            }
        }
    }
    None
}

/// Имя до первого разделителя: скобки, двоеточия, пробела.
fn ident(s: &str) -> String {
    s.chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect()
}

/// Объявления, в которые попали изменённые строки.
///
/// Правку относим к ПОСЛЕДНЕМУ объявлению перед ней: точнее лексический разбор
/// не умеет, а «правка в функции, объявленной выше» — ровно то, что человек и
/// имеет в виду. Строки до первого объявления не относим ни к чему: их место —
/// шапка файла, и врать про функцию там нельзя.
pub fn touched(symbols: &[Symbol], changed_lines: &[u32]) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = Vec::new();
    for line in changed_lines {
        let Some(sym) = symbols
            .iter()
            .filter(|s| s.line <= *line)
            .max_by_key(|s| s.line)
        else {
            continue;
        };
        if !out.iter().any(|s| s.line == sym.line) {
            out.push(sym.clone());
        }
    }
    out.sort_by_key(|s| s.line);
    out
}

/// Номера изменённых строк из ханков диффа: считаем по новой стороне — она и
/// есть то, что человек видит в файле сейчас.
pub fn changed_lines(hunks: &[crate::gitdiff::Hunk]) -> Vec<u32> {
    let mut out = Vec::new();
    for h in hunks {
        let mut no = h.new_start;
        for l in &h.lines {
            match l.t.as_str() {
                "+" => {
                    out.push(no);
                    no += 1;
                }
                "-" => {
                    // Удалённая строка живёт на старой стороне; относим её к
                    // тому месту, где была — это ближайшая новая строка.
                    out.push(no);
                }
                _ => no += 1,
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_are_found_across_the_languages_of_this_house() {
        let text = "\
use std::io;

pub fn collect() {}

struct Change {}

function render() {}

class Panel {}

def parse():

func main() {}
";
        let syms = symbols_of(text);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["collect", "Change", "render", "Panel", "parse", "main"]);
        assert_eq!(syms[0].kind, "функция");
        assert_eq!(syms[1].kind, "тип");
        assert_eq!(syms[3].kind, "класс");
        assert_eq!(syms[0].line, 3);
    }

    #[test]
    fn markdown_headings_are_the_structure_of_a_doc() {
        let syms = symbols_of("# Заголовок\nтекст\n## Раздел\n");
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].kind, "заголовок");
        assert_eq!(syms[1].name, "Раздел");
    }

    /// Неизвестный синтаксис не должен рождать выдуманные объявления.
    #[test]
    fn unknown_syntax_gives_nothing() {
        assert!(symbols_of("SELECT * FROM t;\n{\"json\": 1}\n").is_empty());
    }

    #[test]
    fn a_change_belongs_to_the_declaration_above_it() {
        let syms = symbols_of("fn первая() {}\nтело\nтело\nfn вторая() {}\nтело\n");
        // Правка в третьей строке — это «первая», в пятой — «вторая».
        let t = touched(&syms, &[3, 5]);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].name, "первая");
        assert_eq!(t[1].name, "вторая");
        // Повтор не задваивает: две правки в одной функции — одна строка списка.
        assert_eq!(touched(&syms, &[2, 3]).len(), 1);
    }

    /// Правка в шапке файла не относится ни к какой функции: врать про неё
    /// хуже, чем промолчать.
    #[test]
    fn changes_above_the_first_declaration_belong_to_nothing() {
        let syms = symbols_of("use std::io;\n\nfn первая() {}\n");
        assert!(touched(&syms, &[1]).is_empty());
    }

    #[test]
    fn changed_lines_are_counted_on_the_new_side() {
        use crate::gitdiff::{Hunk, Line};
        let h = Hunk {
            old_start: 10,
            new_start: 10,
            lines: vec![
                Line { t: " ".into(), s: "контекст".into() },
                Line { t: "+".into(), s: "новая".into() },
                Line { t: "-".into(), s: "старая".into() },
                Line { t: " ".into(), s: "контекст".into() },
            ],
        };
        // Контекст 10, добавленная 11, удалённая относится к 12 (месту, где
        // была), дальше контекст.
        assert_eq!(changed_lines(&[h]), vec![11, 12]);
    }
}
