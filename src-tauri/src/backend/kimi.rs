//! Kimi-бэкенд (Moonshot `kimi`, Kimi Code CLI). Sync-методы шва; async/stateful-части —
//! свободными функциями в профильных модулях. Транскрипт наполняется по инкрементам
//! (см. спеку `2026-08-19-kimi-cli-support-design.md`); здесь то, что известно статически.
//!
//! Важно про источники: `~/.kimi-code` — это **Kimi Code CLI**, актуальный продукт.
//! Legacy `kimi-cli` жил в `~/.kimi` и имел другой набор хуков; его документацию
//! использовать нельзя.

use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::{Agent, Backend};
use crate::transcript::ChatItem;

pub struct KimiBackend;

/// Статический инстанс для диспетчера `backend()`.
pub static KIMI: KimiBackend = KimiBackend;

/// Дом Kimi Code CLI: `$KIMI_CODE_HOME` или `~/.kimi-code`.
pub fn kimi_home() -> PathBuf {
    match std::env::var("KIMI_CODE_HOME") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => crate::util::home_dir().join(".kimi-code"),
    }
}

/// Настоящий `kimi` в PATH (+типовые каталоги), минуя наш шим `~/.jarvis/shims`.
///
/// Штатная установка кладёт бинарь в `<дом>/bin/kimi` и добавляет его в PATH,
/// поэтому дом проверяем явно — так детект работает и до правки PATH.
pub fn resolve_kimi_bin() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    for extra in [
        kimi_home().join("bin"),
        crate::util::home_dir().join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ] {
        if !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    let shims = crate::util::jarvis_dir().join("shims");
    for d in dirs {
        if d == shims {
            continue;
        }
        let p = d.join("kimi");
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                return Some(p);
            }
        }
    }
    None
}

/// Найти каталог сессии Kimi по `session_id`.
///
/// Хук Kimi НЕ приносит путь к транскрипту (в отличие от Claude и Codex), поэтому
/// демон обязан находить его сам. Раскладка: `<дом>/sessions/<wd_ключ>/<session_id>/`,
/// где `wd_ключ` = `workdir_key(cwd)`. Ключ мы тут НЕ считаем: на входе только
/// sid, cwd неизвестен — перебираем каталоги первого уровня и проверяем наличие
/// подкаталога с именем sid (воркспейсов единицы, цена перебора нулевая).
pub fn find_session_dir_by_sid(sid: &str) -> Option<PathBuf> {
    find_session_dir_in(&kimi_home().join("sessions"), sid)
}

/// Чистое ядро поиска (тестируется на temp-каталоге без env).
fn find_session_dir_in(root: &Path, sid: &str) -> Option<PathBuf> {
    if sid.is_empty() || sid.contains('/') {
        return None; // пустой или подозрительный sid — не ходим по путям
    }
    let rd = std::fs::read_dir(root).ok()?;
    for e in rd.flatten() {
        let cand = e.path().join(sid);
        if cand.is_dir() {
            return Some(cand);
        }
    }
    None
}

/// Транскрипт главного агента сессии. Сабагенты пишут свои `wire.jsonl`
/// в `agents/agent-N/`; для ленты чата нужен только `main`.
pub fn wire_path_for_sid(sid: &str) -> Option<PathBuf> {
    find_session_dir_by_sid(sid).map(|d| d.join("agents/main/wire.jsonl"))
}

/* ==================== доверие к каталогу ==================== */

/// Отметки «этому каталогу доверяем» — `<дом>/workspace-trust/<ключ>`.
fn workspace_trust_dir() -> PathBuf {
    kimi_home().join("workspace-trust")
}

