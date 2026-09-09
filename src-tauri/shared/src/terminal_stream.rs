//! A bounded, byte-preserving tmux control client, shared by desktop and node.
//!
//! A stream owns only its attached client process. All commands use that same
//! connection, so a restarted server cannot receive input intended for an old
//! pane ID. No global options, hooks, sessions or panes are created here.
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, watch, Mutex as AsyncMutex};

const MAX_STREAMS: usize = 16;
const MAX_INPUT: usize = 64 * 1024;
const MAX_PASTE: usize = 1024 * 1024;
const RING_BYTES: usize = 4 * 1024 * 1024;
const POLL_BYTES: usize = 256 * 1024;
const MAX_LINE: usize = 2 * 1024 * 1024;
const MAX_REPLY: usize = 16 * 1024 * 1024;
const MAX_INITIAL: usize = 4 * 1024 * 1024;
const HISTORY_LINES: usize = 2000;
const MAX_HISTORY_LINES: usize = 100_000;
const IDLE_TTL: Duration = Duration::from_secs(120);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(8);
const META: &str = "#{pane_id}|#{pane_pid}|#{pid}|#{session_id}|#{window_id}|#{window_panes}|#{pane_width}|#{pane_height}|#{history_size}|#{cursor_x}|#{cursor_y}|#{alternate_on}|#{cursor_flag}|#{cursor_shape}|#{cursor_blinking}|#{keypad_cursor_flag}|#{keypad_flag}|#{insert_flag}|#{origin_flag}|#{wrap_flag}|#{mouse_standard_flag}|#{mouse_button_flag}|#{mouse_all_flag}|#{mouse_sgr_flag}|#{mouse_utf8_flag}|#{scroll_region_upper}|#{scroll_region_lower}|#{pane_in_mode}|#{pane_synchronized}|#{pane_dead}|#{pane_input_off}|#{alternate_saved_x}|#{alternate_saved_y}|#{session_attached}";
static HUB: OnceLock<Arc<Hub>> = OnceLock::new();
static NEXT: AtomicU64 = AtomicU64::new(1);

fn error(code: &str, message: &str) -> Value {
    json!({"ok":false,"code":code,"error":message})
}

/// Node and desktop use exactly the same protocol; callers bind stream IDs to
/// their own session identity before forwarding subsequent requests.
pub async fn dispatch(action: &str, payload: &Value) -> Value {
    let hub = HUB.get_or_init(|| Hub::new("jarvis"));
    hub.dispatch(action, payload).await
}

struct Hub {
    socket: String,
    streams: Mutex<HashMap<String, Arc<Stream>>>,
}

impl Hub {
    fn new(socket: &str) -> Arc<Self> {
        let hub = Arc::new(Self {
            socket: socket.into(),
            streams: Mutex::new(HashMap::new()),
        });
        let weak = Arc::downgrade(&hub);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(15)).await;
                let Some(hub) = weak.upgrade() else { break };
                hub.reap(Instant::now());
            }
        });
        hub
    }

    fn reap(&self, now: Instant) {
        self.streams.lock().unwrap().retain(|_, stream| {
            if now.saturating_duration_since(*stream.touched.lock().unwrap()) >= IDLE_TTL {
                stream.close("Terminal connection expired after inactivity");
                false
            } else {
                true
            }
        });
    }

    async fn dispatch(&self, action: &str, payload: &Value) -> Value {
        if !matches!(
            action,
            "open" | "poll" | "input" | "resize" | "history" | "close"
        ) {
            return error(
                "unsupported",
                "This terminal stream action is not supported",
            );
        }
        if action == "open" {
            return self.open(payload).await;
        }
        let Some(id) = payload.get("streamId").and_then(Value::as_str) else {
            return error("invalid_request", "streamId is required");
        };
        let stream = self.streams.lock().unwrap().get(id).cloned();
        let Some(stream) = stream else {
            return if action == "close" {
                json!({"ok":true,"closed":true})
            } else {
                error(
                    "stream_missing",
                    "Terminal connection expired or was closed; reconnect",
                )
            };
        };
        *stream.touched.lock().unwrap() = Instant::now();
        if action == "close" {
            stream.close("Terminal connection closed");
            self.streams.lock().unwrap().remove(id);
            return json!({"ok":true,"closed":true});
        }
        match action {
            "poll" => {
                let Some(cursor) = payload.get("cursor").and_then(Value::as_u64) else {
                    return error("invalid_request", "A nonnegative cursor is required");
                };
                stream.poll(cursor).await
            }
            "input" => stream.input(payload).await,
            "resize" => stream.resize(payload).await,
            "history" => stream.history().await,
            _ => unreachable!(),
        }
    }

    async fn open(&self, payload: &Value) -> Value {
        let history_lines = match payload.get("historyLines") {
            None => HISTORY_LINES,
            Some(value) => match value
                .as_u64()
                .filter(|n| (HISTORY_LINES as u64..=MAX_HISTORY_LINES as u64).contains(n))
            {
                Some(value) => value as usize,
                None => {
                    return error(
                        "invalid_history",
                        "historyLines must be an integer from 2000 to 100000",
                    )
                }
            },
        };
        let Some(pane) = payload
            .get("pane")
            .and_then(Value::as_str)
            .filter(|s| valid_pane(s))
        else {
            return error(
                "invalid_pane",
                "An exact tmux pane ID such as %12 is required",
            );
        };
        self.reap(Instant::now());
        let id = match unique_id() {
            Ok(id) => id,
            Err(e) => return error("unavailable", &e),
        };
        let stream = Stream::new(id.clone(), pane.into());
        {
            let mut streams = self.streams.lock().unwrap();
            if streams.len() >= MAX_STREAMS {
                return error(
                    "stream_limit",
                    "Close another terminal connection before opening one",
                );
            }
            streams.insert(id.clone(), stream.clone());
        }
        match stream.start(&self.socket, history_lines).await {
            Ok(mut result) => {
                result["ok"] = json!(true);
                result["streamId"] = json!(id);
                result
            }
            Err(message) => {
                stream.close(&message);
                self.streams.lock().unwrap().remove(&id);
                if message == "tmux is not installed" {
                    json!({"ok":false,"code":"missing_tmux","needsTmux":true,"error":message})
                } else {
                    error("stream_unavailable", &message)
                }
            }
        }
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        for stream in self.streams.get_mut().unwrap().values() {
            stream.close("Terminal hub stopped");
        }
    }
}

#[derive(Clone, Debug)]
struct Metadata {
    fields: Vec<String>,
    cols: u64,
    rows: u64,
}

