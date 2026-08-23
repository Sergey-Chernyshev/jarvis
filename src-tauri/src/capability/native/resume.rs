//! Оживить мёртвую сессию с её прежним контекстом — `sessions.resume`, и
//! список того, что вообще можно оживить, — `sessions.revivable`.
//!
//! Зачем. Контекст умирает вместе с процессом: перезагрузка, закрытый терминал,
//! упавший CLI. Транскрипт при этом остаётся на диске целым, и вернуть по нему
//! разговор умеет сам CLI (`claude --resume`, `kimi -S`). Не умели этого только
//! джарвисы: у них не было инструмента, и человек поднимал сессии руками.
//!
//! Запуск идёт ТЕМ ЖЕ путём, что ручной из панели, — `ipc::launch_core` с
//! `session_id`. Второй реализации запуска в проекте нет и не будет.
//!
//! Три вещи, которые здесь важнее самого подъёма:
//!
//! 1. **Родной каталог.** Снято с экрана: `kimi -S` из чужого каталога НЕ
//!    выходит молча — он встаёт на вопрос о доверии к этой папке, и курсор в нём
//!    стоит на «Don't trust», то есть слепой Enter означает выход. Claude в
//!    чужом каталоге поднимет разговор про чужой проект. Каталог берётся из
//!    транскрипта (поле `cwd` пишут оба CLI) — человек его не вводит и не должен
//!    угадывать.
//! 2. **Успех — это пришедший хук, а не запущенный процесс.** Тот же класс, что
//!    с талоном запуска: стартовал, умер через секунду, отчитались успехом.
//! 3. **Цена до нажатия.** Первый ход после оживления идёт по холодному кэшу:
//!    весь контекст оплачивается как вход. Мегабайты цену НЕ предсказывают —
//!    замер на живых файлах: 4.7 МБ несли 436 550 токенов, а 37.4 МБ — 310 579.
//!    Поэтому решение принимается по токенам, а размер идёт справочно.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::backend::{backend, Agent};
use crate::capability::contract::{CapabilityMeta, Provenance, RiskClass};
use crate::capability::registry::make_handler;
use crate::capability::DaemonRegistry;
use crate::daemon::Daemon;
use crate::revive::{self, ReviveCost, Verdict};

use super::arg_str;

/// Сколько ждём хук оживлённой сессии. CLI поднимает разговор с диска: на
/// 37 МБ это заняло меньше 25 секунд живьём, минуты хватает с запасом.
const HOOK_WAIT: Duration = Duration::from_secs(60);
/// Шаг опроса реестра. Чаще незачем — хук приходит не мгновенно.
const HOOK_POLL: Duration = Duration::from_millis(500);

/* ================= где живёт сессия ================= */

/// Транскрипт сессии и агент, которому он принадлежит.
///
/// Перебираем всех: id мёртвой сессии приходит без пометки, чей он, а формат
/// имени у kimi (`session_…`) и claude (голый uuid) различается не всегда.
pub fn find_transcript(sid: &str) -> Option<(Agent, PathBuf)> {
    for a in Agent::all().iter().copied() {
        if let Some(p) = backend(a).find_transcript_by_sid(sid) {
            return Some((a, p));
        }
    }
    None
}

/// Родной рабочий каталог сессии из её транскрипта.
///
/// Поле `cwd` пишут и claude, и kimi (проверено на живых файлах обоих). Берём
/// его, а не разбираем имя каталога проекта: у claude оно закодировано
/// неоднозначно — `-Users-x-FastWorkBot-server` это и `FastWorkBot/server`, и
/// `FastWorkBot-server`, и угадывать тут нечем.
///
/// Читаем ПОТОКОМ и с начала: `cwd` стоит в первых записях, а файл бывает в
/// десятки мегабайт.
pub fn cwd_from_transcript(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path).ok()?;
    for line in BufReader::new(f).lines().map_while(Result::ok).take(500) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(c) = v.get("cwd").and_then(Value::as_str) {
            let c = c.trim();
            if !c.is_empty() {
                return Some(c.to_string());
            }
        }
    }
    None
}

/// Что делать с каталогом, которого нет.
#[derive(Debug, PartialEq)]
pub enum DirPlan {
    /// Каталог на месте — оживляем как есть.
    Exists,
    /// Каталога нет, но родитель есть: создаём и ГРОМКО предупреждаем.
    ///
    /// Предупреждение обязательно. Сессия помнит файлы, которых в пустом
    /// каталоге больше нет, и будет уверенно на них ссылаться. Это хуже
    /// отказа: отказ виден, а уверенная ссылка на несуществующий файл — нет.
    Recreate,
    /// Даже родителя нет — создавать целую ветку каталогов молча нельзя:
    /// вместо `/Users/x/work/proj` так легко получить опечатку с пустым
    /// деревом, которое потом никто не отличит от настоящего.
    Refuse(String),
}

