//! Сколько контекста занял разговор — и сколько осталось до потолка.
//!
//! Занятое — ФАКТ провайдера, а не наша оценка: у каждой записи ассистента есть
//! `message.usage`, и сумма трёх ВХОДНЫХ полей (`input_tokens`,
//! `cache_creation_input_tokens`, `cache_read_input_tokens`) и есть тот промпт,
//! который ушёл модели. Считать токены самим тут нечего и незачем.
//!
//! Потолок честен ровно наполовину. В транскрипте у записи стоит
//! `message.model: "claude-opus-5"` — БЕЗ пометки про миллион, потому что окно
//! задаёт не имя модели, а строка запуска (`opus[1m]`). Взять окно по этой
//! строке значит получить 200 000 и «занято 150%». Настоящее число приносит сам
//! CLI в событии `result`: `modelUsage.<модель>.contextWindow`. Его и берём;
//! таблица бэкенда остаётся запасным ходом и помечается оценкой.

use serde::Serialize;
use serde_json::{json, Value};

use super::history;

/// Сколько хвоста транскрипта читаем. То же, что у истории чата: отметки сжатия
/// должны совпадать с теми репликами, которые человек видит в ленте.
const TAIL_BYTES: u64 = 512 * 1024;

/// Порог предупреждения. Человек должен узнать про исход контекста заранее, а не
/// по внезапно поглупевшему собеседнику: за оставшейся седьмой частью окна ещё
/// успевает поместиться нормальный ход.
pub const NEAR: f64 = 0.85;

/// Занятый контекст из `usage`: сумма трёх ВХОДНЫХ полей.
///
/// `output_tokens` сюда не входит — он уже уехал человеку, а в следующем запросе
/// вернётся частью входа. `None` — ни одного входного поля нет: ноль тут
/// означал бы «контекст пуст», а это неправда.
pub fn used_tokens(usage: &Value) -> Option<u64> {
    const IN: [&str; 3] = [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ];
    let mut sum = 0u64;
    let mut seen = false;
    for k in IN {
        if let Some(n) = usage.get(k).and_then(Value::as_u64) {
            sum += n;
            seen = true;
        }
    }
    seen.then_some(sum)
}

/// Потолок окна из события `result` — единственное место, где CLI называет его
/// сам. Берём наибольший: в ходе могла поучаствовать вспомогательная модель
/// поменьше, а разговор живёт в главной.
pub fn window_from_result(v: &Value) -> Option<u64> {
    v.get("modelUsage")?
        .as_object()?
        .values()
        .filter_map(|m| m.get("contextWindow").and_then(Value::as_u64))
        .filter(|w| *w > 0)
        .max()
}

/// Последний факт про контекст в транскрипте: занято столько-то, модель такая-то.
/// Идём с конца — предыдущие ходы говорят про прошлое.
pub fn last_usage(entries: &[Value]) -> Option<(u64, String)> {
    entries.iter().rev().find_map(|e| {
        (e.get("type").and_then(Value::as_str) == Some("assistant")).then_some(())?;
        let used = used_tokens(e.pointer("/message/usage")?)?;
        let model = e
            .pointer("/message/model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Some((used, model))
    })
}

/// Момент, когда контекст сжали. Без этой отметки разрыв в памяти агента
/// выглядит как его ошибка.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Squeeze {
    /// Мс эпохи. `None` — у записи не было таймстампа (так приходит из потока).
    pub at: Option<i64>,
    /// Сколько было до и сколько стало. Пусто — CLI чисел не назвал.
    pub pre: Option<u64>,
    pub post: Option<u64>,
    /// `auto` — упёрлись в потолок, `manual` — попросил человек.
    pub trigger: String,
}

