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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
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

/// Дедлайн гейта на весь вызов. Больше `HOOK_WAIT`, и в этом всё дело: общий
/// дедлайн (30 с) обрубал вызов раньше, чем сессия успевала отметиться, и отдавал
/// `failed:timeout` про сессию, которая как раз встала. Запас сверх ожидания —
/// на проверку целостности и оценку транскрипта, они идут до запуска.
const GATE_DEADLINE: Duration = Duration::from_secs(90);

/* ================= пороги: чем цену держат вместо карточки ================= */

/// Размер транскрипта, с которого оживление спрашивает человека даже при
/// выданном гранте (мегабайты, ключ `resume.confirmMb`).
///
/// 20 МБ — не круглое число ради круглого: на этой машине из 1570 транскриптов
/// claude такой порог перерастают четыре. То есть молча идёт всё обычное, а
/// вопрос достаётся ровно тем сессиям, про которые человек и сам бы задумался.
pub const CONFIRM_MB: f64 = 20.0;

/// Цена первого хода, с которой оживление спрашивает даже при гранте (доллары,
/// ключ `resume.confirmUsd`). Стена по деньгам — главная из двух: размер цену не
/// предсказывает, а доллар за один ход уже заметен в недельном счёте.
pub const CONFIRM_USD: f64 = 1.0;

/// Порог «крупного» оживления ночью (ключ `resume.nightUsd`). Не догадка: при
/// остатке недели около 30% и дневной норме порядка 8% один такой ход заметен в
/// счёте, а решить, нужен ли он, ночью некому. Ночью это ОТКАЗ, а не вопрос —
/// будить человека ради денег, которые подождут до утра, незачем.
pub const NIGHT_USD: f64 = 3.0;

/// Число из настроек с прежним умолчанием. Читаем на каждый вызов: человек
/// правит порог ровно тогда, когда тот ему мешает.
fn tune(d: &Arc<Daemon>, key: &str, default: f64) -> f64 {
    d.settings
        .load()
        .pointer(&format!("/resume/{key}"))
        .and_then(Value::as_f64)
        .filter(|v| *v > 0.0)
        .unwrap_or(default)
}

/* ================= где живёт сессия ================= */

/// Транскрипт сессии и агент, которому он принадлежит.
///
/// Перебираем всех: id мёртвой сессии приходит без пометки, чей он, а формат
/// имени у kimi (`session_…`) и claude (голый uuid) различается не всегда.
pub fn find_transcript(sid: &str) -> Option<(Agent, PathBuf)> {
    // Сначала спрашиваем бэкенд: у kimi и codex поиск по id реализован и знает
    // их раскладку лучше нас.
    for a in Agent::all().iter().copied() {
        if let Some(p) = backend(a).find_transcript_by_sid(sid) {
            return Some((a, p));
        }
    }
    // У claude этого поиска НЕТ — путь ему обычно приносит хук, а у мёртвой
    // сессии хука нет. Поэтому добираем тем же перечислением, что кормит
    // `sessions.revivable`: список и подъём обязаны видеть одно и то же.
    // Живая проверка поймала ровно это расхождение — сессия была в списке и не
    // находилась при оживлении.
    for a in Agent::all().iter().copied() {
        if let Some((_, p)) = transcripts_of(a).into_iter().find(|(id, _)| id == sid) {
            return Some((a, p));
        }
    }
    None
}

/// Родной рабочий каталог сессии из её транскрипта.
///
/// Два разных случая, и второй стоил живого дефекта.
///
/// **claude** пишет `cwd` записью верхнего уровня — берём его и всё.
///
/// **kimi** верхнего `cwd` не пишет ВООБЩЕ: путь встречается только внутри
/// записей (снимки инструментов, аргументы вызовов). Пока мы читали лишь
/// верхний уровень, оживление kimi отказывало со словами «каталог не записан», и
/// снаружи это выглядело как «капабилити сделана только под claude».
///
/// Поэтому для вложенного случая берём САМЫЙ ЧАСТЫЙ абсолютный путь и сверяем
/// его с именем каталога сессии: kimi раскладывает их как
/// `~/.kimi-code/sessions/wd_<имя>_<хэш>/…`, где `<имя>` — имя рабочей папки в
/// нижнем регистре. Хэш обратимым не бывает, а вот ПРОВЕРИТЬ кандидата им можно
/// — этого достаточно и это честнее, чем брать первый попавшийся путь: внутри
/// транскрипта попадаются и чужие каталоги, в которых агент что-то запускал.
///
/// Имя каталога проекта у claude не разбираем: там оно закодировано
/// неоднозначно — `-Users-x-FastWorkBot-server` это и `FastWorkBot/server`, и
/// `FastWorkBot-server`.
///
/// **Третий источник — ПРОЗА системного промпта**, и без него оживление
/// отказывало каждой пятой kimi-сессии. Замер на живых файлах: из 37 транскриптов
/// kimi каталог определялся у 30, а семь отказывали со словами «каталог не
/// записан». Это оказались КОРОТКИЕ сессии (4–9 строк), где до вызова
/// инструментов дело не дошло, — и путь в них есть ровно один раз, в тексте
/// преамбулы: «The current working directory is `/Users/…/FastWorkBot`».
/// Поля `cwd` в них нет вообще, ни на каком уровне.
///
/// Проза — источник последний по надёжности и последний по порядку: сверка с
/// именем каталога сессии для неё обязательна так же, как для вложенных полей.
///
/// Читаем ПОТОКОМ и с начала: путь стоит в первых записях, а файл бывает в
/// десятки мегабайт.
pub fn cwd_from_transcript(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path).ok()?;
    let mut deep: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut prose: Option<String> = None;
    for line in BufReader::new(f).lines().map_while(Result::ok).take(500) {
        // Проза ищется по СЫРОЙ строке, до разбора: она лежит внутри текста
        // сообщения, и добираться до неё обходом дерева значило бы просматривать
        // каждую строку любой записи. Первое совпадение и есть преамбула.
        if prose.is_none() {
            prose = cwd_from_prose(&line);
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // Верхний уровень — самый надёжный источник, дальше не ищем.
        if let Some(c) = v.get("cwd").and_then(Value::as_str) {
            let c = c.trim();
            if c.starts_with('/') {
                return Some(c.to_string());
            }
        }
        collect_cwd(&v, &mut deep);
    }
    let want = kimi_dir_name(path);
    let fits = |p: &String| match &want {
        // Имя каталога сессии знаем — кандидат обязан ему соответствовать.
        Some(name) => base_name(p).eq_ignore_ascii_case(name),
        // Не знаем (не kimi-раскладка) — сверять не с чем, берём частый.
        None => true,
    };
    let mut best: Vec<(String, usize)> = deep.into_iter().collect();
    // Частота решает; при равенстве — короткий путь: он ближе к корню проекта,
    // а вложенный подкаталог почти всегда след одной команды, а не сессии.
    best.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.len().cmp(&b.0.len())));
    best.into_iter()
        .map(|(p, _)| p)
        .find(&fits)
        .or_else(|| prose.filter(fits))
}

