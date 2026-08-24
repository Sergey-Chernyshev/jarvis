//! Оживление старых сессий (`claude --resume`, `kimi -S`) — проверка ДО запуска.
//!
//! Живой факт, ради которого этот модуль существует: `claude --resume` поднимает
//! транскрипт молча, без запроса к модели, и точно так же молча оживляет файл,
//! обрубленный посреди строки, — до места обрыва, без единого предупреждения.
//! CLI выглядит здоровым. Значит, целостность обязаны проверять МЫ, а не он.
//!
//! Три чистые функции:
//! - [`check_integrity`] — вердикт по транскрипту: годен / оборван / битый / пуст;
//! - [`assess`] — что стоит оживление в токенах (мегабайты цену не предсказывают:
//!   в JSONL лежат выводы инструментов и субагентские ветки, которые в контекст
//!   не возвращаются — файл поменьше может нести контекста БОЛЬШЕ);
//! - [`revive_cost`] — деньги из токенов по цене холодного кэша первого хода.
//!
//! Ничего отсюда не подключено к остальному коду — только объявление модуля
//! в `main.rs`. Проводка (IPC/команды) — отдельный шаг, не этого модуля.

use serde::Serialize;
use serde_json::Value;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Хвост транскрипта, который читаем ради оценки цены оживления. Последняя
/// запись с `usage` почти всегда у самого конца файла (это же допущение уже
/// делает `agent::context::for_chat` с 512 КБ) — гонять по диску весь файл
/// в десятки мегабайт ради одного числа с конца незачем. Взято чуть щедрее:
/// это разовая проверка перед стартом сессии, а не поле в шапке чата, которое
/// перечитывается на каждый рендер.
const ASSESS_TAIL_BYTES: u64 = 2 * 1024 * 1024;

/// Порог доли битых строк, после которого «единичный сбой» становится
/// «системной порчей». Ниже порога — файл всё ещё можно попытаться оживить
/// (хвост цел, потеряна одна запись в середине); выше — доверять ленте нельзя.
const HEAVY_CORRUPTION_RATIO: f64 = 0.05;

// ---------------------------------------------------------------------------
// 1. Целостность транскрипта
// ---------------------------------------------------------------------------

/// Вердикт по целостности транскрипта — можно ли им оживлять сессию.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Хвост цел, реплики есть — годен к оживлению.
    Ok,
    /// В файле нет ни байта или ни одной строки — начатой сессии попросту нет.
    /// Отдельно от [`Verdict::NoMessages`]: там разбор строк идёт нормально,
    /// здесь разбирать нечего вовсе.
    Empty,
    /// Строки разобрались как JSON, но ни одна не оказалась репликой
    /// (`user`/`assistant`) — только служебные записи. Разговора не было.
    NoMessages,
    /// Последняя НЕПУСТАЯ строка не разобралась как JSON. Ровно то самое:
    /// обрыв копирования или падение диска приходится именно на хвост, а
    /// `claude --resume` оживит такой файл молча, до места обрыва.
    Truncated,
    /// Битые строки есть, но НЕ на хвосте — разбор сбоил в середине файла.
    /// Это не обрыв: реплики после битой строки на месте.
    Corrupted,
}

/// Итог проверки одного файла: вердикт + причина словами + счётчики, по
/// которым эту причину можно проверить самому.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Integrity {
    pub verdict: Verdict,
    /// Причина по-человечески: что увидели и что с этим делать. Не флаг —
    /// текст, из которого ясно, чинить файл или можно смело оживлять.
    pub reason: String,
    /// Непустых строк во всём файле.
    pub total_lines: u64,
    /// Из них не разобравшихся как JSON.
    pub bad_lines: u64,
    /// Из разобравшихся — реплик (`user`/`assistant`).
    pub message_lines: u64,
}

impl Integrity {
    fn empty(reason: impl Into<String>) -> Self {
        Integrity { verdict: Verdict::Empty, reason: reason.into(), total_lines: 0, bad_lines: 0, message_lines: 0 }
    }
}