impl Metadata {
    fn parse(bytes: &[u8], pane: &str) -> Result<Self, String> {
        let line = std::str::from_utf8(bytes)
            .map_err(|_| "Invalid tmux geometry")?
            .trim_end_matches('\n');
        let fields: Vec<String> = line.split('|').map(str::to_owned).collect();
        if fields.len() != 34
            || fields[0] != pane
            || !valid_number(&fields[1])
            || !valid_number(&fields[2])
            || !valid_prefixed_id(&fields[3], '$')
            || !valid_prefixed_id(&fields[4], '@')
        {
            return Err("The target pane is unavailable or its identity changed".into());
        }
        let cols = fields[6].parse::<u64>().map_err(|_| "Invalid pane width")?;
        let rows = fields[7]
            .parse::<u64>()
            .map_err(|_| "Invalid pane height")?;
        if !(1..=1000).contains(&cols) || !(1..=1000).contains(&rows) {
            return Err("Pane dimensions exceed the terminal stream limit".into());
        }
        Ok(Self { fields, cols, rows })
    }
    fn n(&self, at: usize) -> u64 {
        self.fields[at].parse().unwrap_or(0)
    }
    fn same_pane(&self, other: &Self) -> bool {
        // Pane IDs are never reused inside one server; pane_pid also catches respawn-pane.
        self.fields[..4] == other.fields[..4]
    }
}

#[derive(Clone)]
struct Chunk {
    seq: u64,
    data: Vec<u8>,
}

#[derive(Default)]
struct Ring {
    chunks: VecDeque<Chunk>,
    bytes: usize,
    cursor: u64,
    ready: bool,
    closed: Option<String>,
}

impl Ring {
    fn append(&mut self, data: Vec<u8>) {
        if !self.ready || self.closed.is_some() {
            return;
        }
        for part in data.chunks(32 * 1024) {
            self.cursor += 1;
            self.bytes += part.len();
            self.chunks.push_back(Chunk {
                seq: self.cursor,
                data: part.to_vec(),
            });
        }
        while self.bytes > RING_BYTES {
            if let Some(old) = self.chunks.pop_front() {
                self.bytes -= old.data.len();
            }
        }
    }

    fn slice(&self, since: u64) -> Value {
        let oldest = self
            .chunks
            .front()
            .map(|c| c.seq)
            .unwrap_or(self.cursor + 1);
        let gap = since > self.cursor || since.saturating_add(1) < oldest;
        let mut bytes = 0;
        let mut cursor = since;
        let chunks: Vec<Value> = if gap {
            vec![]
        } else {
            self.chunks
                .iter()
                .filter(|c| c.seq > since)
                .take_while(|chunk| {
                    bytes += chunk.data.len();
                    bytes <= POLL_BYTES
                })
                .map(|chunk| {
                    cursor = chunk.seq;
                    json!({"seq":chunk.seq,"data":chunk.data})
                })
                .collect()
        };
        let drained = self.closed.is_some() && (gap || cursor == self.cursor);
        json!({"ok":true,"cursor":if gap {self.cursor} else {cursor},"chunks":chunks,
            "closed":drained,"gap":gap,"error":if drained {self.closed.as_ref()} else {None}})
    }
}

struct Pending {
    start: Vec<u8>,
    end: Vec<u8>,
    started: bool,
    initial: bool,
    bytes: usize,
    blocks: Vec<Vec<u8>>,
    truncated: bool,
    done: oneshot::Sender<Result<Reply, String>>,
}

struct Reply {
    blocks: Vec<Vec<u8>>,
    truncated: bool,
}
impl std::ops::Deref for Reply {
    type Target = [Vec<u8>];
    fn deref(&self) -> &Self::Target {
        &self.blocks
    }
}

struct Block {
    header: Vec<u8>,
    lines: VecDeque<Vec<u8>>,
    bytes: usize,
    truncated: bool,
    limit: usize,
}

impl Block {
    fn push(&mut self, line: &[u8]) {
        let mut line = line.to_vec();
        line.push(b'\n');
        self.bytes += line.len();
        self.lines.push_back(line);
        // Keep complete newest lines, including the current viewport. A large
        // scrollback export cannot allocate memory proportional to its history.
        while self.bytes > self.limit && self.lines.len() > 1 {
            self.bytes -= self.lines.pop_front().unwrap().len();
            self.truncated = true;
        }
    }
}

struct Stream {
    id: String,
    pane: String,
    ring: Mutex<Ring>,
    metadata: Mutex<Option<Metadata>>,
    touched: Mutex<Instant>,
    pending: Mutex<Option<Pending>>,
    commands: AsyncMutex<()>,
    stdin: AsyncMutex<Option<ChildStdin>>,
    bell: watch::Sender<u64>,
    stop: watch::Sender<bool>,
    dirty: AtomicBool,
}

impl Stream {
    fn new(id: String, pane: String) -> Arc<Self> {
        let (bell, _) = watch::channel(0);
        let (stop, _) = watch::channel(false);
        Arc::new(Self {
            id,
            pane,
            ring: Mutex::new(Ring::default()),
            metadata: Mutex::new(None),
            touched: Mutex::new(Instant::now()),
            pending: Mutex::new(None),
            commands: AsyncMutex::new(()),
            stdin: AsyncMutex::new(None),
            bell,
            stop,
            dirty: AtomicBool::new(false),
        })
    }

    fn closed(&self) -> Option<String> {
        self.ring.lock().unwrap().closed.clone()
    }
    fn close(&self, reason: &str) {
        let mut ring = self.ring.lock().unwrap();
        if ring.closed.is_none() {
            ring.closed = Some(reason.into());
        }
        drop(ring);
        self.stop.send_replace(true);
        self.bell.send_modify(|n| *n = n.wrapping_add(1));
        if let Some(pending) = self.pending.lock().unwrap().take() {
            let _ = pending.done.send(Err(reason.into()));
        }
    }

