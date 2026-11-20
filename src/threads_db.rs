use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;

use rusqlite::Connection;
use serde_json::{Map, Value};

use crate::sqlite::{self, POLL_INTERVAL};

/// The `harness` numbers of the agents that atuin 18.24 has written so far.
const CLAUDE_CODE: i64 = 1;
const OPENCODE: i64 = 4;
/// The `parent_kind` atuin writes for a session that an agent started for a subtask.
const SUBAGENT: i64 = 0;
/// Tool input fields that best say what a call did, most telling first.
const SUMMARY_FIELDS: [&str; 10] = [
    "command",
    "file_path",
    "filePath",
    "path",
    "pattern",
    "url",
    "query",
    "description",
    "prompt",
    "template",
];

pub fn default_path(home: &Path) -> PathBuf {
    sqlite::data_dir(home).join("threads.db")
}

/// This machine's host id in the hyphenated form that `sessions.host_id` uses.
pub fn local_host_id(home: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(sqlite::data_dir(home).join("host_id")).ok()?;
    hyphenate(raw.trim())
}

fn hyphenate(hex: &str) -> Option<String> {
    (hex.len() == 32 && hex.is_ascii()).then(|| {
        format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        )
    })
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tokens {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

#[derive(Default, Debug)]
pub struct Thread {
    /// The `sessions` row id.
    pub id: i64,
    pub harness: i64,
    /// The agent's own id for the session, which `atuin resume` accepts.
    pub session_id: String,
    pub parent_kind: Option<i64>,
    pub parent_session_id: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub model: Option<String>,
    pub host_id: Option<String>,
    /// Milliseconds since the Unix epoch, UTC.
    pub started_at: i64,
    /// Milliseconds since the Unix epoch, UTC.
    pub updated_at: i64,
    pub message_count: i64,
    pub tokens: Tokens,
    pub title: Option<String>,
    pub preview: Option<String>,
    /// Lowercase title, preview, directory, branch and session id, for the thread filter.
    pub search_text: String,
}

impl Thread {
    pub fn display_title(&self) -> &str {
        [&self.title, &self.preview]
            .into_iter()
            .flatten()
            .map(|text| text.trim())
            .find(|text| !text.is_empty())
            .unwrap_or(&self.session_id)
    }

    pub fn is_subagent(&self) -> bool {
        self.parent_kind == Some(SUBAGENT)
    }

    pub fn harness_name(&self) -> Cow<'static, str> {
        match self.harness {
            CLAUDE_CODE => "Claude Code".into(),
            OPENCODE => "OpenCode".into(),
            other => format!("Agent {other}").into(),
        }
    }

    pub fn resume_command(&self) -> String {
        format!("atuin resume {}", self.session_id)
    }
}

#[derive(Debug)]
pub struct Message {
    pub seq: i64,
    /// Milliseconds since the Unix epoch, UTC.
    pub timestamp: i64,
    pub role: Role,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Tool,
    System,
    Other(String),
}

impl Role {
    /// atuin stores the role as JSON: `"User"`, or `{"Other":"away_summary"}` for the rest.
    fn parse(json: &str) -> Self {
        match serde_json::from_str::<Value>(json) {
            Ok(Value::String(name)) => match name.as_str() {
                "User" => Self::User,
                "Assistant" => Self::Assistant,
                "Tool" => Self::Tool,
                "System" => Self::System,
                _ => Self::Other(name),
            },
            Ok(Value::Object(fields)) => match fields.get("Other").and_then(Value::as_str) {
                Some(name) => Self::Other(name.to_owned()),
                None => Self::Other(json.to_owned()),
            },
            _ => Self::Other(json.to_owned()),
        }
    }
}

#[derive(Debug)]
pub enum Block {
    Text(String),
    /// The reasoning text is not captured, only how long it was.
    Reasoning(Option<i64>),
    Tool(ToolCall),
    /// A tool output whose call is not in the thread.
    Output(ToolOutput),
    Summary(String),
    Error(String),
    /// A block kind this version does not know, as pretty JSON.
    Other(String),
}

#[derive(Debug)]
pub struct ToolCall {
    pub id: String,
    /// The tool name and one line that says what the call did, such as the command or the file.
    pub title: String,
    /// The input as `field: value` lines.
    pub input: String,
    pub output: Option<ToolOutput>,
}