/// Запись о сжатии: `system` / `compact_boundary`. Мета приезжает в двух видах —
/// `compactMetadata` в транскрипте и `compact_metadata` в потоке, — поэтому
/// разбор один на оба: два разбора разъехались бы на первой же правке.
pub fn squeeze_of(e: &Value) -> Option<Squeeze> {
    if e.get("type").and_then(Value::as_str) != Some("system")
        || e.get("subtype").and_then(Value::as_str) != Some("compact_boundary")
    {
        return None;
    }
    let meta = e
        .get("compactMetadata")
        .or_else(|| e.get("compact_metadata"));
    let num = |camel: &str, snake: &str| {
        meta.and_then(|m| m.get(camel).or_else(|| m.get(snake)))
            .and_then(Value::as_u64)
    };
    Some(Squeeze {
        at: e
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(crate::transcript::parse_ts),
        pre: num("preTokens", "pre_tokens"),
        post: num("postTokens", "post_tokens"),
        trigger: meta
            .and_then(|m| m.get("trigger"))
            .and_then(Value::as_str)
            .unwrap_or("auto")
            .to_string(),
    })
}

/// Счётчик для шапки: занято, потолок, доля, остаток и признак «пора сказать».
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Gauge {
    /// Токенов занято. Всегда факт провайдера.
    pub used: u64,
    /// Потолок. `None` — окна не знаем, и доли не будет: доля без знаменателя
    /// это выдуманное число.
    pub window: Option<u64>,
    /// Потолок назвал CLI, а не наша таблица. `false` — в интерфейсе оценка.
    pub exact: bool,
    /// Доля занятого, 0..1.
    pub frac: Option<f64>,
    /// Сколько токенов ещё влезет.
    pub left: Option<u64>,
    /// Контекст на исходе — пора предупредить человека.
    pub near: bool,
}

/// Свести факты в счётчик.
///
/// Оценка, которая МЕНЬШЕ уже занятого, — не оценка, а опровергнутая догадка:
/// такой потолок выбрасываем и честно говорим «окна не знаю». Именно так
/// выглядит `opus[1m]` глазами таблицы: 300k занятого против 200k по имени
/// модели. Факт CLI не проверяем ничем — он и есть проверка.
pub fn gauge(used: u64, window: Option<u64>, exact: bool) -> Gauge {
    let window = window.filter(|w| *w > 0 && (exact || *w >= used));
    let frac = window.map(|w| used as f64 / w as f64);
    Gauge {
        used,
        window,
        exact: exact && window.is_some(),
        frac,
        left: window.map(|w| w.saturating_sub(used)),
        near: frac.is_some_and(|f| f >= NEAR),
    }
}