/// Путь в том написании, в каком его увидит сам kimi: cwd он берёт из `getcwd`,
/// а тот уже разыменовал симлинки (`/tmp/x` → `/private/tmp/x` на macOS).
/// Промах по написанию — промах по ключу, то есть ровно тот висящий вопрос,
/// ради которого всё это и пишется.
fn trust_root(cwd: &Path) -> String {
    std::fs::canonicalize(cwd)
        .unwrap_or_else(|_| cwd.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Помечен ли каталог доверенным. Нужно не только до запуска, но и после:
/// «сессия не встала» без причины человек чинить не может.
pub fn workspace_trusted(cwd: &Path) -> bool {
    workspace_trust_dir().join(workdir_key(&trust_root(cwd))).exists()
}

/// Пометить каталог доверенным ДО запуска kimi.
///
/// Без отметки TUI встаёт на «Trust this folder?» и ждёт клавишу: хук старта не
/// приходит, сессия не регистрируется, талон `sessions.spawn` снимается по
/// таймауту, а первый промпт уезжает в никуда. Человек у панели нажал бы сам —
/// поднятая агентом сессия зрителей не имеет.
///
/// Способ штатный: тот же файл той же формы пишет сам CLI по кнопке Trust
/// (`WorkspaceTrustService.trust()` → `workspace-trust/<ключ>`); флага запуска,
/// снимающего вопрос, у 0.38 нет — ни в `--help`, ни в env, ни в config.toml.
///
/// Цена честная: вместе с вопросом включаются project-level MCP-серверы из
/// `<git-root>/.mcp.json` и `<cwd>/.kimi-code/mcp.json`. Это ровно то, что
/// человек включает кнопкой, и ровно то, на что он подписался, выдав грант на
/// запуск сессий в этом каталоге.
pub fn ensure_workspace_trust(cwd: &Path) -> Result<(), String> {
    ensure_workspace_trust_in(&workspace_trust_dir(), cwd)
}

/// Чистое ядро отметки (тестируется на temp-каталоге без env).
fn ensure_workspace_trust_in(dir: &Path, cwd: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let root = trust_root(cwd);
    let dst = dir.join(workdir_key(&root));
    if dst.exists() {
        return Ok(()); // уже доверено — чужую отметку не переписываем
    }
    let doc = serde_json::json!({ "root": root, "trustedAt": crate::util::now_ms() }).to_string();
    // tmp+rename: kimi пишет этот файл атомарно и читает его на старте — рваный
    // JSON под гонкой означал бы тот же вопрос, только ещё и с ошибкой.
    let tmp = dir.join(format!(".{}.tmp", std::process::id()));
    std::fs::write(&tmp, doc).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &dst).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", dst.display())
    })
}

/// Ключ каталога в хранилищах Kimi: `wd_<слаг basename>_<sha256(путь)[..12]>`.
/// Одним и тем же ключом названы и каталог сессий, и файл доверия.
pub fn workdir_key(path: &str) -> String {
    let norm = path.replace('\\', "/");
    let norm = norm.trim_end_matches('/');
    let base = norm.rsplit('/').next().unwrap_or(norm);
    format!("wd_{}_{}", workdir_slug(base), &sha256_hex(norm.as_bytes())[..12])
}

/// Слаг имени каталога — точный порт `slugifyWorkDirName`: нижний регистр,
/// любая пачка чужих символов схлопывается в один дефис, края обрезаются
/// до и после лимита в 40 символов.
fn workdir_slug(name: &str) -> String {
    let mut out = String::new();
    let mut prev_bad = false;
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            prev_bad = false;
        } else {
            if !prev_bad {
                out.push('-');
            }
            prev_bad = true;
        }
    }
    let cut: String = out.trim_matches('-').chars().take(40).collect();
    let slug = cut.trim_matches('-');
    if slug.is_empty() || slug == "." || slug == ".." {
        "workspace".to_string()
    } else {
        slug.to_string()
    }
}

/// SHA-256 (FIPS 180-4). Свои сорок строк вместо новой строки в общем
/// `Cargo.toml`: хеш нужен ровно в одном месте — ключе каталога Kimi, — а
/// манифест общий на весь воркспейс.
fn sha256_hex(data: &[u8]) -> String {
    #[rustfmt::skip]
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let (a, b) = (w[i - 15], w[i - 2]);
            let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
            let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            v = [t1.wrapping_add(s0.wrapping_add(maj)), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for (dst, src) in h.iter_mut().zip(v) {
            *dst = dst.wrapping_add(src);
        }
    }
    h.iter().map(|w| format!("{w:08x}")).collect()
}