/// Проверить целостность транскрипта. Читает файл ПОТОКОМ (`BufReader`,
/// построчно) — транскрипт бывает в десятки мегабайт, и грузить его целиком
/// в память ради одной проверки ни к чему.
///
/// Хвост проверяется строго: обрыв при копировании и при падении диска
/// приходится именно на последнюю строку, поэтому решение «оборван или нет»
/// зависит ТОЛЬКО от разбора последней непустой строки — не от доли битых
/// строк по всему файлу (это отдельный вопрос, см. [`Verdict::Corrupted`]).
pub fn check_integrity(path: &Path) -> Integrity {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return Integrity::empty(format!("файл не открывается ({e}) — считаем, что сессии нет")),
    };
    if meta.len() == 0 {
        return Integrity::empty("файл пустой — сессия ещё не написала ни одной строки");
    }
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => return Integrity::empty(format!("файл не читается ({e})")),
    };

    let mut total: u64 = 0;
    let mut bad: u64 = 0;
    let mut messages: u64 = 0;
    // Разобралась ли ПОСЛЕДНЯЯ непустая строка. Перезаписывается на каждой
    // непустой строке по порядку, так что к концу цикла несёт ответ ровно
    // про хвост — а не про долю по всему файлу.
    let mut tail_ok = true;

    for line in BufReader::new(file).lines() {
        let raw = match line {
            // Невалидный UTF-8 посреди строки — тот же обрыв, только на
            // байтовом уровне, а не на JSON-скобке: байты потеряны, строка бита.
            Err(_) => {
                total += 1;
                bad += 1;
                tail_ok = false;
                continue;
            }
            Ok(s) => s,
        };
        let t = raw.trim();
        if t.is_empty() {
            continue; // пустая строка (напр. финальный \n) — не запись, хвоста не меняет
        }
        total += 1;
        match serde_json::from_str::<Value>(t) {
            Ok(v) => {
                tail_ok = true;
                if is_message(&v) {
                    messages += 1;
                }
            }
            Err(_) => {
                bad += 1;
                tail_ok = false;
            }
        }
    }

    if total == 0 {
        return Integrity::empty("в файле нет ни одной строки — начатой сессии нет");
    }

    if !tail_ok {
        return Integrity {
            verdict: Verdict::Truncated,
            reason: "последняя строка транскрипта не разобралась как JSON — файл обрублен \
                     на хвосте (обрыв копирования или падение диска пришлись на последнюю \
                     запись); оживление молча покажет ленту только до места обрыва, дальше — тишина"
                .to_string(),
            total_lines: total,
            bad_lines: bad,
            message_lines: messages,
        };
    }

    if bad > 0 {
        let ratio = bad as f64 / total as f64;
        let reason = if ratio >= HEAVY_CORRUPTION_RATIO {
            format!(
                "{bad} из {total} строк не разбираются как JSON ({:.1}%) — это не единичный \
                 сбой, а системная порча файла; хвост цел, но доверять остальному нельзя",
                ratio * 100.0
            )
        } else {
            format!(
                "{bad} из {total} строк не разбираются как JSON ({:.2}%) — единичный сбой в \
                 середине файла, не обрыв: хвост цел, реплики после сбоя на месте",
                ratio * 100.0
            )
        };
        return Integrity { verdict: Verdict::Corrupted, reason, total_lines: total, bad_lines: bad, message_lines: messages };
    }

    if messages == 0 {
        return Integrity {
            verdict: Verdict::NoMessages,
            reason: "все строки разобрались, но реплик (user/assistant) в файле нет — \
                     оживлять нечего, разговора не было"
                .to_string(),
            total_lines: total,
            bad_lines: bad,
            message_lines: messages,
        };
    }

    Integrity {
        verdict: Verdict::Ok,
        reason: "хвост цел, реплики есть — годен к оживлению".to_string(),
        total_lines: total,
        bad_lines: bad,
        message_lines: messages,
    }
}

fn is_message(v: &Value) -> bool {
    matches!(v.get("type").and_then(Value::as_str), Some("user") | Some("assistant"))
}

// ---------------------------------------------------------------------------
// 2. Оценка цены оживления — в токенах, не в мегабайтах
// ---------------------------------------------------------------------------

