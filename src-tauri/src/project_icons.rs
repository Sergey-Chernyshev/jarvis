//! Bounded, read-only discovery of project artwork. Remote discovery ships our
//! own isolated Python snippet over SSH; no project tools or URLs are run.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const MAX_FILE: usize = 128 * 1024;
const MAX_TOTAL: usize = 768 * 1024;
const MAX_RAW: usize = 24;
const MAX_CANDIDATES: usize = 12;
const MAX_ENTRIES: usize = 6000;
const MAX_DIRS: usize = 256;
const MAX_DEPTH: usize = 5;
const MAX_REMOTE_OUTPUT: usize = 1100 * 1024;

#[derive(Serialize, Deserialize)]
struct RawIcon {
    path: String,
    data: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Scan {
    candidates: Vec<RawIcon>,
    truncated: bool,
}

pub(crate) fn relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

fn icon_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let Some((stem, extension)) = lower.rsplit_once('.') else {
        return false;
    };
    if !matches!(extension, "png" | "jpg" | "jpeg" | "webp" | "ico" | "svg") {
        return false;
    }
    ["favicon", "apple-touch-icon", "apple-icon", "icon", "logo"]
        .iter()
        .any(|prefix| {
            stem == *prefix
                || stem.strip_prefix(prefix).is_some_and(|tail| {
                    tail.starts_with('-')
                        || tail.starts_with('_')
                        || tail.starts_with('.')
                        || tail.starts_with(|c: char| c.is_ascii_digit())
                })
        })
}

fn ignored_dir(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "node_modules" | "vendor" | "target" | "dist" | "build" | "coverage" | "__pycache__"
        )
}

fn directory_rank(name: &str) -> u8 {
    match name {
        "public" | "static" | "assets" | "app" => 0,
        "src" | "apps" | "packages" => 1,
        _ => 2,
    }
}

// Walk using directory descriptors. Every descendant is opened relative to an
// already-open parent with O_NOFOLLOW, so a concurrent symlink replacement
// cannot redirect either enumeration or file reads outside the project.
fn open_at(directory: &std::fs::File, name: &std::ffi::OsStr) -> std::io::Result<std::fs::File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    let name = std::ffi::CString::new(name.as_bytes())?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

fn directory_names(
    directory: &std::fs::File,
    limit: usize,
) -> std::io::Result<Vec<std::ffi::OsString>> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStringExt};
    let fd = unsafe { libc::dup(directory.as_raw_fd()) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(fd);
        }
        return Err(error);
    }
    let mut names = Vec::new();
    while names.len() < limit {
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(std::ffi::OsString::from_vec(name.to_vec()));
        }
    }
    unsafe {
        libc::closedir(stream);
    }
    Ok(names)
}