/// Срез контекста ОДНОГО разговора: у каждого чата своя лента, и общая цифра по
/// приложению тут ничего не значит.
///
/// Едет вместе со списком чатов (`history::chats_json`), а не отдельной
/// командой: список окно и так спрашивает при каждом открытии и переключении,
/// а лишний ipc-вызов — это ещё один шов, который может разъехаться.
///
/// `saved_window` — потолок, услышанный от CLI прошлым ходом; он важнее
/// таблицы, потому что имя модели в транскрипте про размер окна молчит.
pub fn for_chat(dir: &std::path::Path, sid: &str, saved_window: Option<u64>) -> Value {
    // Читаем СЫРЫЕ записи, а не цепочку реплик: отметка сжатия в цепочку не
    // входит, и `read_entries` унесла бы её вместе с остальным служебным.
    let entries: Vec<Value> = history::transcript_path(dir, sid)
        .filter(|p| p.is_file())
        .and_then(|p| crate::transcript::read_recent_text(&p, TAIL_BYTES))
        .map(|t| crate::transcript::entries_from_text(&t))
        .unwrap_or_default();

    let last = last_usage(&entries);
    let model = last.as_ref().map(|(_, m)| m.as_str()).unwrap_or("");
    let (window, exact) = match saved_window {
        Some(w) => (Some(w), true),
        None => (
            crate::backend::backend(crate::backend::Agent::Claude).context_window(model),
            false,
        ),
    };
    json!({
        "model": model,
        "ctx": last.as_ref().map(|(u, _)| gauge(*u, window, exact)),
        "squeezes": entries.iter().filter_map(squeeze_of).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Живая запись из транскрипта владельца: 2 + 843 + 299382 = 300227.
    /// `output_tokens` в контекст не входит — он уже отдан наружу.
    #[test]
    fn used_context_is_the_sum_of_three_input_fields() {
        let u = json!({
            "input_tokens": 2,
            "cache_creation_input_tokens": 843,
            "cache_read_input_tokens": 299382,
            "output_tokens": 266,
        });
        assert_eq!(used_tokens(&u), Some(300_227));
        // Ни одного входного поля — «не знаю», а не «пусто».
        assert_eq!(used_tokens(&json!({ "output_tokens": 10 })), None);
        assert_eq!(used_tokens(&json!({ "input_tokens": 5 })), Some(5));
    }

    #[test]
    fn window_comes_from_the_cli_not_from_the_model_name() {
        let v = json!({
            "type": "result",
            "modelUsage": {
                "claude-opus-5": { "inputTokens": 12, "contextWindow": 1_000_000 },
                "claude-haiku-4-5": { "inputTokens": 3, "contextWindow": 200_000 },
            }
        });
        assert_eq!(window_from_result(&v), Some(1_000_000), "разговор живёт в главной модели");
        assert_eq!(window_from_result(&json!({ "type": "result" })), None);
        assert_eq!(
            window_from_result(&json!({ "modelUsage": { "m": { "contextWindow": 0 } } })),
            None,
            "нулевой потолок — не потолок"
        );
    }

    #[test]
    fn last_usage_walks_from_the_end() {
        let entries = vec![
            json!({"type":"assistant","message":{"model":"claude-opus-5","usage":{"input_tokens":10}}}),
            json!({"type":"user","message":{"role":"user","content":"привет"}}),
            json!({"type":"assistant","message":{"model":"claude-opus-5","usage":{
                "input_tokens":2,"cache_creation_input_tokens":843,"cache_read_input_tokens":299382}}}),
            // хвост без usage прошлое не отменяет
            json!({"type":"assistant","message":{"model":"claude-opus-5"}}),
        ];
        assert_eq!(last_usage(&entries), Some((300_227, "claude-opus-5".into())));
        assert_eq!(last_usage(&[]), None);
    }

    /// Ловушка с окном: имя модели в транскрипте про миллион молчит, поэтому
    /// таблица даёт 200k — и на 300k занятого она опровергнута фактом. Врать
    /// «занято 150%» нельзя: лучше сказать, что окна не знаем.
    #[test]
    fn an_estimate_below_the_used_context_is_thrown_away() {
        let g = gauge(300_227, Some(200_000), false);
        assert_eq!(g.window, None, "оценка опровергнута занятым");
        assert_eq!(g.frac, None);
        assert!(!g.exact);
        assert!(!g.near, "без окна порог не срабатывает — не от чего считать");

        // тот же занятый при честном потолке
        let g = gauge(300_227, Some(1_000_000), true);
        assert_eq!(g.window, Some(1_000_000));
        assert_eq!(g.left, Some(699_773));
        assert!(g.exact, "число от CLI — факт, а не оценка");
        assert!((g.frac.unwrap() - 0.300227).abs() < 1e-9);
        assert!(!g.near);
    }

    #[test]
    fn without_a_window_there_is_no_share_and_no_exactness() {
        let g = gauge(1_234, None, true);
        assert_eq!(g.used, 1_234);
        assert_eq!((g.window, g.frac, g.left), (None, None, None));
        assert!(!g.exact, "нечему быть точным");
    }

    #[test]
    fn the_warning_fires_before_the_wall() {
        assert!(!gauge(840_000, Some(1_000_000), true).near);
        assert!(gauge(850_000, Some(1_000_000), true).near, "ровно на пороге — уже пора");
        assert!(gauge(990_000, Some(1_000_000), true).near);
    }

    /// Сжатие приходит двумя видами меты — из транскрипта и из потока. Разбор
    /// один: пропустить один из них значит потерять отметку в ленте.
    #[test]
    fn compaction_is_read_from_both_shapes() {
        let disk = json!({
            "type": "system", "subtype": "compact_boundary",
            "timestamp": "2026-08-20T13:45:00Z",
            "compactMetadata": { "trigger": "auto", "preTokens": 780_000, "postTokens": 42_000 },
        });
        let s = squeeze_of(&disk).expect("отметка с диска не разобралась");
        assert_eq!((s.pre, s.post, s.trigger.as_str()), (Some(780_000), Some(42_000), "auto"));
        assert_eq!(s.at, crate::transcript::parse_ts("2026-08-20T13:45:00Z"));

        let stream = json!({
            "type": "system", "subtype": "compact_boundary", "session_id": "s-1",
            "compact_metadata": { "trigger": "manual", "pre_tokens": 700, "post_tokens": 40 },
        });
        let s = squeeze_of(&stream).expect("отметка из потока не разобралась");
        assert_eq!((s.pre, s.post, s.trigger.as_str()), (Some(700), Some(40), "manual"));
        assert_eq!(s.at, None);

        // соседние служебные записи отметкой не притворяются
        assert_eq!(squeeze_of(&json!({"type":"system","subtype":"init"})), None);
        assert_eq!(squeeze_of(&json!({"type":"assistant"})), None);
    }

    /// Срез по живому транскрипту: занятое — факт с диска, потолок — оценка по
    /// модели, пока CLI не назвал свой; отметка сжатия едет вместе с ними.
    #[test]
    fn a_chat_snapshot_reads_disk_and_marks_the_estimate() {
        let d = std::env::temp_dir().join(format!("jarvis-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("s-1.jsonl"),
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"привет\"}}\n\
             {\"type\":\"system\",\"subtype\":\"compact_boundary\",\"timestamp\":\"2026-08-20T10:00:00Z\",\
              \"compactMetadata\":{\"trigger\":\"auto\",\"preTokens\":780000,\"postTokens\":42000}}\n\
             {\"type\":\"assistant\",\"message\":{\"model\":\"claude-sonnet-4-5\",\"usage\":\
              {\"input_tokens\":2,\"cache_creation_input_tokens\":843,\"cache_read_input_tokens\":99155}}}\n",
        )
        .unwrap();

        let v = for_chat(&d, "s-1", None);
        assert_eq!(v["model"], json!("claude-sonnet-4-5"));
        assert_eq!(v["ctx"]["used"], json!(100_000), "занятое считаем по трём входным полям");
        assert_eq!(v["ctx"]["window"], json!(200_000), "потолок — из модели, а не из константы");
        assert_eq!(v["ctx"]["exact"], json!(false), "таблица — оценка, и это надо сказать");
        assert_eq!(v["ctx"]["left"], json!(100_000));
        assert_eq!(v["squeezes"][0]["pre"], json!(780_000));
        assert_eq!(v["squeezes"].as_array().unwrap().len(), 1);

        // Услышанный от CLI потолок бьёт таблицу — и перестаёт быть оценкой.
        let v = for_chat(&d, "s-1", Some(1_000_000));
        assert_eq!(v["ctx"]["window"], json!(1_000_000));
        assert_eq!(v["ctx"]["exact"], json!(true));

        // Разговора на диске нет — молчим, а не выдумываем ноль.
        let v = for_chat(&d, "s-ghost", None);
        assert_eq!(v["ctx"], Value::Null);
        assert_eq!(v["squeezes"], json!([]));
        let _ = std::fs::remove_dir_all(&d);
    }
}