/// Решение по каталогу. Чистая: ни диска, ни демона — только факты о путях.
pub fn plan_dir(cwd: &str, exists: bool, parent_exists: bool) -> DirPlan {
    if exists {
        return DirPlan::Exists;
    }
    if parent_exists {
        return DirPlan::Recreate;
    }
    DirPlan::Refuse(format!(
        "родного каталога «{cwd}» нет, и его родителя тоже — создать всю ветку \
         вслепую нельзя: одна опечатка в пути даёт пустое дерево, неотличимое от \
         настоящего. Создай каталог сам и позови снова"
    ))
}

/// Все транскрипты агента на диске: `(id сессии, файл)`.
///
/// ЕДИНСТВЕННОЕ место, где перечисляются транскрипты — знание о раскладке
/// каталогов у каждого CLI своё, и размазывать его по коду нельзя. Появится
/// третий такой случай — метод переезжает в `Backend` рядом с
/// `find_transcript_by_sid`; ради двух заводить трейт-метод дороже, чем
/// назвать это вслух здесь.
fn transcripts_of(a: Agent) -> Vec<(String, PathBuf)> {
    let home = crate::util::home_dir();
    let mut out = Vec::new();
    match a {
        // claude: ~/.claude/projects/<проект>/<uuid>.jsonl
        Agent::Claude => {
            let root = crate::util::claude_dir().join("projects");
            for proj in read_dirs(&root) {
                for f in std::fs::read_dir(&proj).into_iter().flatten().flatten() {
                    let p = f.path();
                    if p.extension().is_some_and(|x| x == "jsonl") {
                        if let Some(sid) = p.file_stem().map(|s| s.to_string_lossy().into_owned()) {
                            out.push((sid, p));
                        }
                    }
                }
            }
        }
        // kimi: ~/.kimi-code/sessions/wd_<проект>/<session_…>/agents/main/wire.jsonl
        Agent::Kimi => {
            let root = home.join(".kimi-code").join("sessions");
            for wd in read_dirs(&root) {
                for sess in read_dirs(&wd) {
                    let Some(sid) = sess.file_name().map(|s| s.to_string_lossy().into_owned())
                    else {
                        continue;
                    };
                    let f = sess.join("agents").join("main").join("wire.jsonl");
                    if f.is_file() {
                        out.push((sid, f));
                    }
                }
            }
        }
        // codex: на машине разработки его нет, раскладку подтвердить нечем.
        // Пустой список честнее выдуманного пути: «ничего не нашлось» человек
        // проверит, а список из несуществующих файлов — нет.
        Agent::Codex => {}
    }
    out
}

