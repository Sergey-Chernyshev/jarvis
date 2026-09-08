//! Teleport discovery and onboarding through the installed tsh CLI.
//! Only public profile metadata crosses IPC. Login runs in the user's terminal;
//! Jarvis never receives passwords, MFA responses, keys, or identity claims.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

const MAX_STATUS: usize = 1024 * 1024;
const MAX_NODES: usize = 4 * 1024 * 1024;
const MAX_STDERR: usize = 32 * 1024;
const MAX_PROFILES: usize = 64;
const MAX_NODE_COUNT: usize = 512;

fn executable() -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let mut paths = vec![
        PathBuf::from("/opt/homebrew/bin/tsh"),
        PathBuf::from("/usr/local/bin/tsh"),
    ];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(
            std::env::split_paths(&path)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join("tsh")),
        );
    }
    paths.extend([
        PathBuf::from("/usr/bin/tsh"),
        crate::util::home_dir().join(".local/bin/tsh"),
    ]);
    paths.into_iter().find(|path| {
        path.metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    })
}

pub(crate) fn validate_proxy(proxy: &str) -> Result<String, String> {
    if proxy.len() > 300 || proxy.chars().any(char::is_control) {
        return Err("Некорректный Teleport proxy".into());
    }
    let proxy = proxy.trim();
    crate::install::remote::connection::validate_teleport_proxy(proxy)?;
    Ok(proxy.to_ascii_lowercase())
}

fn optional_proxy(proxy: Option<&str>) -> Result<Option<String>, String> {
    proxy
        .filter(|proxy| !proxy.trim().is_empty())
        .map(validate_proxy)
        .transpose()
}

fn validate_cluster(cluster: &str) -> Result<String, String> {
    if cluster.is_empty()
        || cluster.len() > 253
        || cluster.starts_with('-')
        || !cluster
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err("Некорректное имя Teleport-кластера".into());
    }
    Ok(cluster.into())
}

fn profile_proxy(value: &str) -> Option<String> {
    let host = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .unwrap_or(value)
        .trim_end_matches('/');
    validate_proxy(host).ok()
}

fn same_proxy(a: &str, b: &str) -> bool {
    // tsh profile_url commonly includes :443 even when the user omitted it.
    a.trim_end_matches(":443")
        .eq_ignore_ascii_case(b.trim_end_matches(":443"))
}

fn string(value: &Value, key: &str, max: usize) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty() && v.len() <= max && !v.chars().any(char::is_control))
        .map(str::to_string)
}

fn logins(value: &Value) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(values) = value.get("logins").and_then(Value::as_array) {
        for name in values.iter().take(256).filter_map(Value::as_str) {
            if name.len() <= 128
                && !name.is_empty()
                && !name.starts_with('-')
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                && !names.iter().any(|v| v == name)
            {
                names.push(name.to_string());
            }
        }
    }
    names
}

fn profile(value: &Value, now: i64) -> Option<Value> {
    let proxy = profile_proxy(&string(value, "profile_url", 512)?)?;
    let cluster = string(value, "cluster", 253)?;
    let username = string(value, "username", 320)?;
    let valid_until = string(value, "valid_until", 80);
    let authenticated = valid_until
        .as_deref()
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .is_some_and(|v| v.timestamp() > now);
    Some(
        json!({"proxy":proxy,"cluster":cluster,"username":username,"logins":logins(value),"validUntil":valid_until,"authenticated":authenticated}),
    )
}

fn sanitized_status(raw: &Value, requested: Option<&str>, now: i64) -> Result<Value, String> {
    if !raw.is_object()
        || (!raw
            .get("active")
            .is_some_and(|v| v.is_null() || v.is_object())
            && !raw.get("profiles").is_some_and(Value::is_array))
    {
        return Err("tsh вернул неизвестный формат статуса".into());
    }
    let active = raw.get("active").and_then(|v| profile(v, now));
    let mut profiles = Vec::new();
    for candidate in active.iter().cloned().chain(
        raw.get("profiles")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(MAX_PROFILES)
            .filter_map(|v| profile(v, now)),
    ) {
        if !profiles.iter().any(|known: &Value| {
            known["proxy"] == candidate["proxy"]
                && known["cluster"] == candidate["cluster"]
                && known["username"] == candidate["username"]
        }) {
            profiles.push(candidate);
        }
    }
    let selected = match requested {
        Some(proxy) => profiles
            .iter()
            .find(|p| same_proxy(p["proxy"].as_str().unwrap_or(""), proxy)),
        None => active.as_ref(),
    };
    let mut status = selected.cloned().unwrap_or_else(||json!({"proxy":requested,"cluster":null,"username":null,"logins":[],"validUntil":null,"authenticated":false}));
    status["profiles"] = json!(profiles);
    status["ok"] = json!(true);
    status["available"] = json!(true);
    Ok(status)
}