    async fn start(self: &Arc<Self>, socket: &str, history_lines: usize) -> Result<Value, String> {
        let mut child = Command::new(tmux_binary())
            .args([
                "-u",
                "-L",
                socket,
                "-C",
                "attach-session",
                "-E",
                "-f",
                "ignore-size",
                "-t",
                &self.pane,
            ])
            .env_remove("TMUX")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    "tmux is not installed"
                } else {
                    "tmux cannot be started"
                }
            })?;
        *self.stdin.lock().await = child.stdin.take();
        let stream = self.clone();
        tokio::spawn(async move {
            stream.read_client(child).await;
        });
        // All commands in this line are synchronous in tmux's command queue.
        // control.c orders %output before subsequent command response blocks.
        // Thus output preceding the final marker is represented by this capture;
        // output following it is appended by the reader, including when HTTP is slow.
        let commands = format!(
            "{} ; capture-pane -p -e -C -J -t {} -S -{} ; capture-pane -p -e -C -N -t {} ; capture-pane -p -a -q -e -C -J -t {} -S -{} ; capture-pane -p -P -C -t {}",
            self.metadata_command(),
            self.pane,
            history_lines,
            self.pane,
            self.pane,
            history_lines,
            self.pane
        );
        let blocks = self.command(&commands, true).await?;
        if blocks.len() != 5 {
            return Err("Unexpected tmux capture response".into());
        }
        let meta = Metadata::parse(&blocks[0], &self.pane)?;
        if meta.n(29) != 0 {
            return Err("The terminal process has exited".into());
        }
        // Copy-mode formats describe its synthetic screen rather than the PTY.
        if meta.n(27) != 0 {
            return Err("Exit tmux copy mode before opening the live terminal".into());
        }
        let (initial, initial_truncated) =
            initial_screen(&meta, &blocks[1], &blocks[2], &blocks[3], &blocks[4])?;
        let saved_history_at_limit = meta.n(11) != 0
            && blocks[3].iter().filter(|b| **b == b'\n').count()
                >= history_lines + meta.rows as usize;
        let result = json!({"cursor":0,"cols":meta.cols,"rows":meta.rows,"initial":initial,
            "historyTruncated":initial_truncated || blocks.truncated || saved_history_at_limit || meta.n(8) > history_lines as u64,
            "historyLines":history_lines,
            "readOnlyGeometry":meta.n(5) != 1 || meta.n(33) > 1,"alternateScreen":meta.n(11) != 0});
        *self.metadata.lock().unwrap() = Some(meta);
        Ok(result)
    }

    fn metadata_command(&self) -> String {
        format!("display-message -p -t {} '{}'", self.pane, META)
    }

    async fn command(&self, commands: &str, initial: bool) -> Result<Reply, String> {
        let _order = self.commands.lock().await;
        if let Some(reason) = self.closed() {
            return Err(reason);
        }
        let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
        let start = format!("JARVIS_START_{}_{}", self.id, nonce);
        let end = format!("JARVIS_END_{}_{}", self.id, nonce);
        let line = format!("display-message -p {start} ; {commands} ; display-message -p {end}\n");
        let (done, result) = oneshot::channel();
        *self.pending.lock().unwrap() = Some(Pending {
            start: format!("{start}\n").into_bytes(),
            end: format!("{end}\n").into_bytes(),
            started: false,
            initial,
            bytes: 0,
            blocks: vec![],
            truncated: false,
            done,
        });
        let operation = async {
            let mut stdin = self.stdin.lock().await;
            let writer = stdin.as_mut().ok_or("Terminal client is not connected")?;
            writer
                .write_all(line.as_bytes())
                .await
                .map_err(|_| "Terminal input connection closed")?;
            writer
                .flush()
                .await
                .map_err(|_| "Terminal input connection closed")?;
            drop(stdin);
            result
                .await
                .map_err(|_| "Terminal command acknowledgement was lost".to_string())?
        };
        match tokio::time::timeout(COMMAND_TIMEOUT, operation).await {
            Ok(Ok(blocks)) => Ok(blocks),
            Ok(Err(message)) => {
                self.close(&message);
                Err(message)
            }
            Err(_) => {
                let message = "Terminal command timed out; delivery is uncertain, reconnect before continuing";
                self.close(message);
                Err(message.into())
            }
        }
    }

    async fn read_client(self: Arc<Self>, mut child: Child) {
        let Some(mut stdout) = child.stdout.take() else {
            self.close("Terminal output is unavailable");
            return;
        };
        let mut stop = self.stop.subscribe();
        let mut buffer = [0u8; 16 * 1024];
        let mut line = Vec::new();
        let mut block: Option<Block> = None;
        let reason = 'read: loop {
            if *stop.borrow() {
                break "Terminal connection closed";
            }
            let read = tokio::select! {
                _ = stop.changed() => break 'read "Terminal connection closed",
                read = stdout.read(&mut buffer) => read,
            };
            match read {
                Ok(0) => break "The tmux client disconnected or the terminal exited",
                Err(_) => break "Terminal output connection failed",
                Ok(count) => {
                    for &byte in &buffer[..count] {
                        if byte != b'\n' {
                            line.push(byte);
                            if line.len() > MAX_LINE {
                                break 'read "Terminal output exceeded the line limit";
                            }
                            continue;
                        }
                        if let Err(message) = self.line(&line, &mut block) {
                            break 'read message;
                        }
                        line.clear();
                    }
                }
            }
        };
        self.close(reason);
        // kill_on_drop is a fallback; wait reaps the attached client, never its pane.
        let _ = child.kill().await;
        let _ = child.wait().await;
        self.stdin.lock().await.take();
    }

    fn line(&self, line: &[u8], block: &mut Option<Block>) -> Result<(), &'static str> {
        if let Some(current) = block.as_mut() {
            let end = line
                .strip_prefix(b"%end ")
                .filter(|rest| *rest == current.header.as_slice());
            let failed = line
                .strip_prefix(b"%error ")
                .filter(|rest| *rest == current.header.as_slice());
            if end.is_some() || failed.is_some() {
                if failed.is_some() {
                    return Err("tmux rejected the terminal command; the target may have closed");
                }
                let current = block.take().unwrap();
                let data = current.lines.into_iter().flatten().collect();
                return self.block(data, current.truncated);
            }
            current.push(line);
            return Ok(());
        }
        if let Some(header) = line.strip_prefix(b"%begin ") {
            let initial = self
                .pending
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|p| p.initial);
            *block = Some(Block {
                header: header.to_vec(),
                lines: VecDeque::new(),
                bytes: 0,
                truncated: false,
                limit: if initial { MAX_INITIAL } else { MAX_REPLY },
            });
        } else if line.starts_with(b"%exit") {
            return Err("The tmux client disconnected or the terminal exited");
        } else if let Some(rest) = line.strip_prefix(b"%output ") {
            if let Some(space) = rest.iter().position(|b| *b == b' ') {
                if &rest[..space] == self.pane.as_bytes() {
                    let data =
                        unescape(&rest[space + 1..]).map_err(|_| "Invalid tmux output encoding")?;
                    self.ring.lock().unwrap().append(data);
                    self.bell.send_modify(|n| *n = n.wrapping_add(1));
                }
            }
        } else if line.starts_with(b"%layout-change ")
            || line.starts_with(b"%pane-mode-changed ")
            || line.starts_with(b"%window-close ")
            || line.starts_with(b"%session-changed ")
        {
            self.dirty.store(true, Ordering::Relaxed);
            self.bell.send_modify(|n| *n = n.wrapping_add(1));
        } else if line.starts_with(b"%pause ") || line.starts_with(b"%extended-output ") {
            return Err("Terminal stream was paused; reconnect to restore a complete screen");
        }
        Ok(())
    }

    fn block(&self, data: Vec<u8>, truncated: bool) -> Result<(), &'static str> {
        let mut pending = self.pending.lock().unwrap();
        let Some(request) = pending.as_mut() else {
            return Ok(());
        };
        if data == request.start {
            request.started = true;
            return Ok(());
        }
        if !request.started {
            return Ok(());
        }
        if data == request.end {
            let request = pending.take().unwrap();
            if request.initial {
                self.ring.lock().unwrap().ready = true;
            }
            let _ = request.done.send(Ok(Reply {
                blocks: request.blocks,
                truncated: request.truncated,
            }));
        } else {
            request.truncated |= truncated;
            request.bytes += data.len();
            if request.bytes > MAX_REPLY + 16 * 1024 {
                return Err("Terminal response exceeded the size limit");
            }
            request.blocks.push(data);
        }
        Ok(())
    }

    async fn fresh_metadata(&self) -> Result<Metadata, String> {
        let blocks = self.command(&self.metadata_command(), false).await?;
        let meta = Metadata::parse(
            blocks.first().ok_or("Missing terminal geometry")?,
            &self.pane,
        )?;
        let previous = self.metadata.lock().unwrap().clone();
        if previous.as_ref().is_some_and(|old| !old.same_pane(&meta)) || meta.n(29) != 0 {
            self.close("The terminal process changed or exited; reconnect");
            return Err("The terminal process changed or exited; reconnect".into());
        }
        *self.metadata.lock().unwrap() = Some(meta.clone());
        Ok(meta)
    }

    async fn poll(&self, cursor: u64) -> Value {
        let mut bell = self.bell.subscribe();
        if self.dirty.swap(false, Ordering::Relaxed) && self.closed().is_none() {
            let _ = self.fresh_metadata().await;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            let mut result = self.ring.lock().unwrap().slice(cursor);
            if let Some(meta) = self.metadata.lock().unwrap().as_ref() {
                result["cols"] = json!(meta.cols);
                result["rows"] = json!(meta.rows);
                result["readOnlyGeometry"] = json!(meta.n(5) != 1 || meta.n(33) > 1);
            }
            if result["closed"] == true
                || result["gap"] == true
                || result["chunks"].as_array().is_some_and(|a| !a.is_empty())
            {
                return result;
            }
            if tokio::time::timeout_at(deadline, bell.changed())
                .await
                .is_err()
            {
                return result;
            }
        }
    }

    fn guard(&self) -> Result<String, String> {
        let meta = self
            .metadata
            .lock()
            .unwrap()
            .clone()
            .ok_or("Terminal is not ready")?;
        // The condition and nested synchronous command execute in the same tmux
        // queue. Refuse copy mode, disabled input and synchronize-panes so a byte
        // can never be silently routed to another pane or a synthetic UI.
        Ok(format!("#{{&&:#{{==:#{{pane_pid}},{}}},#{{&&:#{{==:#{{pid}},{}}},#{{&&:#{{==:#{{pane_in_mode}},0}},#{{&&:#{{==:#{{pane_synchronized}},0}},#{{&&:#{{==:#{{pane_dead}},0}},#{{==:#{{pane_input_off}},0}}}}}}}}}}}}",
            meta.fields[1], meta.fields[2]))
    }

    async fn input(&self, payload: &Value) -> Value {
        let paste = match payload.get("paste") {
            Some(Value::Bool(value)) => *value,
            None => false,
            _ => return error("invalid_input", "paste must be a boolean"),
        };
        let data = match bytes(
            payload.get("data"),
            if paste { MAX_PASTE } else { MAX_INPUT },
        ) {
            Ok(data) => data,
            Err(e) => return error("invalid_input", e),
        };
        if paste && (std::str::from_utf8(&data).is_err() || data.contains(&0)) {
            return error(
                "invalid_input",
                "Clipboard paste must contain UTF-8 text without NUL bytes",
            );
        }
        if data.is_empty() {
            return json!({"ok":true,"accepted":0});
        }
        let guard = match self.guard() {
            Ok(guard) => guard,
            Err(e) => return error("not_ready", &e),
        };
        use std::fmt::Write;
        let injection = if paste {
            let name = format!(
                "jarvis-stream-{}-{}",
                self.id,
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let mut escaped = String::with_capacity(data.len() * 4);
            for byte in &data {
                let _ = write!(escaped, "\\{byte:03o}");
            }
            // Octal-only double-quoted data cannot become tmux syntax. A private
            // buffer is deleted by the same synchronous paste command; -p uses
            // the actual pane's bracketed-paste mode, not a guessed initial mode.
            format!(
                "set-buffer -b {name} -- \"{escaped}\" ; paste-buffer -p -r -d -b {name} -t {}",
                self.pane
            )
        } else {
            let mut keys = String::with_capacity(data.len() * 3);
            for byte in &data {
                let _ = write!(keys, " {byte:02x}");
            }
            format!("send-keys -H -t {}{}", self.pane, keys)
        };
        let command = format!(
            "if-shell -F -t {} '{}' {{ {} }} {{ display-message -p JARVIS_INPUT_UNAVAILABLE }}",
            self.pane, guard, injection
        );
        match self.command(&command, false).await {
            Ok(blocks) if blocks.iter().any(|b| b == b"JARVIS_INPUT_UNAVAILABLE\n") => error(
                "input_unavailable",
                "The pane changed, is in copy mode, or has synchronized/disabled input",
            ),
            Ok(_) => json!({"ok":true,"accepted":data.len()}),
            Err(e) => error("input_uncertain", &e),
        }
    }

    async fn resize(&self, payload: &Value) -> Value {
        let cols = payload
            .get("cols")
            .and_then(Value::as_u64)
            .filter(|n| (20..=500).contains(n));
        let rows = payload
            .get("rows")
            .and_then(Value::as_u64)
            .filter(|n| (2..=300).contains(n));
        let (Some(cols), Some(rows)) = (cols, rows) else {
            return error(
                "invalid_size",
                "Terminal size must be 20..500 columns and 2..300 rows",
            );
        };
        let meta = match self.fresh_metadata().await {
            Ok(meta) => meta,
            Err(e) => return error("stream_closed", &e),
        };
        if meta.n(5) != 1 || meta.n(33) > 1 {
            return json!({"ok":true,"cols":meta.cols,"rows":meta.rows,"readOnlyGeometry":true});
        }
        let guard = format!(
            "#{{&&:#{{==:#{{pane_pid}},{}}},#{{&&:#{{==:#{{window_panes}},1}},#{{==:#{{session_attached}},1}}}}}}",
            meta.fields[1]
        );
        let commands = format!("if-shell -F -t {} '{}' {{ resize-window -t {} -x {} -y {} }} {{ display-message -p JARVIS_RESIZE_SKIPPED }}",
            self.pane, guard, self.pane, cols, rows);
        if let Err(e) = self.command(&commands, false).await {
            return error("resize_failed", &e);
        }
        match self.fresh_metadata().await {
            Ok(meta) => {
                json!({"ok":true,"cols":meta.cols,"rows":meta.rows,"readOnlyGeometry":meta.n(5) != 1 || meta.n(33) > 1})
            }
            Err(e) => error("stream_closed", &e),
        }
    }

    async fn history(&self) -> Value {
        let previous = self.metadata.lock().unwrap().clone();
        // Metadata and capture share one synchronous queue batch. A respawn
        // between requests cannot return a new process's history as the old one.
        // While an alternate screen is active, -a reads the saved primary grid.
        let capture = format!(
            "capture-pane -p -C -J -t {} -S -{}",
            self.pane, MAX_HISTORY_LINES
        );
        let commands = format!(
            "{} ; if-shell -F -t {} '#{{alternate_on}}' {{ {} -a }} {{ {} }}",
            self.metadata_command(),
            self.pane,
            capture,
            capture
        );
        match self.command(&commands, false).await {
            Ok(blocks) => {
                let meta = match blocks
                    .first()
                    .ok_or("Missing terminal metadata".to_string())
                    .and_then(|b| Metadata::parse(b, &self.pane))
                {
                    Ok(meta) => meta,
                    Err(e) => return error("capture_failed", &e),
                };
                if previous.as_ref().is_some_and(|old| !old.same_pane(&meta)) || meta.n(29) != 0 {
                    self.close("The terminal process changed or exited; reconnect");
                    return error(
                        "stream_closed",
                        "The terminal process changed or exited; reconnect",
                    );
                }
                match blocks.last().map(|b| unescape_capture(b)) {
                    Some(Ok(text)) => {
                        let full = meta.n(11) != 0
                            && text.iter().filter(|b| **b == b'\n').count()
                                >= MAX_HISTORY_LINES + meta.rows as usize;
                        json!({"ok":true,"text":String::from_utf8_lossy(&text),"truncated":blocks.truncated || full || meta.n(8)>MAX_HISTORY_LINES as u64})
                    }
                    _ => error("capture_failed", "Invalid terminal history response"),
                }
            }
            Err(e) => error("capture_failed", &e),
        }
    }
}