/// Что мы знаем про цену оживления ОДНОГО транскрипта.
///
/// Мегабайты цену не предсказывают: в JSONL лежат выводы инструментов,
/// повторы и субагентские ветки, а в контекст они не возвращаются — файл
/// поменьше вполне может нести контекста больше, чем файл побольше.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Assessment {
    /// Токены контекста последнего usage: `input + cache_creation + cache_read`
    /// у последней записи ассистента (см. `agent::context::used_tokens` — тот
    /// же способ счёта, второго тут не заводим). `None` — ни одной записи с
    /// `usage` не нашлось: это отдельное состояние «не знаем», а не ноль —
    /// ноль прочитался бы как «оживление бесплатно».
    pub context_tokens: Option<u64>,
    pub file_bytes: u64,
    /// Мс эпохи последней реплики с валидным таймстампом в файле.
    pub last_message_at: Option<i64>,
    /// Сырой id модели последней записи с usage (не «человеческое» имя — его
    /// наводит вызывающий через `Backend::friendly_model`, если понадобится).
    pub model: Option<String>,
    /// Реплик (`user`/`assistant`) за ВСЮ сессию — считаем потоком по всему
    /// файлу, а не по хвосту: числу реплик мегабайты тоже не указ.
    pub message_count: u64,
}

/// Оценить транскрипт: токены контекста, размер, время и модель последней
/// реплики, число реплик за сессию.
///
/// ДОРОГАЯ: считает реплики полным проходом по файлу. На 50 МБ это секунды, на
/// каталоге из полутора тысяч транскриптов — минуты. Перечислениям нужен
/// [`assess_light`], а эта — только там, где число реплик действительно
/// показывают человеку.
pub fn assess(path: &Path) -> Assessment {
    Assessment { message_count: count_messages(path), ..assess_light(path) }
}

/// То же, но БЕЗ числа реплик (оно остаётся нулём) — и без полного прохода по
/// файлу вместе с ним.
///
/// Всё остальное и так берётся из хвоста: размер — из метаданных, токены, модель
/// и время последней реплики — из последних килобайт. Стоимость такой оценки не
/// зависит от размера файла, поэтому её не жалко звать хоть на каждый транскрипт
/// в каталоге.
pub fn assess_light(path: &Path) -> Assessment {
    let file_bytes = fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    // Токены/модель/время последней реплики — из ХВОСТА (см. ASSESS_TAIL_BYTES):
    // переиспользуем ровно те функции, которыми считает `agent::context`, а не
    // заводим второй способ счёта.
    let tail = crate::transcript::read_recent_text(path, ASSESS_TAIL_BYTES).unwrap_or_default();
    let tail_entries = crate::transcript::entries_from_text(&tail);
    let last = crate::agent::context::last_usage(&tail_entries);
    let last_message_at = tail_entries.iter().rev().find_map(|e| {
        e.get("timestamp").and_then(Value::as_str).and_then(crate::transcript::parse_ts)
    });

    Assessment {
        context_tokens: last.as_ref().map(|(u, _)| *u),
        file_bytes,
        last_message_at,
        model: last.map(|(_, m)| m).filter(|m| !m.is_empty()),
        message_count: 0,
    }
}

/// Число реплик за всю сессию: лёгкий потоковый проход по файлу, тип записи
/// без остального разбора. Отдельно от `check_integrity`: оценке цены не
/// нужна причина брака, только счётчик, и незачем гонять два прохода с одной
/// и той же логикой построчного чтения ради разных полей одной структуры.
fn count_messages(path: &Path) -> u64 {
    let Ok(file) = File::open(path) else { return 0 };
    let mut n = 0u64;
    // `.flatten()` тут — не срез до первой ошибки (как было бы с `map_while`),
    // а именно пропуск битых строк с продолжением счёта дальше: одна плохая
    // строка не должна занижать число реплик после неё.
    for line in BufReader::new(file).lines().flatten() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(t) {
            if is_message(&v) {
                n += 1;
            }
        }
    }
    n
}

// ---------------------------------------------------------------------------
// 3. Деньги из токенов
// ---------------------------------------------------------------------------

/// Цена первого хода после оживления — по модели, если она известна.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ReviveCost {
    /// Модель распознана — точная цена по прайсу `Backend::price`.
    Known { usd: f64, model: String },
    /// Модель не распознана, но вилка цен известных моделей известна:
    /// возвращаем диапазон, а не молча самую дешёвую ставку.
    Range { usd_low: f64, usd_high: f64 },
    /// Токенов контекста нет вовсе (см. [`Assessment::context_tokens`]) —
    /// считать не от чего.
    Unknown,
}

