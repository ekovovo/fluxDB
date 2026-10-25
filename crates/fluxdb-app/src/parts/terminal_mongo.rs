// MongoDB 命令行终端：库内执行 Extended JSON 命令的 REPL。
//
// mongosh 不一定安装在本机，且它需要真实 PTY 与会话协议；这里改为「自有行编辑 + 连接器执行」：
//   - `MongoReplTransport`：一个后台线程接收按键字节、维护输入行与历史、把命令交给连接器执行，
//     并把回显 / 结果 / prompt 作为终端字节流回传，交给桌面端既有 TermGrid 渲染。
//   - `MongoCliAdapter`：会话命名 / prompt / 状态文案（供终端 surface 展示）。
//   - `MongoCliCompletion`：命令名补全（Tab）与危险命令识别（dropDatabase / shutdown 等）。
//
// 边界：不经过 PTY，密码不会出现在任何子进程 argv；命令文本与结果只在本地终端内呈现。

// 本文件随 `include!` 汇入 crate 根作用域，与 `terminal_redis.rs` 共享命名空间；
// 为避免 `use` 重名冲突，统一用模块别名限定终端核心类型。
use fluxdb_core::terminal as mterm;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

const MONGO_TERM_COLS: u16 = 120;
const MONGO_TERM_ROWS: u16 = 32;

/// 需要二次确认的危险命令（键名，判断 JSON 命令对象的第一个键）。
const DANGEROUS_MONGO_COMMANDS: &[&str] = &["dropDatabase", "drop", "delete", "shutdown", "setParameter"];

/// 可补全 / 可提示的命令键。桌面端只做键名补全，不猜测参数结构。
const MONGO_COMMAND_KEYS: &[&str] = &[
    "ping",
    "buildInfo",
    "serverStatus",
    "dbStats",
    "collStats",
    "listCollections",
    "listIndexes",
    "count",
    "distinct",
    "find",
    "aggregate",
    "insert",
    "update",
    "delete",
    "dropDatabase",
    "drop",
    "createIndexes",
];

