//! Explicit, bounded artifact previews on the session's own host and local notes.
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
const LIMIT: usize = 25 * 1024 * 1024;
fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.into()
    } else {
        cwd.join(p)
    }
}
fn attachment_path(path: &Path) -> bool {
    let roots = [
        std::env::temp_dir(),
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
    ];
    roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| {
            path.strip_prefix(root).ok().is_some_and(|p| {
                p.components().next().is_some_and(|c| {
                    c.as_os_str()
                        .to_string_lossy()
                        .starts_with("jarvis-attachment.")
                })
            })
        })
}
fn local_read(cwd: Option<&str>, path: &str, allowed: &[String]) -> Result<Vec<u8>, String> {
    let base = Path::new(cwd.unwrap_or("/"));
    let file = resolve(base, path)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let in_project = cwd
        .and_then(|p| Path::new(p).canonicalize().ok())
        .is_some_and(|p| p != Path::new("/") && file.starts_with(p));
    let from_facts = allowed
        .iter()
        .any(|p| resolve(base, p).canonicalize().ok().as_ref() == Some(&file));
    if !in_project && !from_facts && !attachment_path(&file) {
        return Err("Файл находится за пределами проекта и вложений чата".into());
    }
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Выбери обычный файл".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(file)
        .map_err(|e| e.to_string())?
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > LIMIT {
        return Err("Предпросмотр доступен для файлов до 25 МБ".into());
    }
    Ok(bytes)
}
// Runs through the configured account, including Teleport. Resolve symlinks on
// that host; never substitute a same-named local file for a remote artifact.
fn remote_script(cwd: Option<&str>, path: &str, allowed: &[String]) -> String {
    let args =
        serde_json::to_string(&json!({"cwd":cwd,"path":path,"allowed":allowed,"limit":LIMIT}))
            .unwrap();
    format!(
        "python3 -c {} {}",
        crate::util::shell_quote(
            r#"import os,sys,json,base64,stat
v=json.loads(sys.argv[1]); base=os.path.realpath(v['cwd'] or '/')
def resolve(p): return os.path.realpath(os.path.join(base,p))
p=resolve(v['path'])
in_project=v['cwd'] and base!='/' and os.path.commonpath([base,p])==base
allowed=[resolve(x) for x in v['allowed']]
in_attachment=any(p.startswith(root+'/jarvis-attachment.') for root in ['/tmp','/private/tmp'])
if not (in_project or p in allowed or in_attachment): raise Exception('Файл находится за пределами проекта и вложений чата')
fd=os.open(p,os.O_RDONLY|os.O_NONBLOCK)
with os.fdopen(fd,'rb') as f:
 if not stat.S_ISREG(os.fstat(f.fileno()).st_mode): raise Exception('Выбери обычный файл')
 data=f.read(v['limit']+1)
if len(data)>v['limit']: raise Exception('Предпросмотр доступен для файлов до 25 МБ')
print(base64.b64encode(data).decode('ascii'))"#
        ),
        crate::util::shell_quote(&args)
    )
}
fn mime(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "pdf" => "application/pdf",
        "html" | "htm" => "text/html",
        "md" | "markdown" => "text/markdown",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "doc" => "application/msword",
        "rtf" => "application/rtf",
        _ => "application/octet-stream",
    }
}
#[cfg(target_os = "macos")]
async fn document_text(bytes: &[u8], name: &str) -> Option<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let format = name.rsplit('.').next()?.to_ascii_lowercase();
    if !["docx", "doc", "rtf"].contains(&format.as_str()) {
        return None;
    }
    let mut child = tokio::process::Command::new("/usr/bin/textutil")
        .args(["-convert", "txt", "-format", &format, "-stdin", "-stdout"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let converted = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(
            async {
                stdin.write_all(bytes).await?;
                stdin.shutdown().await?;
                drop(stdin);
                Ok::<(), std::io::Error>(())
            },
            async {
                let mut output = Vec::new();
                stdout
                    .take((LIMIT + 1) as u64)
                    .read_to_end(&mut output)
                    .await?;
                Ok::<_, std::io::Error>(output)
            },
            child.wait()
        )
    })
    .await
    .ok()?;
    match converted {
        (Ok(()), Ok(output), Ok(exit)) if exit.success() && output.len() <= LIMIT => {
            Some(String::from_utf8_lossy(&output).into_owned())
        }
        _ => None,
    }
}
#[tauri::command]
pub async fn artifact_read(
    app: tauri::AppHandle,
    session_id: Option<String>,
    path: String,
) -> Value {
    async fn read(
        app: tauri::AppHandle,
        session_id: Option<String>,
        path: String,
    ) -> Result<Value, String> {
        if path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control) {
            return Err("Некорректный путь".into());
        }
        let d = crate::daemon::Daemon::get(&app);
        let session = if let Some(id) = session_id.as_deref() {
            Some(d.session(id).ok_or("Сессия не найдена")?)
        } else {
            None
        };
        let cwd = session.as_ref().and_then(|s| s.cwd.clone());
        let mut allowed = Vec::new();
        if let Some(id) = session_id.as_deref().filter(|_| {
            !attachment_path(Path::new(&path))
                && !cwd.as_ref().is_some_and(|cwd| {
                    cwd != "/"
                        && resolve(Path::new(cwd), &path).starts_with(cwd)
                        && !Path::new(&path)
                            .components()
                            .any(|c| matches!(c, std::path::Component::ParentDir))
                })
        }) {
            if let Some((be, entries)) = d.turn_entries(id).await {
                let (_, turns) = crate::turns::segment(be, &entries);
                allowed = turns
                    .into_iter()
                    .flat_map(|t| t.facts.files.into_iter().map(|f| f.path))
                    .collect();
            }
        }
        let bytes = if let Some(machine) = session.as_ref().and_then(|s| s.remote.as_ref()) {
            let node = d.remotes.node(machine).ok_or("Машина не подключена")?;
            let host = crate::bundle::host::Host::ConfiguredSsh {
                machine: machine.clone(),
                connection: node.cfg.connection(),
            };
            let output = host
                .sh_data(
                    "/",
                    &remote_script(cwd.as_deref(), &path, &allowed),
                    Duration::from_secs(45),
                )
                .await?;
            base64::engine::general_purpose::STANDARD
                .decode(output.trim())
                .map_err(|_| "Узел вернул некорректные данные файла")?
        } else {
            let p = path.clone();
            tokio::task::spawn_blocking(move || local_read(cwd.as_deref(), &p, &allowed))
                .await
                .map_err(|e| e.to_string())??
        };
        if bytes.len() > LIMIT {
            return Err("Предпросмотр доступен для файлов до 25 МБ".into());
        }
        let name = Path::new(&path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let mut result = json!({"ok":true,"name":name,"path":path,"type":mime(&name),"size":bytes.len(),"key":format!("sha256:{:x}",Sha256::digest(&bytes)),"dataBase64":base64::engine::general_purpose::STANDARD.encode(&bytes)});
        #[cfg(target_os = "macos")]
        if let Some(content) = document_text(&bytes, &name).await {
            result["content"] = json!(content);
            result["converted"] = json!(true);
        }
        Ok(result)
    }
    match read(app, session_id, path).await {
        Ok(v) => v,
        Err(e) => json!({"ok":false,"error":e}),
    }
}
static NOTES_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn notes_at(
    root: &Path,
    key: &str,
    note: Option<Value>,
    remove_id: Option<String>,
) -> Result<Value, String> {
    if key.len() > 512 || key.is_empty() {
        return Err("Некорректный файл".into());
    }
    let _lock = NOTES_LOCK.lock().unwrap();
    let file = root.join(format!("{:x}.json", Sha256::digest(key.as_bytes())));
    let mut notes: Vec<Value> = match std::fs::read(&file) {
        Ok(data) => serde_json::from_slice(&data)
            .map_err(|e| format!("Не удалось прочитать заметки: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.to_string()),
    };
    let changed = note.is_some() || remove_id.is_some();
    if let Some(id) = remove_id {
        notes.retain(|n| n["id"].as_str() != Some(&id));
    }
    if let Some(note) = note {
        let id = note["id"]
            .as_str()
            .filter(|v| {
                !v.is_empty()
                    && v.len() < 100
                    && v.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            })
            .ok_or("Некорректная заметка")?;
        let text = note["text"]
            .as_str()
            .filter(|v| !v.trim().is_empty() && v.len() <= 12000)
            .ok_or("Напиши комментарий до 12 000 символов")?;
        if note.to_string().len() > 20000 {
            return Err("Заметка слишком большая".into());
        }
        let clean = json!({"id":id,"text":text,"anchor":note["anchor"],"at":crate::util::now_ms()});
        if let Some(old) = notes.iter_mut().find(|n| n["id"] == id) {
            *old = clean;
        } else {
            if notes.len() >= 500 {
                return Err("Не больше 500 заметок к файлу".into());
            }
            notes.push(clean);
        }
    }
    if changed {
        std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let temp = file.with_extension("tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut out = options.open(&temp).map_err(|e| e.to_string())?;
        out.write_all(&serde_json::to_vec(&notes).unwrap())
            .map_err(|e| e.to_string())?;
        out.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(temp, file).map_err(|e| e.to_string())?;
    }
    Ok(json!({"ok":true,"notes":notes}))
}
#[tauri::command]
pub async fn artifact_notes(key: String, note: Option<Value>, remove_id: Option<String>) -> Value {
    tokio::task::spawn_blocking(move || {
        match notes_at(
            &crate::util::jarvis_dir().join("artifact-notes"),
            &key,
            note,
            remove_id,
        ) {
            Ok(v) => v,
            Err(e) => json!({"ok":false,"error":e}),
        }
    })
    .await
    .unwrap_or_else(|e| json!({"ok":false,"error":e.to_string()}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_scope_and_binary_preview() {
        let dir = std::env::temp_dir().join(format!("artifact-test-{}", crate::util::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), b"binary\0image").unwrap();
        assert_eq!(
            local_read(dir.to_str(), "a.png", &[]).unwrap(),
            b"binary\0image"
        );
        assert!(local_read(Some("/"), dir.join("a.png").to_str().unwrap(), &[]).is_err());
        assert!(local_read(dir.to_str(), ".", &[]).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hosts", dir.join("escape")).unwrap();
            assert!(local_read(dir.to_str(), "escape", &[]).is_err());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn word_preview_extracts_real_document_text() {
        use std::io::Write;
        let mut child = std::process::Command::new("/usr/bin/textutil")
            .args(["-convert", "docx", "-format", "txt", "-stdin", "-stdout"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all("Проверка документа".as_bytes())
            .unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(result.status.success());
        assert!(document_text(&result.stdout, "report.docx")
            .await
            .unwrap()
            .contains("Проверка документа"));
    }
    #[test]
    fn private_attachment_and_limits() {
        let root = std::env::temp_dir().join(format!(
            "jarvis-attachment.preview-{}",
            crate::util::now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("draft.pdf"), b"%PDF").unwrap();
        assert_eq!(
            local_read(None, root.join("draft.pdf").to_str().unwrap(), &[]).unwrap(),
            b"%PDF"
        );
        let big = std::fs::File::create(root.join("big")).unwrap();
        big.set_len((LIMIT + 1) as u64).unwrap();
        assert!(local_read(None, root.join("big").to_str().unwrap(), &[])
            .unwrap_err()
            .contains("25 МБ"));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn notes_survive_reopen_edit_delete_and_invalid_update() {
        let dir =
            std::env::temp_dir().join(format!("artifact-notes-test-{}", crate::util::now_ms()));
        let note = json!({"id":"n1","text":"Перенеси заголовок","anchor":{"x":0.3,"y":0.6}});
        assert_eq!(
            notes_at(&dir, "file", Some(note), None).unwrap()["notes"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            notes_at(&dir, "file", None, None).unwrap()["notes"][0]["anchor"]["x"],
            0.3
        );
        assert!(notes_at(&dir, "file", Some(json!({"id":"n1","text":""})), None).is_err());
        assert_eq!(
            notes_at(&dir, "file", None, Some("n1".into())).unwrap()["notes"],
            json!([])
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn remote_reader_quotes_paths_and_does_not_follow_escape() {
        let dir =
            std::env::temp_dir().join(format!("artifact-host-test-{}", crate::util::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a' $(id).pdf"), b"%PDF\0payload").unwrap();
        let out = crate::bundle::host::Host::Local
            .sh_data(
                "/",
                &remote_script(dir.to_str(), "a' $(id).pdf", &[]),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(out.trim())
                .unwrap(),
            b"%PDF\0payload"
        );
        assert!(crate::bundle::host::Host::Local
            .sh_data(
                "/",
                &remote_script(dir.to_str(), "/etc/hosts", &[]),
                Duration::from_secs(10)
            )
            .await
            .is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