/// Перевести токены контекста в деньги первого хода после оживления.
///
/// ДОПУЩЕНИЕ (важно): первый запрос после `--resume`/`-S` идёт по ХОЛОДНОМУ
/// кэшу — сторона провайдера ещё не видела этот транскрипт в своём кэше,
/// поэтому ВЕСЬ контекст (в т.ч. то, что раньше было `cache_read`) оплачивается
/// как обычный вход по полной входной цене, а не по льготной ставке чтения
/// кэша. Это дороже, чем «тёплое» продолжение уже открытой сессии, — но именно
/// такой выглядит первый счёт после оживления, и здесь считаем ровно его.
///
/// `model` — сырой id из транскрипта (`Assessment::model`), не «человеческое»
/// имя: сюда же кладём результат `Backend::friendly_model`.
pub fn revive_cost(context_tokens: Option<u64>, model: Option<&str>) -> ReviveCost {
    let Some(tokens) = context_tokens else { return ReviveCost::Unknown };

    let backend = crate::backend::backend(crate::backend::Agent::Claude);
    let friendly = model.map(|m| backend.friendly_model(m));
    let known = friendly.as_deref().filter(|f| backend.models().iter().any(|(_, label)| label == f));

    match known {
        Some(friendly) => {
            let (input_price, _output_price) = backend.price(friendly);
            ReviveCost::Known { usd: tokens_to_usd(tokens, input_price), model: friendly.to_string() }
        }
        None => {
            // Модель незнакомая (или её вовсе не видно в транскрипте) — не
            // подставляем самую дешёвую ставку молча, а даём вилку по всем
            // известным ценам Claude.
            let prices: Vec<f64> = backend.models().iter().map(|(_, label)| backend.price(label).0).collect();
            let lo = prices.iter().cloned().fold(f64::INFINITY, f64::min);
            let hi = prices.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            ReviveCost::Range { usd_low: tokens_to_usd(tokens, lo), usd_high: tokens_to_usd(tokens, hi) }
        }
    }
}