impl Backend for KimiBackend {
    fn agent(&self) -> Agent {
        Agent::Kimi
    }
    fn cli_found(&self) -> bool {
        resolve_kimi_bin().is_some()
    }
    fn read_entries(&self, file: &Path, max_bytes: u64) -> Vec<Value> {
        // wire.jsonl линейный (append-only, без uuid/parentUuid) → просто хвост JSONL.
        crate::transcript::read_recent_entries(file, max_bytes)
    }
    fn entries_from_text(&self, text: &str) -> Vec<Value> {
        crate::transcript::entries_from_text(text)
    }
    fn to_chat_items(&self, entry: &Value) -> Vec<ChatItem> {
        super::kimi_transcript::to_chat_items(entry)
    }
    fn extract_title(&self, entries: &[Value]) -> Option<String> {
        // основной заголовок живёт в state.json рядом с wire.jsonl (читает демон);
        // здесь фолбэк — первая реплика юзера, как у Codex.
        super::kimi_transcript::extract_title(entries)
    }
    fn extract_branch(&self, _entries: &[Value]) -> Option<String> {
        None // Kimi не сохраняет ветку нигде — фолбэк по .git/HEAD от cwd
    }
    fn extract_model(&self, entries: &[Value]) -> Option<String> {
        super::kimi_transcript::extract_model(entries)
    }
    fn transcript_dir_for(&self, _cwd: &str) -> Option<PathBuf> {
        None // путь к транскрипту резолвится по sid, а не по cwd — см. wire_path_for_sid
    }
    fn find_transcript_by_sid(&self, sid: &str) -> Option<PathBuf> {
        // Для Kimi это не фолбэк, а основной путь: его хуки `transcript_path`
        // не приносят вовсе — проверено на живом payload всех событий.
        wire_path_for_sid(sid).filter(|p| p.exists())
    }
    fn final_reply(&self, entries: &[Value]) -> Option<String> {
        super::kimi_transcript::full_final_reply(entries)
    }
    fn session_before_prompt(&self) -> bool {
        // Kimi Code 0.38 сессию до первой реплики НЕ заводит и говорит об этом
        // на своём же приветственном экране: «No session yet — one will be
        // created on your first message», поле `Session:` пустое, хука
        // `session-start` нет. Ждать его = ждать вечно (снято с живого лога:
        // четыре подъёма подряд, ноль хуков, четыре таймаута по 90 с).
        false
    }
    fn supports_custom_answer(&self) -> bool {
        // Проверено на живом пикере Kimi Code 0.37: строка «Other» есть всегда
        // и стоит последней — выбираешь её, печатаешь ответ, Enter сохраняет.
        true
    }
    fn resume_cmd(&self, sid: &str) -> String {
        format!("kimi -S {sid}")
    }
    fn friendly_model(&self, id: &str) -> String {
        // На проводе бывает и полный алиас (`kimi-code/k3`), и короткое имя (`k3`).
        let v = id.rsplit('/').next().unwrap_or(id).to_lowercase();
        if v.starts_with("k3") {
            return if v.contains("256k") { "K3-256k" } else { "K3" }.to_string();
        }
        if v.contains("kimi-for-coding") {
            return if v.contains("highspeed") {
                "K2.7 Coding Highspeed"
            } else {
                "K2.7 Coding"
            }
            .to_string();
        }
        v
    }
    fn models(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("kimi-code/k3", "K3"),
            ("kimi-code/k3-256k", "K3-256k"),
            ("kimi-code/kimi-for-coding", "K2.7 Coding"),
            ("kimi-code/kimi-for-coding-highspeed", "K2.7 Coding Highspeed"),
        ]
    }
    fn effort_levels(&self) -> &'static [&'static str] {
        // `support_efforts` модели K3 из config.toml; дефолт — high.
        &["low", "high", "max"]
    }
    fn has_separate_effort(&self) -> bool {
        true // у Kimi effort задаётся отдельно от модели, как у Claude
    }
    fn price(&self, _model: &str) -> (f64, f64) {
        // ОЦЕНКА: официальных цен в конфиге Kimi нет, $/1M (in, out).
        (0.6, 2.5)
    }
    /// У K3 миллион — он же стоит в его собственной панели («/ 1M»); у
    /// K3-256k ровно четверть от него, и это тот самый случай, ради которого
    /// потолок берётся из модели, а не из общей константы.
    fn context_window(&self, model: &str) -> Option<u64> {
        let v = model.rsplit('/').next().unwrap_or(model).to_lowercase();
        if v.contains("256k") {
            return Some(256_000);
        }
        if v.starts_with("k3") {
            return Some(1_000_000);
        }
        v.contains("kimi-for-coding").then_some(256_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_model_understands_alias_and_short_name() {
        assert_eq!(KIMI.friendly_model("kimi-code/k3"), "K3");
        assert_eq!(KIMI.friendly_model("k3"), "K3");
        assert_eq!(KIMI.friendly_model("kimi-code/k3-256k"), "K3-256k");
        assert_eq!(KIMI.friendly_model("kimi-code/kimi-for-coding"), "K2.7 Coding");
        assert_eq!(
            KIMI.friendly_model("kimi-code/kimi-for-coding-highspeed"),
            "K2.7 Coding Highspeed"
        );
    }

    #[test]
    fn resume_and_effort_shape() {
        assert_eq!(KIMI.resume_cmd("session_abc"), "kimi -S session_abc");
        assert!(KIMI.has_separate_effort(), "effort у Kimi отдельный, не внутри /model");
        assert_eq!(KIMI.effort_levels(), &["low", "high", "max"]);
        assert_eq!(KIMI.models().len(), 4);
    }

    #[test]
    fn find_session_dir_scans_workspaces() {
        // <дом>/sessions/<wd_*>/<session_id>/ — sid лежит на втором уровне.
        let root = std::env::temp_dir().join("jarvis-kimi-session-test");
        let _ = std::fs::remove_dir_all(&root);
        let want = root.join("wd_proj_0123456789ab/session_AAA");
        std::fs::create_dir_all(&want).unwrap();
        std::fs::create_dir_all(root.join("wd_other_ba9876543210/session_BBB")).unwrap();

        assert_eq!(find_session_dir_in(&root, "session_AAA").as_deref(), Some(want.as_path()));
        assert_eq!(find_session_dir_in(&root, "session_ZZZ"), None, "нет матча → None");
        assert_eq!(find_session_dir_in(&root, ""), None, "пустой sid → None");
        assert_eq!(find_session_dir_in(&root, "../etc"), None, "sid с путём отвергаем");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // 64 байта ровно — граница блока: паддинг уезжает во второй блок
        assert_eq!(
            sha256_hex(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    #[test]
    fn workdir_key_matches_live_kimi() {
        // Ключи сняты с живого ~/.kimi-code (0.38.0): и каталоги сессий, и
        // файлы workspace-trust названы ровно так. Если тест покраснел —
        // формат ключа у Kimi поехал, и доверие мы больше не проставим.
        assert_eq!(workdir_key("/Users/pozitiv4500/Goool"), "wd_goool_77dc8e222ca0");
        assert_eq!(workdir_key("/private/tmp/jarvis-hooks"), "wd_jarvis-hooks_9b8f89d04ef6");
        assert_eq!(
            workdir_key("/Users/pozitiv4500/PycharmProjects/FastWorkBot/wt-server-71523"),
            "wd_wt-server-71523_4e00b9778a3a"
        );
        // хвостовой слэш kimi срезает ДО хеширования — иначе ключ разъедется
        assert_eq!(workdir_key("/Users/pozitiv4500/Goool/"), "wd_goool_77dc8e222ca0");
    }

    #[test]
    fn workdir_slug_ports_kimi_rules() {
        assert_eq!(workdir_slug("MySmallProject"), "mysmallproject");
        assert_eq!(workdir_slug("wt-server_1.2"), "wt-server_1.2");
        // пачка чужих символов схлопывается в ОДИН дефис, края обрезаются
        assert_eq!(workdir_slug(" a  b "), "a-b");
        assert_eq!(workdir_slug("a-№-b"), "a---b", "дефис из ввода живёт своей жизнью");
        // не осталось ничего пригодного — у Kimi это «workspace»
        assert_eq!(workdir_slug("проект"), "workspace");
        assert_eq!(workdir_slug(".."), "workspace");
        assert_eq!(workdir_slug(&"x".repeat(50)), "x".repeat(40), "лимит 40 символов");
    }

    #[test]
    fn trust_mark_is_written_once_and_is_readable_by_kimi() {
        let dir = std::env::temp_dir().join("jarvis-kimi-trust-test");
        let _ = std::fs::remove_dir_all(&dir);
        let cwd = std::env::temp_dir().join("jarvis-kimi-trust-cwd");
        std::fs::create_dir_all(&cwd).unwrap();

        ensure_workspace_trust_in(&dir, &cwd).unwrap();
        let root = trust_root(&cwd);
        let f = dir.join(workdir_key(&root));
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(doc["root"].as_str(), Some(root.as_str()), "root — каноничный путь");
        assert!(doc["trustedAt"].as_i64().unwrap_or(0) > 0, "{doc}");

        // повторный вызов не трогает уже стоящую отметку
        let was = doc["trustedAt"].as_i64().unwrap();
        ensure_workspace_trust_in(&dir, &cwd).unwrap();
        let again: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(again["trustedAt"].as_i64(), Some(was), "отметку не переписываем");
        // временных файлов после себя не оставляем
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 1, "{left:?}");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&cwd);
    }
}
