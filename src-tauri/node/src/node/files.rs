//! Кусок транскрипта — единственное, что узел показывает из файловой системы.
//!
//! Узел не файловый сервер: путь канонизируется и обязан лежать под корнем
//! транскриптов (`~/.claude`, `~/.codex`). Канонизация, а не разбор строки —
//! она разворачивает и `..`, и симлинк, поэтому подставить путь наружу нельзя
//! ни склейкой, ни ссылкой из разрешённого каталога (дизайн 2026-08-05).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Потолок одного куска. Ноут дочитывает хвост следующим запросом с `next`,
/// поэтому жирный транскрипт не занимает узел одним ответом и не раздувает
/// туннель разовым мегабайтом.
pub const CHUNK_MAX: usize = 512 * 1024;

/// Почему отказали. Разделение нужное: «транскрипта ещё нет» (свежая сессия,
/// файл не создан) ноут переживает ожиданием, а «вне корней» — это конец
/// разговора, повторять запрос бессмысленно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    /// Файла нет, но его каталог под разрешённым корнем.
    Missing,
    /// Вне корней транскриптов — мимо, через `..`, через симлинк либо не файл.
    /// Тот же ответ отдаём и на несуществующий путь за границами: узел не
    /// подтверждает существование того, что и так не отдал бы.
    Outside,
}

/// Кусок файла с точным указанием, докуда дочитано.
pub struct Chunk {
    pub data: String,
    /// Фактическое смещение чтения. Если оно меньше запрошенного — файл
    /// переписали с нуля (compact/rotate), и ноуту надо перечитать всё.
    pub from: u64,
    /// Смещение для следующего запроса.
    pub next: u64,
    pub size: u64,
}

/// Корни, из которых узлу разрешено читать. Канонизируем сразу: домашний
/// каталог бывает за симлинком (на macOS — почти всегда), и сравнение сырых
/// строк давало бы ложный отказ на совершенно легальном пути.
pub fn transcript_roots(home: &Path) -> Vec<PathBuf> {
    let codex = match std::env::var("CODEX_HOME") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => home.join(".codex"),
    };
    [home.join(".claude"), codex]
        .into_iter()
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .collect()
}

/// Запрошенный путь → реальный путь под одним из корней.
pub fn resolve(path: &str, roots: &[PathBuf]) -> Result<PathBuf, Denial> {
    if path.is_empty() {
        return Err(Denial::Outside);
    }
    if let Ok(real) = std::fs::canonicalize(path) {
        // is_file: каталог формально «под корнем», но отдавать из него нечего —
        // и листингом каталогов узел тем более не занимается
        return if inside(&real, roots) && real.is_file() {
            Ok(real)
        } else {
            Err(Denial::Outside)
        };
    }
    // Файла нет. Сказать «пока нет» можно только если его КАТАЛОГ под корнем;
    // иначе даже факт отсутствия наружу не выносим.
    let parent = Path::new(path).parent().unwrap_or_else(|| Path::new(""));
    match std::fs::canonicalize(parent) {
        Ok(dir) if inside(&dir, roots) => Err(Denial::Missing),
        _ => Err(Denial::Outside),
    }
}

/// starts_with у Path сравнивает по компонентам, а не по символам: `~/.claudex`
/// не пролезет как `~/.claude`.
fn inside(real: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| real.starts_with(root))
}

pub fn read_chunk(path: &Path, from: u64) -> Result<Chunk, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    // смещение за концом = файл переписали; читаем с конца (пусто), а ноут
    // увидит расхождение from/size и перечитает с нуля
    let from = from.min(size);
    let want = ((size - from) as usize).min(CHUNK_MAX);
    let mut buf = Vec::new();
    if want > 0 {
        let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
        f.seek(SeekFrom::Start(from)).map_err(|e| e.to_string())?;
        // take, а не read_exact: файл дописывают прямо сейчас, и жёсткая длина
        // превращала бы обычную гонку со stat в ошибку на ровном месте
        f.take(want as u64)
            .read_to_end(&mut buf)
            .map_err(|e| e.to_string())?;
    }
    let (data, taken) = decode(&buf);
    Ok(Chunk { data, from, next: from + taken as u64, size })
}