/// Рабочий каталог из текста преамбулы kimi: «The current working directory is
/// `/путь`». Берём то, что в обратных кавычках, — сам kimi так его и выделяет,
/// а без кавычек хвост фразы не отличить от начала следующего предложения.
fn cwd_from_prose(line: &str) -> Option<String> {
    // Экранированные кавычки JSON тут не мешают: обратная кавычка не
    // экранируется, а сам путь пробелов и кавычек не содержит.
    let i = line.find("current working directory is `")? + "current working directory is `".len();
    let rest = &line[i..];
    let p = rest.split('`').next()?.trim();
    (p.starts_with('/') && p.len() > 1).then(|| p.to_string())
}

/// Все значения `cwd`-подобных полей на любой глубине — с подсчётом частоты.
fn collect_cwd(v: &Value, out: &mut std::collections::HashMap<String, usize>) {
    match v {
        Value::Object(m) => {
            for (k, val) in m {
                if k == "cwd" || k == "workingDirectory" {
                    if let Some(s) = val.as_str() {
                        let s = s.trim();
                        if s.starts_with('/') {
                            *out.entry(s.to_string()).or_default() += 1;
                            continue;
                        }
                    }
                }
                collect_cwd(val, out);
            }
        }
        Value::Array(a) => {
            for val in a {
                collect_cwd(val, out);
            }
        }
        _ => {}
    }
}

/// Имя рабочей папки из раскладки kimi: `…/sessions/wd_<имя>_<хэш>/…` → `<имя>`.
/// `None` — путь не из kimi-раскладки, сверять не с чем.
pub fn kimi_dir_name(transcript: &Path) -> Option<String> {
    let s = transcript.to_string_lossy();
    let i = s.find("/sessions/wd_")? + "/sessions/wd_".len();
    let rest = &s[i..];
    let dir = rest.split('/').next()?;
    // Хвост после последнего `_` — хэш; имя может само содержать дефисы и цифры.
    let name = dir.rsplit_once('_')?.0;
    (!name.is_empty()).then(|| name.to_string())
}