fn scan_local(root: &Path) -> Result<Scan, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let root = root
        .canonicalize()
        .map_err(|_| "Каталог проекта не найден")?;
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)
        .map_err(|_| "Путь проекта должен указывать на доступный каталог")?;
    let mut queue = VecDeque::from([(directory, std::path::PathBuf::new(), 0)]);
    let mut scan = Scan::default();
    let mut visited = 0;
    let mut entries_seen = 0;
    let mut total = 0;
    while let Some((directory, relative_directory, depth)) = queue.pop_front() {
        if visited >= MAX_DIRS {
            scan.truncated = true;
            break;
        }
        visited += 1;
        let Ok(mut names) =
            directory_names(&directory, MAX_ENTRIES.saturating_sub(entries_seen) + 1)
        else {
            continue;
        };
        names.sort_by(|a, b| {
            directory_rank(&a.to_string_lossy())
                .cmp(&directory_rank(&b.to_string_lossy()))
                .then_with(|| a.cmp(b))
        });
        for name in names {
            entries_seen += 1;
            if entries_seen > MAX_ENTRIES {
                scan.truncated = true;
                return Ok(scan);
            }
            let Some(name_string) = name.to_str() else {
                continue;
            };
            if ignored_dir(name_string) {
                continue;
            }
            let Ok(file) = open_at(&directory, &name) else {
                continue;
            };
            let Ok(meta) = file.metadata() else { continue };
            let relative = relative_directory.join(&name);
            if meta.is_dir() {
                if depth < MAX_DEPTH {
                    if visited + queue.len() < MAX_DIRS {
                        queue.push_back((file, relative, depth + 1));
                    } else {
                        scan.truncated = true;
                    }
                }
                continue;
            }
            if !meta.is_file()
                || !icon_name(name_string)
                || meta.len() == 0
                || meta.len() > MAX_FILE as u64
            {
                continue;
            }
            let Some(relative) = relative.to_str().filter(|path| relative_path(path)) else {
                continue;
            };
            let mut bytes = Vec::new();
            if file
                .take(MAX_FILE as u64 + 1)
                .read_to_end(&mut bytes)
                .is_err()
                || bytes.is_empty()
                || bytes.len() > MAX_FILE
            {
                continue;
            }
            if total + bytes.len() > MAX_TOTAL || scan.candidates.len() >= MAX_RAW {
                scan.truncated = true;
                return Ok(scan);
            }
            total += bytes.len();
            scan.candidates.push(RawIcon {
                path: relative.into(),
                data: base64::engine::general_purpose::STANDARD.encode(bytes),
            });
        }
    }
    Ok(scan)
}

fn dimensions(width: u32, height: u32) -> bool {
    width > 0 && height > 0 && width <= 8192 && height <= 8192
}

fn raster_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 45
        && bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        && bytes[8..12] == [0, 0, 0, 13]
        && &bytes[12..16] == b"IHDR"
    {
        let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        if !dimensions(width, height) {
            return None;
        }
        let mut at = 8;
        let mut data = false;
        while at + 12 <= bytes.len() {
            let size = u32::from_be_bytes(bytes[at..at + 4].try_into().ok()?) as usize;
            let end = at.checked_add(12)?.checked_add(size)?;
            if end > bytes.len() {
                return None;
            }
            let kind = &bytes[at + 4..at + 8];
            if kind == b"IDAT" {
                data |= size > 0;
            }
            if kind == b"IEND" {
                return (data && size == 0 && end == bytes.len()).then_some("image/png");
            }
            at = end;
        }
        return None;
    }
    if bytes.len() >= 12 && bytes.starts_with(b"\xff\xd8\xff") && bytes.ends_with(b"\xff\xd9") {
        let mut at = 2;
        let mut frame = false;
        while at + 4 <= bytes.len() {
            if bytes[at] != 0xff {
                return None;
            }
            while at < bytes.len() && bytes[at] == 0xff {
                at += 1;
            }
            let marker = *bytes.get(at)?;
            at += 1;
            if marker == 0xda {
                return frame.then_some("image/jpeg");
            }
            let size = u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as usize;
            if size < 2 || at.checked_add(size)? > bytes.len() {
                return None;
            }
            if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
                if size < 8 {
                    return None;
                }
                let height = u16::from_be_bytes(bytes[at + 3..at + 5].try_into().ok()?) as u32;
                let width = u16::from_be_bytes(bytes[at + 5..at + 7].try_into().ok()?) as u32;
                if !dimensions(width, height) {
                    return None;
                }
                frame = true;
            }
            at += size;
        }
        return None;
    }
    if bytes.len() >= 25 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        let size = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
        let chunk = u32::from_le_bytes(bytes[16..20].try_into().ok()?) as usize;
        if size.checked_add(8) != Some(bytes.len()) || chunk.checked_add(20)? > bytes.len() {
            return None;
        }
        let (width, height) = match &bytes[12..16] {
            b"VP8 " if chunk >= 10 && bytes.len() >= 30 && &bytes[23..26] == b"\x9d\x01\x2a" => (
                (u16::from_le_bytes(bytes[26..28].try_into().ok()?) & 0x3fff) as u32,
                (u16::from_le_bytes(bytes[28..30].try_into().ok()?) & 0x3fff) as u32,
            ),
            b"VP8L" if chunk >= 5 && bytes[20] == 0x2f => {
                let bits = u32::from_le_bytes(bytes[21..25].try_into().ok()?);
                ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
            }
            b"VP8X" if chunk == 10 && bytes.len() >= 30 => (
                u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]) + 1,
                u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]) + 1,
            ),
            _ => return None,
        };
        return dimensions(width, height).then_some("image/webp");
    }
    None
}