fn valid_number(value: &str) -> bool {
    !value.is_empty() && value.len() <= 20 && value.bytes().all(|b| b.is_ascii_digit())
}
fn valid_prefixed_id(value: &str, prefix: char) -> bool {
    value.strip_prefix(prefix).is_some_and(valid_number)
}
fn valid_pane(value: &str) -> bool {
    valid_prefixed_id(value, '%')
}

fn bytes(value: Option<&Value>, limit: usize) -> Result<Vec<u8>, &'static str> {
    let array = value
        .and_then(Value::as_array)
        .ok_or("data must be an array of bytes")?;
    if array.len() > limit {
        return Err("Terminal input exceeds the byte limit");
    }
    array
        .iter()
        .map(|n| {
            n.as_u64()
                .filter(|n| *n <= 255)
                .map(|n| n as u8)
                .ok_or("data must contain only integers from 0 to 255")
        })
        .collect()
}

fn tmux_binary() -> String {
    for path in [
        "/opt/homebrew/bin/tmux",
        "/usr/local/bin/tmux",
        "/usr/bin/tmux",
        "/bin/tmux",
    ] {
        if std::path::Path::new(path).is_file() {
            return path.into();
        }
    }
    "tmux".into()
}

fn unique_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| "Cannot create a terminal stream identifier")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// tmux quotes bytes, not Unicode characters. Decode before handing bytes to
/// xterm so a multibyte character split over two notifications stays intact.
fn unescape(value: &[u8]) -> Result<Vec<u8>, &'static str> {
    unescape_bytes(value, false)
}