fn tokens_to_usd(tokens: u64, price_per_million: f64) -> f64 {
    tokens as f64 / 1_000_000.0 * price_per_million
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Каждому тесту — свой файл во временной папке процесса: параллельные
    /// тесты не должны драться за один путь.
    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("jarvis-revive-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    fn write_lines(path: &Path, lines: &[&str]) {
        let mut f = File::create(path).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    // --- check_integrity -----------------------------------------------

    #[test]
    fn intact_transcript_is_ok() {
        let p = tmp("intact.jsonl");
        write_lines(
            &p,
            &[
                r#"{"type":"user","message":{"role":"user","content":"привет"}}"#,
                r#"{"type":"assistant","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":5}}}"#,
            ],
        );
        let r = check_integrity(&p);
        assert_eq!(r.verdict, Verdict::Ok, "{}", r.reason);
        assert_eq!(r.total_lines, 2);
        assert_eq!(r.bad_lines, 0);
        assert_eq!(r.message_lines, 2);
    }

    /// Живой случай владельца: 1174 целых строки, последняя обрублена посреди
    /// JSON. Должно ловиться именно как обрыв — не как «редкая порча».
    #[test]
    fn cut_mid_json_on_last_line_is_truncated_even_with_a_long_intact_prefix() {
        let p = tmp("truncated.jsonl");
        let mut f = File::create(&p).unwrap();
        for i in 0..1174 {
            writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"msg {i}"}}}}"#).unwrap();
        }
        // Последняя строка — обрублена посреди значения, без перевода строки в конце.
        write!(f, r#"{{"type":"assistant","message":{{"model":"claude-sonnet-4-5","usage":{{"inp"#).unwrap();
        drop(f);

        let r = check_integrity(&p);
        assert_eq!(r.verdict, Verdict::Truncated, "{}", r.reason);
        assert_eq!(r.total_lines, 1175);
        assert_eq!(r.bad_lines, 1, "битая только последняя строка — остальные 1174 целы");
        assert!(r.reason.contains("обрубл") || r.reason.contains("обрыв"), "{}", r.reason);
    }

    /// Битая строка В СЕРЕДИНЕ при целом хвосте — другой вердикт, чем обрыв:
    /// разговор после сбоя не потерян.
    #[test]
    fn a_single_bad_line_in_the_middle_differs_from_truncation() {
        let p = tmp("corrupted-middle.jsonl");
        let mut lines: Vec<String> = (0..20)
            .map(|i| format!(r#"{{"type":"user","message":{{"role":"user","content":"msg {i}"}}}}"#))
            .collect();
        lines[10] = "{ЭТО НЕ JSON вообще".to_string();
        lines.push(r#"{"type":"assistant","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":1}}}"#.to_string());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_lines(&p, &refs);

        let r = check_integrity(&p);
        assert_eq!(r.verdict, Verdict::Corrupted, "{}", r.reason);
        assert_ne!(r.verdict, Verdict::Truncated);
        assert_eq!(r.bad_lines, 1);
        assert_eq!(r.total_lines, 21);
    }

    /// Доля битых строк меняет причину, даже если вердикт один и тот же класс:
    /// «почти каждая десятая» — это не «единичный сбой».
    #[test]
    fn heavy_corruption_reads_differently_from_a_single_glitch() {
        let p = tmp("corrupted-heavy.jsonl");
        let mut lines: Vec<String> = (0..100)
            .map(|i| format!(r#"{{"type":"user","message":{{"role":"user","content":"msg {i}"}}}}"#))
            .collect();
        for i in (0..100).step_by(10) {
            lines[i] = "не json".to_string();
        }
        lines.push(r#"{"type":"assistant","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":1}}}"#.to_string());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_lines(&p, &refs);

        let r = check_integrity(&p);
        assert_eq!(r.verdict, Verdict::Corrupted);
        assert_eq!(r.bad_lines, 10);
        assert!(r.reason.contains("систем"), "{}", r.reason);
    }

    #[test]
    fn empty_file_is_not_ok() {
        let p = tmp("empty.jsonl");
        File::create(&p).unwrap();
        let r = check_integrity(&p);
        assert_eq!(r.verdict, Verdict::Empty, "{}", r.reason);
    }

    /// Файл без единого сообщения — не то же самое, что пустой файл: тут есть
    /// валидный JSON, просто ни строчки разговора.
    #[test]
    fn file_with_only_service_records_is_not_ok_but_is_not_empty_either() {
        let p = tmp("no-messages.jsonl");
        write_lines(
            &p,
            &[
                r#"{"type":"system","subtype":"init"}"#,
                r#"{"type":"summary","summary":"старая сводка"}"#,
            ],
        );
        let r = check_integrity(&p);
        assert_eq!(r.verdict, Verdict::NoMessages, "{}", r.reason);
        assert_ne!(r.verdict, Verdict::Empty, "разница между пустым файлом и файлом без сообщений обязана быть видна");
        assert_eq!(r.message_lines, 0);
    }

    #[test]
    fn missing_file_is_reported_as_empty_not_as_a_crash() {
        let p = tmp("does-not-exist.jsonl");
        let _ = std::fs::remove_file(&p);
        let r = check_integrity(&p);
        assert_eq!(r.verdict, Verdict::Empty);
    }

    // --- assess -----------------------------------------------------------

    /// Ровно тот случай, ради которого мегабайты забракованы: файл поменьше
    /// несёт БОЛЬШЕ контекста, чем файл побольше (числа — из живого замера
    /// владельца: 4.7 МБ/436 550 токенов против 37.4 МБ/310 579).
    #[test]
    fn a_smaller_file_can_carry_more_context_than_a_bigger_one() {
        let small = tmp("small-436550.jsonl");
        write_lines(
            &small,
            &[r#"{"type":"assistant","timestamp":"2026-08-21T10:00:00Z","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":50,"cache_creation_input_tokens":1500,"cache_read_input_tokens":435000}}}"#],
        );

        let big = tmp("big-310579.jsonl");
        let mut f = File::create(&big).unwrap();
        // Хвостатые вспомогательные записи — раздувают файл на диске, но в
        // контекст последнего usage не входят: ровно тот механизм (тул-колы,
        // субагентские ветки), из-за которого мегабайты и не годятся в оценку.
        let filler = "x".repeat(50_000);
        for i in 0..800 {
            writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"{filler}-{i}"}}}}"#).unwrap();
        }
        writeln!(f, r#"{{"type":"assistant","timestamp":"2026-08-21T09:00:00Z","message":{{"model":"claude-sonnet-4-5","usage":{{"input_tokens":79,"cache_creation_input_tokens":500,"cache_read_input_tokens":310000}}}}}}"#).unwrap();
        drop(f);

        let a_small = assess(&small);
        let a_big = assess(&big);

        assert_eq!(a_small.context_tokens, Some(436_550));
        assert_eq!(a_big.context_tokens, Some(310_579));
        assert!(a_big.file_bytes > a_small.file_bytes, "большой файл и правда больше на диске");
        assert!(
            a_small.context_tokens.unwrap() > a_big.context_tokens.unwrap(),
            "меньший файл несёт больше контекста — мегабайты тут ни при чём"
        );

        // И цена должна следовать за токенами, а не за байтами на диске.
        let cost_small = revive_cost(a_small.context_tokens, a_small.model.as_deref());
        let cost_big = revive_cost(a_big.context_tokens, a_big.model.as_deref());
        match (cost_small, cost_big) {
            (ReviveCost::Known { usd: s, .. }, ReviveCost::Known { usd: b, .. }) => {
                assert!(s > b, "файл поменьше обязан оцениваться дороже: {s} против {b}");
            }
            other => panic!("ожидали Known/Known: {other:?}"),
        }
    }

    #[test]
    fn assessment_reports_a_dedicated_state_when_usage_is_missing_entirely() {
        let p = tmp("no-usage.jsonl");
        write_lines(
            &p,
            &[
                r#"{"type":"system","subtype":"init"}"#,
                r#"{"type":"user","message":{"role":"user","content":"привет"}}"#,
            ],
        );
        let a = assess(&p);
        assert_eq!(a.context_tokens, None, "не ноль — отдельное состояние «не знаем»");
        assert_eq!(a.message_count, 1);
    }

    #[test]
    fn assessment_counts_messages_across_the_whole_file_not_just_the_tail() {
        let p = tmp("many-messages.jsonl");
        let mut f = File::create(&p).unwrap();
        for i in 0..30 {
            writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":"m{i}"}}}}"#).unwrap();
            writeln!(f, r#"{{"type":"assistant","message":{{"model":"claude-sonnet-4-5","content":[]}}}}"#).unwrap();
        }
        drop(f);
        let a = assess(&p);
        assert_eq!(a.message_count, 60);
    }

    // --- revive_cost --------------------------------------------------------

    #[test]
    fn known_model_prices_by_cold_cache_input_rate() {
        // Sonnet: $3/1M входа (см. ClaudeBackend::price) — холодный вход всего
        // контекста, никакой скидки за "было в cache_read".
        let c = revive_cost(Some(1_000_000), Some("claude-sonnet-4-5"));
        match c {
            ReviveCost::Known { usd, model } => {
                assert_eq!(model, "Sonnet");
                assert!((usd - 3.0).abs() < 1e-9);
            }
            other => panic!("ожидали Known: {other:?}"),
        }
    }

    #[test]
    fn unknown_model_returns_a_range_not_a_silently_cheap_guess() {
        let c = revive_cost(Some(1_000_000), Some("some-future-model-xyz"));
        match c {
            ReviveCost::Range { usd_low, usd_high } => {
                assert!(usd_low > 0.0 && usd_high > usd_low, "вилка, а не одно число");
                // Haiku ($1/1M) — низ вилки, Opus/Fable ($15/1M) — верх.
                assert!((usd_low - 1.0).abs() < 1e-9);
                assert!((usd_high - 15.0).abs() < 1e-9);
            }
            other => panic!("ожидали Range: {other:?}"),
        }
    }

    #[test]
    fn no_model_at_all_still_returns_a_range() {
        let c = revive_cost(Some(500_000), None);
        assert!(matches!(c, ReviveCost::Range { .. }));
    }

    #[test]
    fn missing_tokens_means_unknown_not_zero_cost() {
        assert_eq!(revive_cost(None, Some("claude-opus-4-8")), ReviveCost::Unknown);
    }
}
