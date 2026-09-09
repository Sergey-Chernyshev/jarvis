//! Собрать цикл из человеческого описания.
//!
//! Конструктор спрашивает «что настроить» — двенадцатью полями. Человек же
//! думает не полями, а задачей: «каждую ночь чини флаки-тесты и не трогай CI».
//! Здесь описание превращается в заполненную форму: модель раскладывает его по
//! полям, а человек ПРОВЕРЯЕТ и правит. Ответственность остаётся на человеке —
//! ничего не сохраняется и не запускается само.
//!
//! Модели не доверяем на слово: всё, что она вернула, проходит через
//! [`sanitize`]. Цикл без стен, отрицательные пределы, гейт без команды,
//! выдуманная модель критика — сюда не проходят. Промт просит брать команды из
//! каталога заготовок, потому что выдуманные флаги `gh` — это не ошибка
//! формата, а ночь работы впустую.

use super::model::*;
use super::presets::{self, Slot};

/// Сколько ждать модель. Задача разовая и человек смотрит на спиннер.
pub const TIMEOUT_SECS: u64 = 90;

/// Собрать промт: правила, схема, каталог команд и само описание.
pub fn prompt(text: &str, repo: &str, agent: &str) -> String {
    let mut p = String::from(
        "Ты раскладываешь описание автономного цикла по полям конфигурации. \
         Отвечай СТРОГО одним JSON-объектом, без markdown и текста вокруг.\n\n\
         Про что речь: цикл — это рутина, которую кодинг-агент крутит сам по расписанию. \
         У цикла обязан быть КОНЕЦ (условие выхода) и СТЕНЫ (ограничители), \
         иначе он не завершится и съест лимит аккаунта за ночь.\n\n\
         Правила:\n\
         - Человеческие поля (name, source.goal, имена гейтов) — по-русски. \
         Команды, пути и имена веток — как есть, латиницей.\n\
         - source.command и команды гейтов бери ИЗ КАТАЛОГА ниже, копируя посимвольно. \
         Своё придумывай, только если в каталоге нет ничего подходящего: выдуманный флаг — это ночь работы впустую.\n\
         - Не выдумывай путь к репозиторию: если он не назван, оставь пустую строку.\n\
         - Гейты — команды, которые ОТВЕЧАЮТ кодом выхода. Если проверить нечем, \
         оставь gates пустым и положись на критика.\n\
         - Ограничители ставь разумные: ночной цикл — это примерно 200000 токенов, 20 итераций, 480 минут.\n\
         - name — короткое имя в 2–4 слова, без кавычек.\n\n",
    );
    p.push_str(&format!(
        "Схема (все поля обязательны):\n\
         {{\"name\": string, \"source\": {{\"goal\": string, \"command\": string}}, \
         \"sandbox\": {{\"repo\": string, \"worktree\": bool}}, \
         \"exit\": {{\"gates\": [{{\"name\": string, \"command\": string}}], \
         \"critic\": {{\"enabled\": bool, \"model\": string}}, \"streak\": number}}, \
         \"memory\": {{\"enabled\": bool, \"file\": string}}, \
         \"schedule\": {{\"wake\": \"manual\"|{{\"daily\":{{\"at\":\"HH:MM\"}}}}|{{\"every\":{{\"minutes\":number}}}}}}, \
         \"limits\": {{\"tokens\": number, \"iterations\": number, \"minutes\": number}}, \
         \"sampling\": {{\"every\": number}}}}\n\n\
         Допустимые значения exit.critic.model: {}\n\n",
        models_for(agent).join(", ")
    ));

    p.push_str("Каталог команд-источников (source.command):\n");
    for it in presets::all().iter().filter(|i| i.slot == Slot::Source) {
        p.push_str(&format!("- {} [{}]: {}\n", it.name, it.category, it.command));
    }
    p.push_str("\nКаталог гейтов (exit.gates[].command):\n");
    for it in presets::all().iter().filter(|i| i.slot == Slot::Gate) {
        p.push_str(&format!("- {} [{}]: {}\n", it.name, it.category, it.command));
    }

    if !repo.trim().is_empty() {
        p.push_str(&format!("\nРепозиторий уже выбран человеком: {}\n", repo.trim()));
    }
    p.push_str("\nОписание цикла:\n");
    p.push_str(text.trim());
    p
}

fn models_for(agent: &str) -> Vec<&'static str> {
    crate::backend::backend(crate::backend::Agent::from_label(agent))
        .models()
        .iter()
        .map(|(id, _)| *id)
        .collect()
}