fn ico(bytes: &[u8]) -> bool {
    if bytes.len() < 22 || !bytes.starts_with(&[0, 0, 1, 0]) {
        return false;
    }
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    if count == 0 || count > 32 || bytes.len() < 6 + 16 * count {
        return false;
    }
    (0..count).all(|i| {
        let at = 6 + i * 16;
        let size = u32::from_le_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(bytes[at + 12..at + 16].try_into().unwrap()) as usize;
        size > 0
            && offset >= 6 + 16 * count
            && offset
                .checked_add(size)
                .is_some_and(|end| end <= bytes.len())
    })
}

/// A small SVG drawing vocabulary, with no CSS, entities, scripts, animation,
/// external references or foreign elements. Returned only as an img source.
fn safe_svg(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut text = text.trim_start_matches('\u{feff}').trim();
    if text.starts_with("<?xml ") {
        let Some(end) = text.find("?>") else {
            return false;
        };
        text = text[end + 2..].trim();
    }
    if text.contains("<!")
        || text.contains("<?")
        || text.contains('&')
        || text.contains('\\')
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return false;
    }
    static TAG: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static ATTR: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static REF: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let tags = TAG.get_or_init(|| {
        regex::Regex::new(r"(?s)<(/?)([A-Za-z][A-Za-z0-9:-]*)([^<>]*?)(/?)>").unwrap()
    });
    let attributes = ATTR.get_or_init(|| {
        regex::Regex::new(r#"([A-Za-z_:][A-Za-z0-9_.:-]*)\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap()
    });
    let reference =
        REF.get_or_init(|| regex::Regex::new(r"^url\(\s*#[-A-Za-z0-9_:.]+\s*\)$").unwrap());
    let mut stack = Vec::new();
    let mut at = 0;
    let mut seen_root = false;
    let mut count = 0;
    for tag in tags.captures_iter(text) {
        let matched = tag.get(0).unwrap();
        if !text[at..matched.start()].trim().is_empty() {
            return false;
        }
        at = matched.end();
        count += 1;
        if count > 2048 {
            return false;
        }
        let name = tag.get(2).unwrap().as_str();
        if !matches!(
            name,
            "svg"
                | "g"
                | "path"
                | "circle"
                | "rect"
                | "ellipse"
                | "line"
                | "polyline"
                | "polygon"
                | "defs"
                | "linearGradient"
                | "radialGradient"
                | "stop"
                | "clipPath"
                | "mask"
                | "use"
        ) {
            return false;
        }
        if &tag[1] == "/" {
            if !tag[3].trim().is_empty() || !tag[4].is_empty() || stack.pop() != Some(name) {
                return false;
            }
            continue;
        }
        if stack.is_empty() {
            if seen_root || name != "svg" {
                return false;
            }
            seen_root = true;
        }
        let attrs = tag.get(3).unwrap().as_str();
        let mut attr_at = 0;
        let mut seen = HashSet::new();
        for attr in attributes.captures_iter(attrs) {
            let matched = attr.get(0).unwrap();
            if !attrs[attr_at..matched.start()].trim().is_empty() {
                return false;
            }
            attr_at = matched.end();
            let key = attr.get(1).unwrap().as_str();
            let value = attr.get(2).or_else(|| attr.get(3)).unwrap().as_str();
            if !seen.insert(key) || seen.len() > 64 {
                return false;
            }
            if !matches!(
                key,
                "xmlns"
                    | "xmlns:xlink"
                    | "viewBox"
                    | "width"
                    | "height"
                    | "x"
                    | "y"
                    | "x1"
                    | "x2"
                    | "y1"
                    | "y2"
                    | "cx"
                    | "cy"
                    | "r"
                    | "rx"
                    | "ry"
                    | "d"
                    | "points"
                    | "fill"
                    | "fill-opacity"
                    | "fill-rule"
                    | "clip-rule"
                    | "stroke"
                    | "stroke-width"
                    | "stroke-linecap"
                    | "stroke-linejoin"
                    | "stroke-miterlimit"
                    | "stroke-opacity"
                    | "opacity"
                    | "transform"
                    | "id"
                    | "gradientUnits"
                    | "gradientTransform"
                    | "offset"
                    | "stop-color"
                    | "stop-opacity"
                    | "clipPathUnits"
                    | "maskUnits"
                    | "maskContentUnits"
                    | "preserveAspectRatio"
                    | "href"
                    | "xlink:href"
                    | "clip-path"
                    | "mask"
                    | "color"
                    | "version"
                    | "role"
                    | "aria-hidden"
                    | "focusable"
            ) {
                return false;
            }
            if key == "xmlns" {
                if value != "http://www.w3.org/2000/svg" {
                    return false;
                }
                continue;
            }
            if key == "xmlns:xlink" {
                if value != "http://www.w3.org/1999/xlink" {
                    return false;
                }
                continue;
            }
            let lower = value.to_ascii_lowercase();
            if ["http:", "https:", "file:", "data:", "javascript:", "//"]
                .iter()
                .any(|prefix| lower.contains(prefix))
            {
                return false;
            }
            if matches!(key, "href" | "xlink:href")
                && !(value.starts_with('#')
                    && value.len() > 1
                    && value[1..]
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b)))
            {
                return false;
            }
            if matches!(
                key,
                "fill" | "stroke" | "clip-path" | "mask" | "color" | "stop-color"
            ) && value.contains('(')
                && !reference.is_match(value)
            {
                return false;
            }
        }
        if !attrs[attr_at..].trim().is_empty() {
            return false;
        }
        if tag[4].is_empty() {
            stack.push(name);
        }
    }
    seen_root && stack.is_empty() && text[at..].trim().is_empty()
}