#[derive(Debug, Default)]
pub struct ToolOutput {
    /// The id of the tool call this output answers.
    pub call: String,
    pub text: String,
    pub error: bool,
    pub files: Vec<FileChange>,
}

impl ToolOutput {
    fn merge(&mut self, other: ToolOutput) {
        if self.text.is_empty() {
            self.text = other.text;
        }
        self.error |= other.error;
        self.files.extend(other.files);
    }
}

#[derive(Debug)]
pub struct FileChange {
    pub path: String,
    /// `Add`, `Update` or `Delete`.
    pub change: String,
    /// The hunks in unified diff form.
    pub diff: String,
}

pub fn load_threads(conn: &Connection) -> rusqlite::Result<Vec<Thread>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, harness, session_id, parent_kind, parent_session_id, cwd, git_branch, model,
                host_id, started_at, updated_at, message_count, usage_input, usage_output,
                usage_cache_read, usage_cache_write, title, preview
         FROM sessions
         ORDER BY updated_at DESC, id DESC",
    )?;
    stmt.query_map([], |row| {
        let mut thread = Thread {
            id: row.get(0)?,
            harness: row.get(1)?,
            session_id: row.get(2)?,
            parent_kind: row.get(3)?,
            parent_session_id: row.get(4)?,
            cwd: row.get(5)?,
            git_branch: row.get(6)?,
            model: row.get(7)?,
            host_id: row.get(8)?,
            started_at: row.get(9)?,
            updated_at: row.get(10)?,
            message_count: row.get(11)?,
            tokens: Tokens {
                input: row.get(12)?,
                output: row.get(13)?,
                cache_read: row.get(14)?,
                cache_write: row.get(15)?,
            },
            title: row.get(16)?,
            preview: row.get(17)?,
            search_text: String::new(),
        };
        thread.search_text = [
            thread.title.as_deref(),
            thread.preview.as_deref(),
            thread.cwd.as_deref(),
            thread.git_branch.as_deref(),
            Some(&thread.session_id),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
        Ok(thread)
    })?
    .collect()
}