/// Выдрать JSON из ответа и наложить на заготовку.
///
/// Разбор — как у карточек ходов: модель любит обрамить объект прозой или
/// забором, поэтому ищем от первой `{` и пробуем закрытия с конца.
pub fn parse(out: &str, base: &Loop) -> Option<Loop> {
    let start = out.find('{')?;
    let cut = &out[start..];
    let mut value: Option<serde_json::Value> = None;
    for (i, _) in cut.char_indices().rev().filter(|(_, c)| *c == '}').take(8) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&cut[..=i]) {
            value = Some(v);
            break;
        }
    }
    let value = value?;
    // Разбираем в ту же модель, что и форма: неизвестные поля serde пропустит,
    // недостающие возьмёт из Default — частичный ответ не должен всё обнулить.
    let mut item: Loop = serde_json::from_value(value).ok()?;
    item.id = base.id.clone();
    item.created_at = base.created_at;
    item.last_run_at = base.last_run_at;
    item.agent = base.agent.clone();
    // Репозиторий, выбранный человеком, всегда сильнее придуманного моделью.
    if !base.sandbox.repo.trim().is_empty() {
        item.sandbox.repo = base.sandbox.repo.clone();
    }
    if item.sandbox.branch.trim().is_empty() {
        item.sandbox.branch = Sandbox::default().branch;
    }
    sanitize(&mut item);
    Some(item)
}