fn candidate_mime(bytes: &[u8]) -> Option<&'static str> {
    raster_mime(bytes)
        .or_else(|| ico(bytes).then_some("image/x-icon"))
        .or_else(|| safe_svg(bytes).then_some("image/svg+xml"))
}

pub(crate) fn validate_avatar(value: &Value) -> Result<Value, String> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let object = value.as_object().ok_or("Некорректная аватарка проекта")?;
    let url = object
        .get("dataUrl")
        .and_then(Value::as_str)
        .ok_or("Не передана картинка проекта")?;
    if url.len() > MAX_FILE.div_ceil(3) * 4 + 32 {
        return Err("Аватарка проекта больше 128 КБ".into());
    }
    let (header, data) = url
        .split_once(',')
        .ok_or("Некорректный data URL аватарки")?;
    let expected = match header {
        "data:image/png;base64" => "image/png",
        "data:image/jpeg;base64" => "image/jpeg",
        "data:image/webp;base64" => "image/webp",
        _ => return Err("Сохранять можно только PNG, JPEG или WebP".into()),
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| "Не удалось прочитать base64 аватарки")?;
    if bytes.is_empty() || bytes.len() > MAX_FILE {
        return Err("Аватарка проекта больше 128 КБ или пуста".into());
    }
    if raster_mime(&bytes) != Some(expected) {
        return Err("Формат аватарки не совпадает с изображением".into());
    }
    let source = object
        .get("source")
        .and_then(Value::as_str)
        .filter(|source| matches!(*source, "upload" | "project"))
        .ok_or("Некорректный источник аватарки")?;
    let mut avatar = json!({"dataUrl":format!("data:{expected};base64,{}",base64::engine::general_purpose::STANDARD.encode(bytes)),"source":source});
    if let Some(path) = object.get("path") {
        let path = path
            .as_str()
            .filter(|path| relative_path(path))
            .ok_or("Путь иконки должен находиться внутри проекта")?;
        avatar["path"] = json!(path);
    }
    if let Some(label) = object.get("label") {
        let label = label
            .as_str()
            .filter(|label| label.chars().count() <= 160 && !label.chars().any(char::is_control))
            .ok_or("Некорректное название аватарки")?;
        avatar["label"] = json!(label);
    }
    Ok(avatar)
}