pub fn load_messages(conn: &Connection, thread: i64) -> rusqlite::Result<Vec<Message>> {
    let mut stmt = conn.prepare_cached(
        "SELECT m.seq, m.timestamp, r.value, m.content, m.content_z
         FROM messages m JOIN interned r ON r.id = m.role
         WHERE m.session = ?1
         ORDER BY m.timestamp, m.seq",
    )?;
    let mut messages = stmt
        .query_map([thread], |row| {
            let role: String = row.get(2)?;
            let content: String = row.get(3)?;
            let compressed: Option<Vec<u8>> = row.get(4)?;
            Ok(Message {
                seq: row.get(0)?,
                timestamp: row.get(1)?,
                role: Role::parse(&role),
                blocks: decode_blocks(content, compressed.as_deref()).unwrap_or_else(|err| {
                    vec![Block::Error(format!("Cannot read this message: {err}"))]
                }),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    attach_outputs(&mut messages);
    // Title, agent-name and attachment rows carry no blocks, and paired outputs left their rows empty.
    messages.retain(|message| !message.blocks.is_empty());
    Ok(messages)
}

/// atuin keeps short content as JSON text and compresses the rest with zstd into `content_z`.
fn decode_blocks(content: String, compressed: Option<&[u8]>) -> Result<Vec<Block>, String> {
    let json = match compressed {
        Some(bytes) => {
            let raw = zstd::decode_all(bytes).map_err(|err| err.to_string())?;
            String::from_utf8(raw).map_err(|err| err.to_string())?
        }
        None => content,
    };
    match serde_json::from_str(&json).map_err(|err| err.to_string())? {
        Value::Array(blocks) => Ok(blocks.into_iter().map(parse_block).collect()),
        _ => Err("the content is not a list of blocks".to_owned()),
    }
}

/// Each block is an object with one key that names its kind, such as `{"Text": "…"}`.
fn parse_block(block: Value) -> Block {
    match block {
        Value::Object(fields) if fields.len() == 1 => match fields.into_iter().next() {
            Some((kind, inner)) => parse_kind(&kind, inner),
            None => Block::Other(String::new()),
        },
        other => unknown(&other),
    }
}

fn parse_kind(kind: &str, inner: Value) -> Block {
    match (kind, inner) {
        ("Text", Value::String(text)) => Block::Text(text),
        ("Summary", Value::String(text)) => Block::Summary(text),
        ("Error", Value::String(text)) => Block::Error(text),
        ("ReasoningSummary", inner) => {
            Block::Reasoning(inner.get("tokens").and_then(Value::as_i64))
        }
        ("ToolUse", Value::Object(fields)) => Block::Tool(tool_call(fields)),
        ("ToolResult", Value::Object(mut fields)) => Block::Output(ToolOutput {
            call: string_field(&fields, "call"),
            error: fields
                .get("error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            text: output_text(fields.remove("output").unwrap_or(Value::Null)),
            files: Vec::new(),
        }),
        ("Patch", Value::Object(fields)) => Block::Output(ToolOutput {
            call: string_field(&fields, "call"),
            files: fields
                .get("files")
                .and_then(Value::as_array)
                .map_or_else(Vec::new, |files| files.iter().map(file_change).collect()),
            ..ToolOutput::default()
        }),
        (_, inner) => unknown(&Value::Object(Map::from_iter([(kind.to_owned(), inner)]))),
    }
}

fn unknown(block: &Value) -> Block {
    Block::Other(serde_json::to_string_pretty(block).unwrap_or_default())
}

fn string_field(fields: &Map<String, Value>, key: &str) -> String {
    fields
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn tool_call(mut fields: Map<String, Value>) -> ToolCall {
    let input = fields.remove("input").unwrap_or(Value::Null);
    let name = string_field(&fields, "name");
    let summary = tool_summary(&input);
    ToolCall {
        id: string_field(&fields, "id"),
        title: if summary.is_empty() {
            name.clone()
        } else {
            format!("{name}  {summary}")
        },
        input: tool_input(&input),
        output: None,
    }
}

fn tool_summary(input: &Value) -> String {
    let Value::Object(fields) = input else {
        return String::new();
    };
    let text = SUMMARY_FIELDS
        .iter()
        .find_map(|key| fields.get(*key)?.as_str())
        .or_else(|| fields.values().find_map(Value::as_str))
        .unwrap_or_default();
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    truncate(line, 160)
}

fn truncate(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// Shows strings as they are, so that commands and file contents keep their lines.
fn tool_input(input: &Value) -> String {
    match input {
        Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| match value {
                Value::String(text) if text.contains('\n') => format!("{key}:\n{text}"),
                Value::String(text) => format!("{key}: {text}"),
                other => format!("{key}: {other}"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// An output is a string, or a list of parts such as `{"type":"text","text":"…"}`.
fn output_text(output: Value) -> String {
    match output {
        Value::Null => String::new(),
        Value::String(text) => text,
        Value::Array(parts) => parts
            .iter()
            .map(|part| match part.get("type").and_then(Value::as_str) {
                Some("text") => part
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                Some("tool_reference") => format!(
                    "[tool {}]",
                    part.get("tool_name").and_then(Value::as_str).unwrap_or("?")
                ),
                _ => part.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

fn file_change(file: &Value) -> FileChange {
    let path = file["path"].as_str().unwrap_or("?");
    let mut diff = String::new();
    for hunk in file["hunks"].as_array().into_iter().flatten() {
        let _ = writeln!(
            diff,
            "@@ -{},{} +{},{} @@",
            hunk["old_start"], hunk["old_lines"], hunk["new_start"], hunk["new_lines"]
        );
        for line in hunk["lines"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            diff.push_str(line);
            diff.push('\n');
        }
    }
    FileChange {
        path: match file["moved_to"].as_str() {
            Some(to) => format!("{path} → {to}"),
            None => path.to_owned(),
        },
        change: file["change"].as_str().unwrap_or("Change").to_owned(),
        diff,
    }
}

/// Moves each tool output into the call it answers, so that a call and its result show together.
fn attach_outputs(messages: &mut [Message]) {
    let calls: HashSet<String> = messages
        .iter()
        .flat_map(|message| &message.blocks)
        .filter_map(|block| match block {
            Block::Tool(call) => Some(call.id.clone()),
            _ => None,
        })
        .collect();
    let mut outputs: HashMap<String, ToolOutput> = HashMap::new();
    for message in messages.iter_mut() {
        message.blocks.retain_mut(|block| match block {
            Block::Output(output) if calls.contains(&output.call) => {
                let output = std::mem::take(output);
                outputs
                    .entry(output.call.clone())
                    .or_default()
                    .merge(output);
                false
            }
            _ => true,
        });
    }
    for block in messages.iter_mut().flat_map(|message| &mut message.blocks) {
        if let Block::Tool(call) = block {
            call.output = outputs.remove(&call.id);
        }
    }
}

/// Returns the ids of the threads with a message that contains every word of the query.
pub fn search(conn: &Connection, query: &str) -> rusqlite::Result<HashSet<i64>> {
    let Some(fts) = fts_query(query) else {
        return Ok(HashSet::new());
    };
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT m.session
         FROM messages_fts JOIN messages m ON m.seq = messages_fts.rowid
         WHERE messages_fts MATCH ?1",
    )?;
    stmt.query_map([fts], |row| row.get(0))?.collect()
}

/// Quotes each word so that FTS5 reads it as text, not syntax, and matches it as a prefix while the user types.
fn fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

pub enum Request {
    Open(i64),
    Search(String),
}

pub enum Update {
    Threads(Vec<Thread>),
    Transcript {
        thread: i64,
        messages: Vec<Message>,
    },
    SearchHits {
        query: String,
        threads: HashSet<i64>,
    },
    Failed(String),
}

/// Loads the threads on a background thread and reloads them, and the open transcript, each time atuin writes.
pub fn spawn_loader(
    path: PathBuf,
    notify: impl Fn() + Send + 'static,
) -> (Sender<Request>, Receiver<Update>) {
    let (request_tx, request_rx) = channel();
    let (update_tx, update_rx) = channel();
    thread::spawn(move || run_loader(&path, &request_rx, &update_tx, notify));
    (request_tx, update_rx)
}

fn run_loader(path: &Path, requests: &Receiver<Request>, tx: &Sender<Update>, notify: impl Fn()) {
    let send = |update| {
        let sent = tx.send(update).is_ok();
        notify();
        sent
    };
    if !path.exists() {
        send(Update::Failed(format!(
            "There is no threads database at {}. Turn capture on with `atuin threads enable`.",
            path.display()
        )));
        return;
    }
    let conn = match sqlite::open_read_only(path) {
        Ok(conn) => conn,
        Err(err) => {
            send(Update::Failed(format!(
                "Cannot open {}: {err}",
                path.display()
            )));
            return;
        }
    };
    let mut loaded_version = None;
    // The updated_at and message_count of each thread, to tell when the open transcript changed.
    let mut stamps: HashMap<i64, (i64, i64)> = HashMap::new();
    let mut open: Option<i64> = None;
    let mut open_stamp = None;
    let mut reload_transcript = false;
    let mut search_query: Option<String> = None;
    loop {
        match sqlite::data_version(&conn) {
            Ok(version) if Some(version) == loaded_version => {}
            Ok(version) => match load_threads(&conn) {
                Ok(threads) => {
                    loaded_version = Some(version);
                    stamps = threads
                        .iter()
                        .map(|t| (t.id, (t.updated_at, t.message_count)))
                        .collect();
                    reload_transcript |=
                        open.is_some_and(|id| stamps.get(&id) != open_stamp.as_ref());
                    if !send(Update::Threads(threads)) {
                        return;
                    }
                }
                Err(err) => {
                    if !send(Update::Failed(format!("Cannot read threads: {err}"))) {
                        return;
                    }
                }
            },
            Err(err) => {
                if !send(Update::Failed(format!("Cannot read threads: {err}"))) {
                    return;
                }
            }
        }
        if let Some(id) = open
            && std::mem::take(&mut reload_transcript)
        {
            let update = match load_messages(&conn, id) {
                Ok(messages) => {
                    open_stamp = stamps.get(&id).copied();
                    Update::Transcript {
                        thread: id,
                        messages,
                    }
                }
                Err(err) => Update::Failed(format!("Cannot read the thread: {err}")),
            };
            if !send(update) {
                return;
            }
        }
        if let Some(query) = search_query.take() {
            let update = match search(&conn, &query) {
                Ok(threads) => Update::SearchHits { query, threads },
                Err(err) => Update::Failed(format!("Cannot search messages: {err}")),
            };
            if !send(update) {
                return;
            }
        }

        // Waits for a request or the next poll, then takes every queued request so that only the newest search runs.
        let first = match requests.recv_timeout(POLL_INTERVAL) {
            Ok(request) => Some(request),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        for request in first.into_iter().chain(requests.try_iter()) {
            match request {
                Request::Open(id) => {
                    open = Some(id);
                    reload_transcript = true;
                }
                Request::Search(query) => search_query = Some(query),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE sessions (
            id INTEGER PRIMARY KEY, harness INTEGER NOT NULL, session_id TEXT NOT NULL,
            parent_kind INTEGER, parent_session_id TEXT, cwd TEXT, git_branch TEXT, model TEXT,
            host_id TEXT, started_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
            message_count INTEGER NOT NULL DEFAULT 0, usage_input INTEGER NOT NULL DEFAULT 0,
            usage_output INTEGER NOT NULL DEFAULT 0, usage_cache_read INTEGER NOT NULL DEFAULT 0,
            usage_cache_write INTEGER NOT NULL DEFAULT 0, title TEXT, preview TEXT);
        CREATE TABLE interned (id INTEGER PRIMARY KEY, value TEXT NOT NULL UNIQUE);
        CREATE TABLE messages (
            seq INTEGER PRIMARY KEY, session INTEGER NOT NULL, timestamp INTEGER NOT NULL,
            role INTEGER NOT NULL, content TEXT NOT NULL, content_z BLOB);
        CREATE VIRTUAL TABLE messages_fts USING fts5(
            title, body, tokenize = 'unicode61', content = '', contentless_delete = 1);
        INSERT INTO interned VALUES (1, '\"User\"'), (2, '\"Assistant\"'), (3, '{\"Other\":\"agent-name\"}');
        INSERT INTO sessions (id, harness, session_id, parent_kind, parent_session_id, cwd, started_at, updated_at, message_count, title, preview)
        VALUES (1, 1, 'aaaa-1111', NULL, NULL, '/src/app', 10, 500, 4, 'Fix the Build', 'please fix'),
               (2, 1, 'agent-b2', 0, 'aaaa-1111', '/src/app', 20, 900, 1, NULL, 'look around'),
               (3, 4, 'ses_c3', NULL, NULL, '/tmp', 5, 100, 0, '  ', NULL);";

    fn fixture() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("threads.db");
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch(SCHEMA).unwrap();
        let insert = |seq: i64, session: i64, role: i64, json: &str, compress: bool| {
            let (content, compressed) = if compress {
                (
                    String::new(),
                    Some(zstd::encode_all(json.as_bytes(), 3).unwrap()),
                )
            } else {
                (json.to_owned(), None)
            };
            writer
                .execute(
                    "INSERT INTO messages VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![seq, session, seq * 10, role, content, compressed],
                )
                .unwrap();
        };
        insert(1, 1, 1, r#"[{"Text":"Please fix the build"}]"#, false);
        insert(2, 1, 3, "[]", false);
        insert(
            3,
            1,
            2,
            r#"[{"ReasoningSummary":{"tokens":42}},{"ToolUse":{"id":"t1","name":"Bash","input":{"command":"cargo build\ncargo test","description":"Build"}}}]"#,
            true,
        );
        insert(
            4,
            1,
            1,
            r#"[{"ToolResult":{"call":"t1","output":[{"type":"text","text":"error[E0425]"}],"error":true}},{"Patch":{"call":"t1","files":[{"path":"src/main.rs","change":"Update","moved_to":null,"hunks":[{"old_start":1,"old_lines":1,"new_start":1,"new_lines":1,"lines":["-a","+b"]}]}]}}]"#,
            false,
        );
        insert(
            5,
            1,
            2,
            r#"[{"Mystery":{"x":1}},{"ToolResult":{"call":"gone","output":"orphan","error":false}}]"#,
            false,
        );
        writer
            .execute_batch(
                "INSERT INTO messages_fts (rowid, title, body) VALUES (1, '', 'Please fix the build');
                 INSERT INTO messages_fts (rowid, title, body) VALUES (3, '', 'cargo build');",
            )
            .unwrap();
        (dir, sqlite::open_read_only(&path).unwrap())
    }

    #[test]
    fn lists_threads_newest_first_with_fallback_titles() {
        let (_dir, conn) = fixture();
        let threads = load_threads(&conn).unwrap();
        let ids: Vec<_> = threads.iter().map(|t| t.id).collect();
        assert_eq!(ids, [2, 1, 3]);
        assert_eq!(threads[1].display_title(), "Fix the Build");
        assert_eq!(threads[0].display_title(), "look around");
        assert_eq!(threads[2].display_title(), "ses_c3");
        assert!(threads[0].is_subagent() && !threads[1].is_subagent());
        assert_eq!(threads[2].harness_name(), "OpenCode");
        assert!(threads[1].search_text.contains("fix the build"));
        assert_eq!(threads[1].resume_command(), "atuin resume aaaa-1111");
    }

    #[test]
    fn decodes_messages_and_attaches_outputs_to_their_calls() {
        let (_dir, conn) = fixture();
        let messages = load_messages(&conn, 1).unwrap();
        let seqs: Vec<_> = messages.iter().map(|m| m.seq).collect();
        assert_eq!(seqs, [1, 3, 5], "empty and fully paired rows are dropped");
        assert_eq!(messages[0].role, Role::User);
        assert!(matches!(&messages[0].blocks[..], [Block::Text(t)] if t == "Please fix the build"));

        let [Block::Reasoning(Some(42)), Block::Tool(call)] = &messages[1].blocks[..] else {
            panic!("unexpected blocks: {:?}", messages[1].blocks);
        };
        assert_eq!(call.title, "Bash  cargo build");
        assert_eq!(
            call.input,
            "command:\ncargo build\ncargo test\ndescription: Build"
        );
        let output = call.output.as_ref().unwrap();
        assert!(output.error);
        assert_eq!(output.text, "error[E0425]");
        assert_eq!(output.files[0].path, "src/main.rs");
        assert_eq!(output.files[0].diff, "@@ -1,1 +1,1 @@\n-a\n+b\n");

        let [Block::Other(json), Block::Output(orphan)] = &messages[2].blocks[..] else {
            panic!("unexpected blocks: {:?}", messages[2].blocks);
        };
        assert!(json.contains("Mystery"));
        assert_eq!(orphan.text, "orphan");
    }

    #[test]
    fn parses_roles() {
        assert_eq!(Role::parse("\"Assistant\""), Role::Assistant);
        assert_eq!(
            Role::parse("{\"Other\":\"away_summary\"}"),
            Role::Other("away_summary".into())
        );
        assert_eq!(Role::parse("42"), Role::Other("42".into()));
    }

    #[test]
    fn searches_message_text_by_word_prefix() {
        let (_dir, conn) = fixture();
        assert_eq!(search(&conn, "buil").unwrap(), HashSet::from([1]));
        assert_eq!(search(&conn, "cargo please").unwrap(), HashSet::new());
        assert_eq!(search(&conn, "\"fix\" AND").unwrap(), HashSet::new());
        assert_eq!(search(&conn, "-- ::").unwrap(), HashSet::new());
    }

    #[test]
    fn quotes_fts_terms() {
        assert_eq!(fts_query("git push").as_deref(), Some("\"git\"* \"push\"*"));
        assert_eq!(
            fts_query("say \"hi\"").as_deref(),
            Some("\"say\"* \"\"\"hi\"\"\"*")
        );
        assert_eq!(fts_query("  -- "), None);
    }

    #[test]
    fn summarizes_tool_input() {
        let input = serde_json::json!({"description": "List", "file_path": "/a/b.rs"});
        assert_eq!(tool_summary(&input), "/a/b.rs");
        let long = serde_json::json!({"command": "x".repeat(200)});
        assert_eq!(tool_summary(&long).chars().count(), 161);
        assert_eq!(tool_summary(&serde_json::json!({"n": 1})), "");
    }

    #[test]
    fn hyphenates_host_ids() {
        assert_eq!(
            hyphenate("0194f1a5d60871c1ba69d1e6f38bdb1e").as_deref(),
            Some("0194f1a5-d608-71c1-ba69-d1e6f38bdb1e")
        );
        assert_eq!(hyphenate("short"), None);
    }
}