/// Привести к разумному виду то, что вернула модель.
///
/// Не косметика: цикл отсюда идёт человеку на подтверждение, и подсовывать ему
/// конфигурацию, которая молча не запустится (или запустится без стен), нечестно.
pub fn sanitize(item: &mut Loop) {
    item.name = crate::util::ellipsize(&crate::util::one_line(&item.name), 60);
    item.source.goal = crate::util::one_line(&item.source.goal);
    item.source.command = item.source.command.trim().to_string();

    // Гейт без команды не гейт: он бы молча считался пройденным.
    item.exit.gates.retain(|g| !g.command.trim().is_empty());
    for g in &mut item.exit.gates {
        g.name = crate::util::one_line(&g.name);
        g.command = g.command.trim().to_string();
        if g.name.is_empty() {
            g.name = "проверка".into();
        }
    }
    // Выдуманную модель не пропускаем: селект в форме её всё равно не покажет,
    // а цикл ушёл бы к несуществующему бэкенду.
    let known = models_for(&item.agent);
    if !known.iter().any(|m| *m == item.exit.critic.model) {
        item.exit.critic.model = Critic::default().model;
    }
    // Без гейтов единственный конец — критик; выключенный оставил бы цикл без
    // условия выхода вовсе.
    if item.exit.gates.is_empty() {
        item.exit.critic.enabled = true;
    }
    item.exit.streak = item.exit.streak.clamp(1, 10);

    if item.memory.file.trim().is_empty() {
        item.memory.file = Memory::default().file;
    }
    if let Wake::Daily { at } = &item.schedule.wake {
        // Кривое время не расписание: цикл просто не проснулся бы никогда.
        let ok = at
            .split_once(':')
            .and_then(|(h, m)| Some((h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?)))
            .is_some_and(|(h, m)| h < 24 && m < 60);
        if !ok {
            item.schedule.wake = Wake::Daily { at: "02:00".into() };
        }
    }

    // Стены обязательны. Модель, снявшая все три, оставила бы цикл крутиться до
    // утра и до исчерпания лимита — возвращаем ночной набор по умолчанию.
    let d = Limits::default();
    if item.limits.tokens == 0 && item.limits.iterations == 0 && item.limits.minutes == 0 {
        item.limits = d.clone();
    } else {
        if item.limits.tokens > 0 {
            item.limits.tokens = item.limits.tokens.clamp(10_000, 5_000_000);
        }
        if item.limits.iterations > 0 {
            item.limits.iterations = item.limits.iterations.clamp(1, 500);
        }
        if item.limits.minutes > 0 {
            item.limits.minutes = item.limits.minutes.clamp(5, 24 * 60);
        }
    }
    item.sampling.every = item.sampling.every.min(50);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Loop {
        Loop { id: "loop-1".into(), agent: "claude".into(), created_at: 42, ..Default::default() }
    }

    #[test]
    fn prompt_carries_schema_and_the_catalog() {
        let p = prompt("чини флаки по ночам", "/repo", "claude");
        assert!(p.contains("чини флаки по ночам"), "описание должно попасть в промт");
        assert!(p.contains("\"limits\""), "схемы нет");
        assert!(p.contains("cargo test"), "каталог гейтов не приехал");
        assert!(p.contains("gh issue list"), "каталог источников не приехал");
        assert!(p.contains("/repo"), "выбранный репозиторий не назван");
        assert!(p.contains("opus"), "список моделей критика не назван");
    }

    /// `models_for` идёт через `Agent::from_label`, поэтому третий агент здесь
    /// работает без правок — но промт критика собирается из этого списка, и
    /// молчаливая подстановка чужих моделей стоила бы ночи впустую.
    #[test]
    fn models_come_from_the_agent_backend() {
        assert!(models_for("kimi").contains(&"kimi-code/k3"));
        assert!(models_for("claude").contains(&"opus"));
        assert!(!models_for("kimi").contains(&"opus"), "чужих моделей в списке быть не должно");
        // неизвестная метка — Claude, а не пустой список: цикл не остаётся без модели
        assert_eq!(models_for("что-то новое"), models_for("claude"));
        let p = prompt("чини флаки", "", "kimi");
        assert!(p.contains("kimi-code/k3"), "модели kimi не приехали в промт");
    }

    #[test]
    fn parses_json_wrapped_in_prose_and_fences() {
        let out = "Конечно, вот конфигурация:\n```json\n{\"name\":\"ночной test-fix\",\
            \"source\":{\"goal\":\"чинить флаки\",\"command\":\"cargo test\"},\
            \"exit\":{\"gates\":[{\"name\":\"тесты\",\"command\":\"cargo test\"}],\
            \"critic\":{\"enabled\":true,\"model\":\"opus\"},\"streak\":2},\
            \"limits\":{\"tokens\":200000,\"iterations\":20,\"minutes\":480}}\n```\nГотово!";
        let l = parse(out, &base()).expect("JSON внутри прозы должен разбираться");
        assert_eq!(l.name, "ночной test-fix");
        assert_eq!(l.exit.gates.len(), 1);
        assert_eq!(l.limits.tokens, 200_000);
    }

    #[test]
    fn identity_and_human_choices_survive_the_model() {
        let mut b = base();
        b.sandbox.repo = "/mine".into();
        let out = r#"{"name":"x","sandbox":{"repo":"/выдуманный/путь"},"limits":{"tokens":1000000}}"#;
        let l = parse(out, &b).unwrap();
        assert_eq!(l.id, "loop-1", "id заготовки затёрт");
        assert_eq!(l.created_at, 42);
        assert_eq!(l.sandbox.repo, "/mine", "выбор человека слабее выдумки модели");
        assert_eq!(l.agent, "claude");
    }

    #[test]
    fn a_loop_without_walls_is_not_accepted() {
        let out = r#"{"name":"вечный","limits":{"tokens":0,"iterations":0,"minutes":0}}"#;
        let l = parse(out, &base()).unwrap();
        assert!(l.limits.tokens > 0 || l.limits.iterations > 0 || l.limits.minutes > 0);
        assert!(l.problems().iter().all(|p| !p.contains("ограничителя")));
    }

    #[test]
    fn absurd_numbers_are_clamped() {
        let out = r#"{"name":"x","exit":{"streak":9999},"limits":{"tokens":99999999999,"minutes":100000},
                      "sampling":{"every":900}}"#;
        let l = parse(out, &base()).unwrap();
        assert!(l.exit.streak <= 10);
        assert!(l.limits.tokens <= 5_000_000);
        assert!(l.limits.minutes <= 24 * 60);
        assert!(l.sampling.every <= 50);
    }

    #[test]
    fn empty_gate_and_invented_model_are_dropped() {
        let out = r#"{"name":"x","exit":{"gates":[{"name":"пусто","command":"  "},
                      {"name":"","command":"cargo test"}],
                      "critic":{"enabled":true,"model":"gpt-9-ultra"}}}"#;
        let l = parse(out, &base()).unwrap();
        assert_eq!(l.exit.gates.len(), 1, "гейт без команды считался бы пройденным");
        assert_eq!(l.exit.gates[0].name, "проверка", "безымянному гейту нужна подпись");
        assert_eq!(l.exit.critic.model, "opus", "выдуманная модель не должна пройти");
    }

    #[test]
    fn without_gates_the_critic_stays_on() {
        // Иначе у цикла не остаётся условия выхода вовсе.
        let out = r#"{"name":"x","exit":{"gates":[],"critic":{"enabled":false,"model":"opus"}}}"#;
        let l = parse(out, &base()).unwrap();
        assert!(l.exit.critic.enabled);
    }

    #[test]
    fn broken_time_falls_back_to_a_real_one() {
        let out = r#"{"name":"x","schedule":{"wake":{"daily":{"at":"ночью"}}}}"#;
        let l = parse(out, &base()).unwrap();
        assert_eq!(l.schedule.wake, Wake::Daily { at: "02:00".into() });
    }

    #[test]
    fn junk_is_refused_rather_than_half_parsed() {
        assert!(parse("модель извиняется и ничего не вернула", &base()).is_none());
        assert!(parse("", &base()).is_none());
    }
}
