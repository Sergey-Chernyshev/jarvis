//! Hook trust through the provider's public RPC, using its own key/hash.
//! No private hash reimplementation and no blanket hook-trust bypass.
use serde_json::{json, Map, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const EVENTS: &[&str] = &["session-start", "session-end", "prompt", "pre-tool", "post-tool", "permission", "stop", "subagent-start", "subagent-stop"];
fn quote(s: &str) -> String { format!("'{}'", s.replace('\'', "'\\''")) }

pub fn trust_edits(response: &Value, home: &Path, hook_bin: &Path) -> Result<Map<String, Value>, String> {
    let hooks_path = home.join("hooks.json");
    let path = std::fs::canonicalize(&hooks_path).unwrap_or(hooks_path).to_string_lossy().into_owned();
    let expected: std::collections::HashSet<_> = EVENTS.iter().map(|event|
        format!("{} 'codex' {}", quote(&hook_bin.to_string_lossy()), quote(event))).collect();
    let mut edits = Map::new();
    let mut found = std::collections::HashSet::new();
    for entry in response["data"].as_array().into_iter().flatten() {
        for hook in entry["hooks"].as_array().into_iter().flatten() {
            if hook["sourcePath"].as_str() != Some(path.as_str()) { continue; }
            let Some(command) = hook["command"].as_str().filter(|s| expected.contains(*s)) else { continue; };
            if hook["handlerType"] != "command" || hook["source"] != "user" || hook["isManaged"] == true { continue; }
            found.insert(command.to_owned());
            if hook["enabled"] == false { return Err("Хук Jarvis отключён в Codex. Сохранено существующее отключение".into()); }
            let key = hook["key"].as_str().filter(|s| s.starts_with(&format!("{path}:"))).ok_or("Codex вернул неожиданную идентичность хука")?;
            let hash = hook["currentHash"].as_str().filter(|s| !s.is_empty() && s.len() <= 256).ok_or("Codex не вернул hash хука")?;
            if hook["trustStatus"] != "trusted" { edits.insert(key.into(), json!({"trusted_hash":hash})); }
        }
    }
    if found.len() != expected.len() { return Err(format!("Codex распознал {} из {} хуков Jarvis. Проверь версию CLI и файл hooks.json", found.len(), expected.len())); }
    Ok(edits)
}

pub async fn reconcile(program: &Path, home: &Path, hook_bin: &Path, trust: bool) -> Result<Value, String> {
    reconcile_with_path(program, home, hook_bin, trust, None).await
}

/// A service may have a smaller PATH than the shell that installed a Node.js
/// based CLI. Scope the resolved search path to this child, never process-wide.
pub async fn reconcile_with_path(program: &Path, home: &Path, hook_bin: &Path, trust: bool, search_path: Option<&std::ffi::OsStr>) -> Result<Value, String> {
    let mut command = tokio::process::Command::new(program);
    if let Some(path) = search_path { command.env("PATH", path); }
    let mut child = command.args(["app-server", "--listen", "stdio://"])
        .env("CODEX_HOME", home).env("JARVIS_IGNORE", "1").current_dir(home)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true)
        .spawn().map_err(|e| format!("Codex app-server: {e}"))?;
    let mut input = child.stdin.take().ok_or("Нет stdin Codex")?;
    let mut output = BufReader::new(child.stdout.take().ok_or("Нет stdout Codex")?);
    async fn request(input: &mut tokio::process::ChildStdin, output: &mut BufReader<tokio::process::ChildStdout>, id: u64, method: &str, params: Value) -> Result<Value, String> {
        let mut bytes = serde_json::to_vec(&json!({"id":id,"method":method,"params":params})).map_err(|e|e.to_string())?;
        bytes.push(b'\n'); input.write_all(&bytes).await.map_err(|e|e.to_string())?;
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let mut line = Vec::new();
                let n = (&mut *output).take(4 * 1024 * 1024).read_until(b'\n', &mut line).await.map_err(|e|e.to_string())?;
                if n == 0 { return Err("Codex закрыл RPC до ответа".into()); }
                if line.last() != Some(&b'\n') { return Err("Ответ Codex превысил лимит RPC".into()); }
                let value: Value = serde_json::from_slice(&line).map_err(|e|e.to_string())?;
                if value["id"] == id {
                    if let Some(error) = value.get("error") { return Err(format!("Codex {method}: {error}")); }
                    return value.get("result").cloned().ok_or_else(|| "Нет результата RPC".into());
                }
            }
        }).await.map_err(|_| format!("Codex {method} не ответил за 20 секунд"))?
    }
    let result = async {
        request(&mut input, &mut output, 1, "initialize", json!({"clientInfo":{"name":"jarvis_hook_setup","version":"1"},"capabilities":{"experimentalApi":true}})).await?;
        input.write_all(b"{\"method\":\"initialized\"}\n").await.map_err(|e|e.to_string())?;
        let mut hooks = request(&mut input, &mut output, 2, "hooks/list", json!({"cwds":[home]})).await?;
        if trust {
            let edits = trust_edits(&hooks, home, hook_bin)?;
            if !edits.is_empty() {
                request(&mut input, &mut output, 3, "config/batchWrite", json!({"edits":[{"keyPath":"hooks.state","value":edits,"mergeStrategy":"upsert"}],"filePath":home.join("config.toml"),"reloadUserConfig":true})).await?;
                hooks = request(&mut input, &mut output, 4, "hooks/list", json!({"cwds":[home]})).await?;
                if !trust_edits(&hooks, home, hook_bin)?.is_empty() { return Err("Codex не подтвердил сохранённое доверие к хукам".into()); }
            }
        }
        Ok(hooks)
    }.await;
    let _ = child.kill().await; let _ = child.wait().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hooks() -> Value {
        json!({"data":[{"hooks":EVENTS.iter().enumerate().map(|(n,event)|json!({
            "sourcePath":"/fixture/hooks.json","source":"user","isManaged":false,"enabled":true,
            "key":format!("/fixture/hooks.json:event:{n}:0"),"currentHash":"official-hash","trustStatus":"untrusted",
            "handlerType":"command","command":format!("'/fixture/bin/jarvis-hook' 'codex' {}",quote(event))})).collect::<Vec<_>>()}]})
    }
    #[test] fn exact_owned_source_and_command_only() {
        let mut value = hooks(); let mut foreign = value["data"][0]["hooks"][0].clone();
        foreign["sourcePath"] = "/other/hooks.json".into(); foreign["key"] = "/other/hooks.json:0".into();
        value["data"][0]["hooks"].as_array_mut().unwrap().push(foreign);
        assert_eq!(trust_edits(&value, Path::new("/fixture"), Path::new("/fixture/bin/jarvis-hook")).unwrap().len(), EVENTS.len());
        value["data"][0]["hooks"][0]["command"] = "'/fixture/bin/jarvis-hook' 'codex' 'session-start'; evil".into();
        assert!(trust_edits(&value, Path::new("/fixture"), Path::new("/fixture/bin/jarvis-hook")).is_err());
    }
    #[test] fn disabled_hooks_are_not_reenabled_and_partial_setup_is_not_healthy() {
        let mut value = hooks(); value["data"][0]["hooks"][0]["enabled"] = false.into();
        assert!(trust_edits(&value, Path::new("/fixture"), Path::new("/fixture/bin/jarvis-hook")).is_err());
    }

    #[tokio::test]
    #[ignore = "requires an explicitly selected installed Codex CLI; uses only a disposable home"]
    async fn real_cli_trust_setup_preserves_foreign_hooks_and_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let binary = std::env::var_os("JARVIS_CODEX_TEST_BIN").expect("select a test CLI");
        let dir = std::env::temp_dir().join(format!("jarvis-hook-rpc-{}",std::process::id()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let hook = dir.join("bin/jarvis-hook");
        std::fs::write(&hook,"#!/bin/sh\nprintf '{}\\n'\n").unwrap();
        std::fs::set_permissions(&hook,std::fs::Permissions::from_mode(0o700)).unwrap();
        let names = ["SessionStart","SessionEnd","UserPromptSubmit","PreToolUse","PostToolUse","PermissionRequest","Stop","SubagentStart","SubagentStop"];
        let mut hooks = Map::new();
        for (name,event) in names.iter().zip(EVENTS) {
            hooks.insert((*name).into(),json!([{"hooks":[{"type":"command","command":format!("{} 'codex' {}",quote(&hook.to_string_lossy()),quote(event))}]}]));
        }
        hooks["Stop"].as_array_mut().unwrap().push(json!({"hooks":[{"type":"command","command":"echo foreign-hook"}]}));
        std::fs::write(dir.join("hooks.json"),serde_json::to_vec(&json!({"hooks":hooks})).unwrap()).unwrap();
        std::fs::write(dir.join("config.toml"),"approval_policy = \"on-request\"\nsandbox_mode = \"workspace-write\"\n").unwrap();
        let response = reconcile(Path::new(&binary),&dir,&hook,true).await.unwrap();
        assert!(trust_edits(&response,&dir,&hook).unwrap().is_empty());
        let foreign = response["data"].as_array().unwrap().iter().flat_map(|entry|entry["hooks"].as_array().unwrap()).find(|h|h["command"]=="echo foreign-hook").unwrap();
        assert_eq!(foreign["trustStatus"],"untrusted");
        let config = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        assert!(config.contains("approval_policy = \"on-request\"")); assert!(config.contains("sandbox_mode = \"workspace-write\""));
        let again = reconcile(Path::new(&binary),&dir,&hook,true).await.unwrap();
        assert!(trust_edits(&again,&dir,&hook).unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