fn finish(scan: Scan) -> Value {
    let mut candidates = Vec::new();
    let mut truncated = scan.truncated || scan.candidates.len() > MAX_RAW;
    let mut total = 0;
    for raw in scan.candidates.into_iter().take(MAX_RAW) {
        if !relative_path(&raw.path) || raw.data.len() > MAX_FILE.div_ceil(3) * 4 {
            continue;
        }
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(raw.data) else {
            continue;
        };
        if bytes.is_empty() || bytes.len() > MAX_FILE {
            continue;
        }
        total += bytes.len();
        if total > MAX_TOTAL {
            truncated = true;
            break;
        }
        let Some(mime) = candidate_mime(&bytes) else {
            continue;
        };
        if candidates.len() == MAX_CANDIDATES {
            truncated = true;
            break;
        }
        candidates.push(json!({"path":raw.path,"name":raw.path.rsplit('/').next().unwrap_or(&raw.path),
            "dataUrl":format!("data:{mime};base64,{}",base64::engine::general_purpose::STANDARD.encode(bytes))}));
    }
    candidates.sort_by(|a, b| {
        icon_rank(a["path"].as_str().unwrap()).cmp(&icon_rank(b["path"].as_str().unwrap()))
    });
    json!({"ok":true,"candidates":candidates,"truncated":truncated})
}

fn icon_rank(path: &str) -> (usize, u8, String) {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let rank = if name.starts_with("favicon") {
        0
    } else if name.starts_with("apple-touch-icon") {
        1
    } else if name.starts_with("icon") {
        2
    } else {
        3
    };
    (path.matches('/').count(), rank, path.into())
}

const REMOTE_SCAN: &str = include_str!("project_icons_scan.py");

fn remote_command(root: &str) -> String {
    let code = crate::util::shell_quote(REMOTE_SCAN);
    let root = crate::util::shell_quote(root);
    format!("for p in /usr/bin/python3 /usr/local/bin/python3 /opt/homebrew/bin/python3; do if [ -x \"$p\" ]; then exec \"$p\" -I -c {code} {root}; fi; done; printf '%s\\n' 'Для поиска иконок на машине нужен Python 3' >&2; exit 127")
}

