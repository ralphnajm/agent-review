//! A Model Context Protocol server, run with `agent-review --mcp`, through
//! which a coding agent pulls the reviews the user posted and resolves their
//! comments.
//!
//! The agent starts the server itself and talks to it over stdio, one
//! JSON-RPC message per line. It works whether or not the app is running:
//! both only share the queue on disk (see [`crate::core::queue`]).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::config;
use crate::core::feedback;
use crate::core::git::Repo;
use crate::core::queue::{self, PostedComment, Queue};

/// Protocol revisions the server can speak; the newest is offered by default.
const PROTOCOL_VERSIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

const INSTRUCTIONS: &str = "Agent Review is a desktop app in which the user reviews your uncommitted \
changes and leaves comments on them. When the user asks you to pull, fetch or check their review, call \
get_review, address each comment, then call resolve_comments with the ids of the comments you addressed.";

/// Serves requests from stdin until it closes.
pub fn serve() -> Result<()> {
    let cwd = std::env::current_dir().context("No working directory")?;
    let server = Server::new(cwd, |path| Ok(Repo::discover(path)?.root));
    let mut stdout = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        if let Some(response) = server.handle(&line?) {
            writeln!(stdout, "{response}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

type FindRoot = dyn Fn(&Path) -> Result<PathBuf>;

pub struct Server {
    /// Where the agent runs, and the default project.
    cwd: PathBuf,
    /// The repository root containing a path.
    find_root: Box<FindRoot>,
    /// The queue of a repository root.
    queue_for: fn(&Path) -> Queue,
}

impl Server {
    pub fn new(cwd: PathBuf, find_root: impl Fn(&Path) -> Result<PathBuf> + 'static) -> Self {
        Self { cwd, find_root: Box::new(find_root), queue_for: Queue::for_repo }
    }

    /// Answers one JSON-RPC message; notifications get no answer.
    pub fn handle(&self, line: &str) -> Option<String> {
        if line.trim().is_empty() {
            return None;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            return Some(error(&Value::Null, -32700, "Parse error").to_string());
        };
        let id = message.get("id")?.clone();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match message.get("method").and_then(Value::as_str) {
            Some("initialize") => initialize(&params),
            Some("ping") => json!({}),
            Some("tools/list") => json!({ "tools": tools() }),
            Some("tools/call") => self.call_tool(&params),
            Some(method) => return Some(error(&id, -32601, &format!("Unknown method {method}")).to_string()),
            None => return Some(error(&id, -32600, "Invalid request").to_string()),
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string())
    }

    fn call_tool(&self, params: &Value) -> Value {
        let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let outcome = match params.get("name").and_then(Value::as_str) {
            Some("get_review") => self.get_review(&arguments),
            Some("resolve_comments") => self.resolve_comments(&arguments),
            other => Err(anyhow::anyhow!("Unknown tool {}", other.unwrap_or("(none)"))),
        };
        match outcome {
            Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
            Err(err) => json!({ "content": [{ "type": "text", "text": format!("{err:#}") }], "isError": true }),
        }
    }

    fn queue(&self, arguments: &Value) -> Result<(PathBuf, Queue)> {
        let path = arguments.get("project").and_then(Value::as_str).map_or_else(|| self.cwd.clone(), PathBuf::from);
        let root = (self.find_root)(&path)?;
        let queue = (self.queue_for)(&root);
        Ok((root, queue))
    }

    fn get_review(&self, arguments: &Value) -> Result<String> {
        let (root, queue) = self.queue(arguments)?;
        let entries = queue.load();
        let open: Vec<&PostedComment> = entries.iter().flat_map(queue::Entry::open).collect();
        if open.is_empty() {
            return Ok(format!(
                "There are no open review comments for {}. The user has not posted any, or they are all resolved.",
                root.display()
            ));
        }
        let pulled: Vec<u64> = entries.iter().filter(|e| e.open().next().is_some()).map(|e| e.review.id).collect();
        queue.mark_pulled(&pulled, crate::core::now()).context("Could not record that the review was pulled")?;
        Ok(render(&root, &open))
    }

    fn resolve_comments(&self, arguments: &Value) -> Result<String> {
        let ids: Vec<u64> = arguments
            .get("ids")
            .and_then(Value::as_array)
            .context("Pass the ids of the comments to resolve as `ids`")?
            .iter()
            .filter_map(Value::as_u64)
            .collect();
        let (_, queue) = self.queue(arguments)?;
        let resolved = queue.resolve(&ids, crate::core::now()).context("Could not save the resolved comments")?;
        let skipped: Vec<u64> = ids.iter().copied().filter(|id| !resolved.contains(id)).collect();
        let mut text = if resolved.is_empty() {
            "No comments were resolved.".to_owned()
        } else {
            format!("Resolved {}.", id_list(&resolved))
        };
        if !skipped.is_empty() {
            let _ = write!(text, " Not open (unknown or already resolved): {}.", id_list(&skipped));
        }
        Ok(text)
    }
}

/// The open comments as a Markdown prompt, grouped by file.
fn render(root: &Path, comments: &[&PostedComment]) -> String {
    let mut by_path: BTreeMap<&str, Vec<&PostedComment>> = BTreeMap::new();
    for comment in comments {
        by_path.entry(&comment.path).or_default().push(comment);
    }
    let mut text = format!(
        "The user reviewed the uncommitted changes in {} and left these comments. Paths are relative to the \
         repository root, and line numbers refer to the files when the comment was written. Address each \
         comment, then call resolve_comments with the ids of the comments you addressed.\n\n",
        root.display()
    );
    for (path, comments) in by_path {
        let _ = writeln!(text, "## `{path}`\n");
        for comment in comments {
            feedback::write_comment(
                &mut text,
                comment.anchor.as_ref(),
                &comment.body,
                &format!(" (id {})", comment.id),
            );
        }
        text.push('\n');
    }
    text.trim_end().to_owned() + "\n"
}

fn id_list(ids: &[u64]) -> String {
    let ids: Vec<String> = ids.iter().map(u64::to_string).collect();
    let noun = if ids.len() == 1 { "comment" } else { "comments" };
    format!("{noun} {}", ids.join(", "))
}

fn initialize(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = requested.filter(|v| PROTOCOL_VERSIONS.contains(v)).unwrap_or(PROTOCOL_VERSIONS[2]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "agent-review", "title": config::APP_NAME, "version": config::VERSION },
        "instructions": INSTRUCTIONS,
    })
}

fn tools() -> Value {
    let project = json!({
        "type": "string",
        "description": "A path inside the repository. Defaults to the directory you are working in.",
    });
    json!([
        {
            "name": "get_review",
            "title": "Get review comments",
            "description": "Get the open comments the user posted in Agent Review on the uncommitted changes \
                of this repository, with the lines each one is about. Address each comment, then call \
                resolve_comments with the ids of the comments you addressed.",
            "inputSchema": { "type": "object", "properties": { "project": project } },
            "annotations": { "readOnlyHint": false, "idempotentHint": true },
        },
        {
            "name": "resolve_comments",
            "title": "Resolve review comments",
            "description": "Mark review comments as resolved once you have addressed them. The user sees \
                them as resolved in Agent Review.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ids": {
                        "type": "array",
                        "items": { "type": "integer" },
                        "description": "Ids of the comments you addressed, as shown by get_review.",
                    },
                    "project": project,
                },
                "required": ["ids"],
            },
            "annotations": { "readOnlyHint": false, "idempotentHint": true },
        },
    ])
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::review::{LineAnchor, Side};

    thread_local! {
        static QUEUE_DIR: std::cell::RefCell<PathBuf> = const { std::cell::RefCell::new(PathBuf::new()) };
    }

    fn server(dir: &Path) -> Server {
        QUEUE_DIR.set(dir.to_path_buf());
        let mut server = Server::new(PathBuf::from("/repo/sub"), |_| Ok(PathBuf::from("/repo")));
        server.queue_for = |_| Queue::at(QUEUE_DIR.with_borrow(Clone::clone));
        server
    }

    fn call(server: &Server, name: &str, arguments: &Value) -> (String, bool) {
        let request = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        let response: Value = serde_json::from_str(&server.handle(&request.to_string()).unwrap()).unwrap();
        let result = &response["result"];
        (result["content"][0]["text"].as_str().unwrap().to_owned(), result["isError"] == json!(true))
    }

    #[test]
    fn negotiates_and_lists_tools() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        let init = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#;
        let response: Value = serde_json::from_str(&server.handle(init).unwrap()).unwrap();
        assert_eq!(response["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(response["result"]["serverInfo"]["name"], "agent-review");

        assert_eq!(server.handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#), None);
        let list: Value =
            serde_json::from_str(&server.handle(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap()).unwrap();
        let names: Vec<&str> =
            list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["get_review", "resolve_comments"]);

        let unknown: Value =
            serde_json::from_str(&server.handle(r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#).unwrap()).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
    }

    #[test]
    fn pulls_and_resolves_posted_comments() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        let (text, _) = call(&server, "get_review", &json!({}));
        assert!(text.starts_with("There are no open review comments for /repo."));

        let anchor = LineAnchor { side: Side::New, line: 2, end_line: None, text: "+let x = 1;".into() };
        let queue = Queue::at(dir.path().to_path_buf());
        queue
            .post(
                [
                    (5, "src/main.rs".to_owned(), Some(anchor), "Use a descriptive name.".to_owned()),
                    (6, "README.md".to_owned(), None, "Document the flag.".to_owned()),
                ],
                1,
            )
            .unwrap();

        let (text, error) = call(&server, "get_review", &json!({}));
        assert!(!error);
        assert!(text.contains("## `README.md`\n\n- **File** (id 2)\n  Document the flag.\n"), "{text}");
        assert!(text.contains("## `src/main.rs`\n\n- **Line 2** (id 1): `let x = 1;`\n  Use a descriptive name.\n"));
        assert!(queue.load()[0].progress.pulled_at.is_some());

        let (text, _) = call(&server, "resolve_comments", &json!({ "ids": [1, 9] }));
        assert_eq!(text, "Resolved comment 1. Not open (unknown or already resolved): comment 9.");
        let (text, _) = call(&server, "get_review", &json!({}));
        assert!(!text.contains("(id 1)") && text.contains("(id 2)"));

        let (text, error) = call(&server, "resolve_comments", &json!({}));
        assert!(error && text.contains("`ids`"));
    }
}