#[derive(Debug)]
struct Output {
    status: ExitStatus,
    stdout: Vec<u8>,
}

async fn read_bounded(reader: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| "Не удалось прочитать ответ tsh".to_string())?;
    if bytes.len() > limit {
        return Err("Ответ tsh превысил допустимый размер".into());
    }
    Ok(bytes)
}

async fn run(
    path: &Path,
    args: &[String],
    timeout: Duration,
    limit: usize,
) -> Result<Output, String> {
    let mut child = tokio::process::Command::new(path)
        .args(args)
        .current_dir(crate::util::home_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "Не удалось запустить tsh".to_string())?;
    let stdout = child.stdout.take().ok_or("tsh не предоставил stdout")?;
    let stderr = child.stderr.take().ok_or("tsh не предоставил stderr")?;
    let result = tokio::time::timeout(timeout, async {
        tokio::try_join!(
            read_bounded(stdout, limit),
            read_bounded(stderr, MAX_STDERR),
            async {
                child
                    .wait()
                    .await
                    .map_err(|_| "Не удалось дождаться tsh".to_string())
            }
        )
    })
    .await;
    match result {
        Ok(Ok((stdout, _, status))) => Ok(Output { status, stdout }),
        Ok(Err(error)) => {
            let _ = child.kill().await;
            Err(error)
        }
        Err(_) => {
            let _ = child.kill().await;
            Err("tsh не ответил вовремя. Проверь подключение и обнови состояние.".into())
        }
    }
}

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).into()).collect()
}

async fn read_status(path: &Path, proxy: Option<&str>) -> Result<Value, String> {
    let out = run(
        path,
        &args(&["status", "--format=json", "--client"]),
        Duration::from_secs(5),
        MAX_STATUS,
    )
    .await?;
    // Expired profiles can still be useful even when tsh returns a nonzero code.
    let raw: Value = serde_json::from_slice(&out.stdout).map_err(|_| {
        if out.status.success() {
            "tsh вернул некорректный статус"
        } else {
            "Не удалось прочитать профиль tsh. Выполни вход через Teleport."
        }
        .to_string()
    })?;
    sanitized_status(&raw, proxy, chrono::Utc::now().timestamp())
}

fn missing() -> Value {
    json!({"ok":true,"available":false,"version":null,"authenticated":false,"proxy":null,"cluster":null,"username":null,"logins":[],"validUntil":null,"profiles":[]})
}

pub async fn status(proxy: Option<String>) -> Value {
    let proxy = match optional_proxy(proxy.as_deref()) {
        Ok(proxy) => proxy,
        Err(error) => return json!({"ok":false,"error":error}),
    };
    let Some(path) = executable() else {
        return missing();
    };
    let version_args = args(&["version", "--client", "--format=json"]);
    let (status, version) = tokio::join!(
        read_status(&path, proxy.as_deref()),
        run(&path, &version_args, Duration::from_secs(3), 4096)
    );
    let version = version
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| serde_json::from_slice::<Value>(&out.stdout).ok())
        .and_then(|v| string(&v, "version", 80));
    match status {
        Ok(mut status) => {
            status["version"] = json!(version);
            status
        }
        Err(error) => {
            json!({"ok":false,"available":true,"version":version,"authenticated":false,"proxy":proxy,"cluster":null,"username":null,"logins":[],"validUntil":null,"profiles":[],"error":error})
        }
    }
}