/// Подкаталоги — без паники на отсутствующем корне.
fn read_dirs(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/* ================= оживление ================= */

/// Ждём хук оживлённой сессии: успех — это появление её в реестре.
///
/// Процесс мог стартовать и не дойти до сессии: встать на вопрос о доверии к
/// каталогу, упасть на несовместимом транскрипте, выйти по чужому окружению.
/// Отчитаться успехом по факту запуска значило бы повторить ошибку с талоном.
async fn wait_for_hook(d: &Arc<Daemon>, sid: &str) -> bool {
    let steps = (HOOK_WAIT.as_millis() / HOOK_POLL.as_millis()).max(1);
    for _ in 0..steps {
        tokio::time::sleep(HOOK_POLL).await;
        if d.session(sid).is_some() {
            return true;
        }
    }
    false
}

async fn resume_handler(d: Arc<Daemon>, args: Value) -> Result<Value, String> {
    let sid = arg_str(&args, "id")?;
    let sid = sid.trim().to_string();

    // 1. Живую вторую копией не поднимаем: два процесса на одном транскрипте
    //    испортят разговор обоим. Переключаться человеку и так есть чем.
    if let Some(s) = d.session(&sid) {
        return Ok(json!({
            "ok": true,
            "state": "already-alive",
            "sessionId": sid,
            "name": s.name,
            "note": "сессия уже жива — второй копии не поднимаю, переключись на существующую"
        }));
    }

    // 2. Транскрипт и агент.
    let Some((agent, path)) = find_transcript(&sid) else {
        return Err(format!(
            "транскрипта сессии «{sid}» на диске нет — оживлять нечего. \
             Список доступных отдаёт sessions.revivable"
        ));
    };

    // 3. Целостность. `claude --resume` оживляет обрезанный файл МОЛЧА, до места
    //    обрыва (проверено живьём), поэтому проверяем мы, а не он: сессия с
    //    обрубленной памятью будет уверенно врать, и это хуже отказа.
    let integrity = revive::check_integrity(&path);
    if integrity.verdict != Verdict::Ok {
        return Err(format!(
            "транскрипт не годен к оживлению: {}. Файл: {}",
            integrity.reason,
            path.display()
        ));
    }

    // 4. Родной каталог. Из транскрипта, а не от человека.
    let Some(cwd) = cwd_from_transcript(&path) else {
        return Err(format!(
            "в транскрипте «{}» не записан рабочий каталог — куда поднимать, неизвестно",
            path.display()
        ));
    };
    let p = Path::new(&cwd);
    let plan = plan_dir(
        &cwd,
        p.is_dir(),
        p.parent().map(Path::is_dir).unwrap_or(false),
    );
    let mut notes: Vec<String> = Vec::new();
    match plan {
        DirPlan::Exists => {}
        DirPlan::Refuse(why) => return Err(why),
        DirPlan::Recreate => {
            std::fs::create_dir_all(p)
                .map_err(|e| format!("родной каталог «{cwd}» не создался: {e}"))?;
            notes.push(format!(
                "каталог «{cwd}» пропал и создан заново ПУСТЫМ: сессия помнит файлы, \
                 которых в нём больше нет, и будет на них ссылаться"
            ));
            crate::log::line(&format!("[resume] {sid}: воссоздан пустой каталог {cwd}"));
        }
    }

    // 5. Цена и бюджет. Оживление — дорогой ход: первый запрос идёт по холодному
    //    кэшу, весь контекст оплачивается как вход.
    let a = revive::assess(&path);
    let cost = revive::revive_cost(a.context_tokens, a.model.as_deref());
    let hold = crate::ipc::budget_reserve(
        &d,
        agent.label(),
        a.model.as_deref(),
        true,
        "оживление сессии",
    )
    .await?;

    // 6. Ночью крупное оживление не делается: цена высокая, а человека нет.
    if crate::budget::is_night(&d) {
        if let ReviveCost::Known { usd, .. } = &cost {
            if *usd >= NIGHT_USD {
                hold.release();
                return Err(format!(
                    "ночью крупные оживления не делаю: этот транскрипт поднимет \
                     ~{} токенов контекста, это около ${usd:.2} за первый ход. \
                     Оживлю утром или разбуди меня явно",
                    a.context_tokens.unwrap_or(0)
                ));
            }
        }
    }

    // 7. Подъём — тем же ядром, что и ручной запуск из панели.
    let out = crate::ipc::launch_core(
        &d,
        crate::ipc::LaunchReq {
            cwd: Some(cwd.clone()),
            agent: agent.label().to_string(),
            session_id: Some(sid.clone()),
            machine: None,
            isolate: Some(false),
            mode: None,
            task: None,
            container: None,
            bind: None,
        },
    )
    .await;
    if out.get("ok").and_then(Value::as_bool) != Some(true) {
        hold.release();
        return Ok(out);
    }

    // 8. Успех — это ХУК, а не запущенный процесс.
    if !wait_for_hook(&d, &sid).await {
        hold.release();
        return Err(format!(
            "процесс запустился, но сессия «{sid}» так и не отметилась за {} с — \
             считать это успехом нельзя. Загляни в окно: у kimi из чужого каталога \
             и у claude на вопросе о доверии экран объясняет причину",
            HOOK_WAIT.as_secs()
        ));
    }
    hold.in_flight();

    // Имя сохраняем и помечаем оживление: в списке это должно читаться как
    // «та самая сессия», а не как новая с похожим названием.
    d.with_session(&sid, |s| s.revived = true);
    crate::log::line(&format!("[resume] {sid} оживлена в {cwd}"));

    Ok(json!({
        "ok": true,
        "state": "revived",
        "sessionId": sid,
        "agent": agent.label(),
        "cwd": cwd,
        "contextTokens": a.context_tokens,
        "cost": cost,
        "messages": a.message_count,
        "notes": notes,
    }))
}

/// Порог «крупного» оживления ночью. Не догадка: при остатке недели около 30% и
/// дневной норме порядка 8% один такой ход заметен в счёте, а решить, нужен ли
/// он, ночью некому.
const NIGHT_USD: f64 = 3.0;

/* ================= что можно оживить ================= */

fn revivable_handler(d: Arc<Daemon>, args: Value) -> Result<Value, String> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100) as usize;

    let mut rows: Vec<Value> = Vec::new();
    for a in Agent::all().iter().copied() {
        for (sid, path) in transcripts_of(a) {
            if d.session(&sid).is_some() {
                continue; // живая — оживлять нечего
            }
            let it = revive::assess(&path);
            let cost = revive::revive_cost(it.context_tokens, it.model.as_deref());
            rows.push(json!({
                "id": sid,
                "agent": a.label(),
                "contextTokens": it.context_tokens,
                "cost": cost,
                "messages": it.message_count,
                // Размер — справочно и НАМЕРЕННО не первым: он не предсказывает
                // цену. Замер на живых файлах: 4.7 МБ несли больше контекста,
                // чем 37.4 МБ.
                "bytes": it.file_bytes,
                "lastAt": it.last_message_at,
            }));
        }
    }
    // Сортируем по свежести: оживляют почти всегда недавнее.
    rows.sort_by_key(|r| -(r.get("lastAt").and_then(Value::as_i64).unwrap_or(0)));
    rows.truncate(limit);
    Ok(json!({ "sessions": rows }))
}