/// Последний сегмент пути.
fn base_name(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
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

/* ================= дедуп: одну нить поднимают один раз ================= */

/// Сессии, оживление которых уже идёт. Ровно та же беда, что и с двумя
/// `--resume` по одному транскрипту (см. шаг 1 хендлера), только между ними нет
/// зазора, в который смотрит проверка «уже жива»: от запуска до первого хука
/// проходят десятки секунд, и всё это время сессии в реестре ещё НЕТ. Два
/// джарвиса, спросившие в один момент, оба увидели бы «мёртвая» и оба подняли.
///
/// Процессный, не дисковый: дедуп нужен между потребителями одного демона, а
/// после перезапуска демона поднимать заново — нормально.
fn reviving() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Занять сессию под оживление. `None` — её уже поднимает кто-то другой.
///
/// Возвращает RAII-сторож: место освобождается на ЛЮБОМ выходе, включая ранний
/// `?` и дроп будущего при перезапуске демона. Без этого первая же неудача
/// оставила бы сессию навсегда «оживляемой», и починить это можно было бы только
/// перезапуском — ровно тот класс дефекта, что мы чиним весь день.
fn claim(sid: &str) -> Option<Claim> {
    reviving()
        .lock()
        .unwrap()
        .insert(sid.to_string())
        .then(|| Claim(sid.to_string()))
}

struct Claim(String);

impl Drop for Claim {
    fn drop(&mut self) {
        reviving().lock().unwrap().remove(&self.0);
    }
}

/* ================= карточка: что человек видит вместо id ================= */

/// Карточка подтверждения оживления.
///
/// ОДНА функция на оба пути вопроса: гейт спрашивает её, когда гранта нет, а
/// хендлер — когда грант есть, но транскрипт перерос порог. Разные карточки на
/// один вопрос означали бы, что с грантом человек видит больше, чем без него.
///
/// Здесь НЕ зовётся `revive::assess`: он считает реплики полным проходом по
/// файлу, а на 50 МБ это секунды, которые карточке ни к чему. Цена считается по
/// хвосту — тем же способом, что и везде.
pub fn confirm_card(d: &Arc<Daemon>, sid: &str, reason: Option<&str>) -> Value {
    let Some((agent, path)) = find_transcript(sid) else {
        return json!({ "kind": "revive", "sessionId": sid, "gone": true, "reason": reason });
    };
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let a = revive::assess_light(agent, &path);
    let cost = revive::revive_cost(agent, a.context_tokens, a.model.as_deref());
    json!({
        "kind": "revive",
        "sessionId": sid,
        "agent": agent.label(),
        "label": d.session_label(sid),
        "cwd": cwd_from_transcript(&path).as_deref().map(crate::util::short_home),
        "contextTokens": a.context_tokens,
        "bytes": bytes,
        "cost": cost,
        "lastAt": a.last_message_at,
        // Зачем поднимают — единственное, чего из файла не узнать. Без этого
        // человек решает про деньги, не зная, за что платит.
        "reason": reason,
    })
}

/// Перерос ли транскрипт пороги, за которыми спрашивают даже при гранте.
/// Возвращает причину словами — её же увидит человек в карточке и в логе.
/// Чистая: числа на входе, решение на выходе.
pub fn beyond_grant(bytes: u64, usd: Option<f64>, max_mb: f64, max_usd: f64) -> Option<String> {
    let mb = bytes as f64 / 1_048_576.0;
    match usd {
        // Цена известна и велика — это главная из двух причин, её и называем.
        Some(u) if u >= max_usd => Some(format!(
            "первый ход обойдётся примерно в ${u:.2} при пороге ${max_usd:.2}"
        )),
        // Цена неизвестна вовсе: в транскрипте нет ни одной записи с usage.
        // Молча поднять «неизвестно за сколько» — то же самое, что поднять
        // дорого: спрашиваем, если файл при этом ещё и крупный.
        None if mb >= max_mb => Some(format!(
            "транскрипт {mb:.1} МБ при пороге {max_mb:.0} МБ, а цену по нему \
             определить не удалось — ни одной записи с расходом токенов"
        )),
        _ if mb >= max_mb => Some(format!(
            "транскрипт {mb:.1} МБ при пороге {max_mb:.0} МБ"
        )),
        _ => None,
    }
}

/* ================= копия перед подъёмом ================= */

/// Отложить копию транскрипта рядом, прежде чем звать CLI.
///
/// Решение владельца, и оно правильное независимо от того, кто виноват:
/// оживление — единственная наша операция, которая отдаёт чужому процессу файл
/// с невосстановимой историей и говорит «сделай с ним что-нибудь». Копия стоит
/// секунды диска, а разговор, которого больше нигде нет, — не стоит ничего,
/// пока он есть, и бесконечно много, когда его не стало.
///
/// Сегодняшняя тревога («kimi обнуляет wire.jsonl при `-S`») не подтвердилась:
/// все 37 транскриптов оказались целы, ни один не менялся, а «ноль реплик»
/// оказался нашим же счётчиком, не знавшим диалекта kimi. Но проверка эта — про
/// ПРОШЛОЕ. Про будущее поведение чужого CLI мы не знаем ничего, и узнавать это
/// ценой единственной копии данных не станем.
///
/// Копия НЕ обязательна для подъёма: не легла — предупреждаем словами и идём
/// дальше. Отказать в оживлении из-за нехватки места значило бы сделать
/// страховку дороже страхуемого.
fn backup_transcript(path: &Path, sid: &str) -> Result<PathBuf, String> {
    backup_into(&crate::util::jarvis_dir().join("transcript-backups"), path, sid)
}

/// То же, но каталог назван снаружи, — чтобы тест копий не писал в живой
/// `~/.jarvis` владельца. Проверка сохранности данных, которая сама лезет в
/// чужие данные, — плохая шутка.
fn backup_into(dir: &Path, path: &Path, sid: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("каталог копий не создался: {e}"))?;
    // Имя несёт и сессию, и момент: одну сессию поднимают не по одному разу, и
    // копия, затирающая предыдущую, защищает ровно от одной беды из двух.
    //
    // Миллисекунды для этого НЕ ХВАТАЕТ, и это поймал тест, а не рассуждение:
    // два подъёма подряд укладываются в одну и ту же миллисекунду, и вторая
    // копия молча ложилась поверх первой. Копия, затирающая копию, — худший вид
    // резервной копии: она есть, ей верят, а прежнего состояния в ней уже нет.
    let base = sid.replace(['/', '\\'], "_");
    let stamp = crate::util::now_ms();
    for n in 0..1000 {
        let name = match n {
            0 => format!("{base}-{stamp}.jsonl"),
            n => format!("{base}-{stamp}-{n}.jsonl"),
        };
        let to = dir.join(name);
        if to.exists() {
            continue;
        }
        std::fs::copy(path, &to).map_err(|e| format!("копия не легла: {e}"))?;
        return Ok(to);
    }
    Err("за одну миллисекунду набралась тысяча копий одной сессии — это не подъём, а цикл".into())
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
    // Зачем поднимают. Обязательный: оживление больше не проходит через карточку,
    // и строка в чате — единственное место, где человек узнаёт о нём. Строка без
    // причины («оживлена 3e819d75-…») ответа «зачем?» не даёт, а именно этот
    // вопрос он и задаст, увидев расход.
    let why = args
        .get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .ok_or(
            "нужен аргумент 'reason' — зачем поднимаешь эту сессию, одной строкой. \
             Оживление идёт без карточки подтверждения, и эта строка — всё, что \
             человек увидит в чате о потраченных деньгах",
        )?
        .to_string();

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

    // 1б. …и вторым ЗАХОДОМ тоже. Между запуском и первым хуком проходят десятки
    //     секунд, и всё это время проверка выше видит «мёртвая»: два джарвиса,
    //     спросившие одновременно, подняли бы оба. Сторож снимается сам на любом
    //     выходе ниже, включая ранний отказ.
    let Some(_claim) = claim(&sid) else {
        return Ok(json!({
            "ok": true,
            "state": "already-reviving",
            "sessionId": sid,
            "note": "эту сессию прямо сейчас поднимает кто-то другой — второй раз не поднимаю. \
                     Подожди полминуты и спроси sessions.get"
        }));
    };

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
    let integrity = revive::check_integrity(agent, &path);
    if integrity.verdict != Verdict::Ok {
        return Err(format!(
            "транскрипт не годен к оживлению: {}. Файл: {}",
            integrity.reason,
            path.display()
        ));
    }

    // 4. Родной каталог. Из транскрипта, а не от человека.
    // Транскрипт — основной источник, реестр — запасной: у сессии-обрубка (пара
    // записей, работы не было) пути нет нигде в файле, но демон мог записать его
    // хуком и держать в состоянии.
    let cwd = cwd_from_transcript(&path)
        .or_else(|| d.session(&sid).and_then(|s| s.cwd.clone()));
    let Some(cwd) = cwd.filter(|c| c.starts_with('/')) else {
        return Err(format!(
            "рабочий каталог сессии «{sid}» не удалось установить: в транскрипте \
             «{}» его нет, в реестре тоже. Поднимать вслепую нельзя — из чужого \
             каталога kimi встанет на вопрос о доверии, а claude поднимет разговор \
             про чужой проект",
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
    //    кэшу, весь контекст оплачивается как вход. Лестница проходится ДО
    //    запуска: узнать про стену, когда терминал уже открыт и деньги
    //    потрачены, — то же самое, что не узнать вовсе. Числа отказа (сколько
    //    осталось, сколько придержано, когда сброс) собирает `budget_refusal`.
    let a = revive::assess(agent, &path);
    let cost = revive::revive_cost(agent, a.context_tokens, a.model.as_deref());
    let usd = match &cost {
        ReviveCost::Known { usd, .. } => Some(*usd),
        // Вилка — берём ВЕРХНЮЮ границу: порог существует, чтобы не потратить
        // лишнего, и ошибаться ему положено в сторону вопроса, а не траты.
        ReviveCost::Range { usd_high, .. } => Some(*usd_high),
        ReviveCost::Unknown => None,
    };
    // Отказ обязан назвать ТРИ числа, а не два: сколько осталось и когда сброс
    // говорит лестница, а сколько стоит именно этот подъём — знаем только мы.
    // Без третьего человеку (и агенту) нечем решить, ждать сброса или взять
    // сессию поменьше: «не хватает» без цены не отличить от «не хватает на что
    // угодно».
    let hold = crate::ipc::budget_reserve(
        &d,
        agent.label(),
        a.model.as_deref(),
        true,
        "оживление сессии",
    )
    .await
    .map_err(|e| {
        format!(
            "{e}. Само оживление стоит {}",
            match &cost {
                ReviveCost::Known { usd, model } => format!(
                    "~${usd:.2} ({} токенов контекста, {model}) — первый ход идёт по холодному кэшу",
                    a.context_tokens.unwrap_or(0)
                ),
                ReviveCost::Range { usd_low, usd_high } =>
                    format!("${usd_low:.2}–${usd_high:.2}: модель в транскрипте не названа"),
                ReviveCost::Unknown =>
                    "неизвестно сколько: в транскрипте нет ни одной записи с расходом токенов".into(),
            }
        )
    })?;

    // 6. Ночью крупное оживление не делается: цена высокая, а человека нет.
    //    Здесь именно ОТКАЗ, а не вопрос: будить ради денег, которые подождут до
    //    утра, незачем.
    let night_usd = tune(&d, "nightUsd", NIGHT_USD);
    if crate::budget::is_night(&d) {
        if usd.is_some_and(|u| u >= night_usd) {
            hold.release();
            return Err(format!(
                "ночью крупные оживления не делаю: этот транскрипт поднимет \
                 ~{} токенов контекста, это около ${:.2} за первый ход при ночном \
                 пороге ${night_usd:.2}. Оживлю утром или разбуди меня явно",
                a.context_tokens.unwrap_or(0),
                usd.unwrap_or(0.0)
            ));
        }
    }

    // 6б. Порог, за которым спрашивают ДАЖЕ при выданном гранте.
    //
    //     Оживление вынесено в грант — джарвисы поднимают мёртвые сессии без
    //     карточки, и это решение владельца. Но карточка была последним местом,
    //     где человек видел цену, поэтому мелкое идёт молча, а крупное всё равно
    //     спрашивает. Это не обход гейта: вопрос здесь только ДОБАВЛЯЕТСЯ, снять
    //     его отсюда нельзя — если гейт уже спросил (гранта нет), человек ответил
    //     раньше, и второй раз мы его не дёргаем.
    if let Some(why_ask) = beyond_grant(
        a.file_bytes,
        usd,
        tune(&d, "confirmMb", CONFIRM_MB),
        tune(&d, "confirmUsd", CONFIRM_USD),
    ) {
        if crate::capability::grant::auto_approve_from_settings(&d.settings.load(), "agent")
            .contains("sessions.resume")
        {
            let mut card = confirm_card(&d, &sid, Some(&why));
            if let Some(o) = card.as_object_mut() {
                o.insert("beyondGrant".into(), Value::String(why_ask.clone()));
            }
            let before = format!("{sid}|{:?}", Some(a.file_bytes));
            let outcome = crate::capability::confirm_panel::ask(
                &d,
                "sessions.resume",
                RiskClass::Control.as_str(),
                Provenance::Trusted.as_str(),
                card,
                before,
                || {
                    let now = find_transcript(&sid)
                        .and_then(|(_, p)| std::fs::metadata(p).ok())
                        .map(|m| m.len());
                    format!("{sid}|{now:?}")
                },
            )
            .await;
            crate::log::line(&format!(
                "[resume] {sid}: спросили сверх гранта ({why_ask}) — {}",
                outcome.as_str()
            ));
            if !outcome.allows() {
                hold.release();
                return Err(format!(
                    "оживление не подтверждено ({}): {why_ask}. Разрешение без спроса на \
                     мелкие оживления не распространяется на крупные — порог правится в \
                     settings.json, ключи resume.confirmMb и resume.confirmUsd",
                    outcome.as_str()
                ));
            }
        }
    }

    // 6в. Копия транскрипта — ПЕРЕД тем, как отдать файл чужому процессу.
    //     Последний рубеж: дальше историей распоряжаемся не мы.
    match backup_transcript(&path, &sid) {
        Ok(to) => crate::log::line(&format!(
            "[resume] {sid}: копия транскрипта в {}",
            to.display()
        )),
        Err(e) => {
            // Не отказ: страховка не вправе стоить дороже страхуемого. Но и не
            // молчание — человек должен знать, что подъём пошёл без сетки.
            notes.push(format!(
                "копию транскрипта сделать не вышло ({e}) — поднимаю без неё; \
                 если разговор пропадёт, восстанавливать будет нечем"
            ));
            crate::log::line(&format!("[resume] {sid}: КОПИЯ НЕ СДЕЛАНА — {e}"));
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
            ..Default::default()
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

    // Строка в чате и в логе — обязательная часть решения «без подтверждения».
    // Убрав карточку, мы убрали единственное место, где человек узнавал о
    // расходе ДО него; значит он обязан узнать о нём ПОСЛЕ, и не из выписки
    // провайдера через сутки. Что подняли, зачем и почём — одной строкой.
    let label = d.session_label(&sid);
    announce(&d, &sid, &label, agent.label(), &why, &cwd, &a, &cost, &notes);

    Ok(json!({
        "ok": true,
        "state": "revived",
        "sessionId": sid,
        "agent": agent.label(),
        "cwd": cwd,
        "contextTokens": a.context_tokens,
        "cost": cost,
        "messages": a.message_count,
        "reason": why,
        "notes": notes,
    }))
}

/// Сказать человеку, что и за что подняли, — в чат и в лог одним текстом.
///
/// «Сколько стоило по факту» здесь — токены контекста, снятые с ТОГО САМОГО
/// транскрипта, который CLI сейчас прочитал: именно они уйдут во входе первого
/// хода по холодному кэшу. Считать делту недельного остатка бессмысленно —
/// расход доезжает до чисел провайдера через минуты, и сразу после хука она
/// показала бы ноль. Врать нулём хуже, чем назвать то, что знаешь точно.
#[allow(clippy::too_many_arguments)]
fn announce(
    d: &Arc<Daemon>,
    sid: &str,
    label: &str,
    agent: &str,
    why: &str,
    cwd: &str,
    a: &revive::Assessment,
    cost: &ReviveCost,
    notes: &[String],
) {
    let price = match cost {
        ReviveCost::Known { usd, model } => format!("~${usd:.2} ({model})"),
        ReviveCost::Range { usd_low, usd_high } => {
            format!("${usd_low:.2}–${usd_high:.2}, модель в транскрипте не названа")
        }
        ReviveCost::Unknown => "цену определить не удалось — в транскрипте нет записей с расходом".into(),
    };
    let mut text = format!(
        "Оживил «{label}» ({agent}) в {}. Зачем: {why}. \
         Контекст {} токенов, {} реплик, {:.1} МБ — первый ход по холодному кэшу {price}.",
        crate::util::short_home(cwd),
        a.context_tokens.map(|t| t.to_string()).unwrap_or_else(|| "?".into()),
        a.message_count,
        a.file_bytes as f64 / 1_048_576.0,
    );
    for n in notes {
        text.push_str("\n⚠ ");
        text.push_str(n);
    }
    crate::log::line(&format!("[resume] {sid}: {}", crate::util::one_line(&text)));
    // Канал тот же, что у карточек цепочки: они уже ложатся строкой в ленту
    // чата, и заводить вторую дорогу к тому же месту незачем. Метка чата
    // неизвестна — оживление приходит из капабилити, куда chat_id не доходит;
    // `null` UI кладёт в ту ленту, на которую человек смотрит (так же, как
    // карточку подтверждения).
    let _ = tauri::Emitter::emit(
        &d.app,
        "agent:resumed",
        json!({ "sessionId": sid, "label": label, "agent": agent,
                "reason": why, "text": text, "at": crate::util::now_ms() }),
    );
}

/* ================= что можно оживить ================= */

fn revivable_handler(d: Arc<Daemon>, args: Value) -> Result<Value, String> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100) as usize;

    // Сначала отбираем КОГО показывать, и только потом оцениваем — в таком
    // порядке, а не в обратном.
    //
    // Обратный порядок был живым тупиком: оценка звалась на каждом транскрипте, а
    // обрезка до `limit` шла после. На этой машине это 1570 файлов claude на
    // 952 МБ плюс 246 kimi на 92 МБ, и `revive::assess` читает каждый ЦЕЛИКОМ
    // (считает реплики). Дедлайн гейта — 30 секунд; список не собрался бы никогда,
    // а выглядело бы это как «капабилити висит».
    //
    // Отбор идёт по mtime файла: свежесть транскрипта — это и есть время
    // последней реплики, а метаданные не зависят от размера файла.
    let mut cands: Vec<(Agent, String, PathBuf, i64)> = Vec::new();
    for a in Agent::all().iter().copied() {
        for (sid, path) in transcripts_of(a) {
            if d.session(&sid).is_some() {
                continue; // живая — оживлять нечего
            }
            let mtime = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|dur| dur.as_millis() as i64)
                .unwrap_or(0);
            cands.push((a, sid, path, mtime));
        }
    }
    // Сортируем по свежести: оживляют почти всегда недавнее.
    cands.sort_by_key(|(_, _, _, mtime)| -mtime);
    cands.truncate(limit);

    let rows: Vec<Value> = cands
        .into_iter()
        .map(|(a, sid, path, mtime)| {
            // Лёгкая оценка: реплики полным проходом не считаем — на список их
            // не показывают, а стоят они всего файла целиком. Точное число
            // отдаёт сам подъём, там оно уместно и там оно одно.
            let it = revive::assess_light(a, &path);
            let cost = revive::revive_cost(a, it.context_tokens, it.model.as_deref());
            json!({
                "id": sid,
                "agent": a.label(),
                "contextTokens": it.context_tokens,
                "cost": cost,
                // Размер — справочно и НАМЕРЕННО не первым: он не предсказывает
                // цену. Замер на живых файлах: 4.7 МБ несли больше контекста,
                // чем 37.4 МБ.
                "bytes": it.file_bytes,
                // Время последней реплики из хвоста, а если его нет — mtime
                // файла. Пустое поле сортировкой не отличить от древнего.
                "lastAt": it.last_message_at.unwrap_or(mtime),
            })
        })
        .collect();
    let shown = rows.len();
    Ok(json!({ "sessions": rows, "shown": shown }))
}

pub fn register(reg: &mut DaemonRegistry) {
    // `register_slow`, а не `register`: успех оживления — это ПРИШЕДШИЙ ХУК
    // сессии, а не запущенный процесс, и ждать его меньше, чем CLI поднимает
    // разговор с диска, бессмысленно. Общий дедлайн гейта (30 с) обрубал вызов
    // на полпути и отдавал `failed:timeout` про сессию, которая как раз встала.
    reg.register_slow(
        CapabilityMeta {
            id: "sessions.resume",
            class: RiskClass::Control,
            provenance: Provenance::Trusted,
            description: "Оживить УМЕРШУЮ сессию с её прежним контекстом: процесс поднимается заново, \
разговор возвращается с диска. Зови, когда нужен контекст, которого нет в живых сессиях — \
после перезагрузки, закрытого терминала, упавшего CLI. Рабочий каталог подставляется сам из \
транскрипта, задавать его не надо. Живую сессию второй копией не поднимает — скажет, что она уже жива; \
ту, что уже поднимает кто-то другой, тоже. ОБЯЗАТЕЛЬНО задай 'reason' — зачем поднимаешь: карточки \
подтверждения у мелких оживлений нет, и эта строка единственная объяснит человеку в чате, за что \
списаны деньги. ДОРОГО: первый ход после оживления оплачивается как весь контекст сразу, поэтому вызов \
проходит через бюджет; крупные транскрипты спрашивают человека даже при выданном разрешении, \
а ночью отклоняются. Что можно оживить и почём — sessions.revivable. \
Возвращается ПОСЛЕ того, как сессия отметилась хуком: 'ok' здесь означает живую сессию, а не запуск.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "id мёртвой сессии" },
                    "reason": { "type": "string", "description": "зачем поднимаешь, одной строкой — человек увидит это в чате" }
                },
                "required": ["id", "reason"]
            }),
        },
        make_handler(|d: Arc<Daemon>, args: Value| async move { resume_handler(d, args).await }),
        GATE_DEADLINE,
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

    /// Каталог kimi-сессии берётся из ВЛОЖЕННЫХ записей и сверяется с именем
    /// каталога сессии.
    ///
    /// Живой дефект: у kimi поля `cwd` на верхнем уровне НЕТ вовсе, оно
    /// встречается только внутри записей. Пока читали верхний уровень, оживление
    /// kimi отказывало, и снаружи это выглядело как «сделано только под claude».
    #[test]
    fn a_kimi_working_dir_is_taken_from_nested_records_and_checked_by_the_dir_name() {
        let root = std::env::temp_dir().join(format!("jarvis-kimi-cwd-{}", std::process::id()));
        let sess = root.join("sessions").join("wd_fastworkbot_ff7e80bb2d68").join("session_x");
        std::fs::create_dir_all(&sess).unwrap();
        let f = sess.join("wire.jsonl");
        // Верхнего cwd нет; внутри — свой каталог дважды и ЧУЖОЙ один раз:
        // чужие пути в транскрипте попадаются, там агент что-то запускал.
        std::fs::write(
            &f,
            "{\"type\":\"metadata\"}\n\
             {\"tools\":[{\"args\":{\"cwd\":\"/Users/x/PycharmProjects/FastWorkBot\"}}]}\n\
             {\"call\":{\"args\":{\"cwd\":\"/tmp/somewhere-else\"}}}\n\
             {\"call\":{\"args\":{\"cwd\":\"/Users/x/PycharmProjects/FastWorkBot\"}}}\n",
        )
        .unwrap();
        assert_eq!(kimi_dir_name(&f).as_deref(), Some("fastworkbot"));
        assert_eq!(
            cwd_from_transcript(&f).as_deref(),
            Some("/Users/x/PycharmProjects/FastWorkBot"),
            "взят чужой каталог или не взято ничего"
        );

        // Имя каталога сессии не совпало ни с одним кандидатом — молчать нельзя,
        // но и гадать тоже: пусть вызывающий скажет об этом словами.
        let odd = root.join("sessions").join("wd_othername_deadbeef1234").join("session_y");
        std::fs::create_dir_all(&odd).unwrap();
        let g = odd.join("wire.jsonl");
        std::fs::write(&g, "{\"call\":{\"args\":{\"cwd\":\"/Users/x/Nope\"}}}\n").unwrap();
        assert_eq!(cwd_from_transcript(&g), None, "подставили каталог не от той сессии");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Короткая kimi-сессия тоже оживает: путь берётся из ПРОЗЫ преамбулы.
    ///
    /// Поймано замером на живых файлах, а не рассуждением: из 37 транскриптов
    /// kimi каталог определялся у 30, а семь отказывали со словами «каталог не
    /// записан» — и это выглядело как «kimi иногда не оживляется». Семь оказались
    /// КОРОТКИМИ сессиями (4–9 строк), где до вызова инструментов дело не дошло:
    /// поля `cwd` в них нет вообще, ни на каком уровне, а путь есть ровно один
    /// раз — в тексте системной преамбулы. После правки — 37 из 37.
    #[test]
    fn a_short_kimi_session_takes_its_dir_from_the_preamble_text() {
        let root = std::env::temp_dir().join(format!("jarvis-kimi-prose-{}", std::process::id()));
        let sess = root.join("sessions").join("wd_fastworkbot_ff7e80bb2d68").join("session_z");
        std::fs::create_dir_all(&sess).unwrap();
        let f = sess.join("wire.jsonl");
        // Форма — с живого файла: ни одного `cwd`, путь только в тексте.
        std::fs::write(
            &f,
            "{\"type\":\"meta\"}\n\
             {\"role\":\"system\",\"content\":\"…instead of trusting this value.\\n\\n## Working Directory\\n\\nThe current working directory is `/Users/x/PycharmProjects/FastWorkBot`. This should be considered as the project root…\"}\n",
        )
        .unwrap();
        assert_eq!(
            cwd_from_transcript(&f).as_deref(),
            Some("/Users/x/PycharmProjects/FastWorkBot"),
            "короткая kimi-сессия снова отказывается оживать"
        );

        // Проза — источник ПОСЛЕДНИЙ: сверка с именем каталога сессии для неё
        // обязательна так же, как для вложенных полей. Иначе преамбула, в
        // которой упомянут чужой проект, увела бы подъём не туда.
        let odd = root.join("sessions").join("wd_othername_deadbeef1234").join("session_w");
        std::fs::create_dir_all(&odd).unwrap();
        let g = odd.join("wire.jsonl");
        std::fs::write(
            &g,
            "{\"content\":\"The current working directory is `/Users/x/Nope`.\"}\n",
        )
        .unwrap();
        assert_eq!(cwd_from_transcript(&g), None, "проза протащила каталог не от той сессии");

        // …и поле бьёт прозу, когда есть и то и другое: поле точнее.
        let both = root.join("sessions").join("wd_fastworkbot_ff7e80bb2d68").join("session_v");
        std::fs::create_dir_all(&both).unwrap();
        let h = both.join("wire.jsonl");
        std::fs::write(
            &h,
            "{\"content\":\"The current working directory is `/Users/x/Stale/FastWorkBot`.\"}\n\
             {\"call\":{\"args\":{\"cwd\":\"/Users/x/PycharmProjects/FastWorkBot\"}}}\n",
        )
        .unwrap();
        assert_eq!(
            cwd_from_transcript(&h).as_deref(),
            Some("/Users/x/PycharmProjects/FastWorkBot"),
            "проза перебила поле, хотя поле надёжнее"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Целый транскрипт kimi не смеет читаться как пустой.
    ///
    /// Самая дорогая ошибка этой капабилити, и стоила она не поломки, а ложной
    /// тревоги об уничтожении данных. Счётчик реплик знал только форму claude
    /// (`type: user|assistant`), а kimi таких записей не пишет вовсе. Целый файл
    /// на 1.9 МБ с 86 сообщениями давал НОЛЬ реплик, и дальше:
    ///
    /// - `check_integrity` возвращал `NoMessages` — «оживлять нечего, разговора
    ///   не было». Это и есть настоящая причина «claude поднимает, kimi нет»:
    ///   мы отказывали сами, на целом файле, ещё до запуска kimi;
    /// - снаружи ноль читался как «файл сброшен», и полтора часа выяснялось, не
    ///   стирает ли наш же инструмент воскрешения то, ради чего его зовут.
    ///
    /// Тест на СИНТЕТИКЕ, а не на живых файлах: живые кончаются, а свойство
    /// «ноль реплик получается только у пустого разговора» обязано пережить и
    /// чистую машину, и чужую.
    #[test]
    fn a_whole_kimi_transcript_is_never_read_as_empty() {
        let dir = std::env::temp_dir().join(format!("jarvis-kimi-dialect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("wire.jsonl");
        // Форма — с живого файла владельца, слово в слово по именам полей.
        std::fs::write(
            &f,
            "{\"type\":\"turn.prompt\",\"input\":[{\"type\":\"text\",\"text\":\"привет\"}],\"time\":1787267364253}\n\
             {\"type\":\"context.append_message\",\"message\":{\"role\":\"user\"},\"time\":1787267364250}\n\
             {\"type\":\"context.append_message\",\"message\":{\"role\":\"assistant\"},\"time\":1787267372000}\n\
             {\"type\":\"usage.record\",\"model\":\"kimi-code/k3\",\"usage\":{\"inputOther\":5426,\"output\":39,\"inputCacheRead\":18944,\"inputCacheCreation\":0},\"time\":1787267372831}\n\
             {\"type\":\"turn.ended\",\"turnId\":0,\"reason\":\"completed\",\"time\":1787267372880}\n",
        )
        .unwrap();

        // 1. Годен к оживлению — а не «разговора не было».
        let it = crate::revive::check_integrity(Agent::Kimi, &f);
        assert_eq!(
            it.verdict,
            Verdict::Ok,
            "целый kimi-транскрипт снова забракован: {}",
            it.reason
        );
        assert_eq!(it.message_lines, 2, "реплики kimi снова не считаются");

        // 2. Свежесть берётся из `time`, а не из `timestamp`. Без этого у ВСЕХ
        //    kimi-сессий она нулевая, они уезжают в конец сортировки, и `limit`
        //    срезает их целиком за claude-сессиями: «в списке только claude».
        let a = crate::revive::assess(Agent::Kimi, &f);
        assert_eq!(a.last_message_at, Some(1787267372880), "свежесть kimi снова нулевая");
        assert_eq!(a.message_count, 2);

        // 3. Токены — из полей kimi, и цена считается по прайсу kimi, а не claude.
        assert_eq!(a.context_tokens, Some(5426 + 18944), "контекст kimi не посчитан");
        assert_eq!(a.model.as_deref(), Some("kimi-code/k3"));
        match crate::revive::revive_cost(Agent::Kimi, a.context_tokens, a.model.as_deref()) {
            crate::revive::ReviveCost::Known { usd, model } => {
                assert_eq!(model, "K3", "модель kimi не распознана: {model}");
                assert!(usd > 0.0, "цена kimi вышла нулевой");
            }
            other => panic!("цена kimi посчиталась вилкой или никак: {other:?}"),
        }

        // 4. И ноль по-прежнему возможен — но только там, где разговора правда
        //    не было. Иначе «пусто» перестанет что-либо значить.
        let g = dir.join("service-only.jsonl");
        std::fs::write(&g, "{\"type\":\"tools.update_store\",\"time\":1}\n").unwrap();
        assert_eq!(crate::revive::check_integrity(Agent::Kimi, &g).verdict, Verdict::NoMessages);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Перед оживлением транскрипт копируется — до того, как файл уйдёт чужому
    /// процессу.
    ///
    /// Копия не потому, что kimi уличён (не уличён: все 37 транскриптов целы, ни
    /// один не менялся), а потому, что оживление — единственная наша операция,
    /// отдающая невосстановимую историю чужому CLI со словами «сделай с ней
    /// что-нибудь». Про прошлое мы теперь знаем, про будущее — нет.
    #[test]
    fn the_transcript_is_copied_before_anyone_else_touches_it() {
        let dir = std::env::temp_dir().join(format!("jarvis-backup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("wire.jsonl");
        std::fs::write(&f, "{\"type\":\"turn.prompt\"}\n").unwrap();
        let box_ = dir.join("backups");
        let to = backup_into(&box_, &f, "sid-1").expect("копия не легла");
        assert!(to.is_file(), "копии нет на диске");
        assert_eq!(std::fs::read(&f).unwrap(), std::fs::read(&to).unwrap(), "копия не совпала");
        // Одну сессию поднимают не по одному разу, и два подъёма подряд
        // укладываются в ОДНУ миллисекунду. Копия, затирающая копию, — худший
        // вид резервной копии: она есть, ей верят, а прежнего состояния в ней
        // уже нет. Тест поймал ровно это.
        let again = backup_into(&box_, &f, "sid-1").expect("вторая копия не легла");
        assert_ne!(to, again, "вторая копия затёрла первую");
        assert!(to.is_file() && again.is_file(), "одна из копий исчезла");
        let _ = std::fs::remove_dir_all(&dir);

        // …и копия делается ДО запуска, а не после: после — это уже не копия.
        let src = include_str!("resume.rs");
        let body = src.split("async fn resume_handler").nth(1).expect("хендлер на месте");
        let copy = body.find("backup_transcript(").expect("копии перед подъёмом нет вовсе");
        let launch = body.find("launch_core(").expect("запуска нет");
        assert!(copy < launch, "копия делается после того, как файл отдали CLI");
    }

    /// Список и подъём обязаны видеть ОДНО И ТО ЖЕ.
    ///
    /// Расхождение поймала живая проверка: `sessions.revivable` показывал
    /// claude-сессию, а `sessions.resume` отвечал «транскрипта нет». Причина —
    /// у claude `find_transcript_by_sid` не реализован вовсе (путь приносит
    /// хук, а у мёртвой сессии хука нет), и подъём искал не тем способом, каким
    /// строился список.
    #[test]
    fn what_the_list_offers_is_what_resume_can_find() {
        for a in Agent::all().iter().copied() {
            let all = transcripts_of(a);
            let Some((sid, path)) = all.into_iter().next() else {
                continue; // этого агента на машине нет — проверять нечего
            };
            let found = find_transcript(&sid);
            assert!(
                found.is_some(),
                "{}: сессия {sid} есть в списке, но подъём её не находит",
                a.label()
            );
            let (_, p) = found.unwrap();
            assert_eq!(p, path, "{}: список и подъём указывают на разные файлы", a.label());
        }
    }

    /// Мелкое идёт молча, крупное спрашивает — даже когда грант выдан.
    ///
    /// Это цена решения «воскрешать без подтверждения»: карточка была последним
    /// местом, где человек видел цену, и вместо неё цену держат два порога.
    #[test]
    fn small_revivals_go_quiet_and_big_ones_still_ask() {
        let mb = |n: f64| (n * 1_048_576.0) as u64;
        // обычная сессия: и мелкая, и дешёвая — вопроса нет
        assert_eq!(beyond_grant(mb(3.0), Some(0.12), 20.0, 1.0), None);

        // дорогая при скромном размере: решают ДЕНЬГИ, и названы они
        let why = beyond_grant(mb(4.7), Some(2.40), 20.0, 1.0).expect("дорогое прошло молча");
        assert!(why.contains("$2.40") && why.contains("$1.00"), "{why}");

        // жирная по байтам при известной скромной цене — тоже вопрос: крупный
        // файл это ещё и минуты подъёма, а не только деньги
        let why = beyond_grant(mb(37.4), Some(0.31), 20.0, 1.0).expect("крупное прошло молча");
        assert!(why.contains("37.4 МБ") && why.contains("20 МБ"), "{why}");

        // цену определить не удалось, а файл крупный — молчать нельзя:
        // «неизвестно за сколько» ничем не лучше, чем «дорого»
        let why = beyond_grant(mb(25.0), None, 20.0, 1.0).expect("неизвестная цена прошла молча");
        assert!(why.contains("определить не удалось"), "{why}");
        // …а если он при этом мелкий — не дёргаем: у коротких сессий записи с
        // расходом может не быть просто потому, что ходов было мало
        assert_eq!(beyond_grant(mb(0.4), None, 20.0, 1.0), None);

        // Порог из настроек, а не из константы: подняли — стало тихо.
        assert_eq!(beyond_grant(mb(37.4), Some(0.31), 100.0, 10.0), None);
    }

    /// Отказ по бюджету называет ТРИ числа, а не два.
    ///
    /// Сколько осталось и когда сброс говорит лестница; сколько стоит ИМЕННО
    /// этот подъём, знает только оживление. Без третьего числа отказ не отличим
    /// от «не хватает на что угодно», и решить, ждать ли сброса или взять сессию
    /// поменьше, нечем — а карточки, где это было видно, у оживления больше нет.
    #[test]
    fn a_budget_refusal_names_the_price_of_this_very_revival() {
        let src = include_str!("resume.rs");
        let body = src
            .split("async fn resume_handler")
            .nth(1)
            .and_then(|t| t.split("launch_core").next())
            .expect("хендлер подъёма на месте");
        let refusal = body
            .split("budget_reserve(")
            .nth(1)
            .and_then(|t| t.split("?;").next())
            .expect("гейта бюджета в подъёме нет вовсе");
        assert!(refusal.contains("map_err"), "отказ лестницы уходит как есть, без цены подъёма");
        assert!(refusal.contains("ReviveCost::Unknown"), "неизвестная цена в отказе не названа");
        assert!(
            refusal.contains("холодному кэшу"),
            "в отказе не сказано, почему первый ход стоит весь контекст"
        );
        // И лестница проходится ДО запуска: узнать про стену, когда терминал уже
        // открыт и деньги потрачены, — то же самое, что не узнать вовсе.
        assert!(
            body.contains("budget_reserve("),
            "бюджет перестал спрашиваться до подъёма"
        );
    }

    /// Двое не воскрешают одну сессию одновременно.
    ///
    /// Проверки «уже жива» тут мало: от запуска до первого хука проходят десятки
    /// секунд, и всё это время сессии в реестре НЕТ — два джарвиса, спросившие
    /// разом, оба увидели бы «мёртвая» и оба подняли бы `--resume` по одной нити.
    #[test]
    fn two_jarvises_never_revive_the_same_session_at_once() {
        let sid = format!("dedup-test-{}", std::process::id());
        let first = claim(&sid).expect("первый заход обязан пройти");
        assert!(claim(&sid).is_none(), "вторая копия подъёма прошла — разговор испорчен обоим");
        // соседнюю сессию это не держит
        let other = claim(&format!("{sid}-other")).expect("чужой sid заперт зря");

        // Сторож снимается САМ, на любом выходе. Иначе первая же неудача
        // оставила бы сессию навечно «оживляемой», и лечилось бы это только
        // перезапуском демона.
        drop(first);
        let again = claim(&sid).expect("место не освободилось после выхода");
        drop(again);
        drop(other);
        assert!(reviving().lock().unwrap().is_empty(), "реестр подъёмов подтекает");
    }

    /// Ожидание хука обязано умещаться в дедлайн гейта.
    ///
    /// Дефект был живым и тихим: общий дедлайн гейта — 30 с, а подъёма мы ждём
    /// 60 с. Всё, что встаёт дольше тридцати секунд, получало `failed:timeout`
    /// про сессию, которая как раз встала, — и бронь бюджета при этом не
    /// возвращалась. Сторож здесь потому, что оба числа правятся по отдельности.
    #[test]
    fn the_gate_deadline_outlives_the_wait_for_the_hook() {
        assert!(
            GATE_DEADLINE > HOOK_WAIT,
            "гейт (={:?}) обрубит подъём раньше, чем истечёт ожидание хука (={:?})",
            GATE_DEADLINE,
            HOOK_WAIT
        );
        assert!(
            GATE_DEADLINE > crate::capability::GateConfig::default().handler_timeout,
            "свой дедлайн не длиннее общего — тогда он не нужен вовсе"
        );
        // …и он действительно проставлен при регистрации, а не только объявлен.
        // Режем по СЛЕДУЮЩЕЙ регистрации (`reg.register(`), а не по имени
        // соседа: имя соседа стоит и в описании подъёма — «что можно оживить и
        // почём», — и срез по нему обрубал бы блок раньше самой регистрации.
        let src = include_str!("resume.rs");
        let reg = src.split("pub fn register(").nth(1).expect("регистрация на месте");
        let mine = reg.split("    reg.register(").next().unwrap_or_default();
        assert!(mine.contains("reg.register_slow("), "sessions.resume снова на общем дедлайне");
        assert!(mine.contains("GATE_DEADLINE,"), "дедлайн подъёма не назван при регистрации");
    }

    /// Список отбирает, ПОТОМ оценивает — и никогда наоборот.
    ///
    /// Обратный порядок был живым тупиком: `assess` читает транскрипт целиком, а
    /// обрезка до `limit` шла после него. На машине владельца это 1570 файлов
    /// claude на 952 МБ плюс 246 kimi на 92 МБ — при дедлайне гейта в 30 секунд
    /// список не собрался бы никогда, и выглядело бы это как «капабилити висит».
    #[test]
    fn the_list_picks_first_and_weighs_after() {
        let src = include_str!("resume.rs");
        let body = src
            .split("fn revivable_handler")
            .nth(1)
            .and_then(|t| t.split("#[cfg(test)]").next())
            .expect("хендлер списка на месте");
        // Ищем ВЫЗОВ, а не слово: слово стоит и в объяснении над кодом, ради
        // которого этот сторож и написан, — и ловил бы сам себя.
        let truncate = body.find(".truncate(limit)").expect("обрезки до limit нет вовсе");
        let weigh = body.find("revive::assess_light(").expect("оценки нет вовсе");
        assert!(truncate < weigh, "оценка снова идёт до обрезки — список читает все транскрипты");
        assert!(
            !body.contains("revive::assess(&"),
            "в списке снова полная оценка: она считает реплики проходом по всему файлу"
        );
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