/// Байты → строка + сколько байт реально отдали. Кусок режется по потолку, а не
/// по границе символа, поэтому обрыв многобайтового UTF-8 отдаём укорачиванием:
/// хвостовые байты приедут следующим запросом и склеятся, а не превратятся в
/// «ромбик» посреди русского текста.
fn decode(buf: &[u8]) -> (String, usize) {
    match std::str::from_utf8(buf) {
        Ok(s) => (s.to_string(), buf.len()),
        // error_len() == None — обрыв ровно на границе куска, а не порча файла
        Err(e) if e.error_len().is_none() && e.valid_up_to() > 0 => {
            let end = e.valid_up_to();
            (String::from_utf8_lossy(&buf[..end]).into_owned(), end)
        }
        // байты битые сами по себе — отдаём с заменой и двигаемся дальше,
        // иначе ноут вечно перечитывал бы одно и то же смещение
        Err(_) => (String::from_utf8_lossy(buf).into_owned(), buf.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Изолированная песочница с канонизированным путём: /var → /private/var на
    /// macOS, иначе корень не совпал бы сам с собой.
    fn sandbox(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jarvis-node-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".claude/projects")).unwrap();
        std::fs::write(dir.join("secret.txt"), b"not yours").unwrap();
        std::fs::write(dir.join(".claude/projects/a.jsonl"), b"{}\n").unwrap();
        std::fs::canonicalize(&dir).unwrap()
    }

    fn roots(home: &Path) -> Vec<PathBuf> {
        vec![home.join(".claude")]
    }

    #[test]
    fn resolve_allows_file_under_root() {
        let home = sandbox("allow");
        let want = home.join(".claude/projects/a.jsonl");
        let got = resolve(want.to_str().unwrap(), &roots(&home));
        assert_eq!(got, Ok(want));
        let _ = std::fs::remove_dir_all(&home);
    }

    // Главная проверка: `..` внутри разрешённого корня не выводит наружу.
    #[test]
    fn resolve_rejects_dotdot_escape() {
        let home = sandbox("dotdot");
        let roots = roots(&home);
        for escape in [
            home.join(".claude/../secret.txt"),
            home.join(".claude/projects/../../secret.txt"),
            home.join(".claude/projects/../../../etc/passwd"),
        ] {
            assert_eq!(
                resolve(escape.to_str().unwrap(), &roots),
                Err(Denial::Outside),
                "«..» вывел за корень: {}",
                escape.display()
            );
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    // Симлинк из разрешённого каталога наружу — тот же обход, только через ФС.
    #[test]
    fn resolve_rejects_symlink_escape() {
        let home = sandbox("symlink");
        let link = home.join(".claude/leak.jsonl");
        std::os::unix::fs::symlink(home.join("secret.txt"), &link).unwrap();
        assert_eq!(
            resolve(link.to_str().unwrap(), &roots(&home)),
            Err(Denial::Outside)
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    // Соседний каталог с общим префиксом не должен считаться «своим».
    #[test]
    fn resolve_rejects_sibling_with_shared_prefix() {
        let home = sandbox("prefix");
        std::fs::create_dir_all(home.join(".claudex")).unwrap();
        let f = home.join(".claudex/a.jsonl");
        std::fs::write(&f, b"{}\n").unwrap();
        assert_eq!(resolve(f.to_str().unwrap(), &roots(&home)), Err(Denial::Outside));
        let _ = std::fs::remove_dir_all(&home);
    }

    // Пустой путь, каталог и «файл вне корней» — все отказы, но отказ по
    // отсутствию внутри корня отличается от отказа по границам.
    #[test]
    fn resolve_distinguishes_missing_inside_root_from_outside() {
        let home = sandbox("missing");
        let roots = roots(&home);
        let inside_missing = home.join(".claude/projects/nope.jsonl");
        assert_eq!(
            resolve(inside_missing.to_str().unwrap(), &roots),
            Err(Denial::Missing)
        );
        let outside_missing = home.join("nope.jsonl");
        assert_eq!(
            resolve(outside_missing.to_str().unwrap(), &roots),
            Err(Denial::Outside)
        );
        assert_eq!(resolve("", &roots), Err(Denial::Outside));
        // каталог под корнем — не файл, отдавать нечего
        let dir = home.join(".claude/projects");
        assert_eq!(resolve(dir.to_str().unwrap(), &roots), Err(Denial::Outside));
        let _ = std::fs::remove_dir_all(&home);
    }

    // Кусок ограничен потолком и продолжается ровно с `next`.
    #[test]
    fn chunk_is_capped_and_resumable() {
        let home = sandbox("chunk");
        let f = home.join(".claude/big.jsonl");
        std::fs::write(&f, vec![b'x'; CHUNK_MAX + 100]).unwrap();

        let first = read_chunk(&f, 0).unwrap();
        assert_eq!(first.data.len(), CHUNK_MAX);
        assert_eq!(first.next, CHUNK_MAX as u64);
        assert_eq!(first.size, (CHUNK_MAX + 100) as u64);

        let tail = read_chunk(&f, first.next).unwrap();
        assert_eq!(tail.data.len(), 100);
        assert_eq!(tail.next, tail.size, "хвост дочитан до конца");
        let _ = std::fs::remove_dir_all(&home);
    }

    // Файл переписали с нуля: смещение из прошлой жизни не должен уронить чтение.
    #[test]
    fn chunk_clamps_offset_past_eof() {
        let home = sandbox("truncate");
        let f = home.join(".claude/small.jsonl");
        std::fs::write(&f, b"ab").unwrap();
        let c = read_chunk(&f, 999).unwrap();
        assert_eq!((c.from, c.next, c.size), (2, 2, 2));
        assert!(c.data.is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    // Обрыв на границе многобайтового символа: отдаём валидный префикс,
    // хвостовые байты остаются следующему запросу.
    #[test]
    fn decode_defers_split_utf8_to_next_chunk() {
        let full = "привет".as_bytes();
        let cut = &full[..full.len() - 1]; // «т» разрезан пополам
        let (data, taken) = decode(cut);
        assert_eq!(data, "приве");
        assert_eq!(taken, cut.len() - 1, "недоеденный байт вернётся следующим куском");
    }

    // Битые байты внутри самого файла не должны застопорить чтение навсегда.
    #[test]
    fn decode_does_not_stall_on_broken_bytes() {
        let (data, taken) = decode(&[0xff, 0xfe, b'a']);
        assert_eq!(taken, 3);
        assert!(data.ends_with('a'));
    }
}