/// MongoDB REPL 传输：UI 线程按键字节 → 后台线程 → 输出字节流。
pub struct MongoReplTransport {
    input: Sender<Vec<u8>>,
    output: Receiver<Vec<u8>>,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl MongoReplTransport {
    /// 启动 REPL 线程。`connection_id` 只用于日志与错误上下文，命令实际走连接配置。
    pub fn new(config: fluxdb_core::ConnectionConfig, database: String) -> Self {
        let (input_tx, input_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (output_tx, output_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let worker_alive = alive.clone();
        let connection_id = config.id;
        let worker = std::thread::Builder::new()
            .name("fluxdb-mongo-cli".to_string())
            .spawn(move || {
                let mut repl = MongoRepl::new(config, database);
                repl.greet(&output_tx);
                while worker_alive.load(std::sync::atomic::Ordering::Relaxed) {
                    let bytes = match input_rx.recv() {
                        Ok(bytes) => bytes,
                        Err(_) => break,
                    };
                    repl.feed(&bytes, &output_tx);
                }
                tracing::debug!(connection_id = connection_id.0, "MongoDB 命令行线程退出");
            })
            .ok();
        Self {
            input: input_tx,
            output: output_rx,
            alive,
            worker,
        }
    }

    pub fn write(&mut self, bytes: &[u8]) {
        let _ = self.input.send(bytes.to_vec());
    }

    /// 非阻塞取一段输出；无输出返回 `None`。
    pub fn try_recv(&self) -> Option<Vec<u8>> {
        match self.output.try_recv() {
            Ok(bytes) => Some(bytes),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }

    pub fn close(&mut self) {
        self.alive.store(false, std::sync::atomic::Ordering::Relaxed);
        // 关闭输入通道让阻塞的 recv 立即返回；工作线程随后自然退出。
        let (dead_tx, _dead_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let _ = std::mem::replace(&mut self.input, dead_tx);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for MongoReplTransport {
    fn drop(&mut self) {
        self.alive.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

/// REPL 本体：输入行、历史与连接器执行。
struct MongoRepl {
    config: fluxdb_core::ConnectionConfig,
    database: String,
    line: String,
    /// 历史命令（旧 → 新）；`history_cursor` 为回看位置（`len` = 当前输入行）。
    history: Vec<String>,
    history_cursor: usize,
    /// 跨 chunk 未完整的 UTF-8 尾部。
    pending: Vec<u8>,
    /// 转义序列中间态（方向键等）。
    escape: Vec<u8>,
}

impl MongoRepl {
    fn new(config: fluxdb_core::ConnectionConfig, database: String) -> Self {
        Self {
            config,
            database,
            line: String::new(),
            history: Vec::new(),
            history_cursor: 0,
            pending: Vec::new(),
            escape: Vec::new(),
        }
    }

    fn prompt(&self) -> String {
        format!("{}> ", self.database)
    }

    fn greet(&mut self, out: &Sender<Vec<u8>>) {
        let banner = format!(
            "MongoDB 命令行已连接 {}。输入 Extended JSON 命令（如 {{\"ping\": 1}}），Ctrl+D 退出。\r\n",
            self.database
        );
        let _ = out.send(banner.into_bytes());
        let prompt = self.prompt();
        let _ = out.send(prompt.into_bytes());
    }

    /// 处理一段按键字节。
    fn feed(&mut self, bytes: &[u8], out: &Sender<Vec<u8>>) {
        let mut echo: Vec<u8> = Vec::new();
        let mut commands: Vec<String> = Vec::new();
        for byte in bytes {
            match byte {
                b'\r' | b'\n' => {
                    if self.escape.is_empty() {
                        echo.extend_from_slice(b"\r\n");
                        commands.push(std::mem::take(&mut self.line));
                    } else {
                        self.escape.clear();
                    }
                }
                // 退格：按整字符删除（宽字符占 2 列，退格需移动 2 列再抹 2 列）。
                0x7f | 0x08 => {
                    if self.escape.is_empty() && let Some(ch) = self.line.pop() {
                        let width = fluxdb_core::terminal::char_cell_width(ch).max(1);
                        for _ in 0..width {
                            echo.extend_from_slice(b"\x08 \x08");
                        }
                    }
                }
                // Ctrl+C / Ctrl+U：清空当前输入行。
                0x03 | 0x15 => {
                    self.escape.clear();
                    self.line.clear();
                    echo.extend_from_slice(b"\r");
                    echo.extend_from_slice(self.prompt().as_bytes());
                    echo.extend_from_slice(b"\x1b[K");
                }
                // Ctrl+L：清屏并重画当前行。
                0x0c => {
                    self.escape.clear();
                    echo.extend_from_slice(b"\x1b[2J\x1b[H");
                    echo.extend_from_slice(self.prompt().as_bytes());
                    echo.extend_from_slice(self.line.as_bytes());
                }
                // Ctrl+D：结束会话（与 shell 一致）。
                0x04 => {
                    echo.extend_from_slice(b"\r\n");
                    let _ = out.send(echo);
                    let _ = out.send(Vec::new());
                    return;
                }
                0x1b => {
                    self.escape.clear();
                    self.escape.push(*byte);
                }
                _ if !self.escape.is_empty() => {
                    self.escape.push(*byte);
                    if self.escape.starts_with(b"\x1b[") && self.escape.len() == 3 {
                        let sequence = std::mem::take(&mut self.escape);
                        match sequence.as_slice() {
                            b"\x1b[A" => self.history_previous(&mut echo),
                            b"\x1b[B" => self.history_next(&mut echo),
                            _ => {}
                        }
                    } else if !self.escape.starts_with(b"\x1b[") || self.escape.len() > 8 {
                        self.escape.clear();
                    }
                }
                b'\t' => {}
                _ => {
                    self.pending.push(*byte);
                    // 只有完整 UTF-8 前缀才消费；不完整尾巴留到下一段。
                    let valid = match std::str::from_utf8(&self.pending) {
                        Ok(text) => Some(text.to_string()),
                        Err(error) if error.error_len().is_none() => None,
                        Err(_) => {
                            // 非法字节（非 UTF-8）：丢弃，避免污染输入行。
                            self.pending.clear();
                            None
                        }
                    };
                    if let Some(text) = valid {
                        self.pending.clear();
                        for ch in text.chars() {
                            if ch.is_control() {
                                continue;
                            }
                            self.line.push(ch);
                            let mut buffer = [0u8; 4];
                            echo.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
                        }
                    } else if self.pending.len() > 4 {
                        self.pending.clear();
                    }
                }
            }
        }
        if !echo.is_empty() {
            let _ = out.send(echo);
        }
        for command in commands {
            let rendered = self.execute(&command);
            let _ = out.send(rendered);
        }
    }

    /// 执行一条命令并返回终端要回显的字节（结果 / 错误 + 新 prompt）。
    fn execute(&mut self, command: &str) -> Vec<u8> {
        let command = command.trim().to_string();
        let mut out = String::new();
        if command.is_empty() {
            out.push_str(&self.prompt());
            return out.into_bytes();
        }
        if command.eq_ignore_ascii_case("exit") || command.eq_ignore_ascii_case("quit") {
            out.push_str("已退出 MongoDB 命令行。\r\n");
            return out.into_bytes();
        }
        if self.history.last() != Some(&command) {
            self.history.push(command.clone());
        }
        self.history_cursor = self.history.len();
        match fluxdb_connectors::connector_for(&self.config)
            .and_then(|connector| {
                connector.mongo_run_command(self.config.id, &self.database, &command)
            }) {
            Ok(result) => {
                out.push_str(&result);
                if !result.ends_with('\n') {
                    out.push_str("\r\n");
                }
            }
            Err(error) => {
                out.push_str(&format!("error: {}\r\n", error.message));
            }
        }
        out.push_str(&self.prompt());
        out.into_bytes()
    }

    fn history_previous(&mut self, echo: &mut Vec<u8>) {
        if self.history.is_empty() || self.history_cursor == 0 {
            return;
        }
        self.history_cursor -= 1;
        self.line = self.history[self.history_cursor].clone();
        self.redraw(echo);
    }

    fn history_next(&mut self, echo: &mut Vec<u8>) {
        if self.history_cursor >= self.history.len() {
            return;
        }
        self.history_cursor += 1;
        self.line = self
            .history
            .get(self.history_cursor)
            .cloned()
            .unwrap_or_default();
        self.redraw(echo);
    }

    /// 回车重画当前输入行（历史翻页用）。
    fn redraw(&self, echo: &mut Vec<u8>) {
        echo.extend_from_slice(b"\r");
        echo.extend_from_slice(self.prompt().as_bytes());
        echo.extend_from_slice(self.line.as_bytes());
        echo.extend_from_slice(b"\x1b[K");
    }
}

/// MongoDB 命令行会话 adapter：命名 / prompt / 状态文案（spawn 参数不会被使用）。
pub struct MongoCliAdapter {
    connection_id: fluxdb_core::ConnectionId,
    database: String,
    connection_name: String,
}

impl MongoCliAdapter {
    pub fn new(config: &fluxdb_core::ConnectionConfig, database: String) -> Self {
        Self {
            connection_id: config.id,
            database,
            connection_name: config.name.clone(),
        }
    }
}

impl mterm::TerminalSessionAdapter for MongoCliAdapter {
    fn kind(&self) -> mterm::TerminalSessionKind {
        mterm::TerminalSessionKind::MongoCli
    }

    fn session_key(&self) -> mterm::TerminalSessionKey {
        mterm::TerminalSessionKey::Mongo {
            connection_id: self.connection_id.0,
            database: self.database.clone(),
        }
    }

    fn title(&self) -> String {
        format!("MongoDB CLI - {}", self.database)
    }

    fn status_text(&self, state: mterm::TerminalSessionState) -> String {
        match state {
            mterm::TerminalSessionState::Connecting => "连接中…".to_string(),
            mterm::TerminalSessionState::PtyRunning => "会话已建立".to_string(),
            mterm::TerminalSessionState::Ready => format!("{} · 就绪", self.database),
            mterm::TerminalSessionState::Busy => "执行中…".to_string(),
            mterm::TerminalSessionState::AwaitingConfirmation => "等待确认".to_string(),
            mterm::TerminalSessionState::Exited => "已退出".to_string(),
            mterm::TerminalSessionState::Failed => "启动失败".to_string(),
        }
    }

    /// 不拉起子进程：桌面端按 `kind()` 创建 `MongoReplTransport`；这里的参数只是占位，
    /// 保证 trait 契约（绝不执行，cmd 不存在）。
    fn spawn(&self) -> mterm::TerminalSpawnSpec {
        mterm::TerminalSpawnSpec {
            program: String::new(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            cols: MONGO_TERM_COLS,
            rows: MONGO_TERM_ROWS,
        }
    }

    /// REPL 自己负责回显与 prompt，这里不改状态（状态由桌面端按输出置位）。
    fn on_output(&mut self, _bytes: &[u8], _state: &mut mterm::TerminalSessionState) {}

    fn prompt(&self) -> String {
        format!("{}> ", self.database)
    }

    /// Ctrl+C 不下发任何字节：REPL 侧把 Ctrl+C 当作「清空当前输入行」处理，会话保持存活。
    fn ctrl_c_bytes(&self, _state: mterm::TerminalSessionState) -> &'static [u8] {
        b""
    }

    fn meta(&self) -> std::collections::BTreeMap<String, String> {
        let mut meta = std::collections::BTreeMap::new();
        meta.insert("connection_id".to_string(), self.connection_id.0.to_string());
        meta.insert("database".to_string(), self.database.clone());
        meta.insert("profile".to_string(), self.connection_name.clone());
        meta
    }
}

/// MongoDB 命令行补全 / 危险命令识别。
pub struct MongoCliCompletion;

impl MongoCliCompletion {
    fn is_dangerous_impl(input: &str) -> bool {
        DANGEROUS_MONGO_COMMANDS
            .iter()
            .any(|command| input.contains(&format!("\"{command}\"")))
    }

    /// 当前 token 的替换区间：输入行末尾连续标识符字符。
    fn token_range(input: &str) -> (usize, usize) {
        let end = input.len();
        let start = input
            .char_indices()
            .rev()
            .take_while(|(_, ch)| ch.is_ascii_alphanumeric() || *ch == '_')
            .last()
            .map(|(index, _)| index)
            .unwrap_or(end);
        (start, end)
    }
}

impl mterm::TerminalCompletionAdapter for MongoCliCompletion {
    fn supports_completion(&self) -> bool {
        true
    }

    fn complete(&self, ctx: &mterm::TerminalCommandContext) -> Vec<mterm::TerminalCompletionItem> {
        let (start, end) = Self::token_range(&ctx.input);
        let token = &ctx.input[start..end];
        MONGO_COMMAND_KEYS
            .iter()
            .filter(|key| key.starts_with(token))
            .map(|key| mterm::TerminalCompletionItem {
                label: (*key).to_string(),
                insert_text: (*key).to_string(),
                replace_start: start,
                replace_end: end,
                detail: Some(format!("{key}")),
                kind: mterm::TerminalCompletionKind::Command,
                priority: mterm::TerminalCompletionPriority::Normal,
            })
            .collect()
    }

    fn apply(&self, item: &mterm::TerminalCompletionItem) -> mterm::CompletionApplyEffect {
        mterm::CompletionApplyEffect::Replace {
            start: item.replace_start,
            end: item.replace_end,
            text: item.insert_text.clone(),
        }
    }

    fn is_dangerous(&self, input: &str) -> bool {
        Self::is_dangerous_impl(input)
    }
}

#[cfg(test)]
mod mongo_repl_input_tests {
    use super::*;

    /// 构造一个不连库的 REPL（配置指向不可达地址；本组用例只测输入处理）。
    fn repl() -> (MongoRepl, Sender<Vec<u8>>, Receiver<Vec<u8>>) {
        let config = fluxdb_core::ConnectionConfig {
            id: fluxdb_core::ConnectionId(1),
            name: "unit".into(),
            kind: fluxdb_core::DatabaseKind::MongoDb,
            endpoint: fluxdb_core::Endpoint::Tcp {
                host: "127.0.0.1".into(),
                port: 1,
                database: Some("demo".into()),
            },
            options: std::collections::BTreeMap::new(),
            redis_profile: None,
            mysql_profile: None,
            postgres_profile: None,
            dameng_profile: None,
            sqlserver_profile: None,
            mongo_profile: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        (MongoRepl::new(config, "demo".into()), tx, rx)
    }

    fn drain(rx: &Receiver<Vec<u8>>) -> Vec<u8> {
        let mut bytes = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            bytes.extend_from_slice(&chunk);
        }
        bytes
    }

    #[test]
    fn banner_and_prompt_use_the_target_database() {
        let (mut repl, tx, rx) = repl();
        repl.greet(&tx);
        let output = String::from_utf8_lossy(&drain(&rx)).to_string();
        assert!(output.contains("demo"), "{output}");
        assert!(output.ends_with("demo> "), "{output}");
    }

    #[test]
    fn backspace_erases_one_cell_per_character_width() {
        let (mut repl, tx, rx) = repl();
        repl.feed("ab".as_bytes(), &tx);
        let _ = drain(&rx);
        // 半角字符退格一次；宽字符（中文）占两列，需要抹两格。
        repl.feed(b"\x7f", &tx);
        assert_eq!(drain(&rx), b"\x08 \x08".to_vec());
        assert_eq!(repl.line, "a");

        repl.feed("中".as_bytes(), &tx);
        let _ = drain(&rx);
        repl.feed(b"\x7f", &tx);
        assert_eq!(drain(&rx), b"\x08 \x08\x08 \x08".to_vec());
        assert_eq!(repl.line, "a");
    }

    #[test]
    fn ctrl_c_and_ctrl_u_clear_the_current_line_without_executing() {
        let (mut repl, tx, rx) = repl();
        repl.feed("dropDatabase".as_bytes(), &tx);
        let _ = drain(&rx);
        repl.feed(b"\x03", &tx);
        let output = drain(&rx);
        assert_eq!(output, b"\rdemo> \x1b[K".to_vec());
        assert!(repl.line.is_empty());
        assert!(repl.history.is_empty(), "清行不应产生历史记录");
    }

    #[test]
    fn ctrl_d_ends_the_session_with_an_empty_frame() {
        let (mut repl, tx, rx) = repl();
        repl.feed(b"\x04", &tx);
        let frames = {
            let mut frames = Vec::new();
            while let Ok(chunk) = rx.try_recv() {
                frames.push(chunk);
            }
            frames
        };
        assert_eq!(frames.last().map(Vec::len), Some(0), "空帧表示会话结束");
    }

    #[test]
    fn utf8_split_across_chunks_is_kept_until_complete() {
        let (mut repl, tx, rx) = repl();
        let bytes = "中".as_bytes();
        repl.feed(&bytes[..2], &tx);
        assert!(repl.line.is_empty(), "不完整的 UTF-8 不能进输入行");
        assert!(drain(&rx).is_empty());
        repl.feed(&bytes[2..], &tx);
        assert_eq!(repl.line, "中");
    }

    #[test]
    fn blank_line_only_redraws_the_prompt() {
        let (mut repl, tx, rx) = repl();
        repl.feed(b"\r", &tx);
        let output = String::from_utf8_lossy(&drain(&rx)).to_string();
        assert_eq!(output, "\r\ndemo> ");
    }

    #[test]
    fn history_navigation_redraws_with_erase_to_end_of_line() {
        let (mut repl, tx, rx) = repl();
        repl.history = vec!["{\"ping\": 1}".into(), "{\"buildInfo\": 1}".into()];
        repl.history_cursor = repl.history.len();
        repl.feed(b"\x1b[A", &tx);
        let output = String::from_utf8_lossy(&drain(&rx)).to_string();
        assert_eq!(repl.line, "{\"buildInfo\": 1}");
        assert!(output.ends_with("\x1b[K"), "{output}");
        repl.feed(b"\x1b[A", &tx);
        assert_eq!(repl.line, "{\"ping\": 1}");
        repl.feed(b"\x1b[B", &tx);
        assert_eq!(repl.line, "{\"buildInfo\": 1}");
    }

    #[test]
    fn exit_command_reports_without_touching_the_database() {
        let (mut repl, _tx, _rx) = repl();
        let rendered = String::from_utf8_lossy(&repl.execute(" quit ")).to_string();
        assert!(rendered.contains("已退出"), "{rendered}");
        assert!(repl.history.is_empty());
    }

    #[test]
    fn completion_offers_known_commands_and_marks_writes_dangerous() {
        let completion = MongoCliCompletion;
        let items = completion.complete(&input("dro"));
        assert!(
            items.iter().any(|item| item.insert_text == "dropDatabase"),
            "dropDatabase 应出现在补全里"
        );
        assert!(MongoCliCompletion::is_dangerous_impl(r#"{ "dropDatabase": 1 }"#));
        assert!(!MongoCliCompletion::is_dangerous_impl(r#"{ "ping": 1 }"#));
    }

    /// 构造补全上下文（补全只看输入行走尾的 token）。
    fn input(text: &str) -> mterm::TerminalCommandContext {
        mterm::TerminalCommandContext {
            input: text.to_string(),
            cursor: text.len(),
            prompt: String::new(),
            kind: mterm::TerminalSessionKind::MongoCli,
            meta: std::collections::BTreeMap::new(),
        }
    }
}