/// Grid captures (-C) double literal backslashes, including OSC 8 hyperlink
/// terminators. Notifications and pending input (-P -C) use octal instead.
/// Keep the two formats separate so malformed live frames still fail closed.
fn unescape_capture(value: &[u8]) -> Result<Vec<u8>, &'static str> {
    unescape_bytes(value, true)
}

fn unescape_bytes(value: &[u8], capture: bool) -> Result<Vec<u8>, &'static str> {
    let mut result = Vec::with_capacity(value.len());
    let mut at = 0;
    while at < value.len() {
        if value[at] == b'\\' {
            if capture && value.get(at + 1) == Some(&b'\\') {
                result.push(b'\\');
                at += 2;
                continue;
            }
            let octal = value.get(at + 1..at + 4).ok_or("Incomplete octal escape")?;
            if !octal.iter().all(|b| (b'0'..=b'7').contains(b)) || octal[0] > b'3' {
                return Err("Invalid octal escape");
            }
            result.push((octal[0] - b'0') * 64 + (octal[1] - b'0') * 8 + octal[2] - b'0');
            at += 4;
        } else {
            result.push(value[at]);
            at += 1;
        }
    }
    Ok(result)
}

fn capture_tail(capture: &[u8], budget: usize) -> Result<(&[u8], bool), String> {
    let mut capture = capture.strip_suffix(b"\n").unwrap_or(capture);
    let mut truncated = false;
    let mut rendered_bytes = capture.len() + capture.iter().filter(|b| **b == b'\n').count();
    while rendered_bytes > budget {
        let newline = capture
            .iter()
            .position(|b| *b == b'\n')
            .ok_or("The last terminal line exceeds the snapshot limit")?;
        capture = &capture[newline + 1..];
        rendered_bytes -= newline + 2;
        truncated = true;
    }
    Ok((capture, truncated))
}

fn append_capture(output: &mut Vec<u8>, capture: &[u8]) {
    for &byte in capture {
        if byte == b'\n' {
            output.push(b'\r');
        }
        output.push(byte);
    }
}

