//! Store attachments on the selected agent host, using its configured SSH account.
use base64::Engine;
use serde_json::{json, Value};
use std::{io::Write, process::Stdio, time::Duration};
use tokio::io::AsyncWriteExt;

const MAX_BYTES: usize = 25 * 1024 * 1024;
fn filename(name: &str) -> String {
    let name: String = name
        .chars()
        .take(160)
        .map(|c| {
            if c.is_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() || name.trim_matches('.').is_empty() {
        "attachment.bin".into()
    } else {
        name
    }
}
fn upload_script(name: &str) -> String {
    format!("umask 077\ndir=$(mktemp -d /tmp/jarvis-attachment.XXXXXXXX) || exit 1\nfile=\"$dir\"/{}\ncat > \"$file\" || {{ rm -f \"$file\"; rmdir \"$dir\"; exit 1; }}\nprintf '%s\\n' \"$file\"", crate::util::shell_quote(name))
}
fn save_local(bytes: &[u8], name: &str) -> Result<String, String> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "jarvis-attachment.{}-{}-{}",
        crate::util::now_ms(),
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}
async fn upload(mut command: tokio::process::Command, bytes: &[u8]) -> Result<String, String> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or("Не удалось открыть поток загрузки")?;
    let (written, output) = tokio::time::timeout(Duration::from_secs(90), async {
        tokio::join!(
            async {
                stdin.write_all(bytes).await?;
                stdin.shutdown().await?;
                drop(stdin);
                Ok::<(), std::io::Error>(())
            },
            child.wait_with_output()
        )
    })
    .await
    .map_err(|_| "Истекло время загрузки файла")?;
    written.map_err(|e| e.to_string())?;
    let output = output.map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "Не удалось загрузить файл: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let path = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find(|line| line.starts_with("/tmp/jarvis-attachment."))
        .ok_or("Узел не вернул путь файла")?
        .to_string();
    Ok(path)
}
#[tauri::command]
pub async fn session_save_attachment(
    app: tauri::AppHandle,
    data_base64: String,
    name: String,
    machine: Option<String>,
) -> Value {
    async fn save(
        app: &tauri::AppHandle,
        data: String,
        name: String,
        machine: Option<String>,
    ) -> Result<String, String> {
        if data.len() > MAX_BYTES.div_ceil(3) * 4 {
            return Err("Файл больше 25 МБ".into());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&data)
            .map_err(|_| "Не удалось прочитать файл")?;
        if bytes.len() > MAX_BYTES {
            return Err("Файл больше 25 МБ".into());
        }
        let name = filename(&name);
        let machine = machine.unwrap_or_default();
        if machine.is_empty() || machine == "local" {
            return save_local(&bytes, &name);
        }
        let d = crate::daemon::Daemon::get(app);
        let node = d.remotes.node(&machine).ok_or("Машина не подключена")?;
        let command = node
            .cfg
            .connection()
            .command_with_script(&upload_script(&name))?;
        upload(tokio::process::Command::from(command), &bytes).await
    }
    match save(&app, data_base64, name, machine).await {
        Ok(path) => json!({"ok":true,"path":path}),
        Err(error) => json!({"ok":false,"error":error}),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_cannot_escape_attachment_directory() {
        for input in ["../secret.pdf", "/etc/passwd", "a\n$(id).png", "", ".."] {
            let name = filename(input);
            assert!(!name.contains('/') && !name.contains('\n') && name != "..");
            assert_eq!(std::path::Path::new(&name).components().count(), 1);
        }
    }
    #[test]
    fn local_documents_preserve_bytes_and_extension() {
        let data = b"%PDF-1.7\n\0binary";
        let path = save_local(data, "report.pdf").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), data);
        assert!(path.ends_with("report.pdf"));
        std::fs::remove_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();
    }
    #[tokio::test]
    async fn async_upload_closes_stdin_before_waiting_for_remote_cat() {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", &upload_script("eof-test.pdf")]);
        let path =
            tokio::time::timeout(Duration::from_secs(3), upload(command, b"binary\0payload"))
                .await
                .expect("upload must finish after EOF")
                .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"binary\0payload");
        std::fs::remove_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();
    }
    #[test]
    fn remote_script_accepts_binary_stdin_and_quoted_names() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", &upload_script("report's.pdf")])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"binary\0payload\n")
            .unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(result.status.success());
        let path = String::from_utf8(result.stdout).unwrap();
        let path = path.trim();
        assert_eq!(std::fs::read(path).unwrap(), b"binary\0payload\n");
        std::fs::remove_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
    }
}