fn sanitized_nodes(raw: &Value) -> Result<(Vec<Value>, bool), String> {
    let raw = raw
        .as_array()
        .ok_or("tsh вернул неизвестный формат списка серверов")?;
    let mut nodes = Vec::new();
    for value in raw.iter().take(MAX_NODE_COUNT) {
        let metadata = &value["metadata"];
        let spec = &value["spec"];
        let Some(id) = string(metadata, "name", 253).filter(|id| validate_cluster(id).is_ok())
        else {
            continue;
        };
        let hostname = string(spec, "hostname", 253).unwrap_or_else(|| id.clone());
        let mut labels = serde_json::Map::new();
        if let Some(values) = metadata.get("labels").and_then(Value::as_object) {
            for (key, value) in values.iter().take(64) {
                if key.len() <= 128 && !key.chars().any(char::is_control) {
                    if let Some(value) = value
                        .as_str()
                        .filter(|v| v.len() <= 512 && !v.chars().any(char::is_control))
                    {
                        labels.insert(key.clone(), json!(value));
                    }
                }
            }
        }
        nodes
            .push(json!({"id":id,"target":id,"name":hostname,"hostname":hostname,"labels":labels}));
    }
    nodes.sort_by(|a, b| {
        a["name"]
            .as_str()
            .cmp(&b["name"].as_str())
            .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
    Ok((nodes, raw.len() > MAX_NODE_COUNT))
}

fn inventory_args(proxy: &str, cluster: Option<&str>) -> Vec<String> {
    // tsh ls retries expired/revoked credentials. This global flag suppresses
    // its browser opener; stdin is closed and any printed login URL stays private.
    let mut command = args(&["--browser-login=none", "ls", "--format=json"]);
    command.push(format!("--proxy={proxy}"));
    if let Some(cluster) = cluster {
        command.push(format!("--cluster={cluster}"));
    }
    command
}

pub async fn nodes(proxy: Option<String>, cluster: Option<String>) -> Value {
    let proxy = match optional_proxy(proxy.as_deref()) {
        Ok(proxy) => proxy,
        Err(error) => return json!({"ok":false,"error":error}),
    };
    let cluster = match cluster
        .as_deref()
        .filter(|cluster| !cluster.trim().is_empty())
        .map(validate_cluster)
        .transpose()
    {
        Ok(cluster) => cluster,
        Err(error) => return json!({"ok":false,"error":error}),
    };
    let Some(path) = executable() else {
        return json!({"ok":false,"available":false,"error":"tsh не установлен"});
    };
    let status = match read_status(&path, proxy.as_deref()).await {
        Ok(status) => status,
        Err(error) => return json!({"ok":false,"error":error}),
    };
    if status["authenticated"] != true {
        return json!({"ok":false,"authenticated":false,"error":"Войди в Teleport, чтобы увидеть доступные серверы"});
    }
    let Some(proxy) = status["proxy"].as_str() else {
        return json!({"ok":false,"error":"Не выбран Teleport proxy"});
    };
    let cluster = cluster.or_else(|| status["cluster"].as_str().map(str::to_string));
    let command = inventory_args(proxy, cluster.as_deref());
    match run(&path, &command, Duration::from_secs(15), MAX_NODES).await {
        Ok(out) if out.status.success() => match serde_json::from_slice::<Value>(&out.stdout)
            .ok()
            .as_ref()
            .map(sanitized_nodes)
        {
            Some(Ok((nodes, truncated))) => {
                json!({"ok":true,"nodes":nodes,"truncated":truncated,"logins":status["logins"],"cluster":cluster,"proxy":proxy})
            }
            _ => json!({"ok":false,"error":"tsh вернул некорректный список серверов"}),
        },
        Ok(_) => {
            json!({"ok":false,"error":"Не удалось получить серверы Teleport. Проверь вход, доступ к кластеру и подключение."})
        }
        Err(error) => json!({"ok":false,"error":error}),
    }
}

fn login_command(path: &Path, proxy: &str, renew_user: Option<&str>) -> String {
    let login = [
        path.to_string_lossy().into_owned(),
        "login".into(),
        format!("--proxy={proxy}"),
    ]
    .iter()
    .map(|arg| crate::util::shell_quote(arg))
    .collect::<Vec<_>>()
    .join(" ");
    if let Some(user) = renew_user {
        // tsh 18 reuses a locally valid certificate even if access was revoked.
        // Only the explicit "log out and sign in again" action takes this path.
        // Specify both proxy and identity so no unrelated profiles are removed.
        let logout = [path.to_string_lossy().into_owned(), "logout".into(), format!("--proxy={proxy}"), format!("--user={user}")]
            .iter().map(|arg| crate::util::shell_quote(arg)).collect::<Vec<_>>().join(" ");
        format!("{logout} && {login}")
    } else { login }
}

pub async fn login(proxy: String, terminal: String, custom: String, renew: bool) -> Value {
    let proxy = match validate_proxy(&proxy) {
        Ok(proxy) => proxy,
        Err(error) => return json!({"ok":false,"error":error}),
    };
    let Some(path) = executable() else {
        return json!({"ok":false,"available":false,"error":"tsh не установлен"});
    };
    let renew_user = if renew {
        match read_status(&path, Some(&proxy)).await {
            Ok(status) => status["username"].as_str().map(str::to_string),
            Err(error) => return json!({"ok":false,"error":error}),
        }
    } else { None };
    match tokio::time::timeout(
        Duration::from_secs(15),
        crate::launch::spawn(&terminal, &custom, &login_command(&path, &proxy, renew_user.as_deref())),
    )
    .await
    {
        Ok(Ok(())) => json!({"ok":true,"started":true,"proxy":proxy}),
        Ok(Err(error)) => json!({"ok":false,"error":error}),
        Err(_) => {
            json!({"ok":false,"error":"Терминал не подтвердил открытие. Проверь его перед повторным входом."})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn raw_profile(proxy: &str, expires: &str) -> Value {
        json!({"profile_url":format!("https://{proxy}"),"username":"person@example.org","cluster":"main.example.org", "logins":["root","developer","root","-flag","x;bad"],"valid_until":expires,
            "traits":{"token":"NEVER_CROSS_IPC"},"roles":["private-role"],"ssh_certificate":"PRIVATE_KEY_DATA"})
    }

    #[test]
    fn proxy_and_cluster_reject_options_shell_fragments_and_urls() {
        for value in [
            "proxy.example.org",
            "proxy.example.org:443",
            "localhost:3080",
            "[::1]:443",
        ] {
            assert!(validate_proxy(value).is_ok(), "{value}");
        }
        for value in [
            "",
            "-proxy",
            "https://proxy.example.org",
            "name@proxy",
            "proxy/path",
            "proxy;bad",
            "proxy$(bad)",
            "proxy`bad`",
            "proxy:0",
            "proxy:65536",
            "proxy:443\n",
            "[::1]:443:22",
        ] {
            assert!(validate_proxy(value).is_err(), "{value}");
        }
        for value in ["-flag", "main;bad", "main\nnext", "main next"] {
            assert!(validate_cluster(value).is_err());
        }
        assert_eq!(
            validate_proxy("PROXY.example.org").unwrap(),
            "proxy.example.org"
        );
    }

    #[test]
    fn status_exposes_only_whitelisted_metadata_and_selects_requested_proxy() {
        let raw = json!({"active":raw_profile("first.example.org:443","2099-01-01T00:00:00Z"),"profiles":[raw_profile("other.example.org","2000-01-01T00:00:00Z")]});
        let active = sanitized_status(&raw, None, 1_700_000_000).unwrap();
        assert_eq!(active["authenticated"], true);
        assert_eq!(active["logins"], json!(["root", "developer"]));
        assert_eq!(active["profiles"].as_array().unwrap().len(), 2);
        assert!(!active.to_string().contains("NEVER_CROSS_IPC"));
        assert!(!active.to_string().contains("PRIVATE_KEY_DATA"));
        assert!(!active.to_string().contains("private-role"));
        assert_eq!(
            sanitized_status(&raw, Some("first.example.org"), 1_700_000_000).unwrap()
                ["authenticated"],
            true
        );
        let expired = sanitized_status(&raw, Some("other.example.org"), 1_700_000_000).unwrap();
        assert_eq!(expired["authenticated"], false);
        assert_eq!(expired["proxy"], "other.example.org");
        let missing = sanitized_status(&raw, Some("new.example.org"), 1_700_000_000).unwrap();
        assert_eq!(missing["authenticated"], false);
        assert!(missing["logins"].as_array().unwrap().is_empty());
    }

    #[test]
    fn logged_out_and_unknown_expiry_never_claim_authentication() {
        let out = sanitized_status(&json!({"active":null,"profiles":null}), None, 10).unwrap();
        assert_eq!(out["authenticated"], false);
        assert_eq!(out["available"], true);
        let invalid = json!({"active":raw_profile("proxy.example.org","not-a-date"),"profiles":[]});
        assert_eq!(
            sanitized_status(&invalid, None, 10).unwrap()["authenticated"],
            false
        );
        assert!(sanitized_status(&json!({"unexpected":true}), None, 10).is_err());
        assert!(sanitized_status(&json!([]), None, 10).is_err());
    }

    #[test]
    fn server_inventory_uses_resource_ids_and_drops_unneeded_server_fields() {
        let raw = json!([
            {"metadata":{"name":"uuid-b","labels":{"env":"prod"}},"spec":{"hostname":"server-b","addr":"secret-address","public_addr":"secret-other"}},
            {"metadata":{"name":"uuid-a","labels":{"env":"dev","invalid":false}},"spec":{"hostname":"server-a"}},
            {"metadata":{"name":"bad;command"},"spec":{"hostname":"skip"}}
        ]);
        let (nodes, truncated) = sanitized_nodes(&raw).unwrap();
        assert!(!truncated);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0]["target"], "uuid-a");
        assert_eq!(nodes[0]["name"], "server-a");
        assert_eq!(nodes[0]["labels"], json!({"env":"dev"}));
        assert!(!serde_json::to_string(&nodes)
            .unwrap()
            .contains("secret-address"));
        let many = Value::Array(
            (0..MAX_NODE_COUNT + 1)
                .map(|i| json!({"metadata":{"name":format!("uuid-{i}")},"spec":{}}))
                .collect(),
        );
        let (nodes, truncated) = sanitized_nodes(&many).unwrap();
        assert_eq!(nodes.len(), MAX_NODE_COUNT);
        assert!(truncated);
    }

    #[test]
    fn inventory_pins_proxy_and_cluster_and_never_opens_a_login_browser() {
        assert_eq!(
            inventory_args("proxy.example.org:443", Some("leaf.example.org")),
            args(&[
                "--browser-login=none",
                "ls",
                "--format=json",
                "--proxy=proxy.example.org:443",
                "--cluster=leaf.example.org"
            ])
        );
    }

    #[test]
    fn official_login_argv_is_shell_quoted_and_contains_no_credentials() {
        let command = login_command(
            Path::new("/tmp/tsh 'quoted' $(not-run)"),
            "proxy.example.org:443",
            None,
        );
        assert_eq!(
            command,
            "'/tmp/tsh '\\''quoted'\\'' $(not-run)' 'login' '--proxy=proxy.example.org:443'"
        );
        assert!(!command.contains("password"));
    }

    #[test]
    fn renewal_only_logs_out_the_selected_proxy_and_identity() {
        let command = login_command(Path::new("/opt/homebrew/bin/tsh"), "proxy.example.org:443", Some("person@example.org"));
        assert_eq!(command, "'/opt/homebrew/bin/tsh' 'logout' '--proxy=proxy.example.org:443' '--user=person@example.org' && '/opt/homebrew/bin/tsh' 'login' '--proxy=proxy.example.org:443'");
        assert!(!login_command(Path::new("tsh"), "proxy.example.org", None).contains("logout"));
    }

    #[tokio::test]
    async fn subprocess_stdout_stderr_and_time_are_bounded() {
        let output = run(
            Path::new("/bin/sh"),
            &args(&["-c", "printf '{\"safe\":true}'; printf 'secret' >&2"]),
            Duration::from_secs(2),
            64,
        )
        .await
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"{\"safe\":true}");
        let huge = run(
            Path::new("/bin/sh"),
            &args(&["-c", "printf '123456789'"]),
            Duration::from_secs(2),
            8,
        )
        .await
        .unwrap_err();
        assert!(huge.contains("размер"));
        let timeout = run(
            Path::new("/bin/sh"),
            &args(&["-c", "exec sleep 10"]),
            Duration::from_millis(30),
            8,
        )
        .await
        .unwrap_err();
        assert!(timeout.contains("вовремя"));
    }
}