fn initial_screen(
    meta: &Metadata,
    capture: &[u8],
    viewport: &[u8],
    primary: &[u8],
    pending: &[u8],
) -> Result<(Vec<u8>, bool), String> {
    let pending = pending.strip_suffix(b"\n").unwrap_or(pending);
    let pending = unescape(pending).map_err(str::to_owned)?;
    if pending.len() > MAX_INITIAL / 2 {
        return Err("The terminal has an oversized incomplete escape sequence".into());
    }
    let mut output = b"\x1bc".to_vec();
    let wrap_pending = meta.n(9) >= meta.cols && meta.n(19) != 0;
    let wrap_row = if wrap_pending {
        let line = viewport
            .split(|b| *b == b'\n')
            .nth(meta.n(10) as usize)
            .ok_or("Missing pending-wrap row")?;
        unescape_capture(line).map_err(str::to_owned)?
    } else {
        vec![]
    };
    let mut truncated = false;
    if meta.n(11) != 0 {
        let primary = unescape_capture(primary).map_err(str::to_owned)?;
        let (primary, limited) = capture_tail(&primary, MAX_INITIAL / 2)?;
        truncated |= limited;
        append_capture(&mut output, primary);
        output.extend_from_slice(
            format!("\x1b[{};{}H\x1b[?1049h", meta.n(32) + 1, meta.n(31) + 1).as_bytes(),
        );
    }
    let capture = unescape_capture(capture).map_err(str::to_owned)?;
    let budget = MAX_INITIAL
        .checked_sub(output.len() + pending.len() + wrap_row.len() + 4096)
        .ok_or("The current terminal viewport exceeds the snapshot limit")?;
    let (capture, limited) = capture_tail(&capture, budget)?;
    truncated |= limited;
    append_capture(&mut output, capture);
    let mut state = format!("\x1b[0m\x1b[{};{}r", meta.n(25) + 1, meta.n(26) + 1);
    use std::fmt::Write;
    for (index, mode) in [
        (12, 25),
        (15, 1),
        (18, 6),
        (19, 7),
        (20, 1000),
        (21, 1002),
        (22, 1003),
        (23, 1006),
        (24, 1005),
    ] {
        let _ = write!(
            state,
            "\x1b[?{}{}",
            mode,
            if meta.n(index) != 0 { 'h' } else { 'l' }
        );
    }
    let _ = write!(state, "\x1b[4{}", if meta.n(17) != 0 { 'h' } else { 'l' });
    state.push_str(if meta.n(16) != 0 { "\x1b=" } else { "\x1b>" });
    let row = if meta.n(18) != 0 {
        meta.n(10).saturating_sub(meta.n(25))
    } else {
        meta.n(10)
    };
    let _ = write!(
        state,
        "\x1b[{};{}H",
        row + 1,
        if wrap_pending {
            1
        } else {
            meta.n(9).min(meta.cols.saturating_sub(1)) + 1
        }
    );
    output.extend_from_slice(state.as_bytes());
    if wrap_pending {
        // CUP to the right margin clears VT's pending-wrap bit. Repaint that
        // exact row last instead: its last cell naturally restores the bit, so
        // the next printable byte wraps instead of overwriting the final cell.
        output.extend_from_slice(&wrap_row);
    }
    // tmux's parser may hold half an escape sequence at the snapshot boundary.
    // Append it last so the next raw bytes complete the same sequence in xterm.
    output.extend_from_slice(&pending);
    Ok((output, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_panes_and_byte_validation() {
        for pane in ["%0", "%123456"] {
            assert!(valid_pane(pane));
        }
        for pane in ["0", "%", "%1;kill-server", "%1\n", "session:0.0", "%١"] {
            assert!(!valid_pane(pane));
        }
        assert_eq!(
            bytes(Some(&json!([0, 27, 255])), MAX_INPUT).unwrap(),
            [0, 27, 255]
        );
        for invalid in [json!([-1]), json!([256]), json!([1.5]), json!("bytes")] {
            assert!(bytes(Some(&invalid), MAX_INPUT).is_err());
        }
    }

    #[test]
    fn octal_output_preserves_escape_and_split_utf8_bytes() {
        assert_eq!(
            unescape(br"a\000\033[31m\134\015\012").unwrap(),
            b"a\0\x1b[31m\\\r\n"
        );
        let mut split = unescape(&[0xe2, 0x82]).unwrap();
        split.extend(unescape(&[0xac]).unwrap());
        assert_eq!(String::from_utf8(split).unwrap(), "€");
        for invalid in [br"\".as_slice(), br"\12", br"\999", br"\400"] {
            assert!(unescape(invalid).is_err());
        }
    }

    #[test]
    fn grid_capture_decodes_hyperlinks_and_literal_backslashes() {
        let captured = br"\033]8;;https://example.com\033\\link\033]8;;\033\\ C:\\new\\033";
        assert_eq!(
            unescape_capture(captured).unwrap(),
            b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ C:\\new\\033"
        );
        assert_eq!(unescape_capture(br"trailing\\").unwrap(), b"trailing\\");
        // %output and -P -C never encode a backslash by doubling it.
        assert!(unescape(br"\\").is_err());
        assert_eq!(unescape(br"\033]8;;url\134").unwrap(), b"\x1b]8;;url\\");
        for invalid in [br"\".as_slice(), br"\12", br"\999", br"\400"] {
            assert!(unescape_capture(invalid).is_err());
        }
    }

    #[test]
    fn ring_has_explicit_gaps_and_bounded_replay_pages() {
        let mut ring = Ring::default();
        ring.append(b"already captured".to_vec());
        assert_eq!(ring.cursor, 0);
        ring.ready = true;
        ring.append(vec![b'x'; RING_BYTES + 32768]);
        assert!(ring.bytes <= RING_BYTES);
        assert_eq!(ring.slice(0)["gap"], true);
        let oldest = ring.chunks.front().unwrap().seq;
        let page = ring.slice(oldest - 1);
        assert_eq!(page["gap"], false);
        let count: usize = page["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["data"].as_array().unwrap().len())
            .sum();
        assert!(count <= POLL_BYTES);
        assert!(page["cursor"].as_u64().unwrap() < ring.cursor);
        assert_eq!(ring.slice(ring.cursor + 10)["gap"], true);
    }

    #[test]
    fn closed_stream_drains_every_buffered_page_before_reporting_closed() {
        let mut ring = Ring {
            ready: true,
            ..Ring::default()
        };
        let total = POLL_BYTES * 2 + 17;
        ring.append(vec![b'x'; total]);
        ring.closed = Some("process exited".into());
        let mut cursor = 0;
        let mut received = 0;
        for page in 0..3 {
            let result = ring.slice(cursor);
            cursor = result["cursor"].as_u64().unwrap();
            received += result["chunks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["data"].as_array().unwrap().len())
                .sum::<usize>();
            assert_eq!(result["closed"], page == 2);
            if page < 2 {
                assert!(result["error"].is_null());
            }
        }
        assert_eq!(received, total);
        assert_eq!(cursor, ring.cursor);
    }

    #[test]
    fn captures_keep_complete_newest_lines_and_report_truncation() {
        let mut block = Block {
            header: vec![],
            lines: VecDeque::new(),
            bytes: 0,
            truncated: false,
            limit: 12,
        };
        block.push(b"oldest");
        block.push(b"middle");
        block.push(b"new");
        assert!(block.truncated);
        assert_eq!(
            block.lines.into_iter().flatten().collect::<Vec<_>>(),
            b"middle\nnew\n"
        );
    }

    fn metadata_fixture() -> Metadata {
        let mut fields = vec!["0".to_string(); 34];
        for (i, value) in [
            (0, "%0"),
            (1, "123"),
            (2, "456"),
            (3, "$0"),
            (4, "@0"),
            (5, "1"),
            (6, "4"),
            (7, "2"),
            (12, "1"),
            (19, "1"),
            (26, "1"),
            (33, "1"),
        ] {
            fields[i] = value.into();
        }
        Metadata::parse(fields.join("|").as_bytes(), "%0").unwrap()
    }

    #[test]
    fn initial_restores_pending_wrap_without_a_later_cursor_move() {
        let mut meta = metadata_fixture();
        meta.fields[9] = "4".into();
        let (initial, truncated) =
            initial_screen(&meta, b"ABCD\n\n", b"ABCD\n    \n", b"\n", b"\n").unwrap();
        assert!(!truncated);
        assert!(initial.ends_with(b"\x1b[1;1HABCD"));
    }

    #[test]
    fn initial_restores_saved_primary_before_entering_alternate_screen() {
        let mut meta = metadata_fixture();
        meta.fields[11] = "1".into();
        meta.fields[31] = "2".into();
        meta.fields[32] = "1".into();
        let (initial, truncated) =
            initial_screen(&meta, b"ALT\n\n", b"ALT \n    \n", b"BASE\n\n", b"\n").unwrap();
        assert!(!truncated);
        let initial = String::from_utf8(initial).unwrap();
        assert!(initial.find("BASE").unwrap() < initial.find("\x1b[?1049h").unwrap());
        assert!(initial.contains("\x1b[2;3H\x1b[?1049hALT"));
    }

    #[tokio::test]
    async fn initial_barrier_and_protocol_blocks_do_not_confuse_terminal_text() {
        let stream = Stream::new("test".into(), "%4".into());
        let (done, result) = oneshot::channel();
        *stream.pending.lock().unwrap() = Some(Pending {
            start: b"START\n".to_vec(),
            end: b"END\n".to_vec(),
            started: false,
            initial: true,
            bytes: 0,
            blocks: vec![],
            truncated: false,
            done,
        });
        let mut block = None;
        for line in [
            b"%output %4 old".as_slice(),
            b"%begin 1 1 1",
            b"START",
            b"%end 1 1 1",
            b"%begin 1 2 1",
            b"%output %4 literal text",
            b"%end 8 9 1",
            b"%end 1 2 1",
            b"%begin 1 3 1",
            b"END",
            b"%end 1 3 1",
            b"%output %5 unrelated",
            br"%output %4 new\033[0m",
        ] {
            stream.line(line, &mut block).unwrap();
        }
        let capture = result.await.unwrap().unwrap();
        assert_eq!(
            capture.blocks,
            [b"%output %4 literal text\n%end 8 9 1\n".to_vec()]
        );
        let ring = stream.ring.lock().unwrap();
        assert_eq!(ring.chunks.len(), 1);
        assert_eq!(ring.chunks[0].data, b"new\x1b[0m");
    }

    #[tokio::test]
    async fn expiry_closes_client_state_and_removes_stream() {
        let hub = Hub::new("jarvis-stream-test-no-process");
        let stream = Stream::new("test".into(), "%0".into());
        hub.streams
            .lock()
            .unwrap()
            .insert("test".into(), stream.clone());
        hub.reap(Instant::now() + IDLE_TTL + Duration::from_secs(1));
        assert!(stream.closed().is_some());
        assert!(hub.streams.lock().unwrap().is_empty());
    }

    struct Server {
        name: String,
    }
    impl Server {
        fn new() -> Self {
            Self {
                name: format!(
                    "jarvis-stream-test-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ),
            }
        }
        fn run(&self, args: &[&str]) -> String {
            let out = std::process::Command::new(tmux_binary())
                .args(["-L", &self.name])
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success() && out.stderr.is_empty(),
                "tmux test command failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = std::process::Command::new(tmux_binary())
                .args(["-L", &self.name, "kill-server"])
                .output();
        }
    }

    async fn output(hub: &Hub, id: &str, cursor: &mut u64) -> Vec<u8> {
        let response = hub
            .dispatch("poll", &json!({"streamId":id,"cursor":*cursor}))
            .await;
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(response["gap"], false, "{response}");
        assert_eq!(response["closed"], false, "{response}");
        *cursor = response["cursor"].as_u64().unwrap();
        response["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|chunk| bytes(Some(&chunk["data"]), MAX_REPLY).unwrap())
            .collect()
    }

    #[tokio::test]
    #[ignore = "spawns only an isolated temporary tmux server; run with --ignored"]
    async fn real_tmux_capture_hyperlinks_and_backslashes() {
        let server = Server::new();
        server.run(&[
            "-f", "/dev/null", "new-session", "-d", "-s", "test", "-x", "100", "-y", "24",
            "python3", "-u", "-c",
            "import os,tty;tty.setraw(0);os.write(1,b'\\x1b]8;;https://example.com\\x1b\\\\LINK\\x1b]8;;\\x1b\\\\ C:\\\\new\\\\033\\r\\nREADY');os.read(0,1)",
        ]);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let hub = Hub::new(&server.name);
        let opened = hub.dispatch("open", &json!({"pane":"%0"})).await;
        assert_eq!(opened["ok"], true, "{opened}");
        let initial = bytes(Some(&opened["initial"]), MAX_REPLY).unwrap();
        let text = String::from_utf8_lossy(&initial);
        assert!(text.contains("LINK"), "{text:?}");
        assert!(text.contains(r"C:\new\033"), "{text:?}");
        assert!(text.contains("\x1b]8;"), "OSC 8 fixture was not retained: {text:?}");
        let id = opened["streamId"].as_str().unwrap();
        let history = hub.dispatch("history", &json!({"streamId":id})).await;
        assert_eq!(history["ok"], true, "{history}");
        assert!(history["text"].as_str().unwrap().contains(r"C:\new\033"));
        assert_eq!(hub.dispatch("close", &json!({"streamId":id})).await["ok"], true);
        assert_eq!(server.run(&["display-message", "-p", "-t", "%0", "#{pane_dead}"]).trim(), "0");
    }

    #[tokio::test]
    #[ignore = "spawns only an isolated temporary tmux server; run with --ignored"]
    async fn real_tmux_stream_bytes_paste_geometry_identity_and_close() {
        let server = Server::new();
        server.run(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "test",
            "-x",
            "80",
            "-y",
            "24",
            "sh",
            "-c",
            r"stty raw -echo; printf '\033[?2004hBEFORE'; exec cat",
        ]);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let hub = Hub::new(&server.name);
        let opened = hub.dispatch("open", &json!({"pane":"%0"})).await;
        assert_eq!(opened["ok"], true, "{opened}");
        let initial = bytes(Some(&opened["initial"]), MAX_REPLY).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&initial).matches("BEFORE").count(),
            1
        );
        let id = opened["streamId"].as_str().unwrap();
        let mut cursor = 0;
        let data = b"hello\0\x1b[31m\xe2\x82\xac\\\r\n";
        let sent = hub
            .dispatch("input", &json!({"streamId":id,"data":data.as_slice()}))
            .await;
        assert_eq!(sent["ok"], true, "{sent}");
        let mut received = vec![];
        while received.len() < data.len() {
            received.extend(output(&hub, id, &mut cursor).await);
        }
        assert_eq!(received, data);
        let text = "first\nsecond ' \\\nтретий";
        let pasted = hub
            .dispatch(
                "input",
                &json!({"streamId":id,"paste":true,"data":text.as_bytes()}),
            )
            .await;
        assert_eq!(pasted["ok"], true, "{pasted}");
        let expected = format!("\x1b[200~{text}\x1b[201~").into_bytes();
        let mut received = vec![];
        while received.len() < expected.len() {
            received.extend(output(&hub, id, &mut cursor).await);
        }
        assert_eq!(received, expected);
        let large = "paste line\n".repeat(65536);
        let pasted = hub
            .dispatch(
                "input",
                &json!({"streamId":id,"paste":true,"data":large.as_bytes()}),
            )
            .await;
        assert_eq!(pasted["ok"], true, "{pasted}");
        let expected = format!("\x1b[200~{large}\x1b[201~").into_bytes();
        let mut received = vec![];
        while received.len() < expected.len() {
            received.extend(output(&hub, id, &mut cursor).await);
        }
        assert_eq!(received, expected);
        let resized = hub
            .dispatch("resize", &json!({"streamId":id,"cols":100,"rows":30}))
            .await;
        assert_eq!(resized["cols"], 100, "{resized}");
        assert_eq!(resized["rows"], 30, "{resized}");
        server.run(&[
            "split-window",
            "-d",
            "-t",
            "%0",
            "sh",
            "-c",
            "stty raw -echo; exec cat",
        ]);
        let before = server.run(&[
            "display-message",
            "-p",
            "-t",
            "%1",
            "#{pane_width}|#{pane_height}",
        ]);
        let split = hub
            .dispatch("resize", &json!({"streamId":id,"cols":120,"rows":40}))
            .await;
        assert_eq!(split["readOnlyGeometry"], true, "{split}");
        assert_eq!(
            server.run(&[
                "display-message",
                "-p",
                "-t",
                "%1",
                "#{pane_width}|#{pane_height}"
            ]),
            before
        );
        server.run(&["set-window-option", "-t", "%0", "synchronize-panes", "on"]);
        let denied = hub
            .dispatch(
                "input",
                &json!({"streamId":id,"data":b"DO_NOT_ROUTE".as_slice()}),
            )
            .await;
        assert_eq!(denied["code"], "input_unavailable", "{denied}");
        assert!(!server
            .run(&["capture-pane", "-p", "-t", "%1"])
            .contains("DO_NOT_ROUTE"));
        server.run(&["set-window-option", "-t", "%0", "synchronize-panes", "off"]);
        server.run(&[
            "respawn-pane",
            "-k",
            "-t",
            "%0",
            "sh",
            "-c",
            "stty raw -echo; exec cat",
        ]);
        let denied = hub
            .dispatch(
                "input",
                &json!({"streamId":id,"data":b"STALE_INPUT".as_slice()}),
            )
            .await;
        assert_ne!(denied["ok"], true, "{denied}");
        assert!(!server
            .run(&["capture-pane", "-p", "-t", "%0"])
            .contains("STALE_INPUT"));
        assert_eq!(
            hub.dispatch("close", &json!({"streamId":id})).await["ok"],
            true
        );
        assert_eq!(
            server
                .run(&["display-message", "-p", "-t", "%0", "#{pane_id}"])
                .trim(),
            "%0"
        );
    }

    #[tokio::test]
    #[ignore = "spawns only an isolated temporary tmux server; run with --ignored"]
    async fn real_tmux_initial_barrier_and_large_history() {
        let server = Server::new();
        server.run(&["-f", "/dev/null", "new-session", "-d", "-s", "test", "cat"]);
        server.run(&["set-option", "-g", "history-limit", "100000"]);
        server.run(&["new-window","-d","-t","test","python3","-u","-c",
            "import os,time,tty;tty.setraw(0);time.sleep(.1)\nfor i in range(6000):\n os.write(1,('ROW-%06d\\r\\n'%i).encode())\n if i%100==0:time.sleep(.003)\nos.write(1,b'FINISHED\\r\\n')\nwhile True:\n b=os.read(0,4096)\n if not b:break\n os.write(1,b)"]);
        let hub = Hub::new(&server.name);
        tokio::time::sleep(Duration::from_millis(160)).await;
        let opened = hub.dispatch("open", &json!({"pane":"%1"})).await;
        assert_eq!(opened["ok"], true, "{opened}");
        let id = opened["streamId"].as_str().unwrap();
        let mut all = bytes(Some(&opened["initial"]), MAX_REPLY).unwrap();
        let mut cursor = 0;
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if String::from_utf8_lossy(&all).contains("FINISHED") {
                break;
            }
            all.extend(output(&hub, id, &mut cursor).await);
        }
        let all = String::from_utf8_lossy(&all);
        assert!(all.contains("FINISHED"));
        let numbers: Vec<_> = all
            .split("ROW-")
            .skip(1)
            .filter_map(|part| part.get(..6).and_then(|n| n.parse::<usize>().ok()))
            .collect();
        assert_eq!(
            numbers,
            (0..6000).collect::<Vec<_>>(),
            "snapshot/stream barrier must neither lose nor duplicate lines"
        );
        hub.dispatch("close", &json!({"streamId":id})).await;
        let default = hub.dispatch("open", &json!({"pane":"%1"})).await;
        assert_eq!(default["historyTruncated"], true, "{default}");
        let full = hub
            .dispatch("open", &json!({"pane":"%1","historyLines":100000}))
            .await;
        assert_eq!(full["ok"], true, "{full}");
        assert_eq!(full["historyTruncated"], false);
        let full_text =
            String::from_utf8(bytes(Some(&full["initial"]), MAX_REPLY).unwrap()).unwrap();
        assert!(full_text.contains("ROW-000000"));
        assert!(full_text.contains("ROW-005999"));
        let history = hub
            .dispatch("history", &json!({"streamId":full["streamId"]}))
            .await;
        assert_eq!(history["truncated"], false, "{history}");
        assert!(history["text"].as_str().unwrap().contains("ROW-000000"));
        hub.dispatch("close", &json!({"streamId":default["streamId"]}))
            .await;
        hub.dispatch("close", &json!({"streamId":full["streamId"]}))
            .await;
    }
}
