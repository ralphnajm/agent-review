//! A Model Context Protocol server, run with `agent-review --mcp`, through
//! which a coding agent asks the user to review a version, pulls the reviews
//! the user posted and resolves their comments.
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
use crate::core::git::{self, Repo};
use crate::core::queue::{PostedComment, Queue, ReviewRequest};
use crate::core::review::ReviewedCommit;

/// Protocol revisions the server can speak; the newest is offered by default.
const PROTOCOL_VERSIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

const INSTRUCTIONS: &str = "Agent Review is a desktop app in which the user reviews your changes, \
uncommitted or committed, and leaves comments on them. When you finish a set of changes for the user to review, commit \
them as a draft commit that is not pushed, make sure the product is running if it has one, and call request_review. \
When the user asks you to pull, fetch or check their review, call get_review, address each comment, then call \
resolve_comments with the ids of the comments you addressed. When you address a review of a version you requested, \
amend the draft commit or add commits on top of it, and call request_review again.";

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
            Some("request_review") => self.request_review(&arguments),
            Some("get_review") => self.get_review(&arguments),
            Some("resolve_comments") => self.resolve_comments(&arguments),
            other => Err(anyhow::anyhow!("Unknown tool {}", other.unwrap_or("(none)"))),
        };
        match outcome {
            Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
            Err(err) => json!({ "content": [{ "type": "text", "text": format!("{err:#}") }], "isError": true }),
        }
    }

    fn project(&self, arguments: &Value) -> PathBuf {
        arguments.get("project").and_then(Value::as_str).map_or_else(|| self.cwd.clone(), PathBuf::from)
    }

    fn queue(&self, arguments: &Value) -> Result<(PathBuf, Queue)> {
        let root = (self.find_root)(&self.project(arguments))?;
        let queue = (self.queue_for)(&root);
        Ok((root, queue))
    }

    fn request_review(&self, arguments: &Value) -> Result<String> {
        let repo = Repo::discover(&self.project(arguments))?;
        let commit = repo
            .head()
            .context("There is nothing to review yet. Commit your changes, then request the review again")?;
        let base = match arguments.get("base").and_then(Value::as_str) {
            Some(base) => repo.resolve(base)?,
            None => repo.review_base(&commit),
        };
        let info = repo.commit(&commit)?;
        let url = arguments.get("url").and_then(Value::as_str).map(str::trim).filter(|u| !u.is_empty());
        let request = ReviewRequest {
            id: 0,
            requested_at: crate::core::now(),
            commit,
            subject: info.subject,
            body: info.body,
            base,
            url: url.map(str::to_owned),
        };
        let request = (self.queue_for)(&repo.root).request(request).context("Could not save the review request")?;

        let since = if request.base == git::EMPTY_TREE {
            "all of its changes".to_owned()
        } else {
            format!("the changes since {}", request.base.get(..7).unwrap_or(&request.base))
        };
        let mut text =
            format!("Asked the user to review version {} ({}): {since}", request.short_id(), request.subject);
        if let Some(url) = &request.url {
            let _ = write!(text, ", and the product running at {url}");
        }
        text.push_str(
            ". They review it in Agent Review and post their comments when done. When they ask you to, call \
             get_review. To address their comments, amend the draft commit or add commits on top of it, then call \
             request_review again.",
        );
        if repo.status().is_ok_and(|s| !s.entries.is_empty()) {
            text.push_str(
                "\n\nThere are also uncommitted changes, which this version does not include. If they belong to it, \
                 commit them and call request_review again.",
            );
        }
        Ok(text)
    }

    fn get_review(&self, arguments: &Value) -> Result<String> {
        let (root, queue) = self.queue(arguments)?;
        let entries = queue.load();
        let open: Vec<(Option<&ReviewedCommit>, &PostedComment)> =
            entries.iter().flat_map(|e| e.open().map(|c| (e.review.commit.as_ref(), c))).collect();
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

/// Comments on one file, by path.
type ByPath<'a> = BTreeMap<&'a str, Vec<&'a PostedComment>>;

/// The open comments as a Markdown prompt, grouped by file: those on the
/// uncommitted changes first, then those on each commit.
fn render(root: &Path, comments: &[(Option<&ReviewedCommit>, &PostedComment)]) -> String {
    let mut groups: Vec<(Option<&ReviewedCommit>, ByPath<'_>)> = Vec::new();
    for &(commit, comment) in comments {
        let id = commit.map(|c| &c.id);
        let index = groups.iter().position(|(c, _)| c.map(|c| &c.id) == id).unwrap_or_else(|| {
            groups.push((commit, BTreeMap::new()));
            groups.len() - 1
        });
        groups[index].1.entry(&comment.path).or_default().push(comment);
    }
    groups.sort_by_key(|(commit, _)| commit.is_some());

    let only_uncommitted = groups.iter().all(|(commit, _)| commit.is_none());
    let mut text = if only_uncommitted {
        format!(
            "The user reviewed the uncommitted changes in {} and left these comments. Paths are relative to the \
             repository root, and line numbers refer to the files when the comment was written. Address each \
             comment, then call resolve_comments with the ids of the comments you addressed.\n\n",
            root.display()
        )
    } else {
        format!(
            "The user reviewed changes in {} and left these comments. Paths are relative to the repository \
             root. Line numbers refer to the files when the comment was written, or for a commit or a version, to \
             the files as that commit left them. Address each comment, then call resolve_comments with the ids of \
             the comments you addressed.\n\n",
            root.display()
        )
    };
    let file_heading = if only_uncommitted { "##" } else { "###" };
    for (commit, by_path) in groups {
        match commit {
            _ if only_uncommitted => {}
            None => text.push_str("## Uncommitted changes\n\n"),
            Some(commit) => {
                let what = if commit.base.is_some() { "Version" } else { "Commit" };
                let _ = writeln!(text, "## {what} {} ({})\n", commit.id, commit.subject);
            }
        }
        // The running product's comments have no path, so they come first.
        for (path, comments) in by_path {
            if path.is_empty() {
                let _ = writeln!(text, "{file_heading} The running product\n");
            } else {
                let _ = writeln!(text, "{file_heading} `{path}`\n");
            }
            for comment in comments {
                let tag = format!(" (id {})", comment.id);
                if path.is_empty() {
                    feedback::write_page_comment(&mut text, comment.page.as_deref(), &comment.body, &tag);
                } else {
                    feedback::write_comment(&mut text, comment.anchor.as_ref(), &comment.body, &tag);
                }
            }
            text.push('\n');
        }
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
            "name": "request_review",
            "title": "Request a review",
            "description": "Ask the user to review the work you committed: the version at HEAD, and the running \
                product if there is one. Commit your changes first, as draft commits that are not pushed. Agent \
                Review shows the user the changes since the branch left its upstream, or since the parent of HEAD \
                when it has none.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "Where the user can open the running product, such as \
                            http://localhost:5173. Make sure it is running first. Leave it out when there is \
                            nothing to run.",
                    },
                    "base": {
                        "type": "string",
                        "description": "The commit to compare the version to, when the work spans commits the \
                            default does not cover: a commit id or a revision such as HEAD~3.",
                    },
                    "project": project,
                },
            },
            "annotations": { "readOnlyHint": false, "idempotentHint": false },
        },
        {
            "name": "get_review",
            "title": "Get review comments",
            "description": "Get the open comments the user posted in Agent Review on the changes of this \
                repository, uncommitted ones or commits, with the lines each one is about. Address each comment, then call \
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

    fn comment(note_id: u64, path: &str, anchor: Option<LineAnchor>, body: &str) -> PostedComment {
        PostedComment { id: 0, note_id, path: path.into(), anchor, page: None, body: body.into() }
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
        assert_eq!(names, ["request_review", "get_review", "resolve_comments"]);

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
                None,
                [
                    comment(5, "src/main.rs", Some(anchor), "Use a descriptive name."),
                    comment(6, "README.md", None, "Document the flag."),
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

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn requests_a_review_of_the_committed_version() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        let queue_dir = tempfile::tempdir().unwrap();
        QUEUE_DIR.set(queue_dir.path().to_path_buf());
        let mut server = Server::new(root.clone(), |path| Ok(Repo::discover(path)?.root));
        server.queue_for = |_| Queue::at(QUEUE_DIR.with_borrow(Clone::clone));
        let queue = Queue::at(queue_dir.path().to_path_buf());

        git(&root, &["init", "-q", "-b", "main"]);
        let (text, error) = call(&server, "request_review", &json!({}));
        assert!(error && text.starts_with("There is nothing to review yet."), "{text}");

        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "Start"]);
        let (text, error) = call(&server, "request_review", &json!({}));
        assert!(!error && text.contains(": all of its changes."), "{text}");

        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "Add a page", "-m", "With details."]);
        std::fs::write(root.join("c.txt"), "c\n").unwrap();
        let (text, error) = call(&server, "request_review", &json!({ "url": " http://localhost:5173 " }));
        assert!(!error, "{text}");
        assert!(text.contains("(Add a page)") && text.contains("running at http://localhost:5173."), "{text}");
        assert!(text.contains("There are also uncommitted changes"), "{text}");

        let requests = queue.requests();
        let [start, page] = &requests[..] else { panic!("{requests:?}") };
        assert_eq!(start.base, git::EMPTY_TREE);
        assert_eq!(page.base, start.commit);
        assert_eq!((page.subject.as_str(), page.body.as_str()), ("Add a page", "With details."));
        assert_eq!(page.url.as_deref(), Some("http://localhost:5173"));

        let (_, error) = call(&server, "request_review", &json!({ "base": "HEAD~1" }));
        assert!(!error);
        assert_eq!(queue.requests()[2].base, start.commit);
        let (text, error) = call(&server, "request_review", &json!({ "base": "nope" }));
        assert!(error && text.contains("nope is not a commit"), "{text}");
    }

    #[test]
    fn groups_comments_on_commits_after_the_uncommitted_ones() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        let queue = Queue::at(dir.path().to_path_buf());
        let commit = ReviewedCommit { id: "0123456789abcdef".into(), subject: "Add the parser".into(), base: None };
        queue.post(Some(commit), [comment(1, "src/parse.rs", None, "Handle errors.")], 1).unwrap();
        queue.post(None, [comment(2, "README.md", None, "Document it.")], 2).unwrap();
        let version =
            ReviewedCommit { id: "fedcba9876543210".into(), subject: "Add a page".into(), base: Some("0".into()) };
        let on_page = PostedComment { page: Some("http://localhost:5173/".into()), ..comment(4, "", None, "Bigger.") };
        queue.post(Some(version), [comment(3, "page.html", None, "Center it."), on_page], 3).unwrap();

        let (text, _) = call(&server, "get_review", &json!({}));
        let uncommitted = text.find("## Uncommitted changes\n\n### `README.md`\n\n- **File** (id 2)").unwrap();
        let commit = text
            .find("## Commit 0123456789abcdef (Add the parser)\n\n### `src/parse.rs`\n\n- **File** (id 1)")
            .unwrap();
        assert!(uncommitted < commit, "{text}");
        assert!(
            text.contains(
                "## Version fedcba9876543210 (Add a page)\n\n### The running product\n\n- **Page** (id 4): \
                 `http://localhost:5173/`\n  Bigger.\n\n### `page.html`"
            ),
            "{text}"
        );
        assert!(text.starts_with("The user reviewed changes in /repo"));
    }
}