pub async fn candidates(d: Arc<crate::daemon::Daemon>, machine: String, cwd: String) -> Value {
    if cwd.len() > 4096 || !cwd.starts_with('/') || cwd.chars().any(char::is_control) {
        return json!({"ok":false,"error":"Укажи абсолютный путь проекта"});
    }
    let result = if machine.is_empty() || machine == "local" {
        tauri::async_runtime::spawn_blocking(move || scan_local(Path::new(&cwd)))
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result)
    } else {
        let Some(node) = d.remotes.node(&machine) else {
            return json!({"ok":false,"error":"Машина не настроена"});
        };
        let host = crate::bundle::host::Host::ConfiguredSsh {
            machine,
            connection: node.cfg.connection(),
        };
        match host
            .sh_data("/", &remote_command(&cwd), Duration::from_secs(15))
            .await
        {
            Ok(output) if output.len() <= MAX_REMOTE_OUTPUT => {
                serde_json::from_str::<Scan>(&output)
                    .map_err(|_| "Машина вернула некорректный список иконок".into())
            }
            Ok(_) => Err("Список иконок превысил допустимый размер".into()),
            Err(error) => Err(error),
        }
    };
    match result {
        Ok(scan) => finish(scan),
        Err(error) => json!({"ok":false,"error":error}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6L5sAAAAASUVORK5CYII=";
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("jarvis-project-icons-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn write(&self, path: &str, data: &[u8]) {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }
        fn png(&self, path: &str) {
            self.write(
                path,
                &base64::engine::general_purpose::STANDARD
                    .decode(PNG)
                    .unwrap(),
            );
        }
        fn scan(&self) -> Value {
            finish(scan_local(&self.0).unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn discovers_a_single_favicon_and_sniffs_content_over_extension() {
        let fixture = Fixture::new();
        fixture.png("public/favicon.ico");
        fixture.write("public/logo.png", b"<html>not an image</html>");
        let result = fixture.scan();
        assert_eq!(result["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(result["candidates"][0]["path"], "public/favicon.ico");
        assert!(result["candidates"][0]["dataUrl"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
    }

    #[test]
    fn discovers_multiple_app_icons_but_skips_dependencies_hidden_and_deep_trees() {
        let fixture = Fixture::new();
        for path in [
            "favicon.png",
            "apps/web/public/favicon.png",
            "packages/site/src/app/icon-32.png",
            "src/app/apple-icon.png",
            "node_modules/pkg/favicon.png",
            ".git/logo.png",
            "one/two/three/four/five/six/logo.png",
        ] {
            fixture.png(path);
        }
        let result = fixture.scan();
        let paths: Vec<_> = result["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|icon| icon["path"].as_str().unwrap())
            .collect();
        assert_eq!(
            paths,
            [
                "favicon.png",
                "src/app/apple-icon.png",
                "apps/web/public/favicon.png",
                "packages/site/src/app/icon-32.png"
            ]
        );
    }

    #[test]
    fn does_not_read_symlinks_even_when_they_point_inside_the_root() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        outside.png("favicon.png");
        fixture.png("assets/logo.png");
        std::os::unix::fs::symlink(outside.0.join("favicon.png"), fixture.0.join("favicon.png"))
            .unwrap();
        std::os::unix::fs::symlink(&outside.0, fixture.0.join("public")).unwrap();
        std::os::unix::fs::symlink(
            fixture.0.join("assets/logo.png"),
            fixture.0.join("icon.png"),
        )
        .unwrap();
        let result = fixture.scan();
        assert_eq!(result["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(result["candidates"][0]["path"], "assets/logo.png");
    }

    #[test]
    fn descriptor_walk_rejects_replaced_directory_and_keeps_open_parent_stable() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        fixture.png("assets/logo.png");
        outside.png("favicon.png");
        let root = std::fs::File::open(&fixture.0).unwrap();
        let assets = open_at(&root, std::ffi::OsStr::new("assets")).unwrap();
        std::fs::rename(fixture.0.join("assets"), fixture.0.join("original")).unwrap();
        std::os::unix::fs::symlink(&outside.0, fixture.0.join("assets")).unwrap();
        assert!(open_at(&root, std::ffi::OsStr::new("assets")).is_err());
        assert!(open_at(&assets, std::ffi::OsStr::new("favicon.png")).is_err());
        assert!(open_at(&assets, std::ffi::OsStr::new("logo.png")).is_ok());
    }

    #[test]
    fn candidate_count_and_file_size_are_bounded() {
        let fixture = Fixture::new();
        fixture.write("favicon.png", &vec![b'x'; MAX_FILE + 1]);
        for i in 0..30 {
            fixture.png(&format!("icon-{i:02}.png"));
        }
        let result = fixture.scan();
        assert_eq!(
            result["candidates"].as_array().unwrap().len(),
            MAX_CANDIDATES
        );
        assert_eq!(result["truncated"], true);
        assert!(serde_json::to_vec(&result).unwrap().len() < MAX_REMOTE_OUTPUT);
    }

    #[test]
    fn accepts_only_inert_svg_geometry_and_local_references() {
        assert!(safe_svg(br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><defs><linearGradient id="a"><stop offset="0" stop-color="#f00"/></linearGradient></defs><path fill="url(#a)" d="M0 0h32v32z"/></svg>"##));
        for svg in [
            "<svg><script>alert(1)</script></svg>",
            "<svg onload='alert(1)'/>",
            "<svg><image href='https://example.org/icon.png'/></svg>",
            "<svg><use href='outside.svg#icon'/></svg>",
            "<svg><use href='data:image/svg+xml,anything'/></svg>",
            "<svg><path fill='url(https://example.org/a)'/></svg>",
            "<svg><path style='fill:red'/></svg>",
            "<!DOCTYPE svg [<!ENTITY x SYSTEM 'file:///secret'>]><svg>&x;</svg>",
            "<svg><foreignObject/></svg>",
            "<svg><animate attributeName='href'/></svg>",
            "<svg><style>@import url(https://example.org/x)</style></svg>",
            "<svg><path fill='u\\72l(https://example.org/x)'/></svg>",
            "<svg></svg><svg/>",
        ] {
            assert!(!safe_svg(svg.as_bytes()), "unexpectedly accepted: {svg}");
        }
    }

    #[test]
    fn avatar_validation_rejects_active_data_mime_mismatch_oversize_and_escaping_paths() {
        let valid = json!({"dataUrl":format!("data:image/png;base64,{PNG}"),"source":"upload"});
        assert!(validate_avatar(&valid).is_ok());
        for patch in [
            json!({"dataUrl":"data:image/svg+xml;base64,PHN2Zy8+"}),
            json!({"dataUrl":format!("data:image/jpeg;base64,{PNG}")}),
            json!({"dataUrl":format!("data:image/png;base64,{}", "A".repeat(MAX_FILE * 2))}),
            json!({"source":"network"}),
            json!({"path":"../secret"}),
            json!({"path":"/secret"}),
            json!({"path":"public/../../secret"}),
            json!({"label":"invalid\nlabel"}),
        ] {
            let mut invalid = valid.clone();
            invalid
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert!(validate_avatar(&invalid).is_err(), "accepted {patch}");
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(PNG)
            .unwrap();
        assert_eq!(
            raster_mime(&bytes[..33]),
            None,
            "PNG header alone is not a complete image"
        );
        assert_eq!(raster_mime(b"\xff\xd8\xff\xd9"), None);
        assert!(validate_avatar(&Value::Null).unwrap().is_null());
    }

    #[test]
    fn remote_script_matches_local_results_and_quotes_shell_metacharacters() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let repo = "repo 'quotes' $(touch SHOULD_NOT_EXIST) `touch ALSO_NOT`";
        fixture.png(&format!("{repo}/public/favicon.png"));
        fixture.png(&format!("{repo}/apps/site/src/app/icon.png"));
        outside.png("favicon.png");
        std::os::unix::fs::symlink(&outside.0, fixture.0.join(repo).join("static")).unwrap();
        let root = fixture.0.join(repo);
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(remote_command(root.to_str().unwrap()))
            .current_dir(&fixture.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.len() <= MAX_REMOTE_OUTPUT);
        let remote = finish(serde_json::from_slice::<Scan>(&output.stdout).unwrap());
        assert_eq!(remote, finish(scan_local(&root).unwrap()));
        assert_eq!(remote["candidates"].as_array().unwrap().len(), 2);
        assert!(!fixture.0.join("SHOULD_NOT_EXIST").exists());
        assert!(!fixture.0.join("ALSO_NOT").exists());
    }
}