pub fn register(reg: &mut DaemonRegistry) {
    reg.register(
        CapabilityMeta {
            id: "sessions.resume",
            class: RiskClass::Control,
            provenance: Provenance::Trusted,
            description: "Оживить УМЕРШУЮ сессию с её прежним контекстом: процесс поднимается заново, \
разговор возвращается с диска. Зови, когда нужен контекст, которого нет в живых сессиях — \
после перезагрузки, закрытого терминала, упавшего CLI. Рабочий каталог подставляется сам из \
транскрипта, задавать его не надо. Живую сессию второй копией не поднимает — скажет, что она уже жива. \
ДОРОГО: первый ход после оживления оплачивается как весь контекст сразу, поэтому вызов проходит \
через бюджет, а ночью крупные оживления отклоняются. Что можно оживить и почём — sessions.revivable.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "id мёртвой сессии" }
                },
                "required": ["id"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move { resume_handler(d, args).await }),
    );

    reg.register(
        CapabilityMeta {
            id: "sessions.revivable",
            class: RiskClass::Read,
            provenance: Provenance::Trusted,
            description: "Список сессий, которые можно оживить (мёртвые, но с целым транскриптом на диске): \
id, агент, число реплик, время последней реплики, объём контекста в токенах и оценка цены оживления. \
Решай по ТОКЕНАМ, а не по размеру файла: размер цену не предсказывает — файл вчетверо меньше \
может нести больше контекста. Живые сессии сюда не попадают, для них есть sessions.list.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "description": "сколько вернуть, по умолчанию 20" }
                }
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move { revivable_handler(d, args) }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Каталог, которого нет. Владелец сегодня руками делал `mkdir -p` — значит
    /// это наша работа, а не его. Но не любой ценой: пустой каталог опаснее
    /// отказа, потому что сессия помнит файлы, которых в нём нет.
    #[test]
    fn a_missing_native_dir_is_recreated_only_when_its_parent_is_real() {
        assert_eq!(plan_dir("/tmp/x", true, true), DirPlan::Exists);
        // Родитель есть — каталог вычистили (типичный /tmp после перезагрузки).
        assert_eq!(plan_dir("/tmp/jarvis-review", false, true), DirPlan::Recreate);
        // Родителя нет — это опечатка в пути, а не вычищенный каталог.
        match plan_dir("/Usres/x/proj", false, false) {
            DirPlan::Refuse(why) => {
                assert!(why.contains("родителя тоже"), "причина не названа: {why}");
                assert!(why.contains("Создай каталог сам"), "не сказано, что делать");
            }
            other => panic!("создали дерево вслепую: {other:?}"),
        }
    }

    /// Рабочий каталог берётся из транскрипта, а не из имени каталога проекта:
    /// у claude оно закодировано неоднозначно и восстановлению не поддаётся.
    #[test]
    fn the_native_dir_comes_from_the_transcript_itself() {
        let dir = std::env::temp_dir().join(format!("jarvis-resume-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("t.jsonl");
        std::fs::write(
            &f,
            "{\"type\":\"metadata\"}\n\
             {\"type\":\"user\",\"cwd\":\"/Users/x/PycharmProjects/FastWorkBot\"}\n",
        )
        .unwrap();
        assert_eq!(
            cwd_from_transcript(&f).as_deref(),
            Some("/Users/x/PycharmProjects/FastWorkBot")
        );

        // Нет поля — не выдумываем: пусть вызывающий скажет об этом словами.
        let g = dir.join("no-cwd.jsonl");
        std::fs::write(&g, "{\"type\":\"metadata\"}\n").unwrap();
        assert_eq!(cwd_from_transcript(&g), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
