//! A Model Context Protocol server, run with `agent-review --mcp`, through
//! which a coding agent asks the user to review a commit, pulls the reviews
//! and approvals the user posted, and resolves and replies to their comments.
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
use crate::core::queue::{Entry, PostedComment, Queue, ReviewRequest};
use crate::core::review::ReviewedCommit;
use crate::core::wait;

/// Protocol revisions the server can speak; the newest is offered by default.
const PROTOCOL_VERSIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

const INSTRUCTIONS: &str = "Agent Review is a desktop app in which the user reviews the commits of their \
repository, and the uncommitted changes, with the running product beside the code, and leaves comments on them, \
screenshots of the product included. When you finish a set of changes for the user to review, commit them as a draft \
commit that is not pushed, make sure the product is running if it has one, and call request_review, saying whether \
the user reviews the running product or only the code: the commit opens in Agent Review. If you can run a command in \
the background and be told when it ends, run the wait command request_review gives you, so you hear as soon as the \
user posts their review or approves the work. To let the user review someone else's work (a commit, a branch, a pull request), check it out, run \
it, and call request_review, with a base for a branch or a pull request. When the user asks you to pull, fetch or \
check their review, or the wait command ends, call get_review, then do what they ask with it: address each comment, \
and call reply_to_comments to answer or report on a comment, and resolve_comments with the ids of those you \
addressed, or pass the review on, for example to the pull request. When you address a \
review of your own commit, amend it or add a commit on top of it, and call request_review again with replaces set to \
the request you addressed, so the user's comments carry over to the new commit.";

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
            Some("request_review") => self.request_review(&arguments).map(|text| vec![text_content(&text)]),
            Some("get_review") => self.get_review(&arguments).map(|(text, screenshots)| {
                // The screenshots follow the text, in the order it names them.
                std::iter::once(text_content(&text))
                    .chain(screenshots.iter().filter_map(|s| image_content(s)))
                    .collect()
            }),
            Some("resolve_comments") => self.resolve_comments(&arguments).map(|text| vec![text_content(&text)]),
            Some("reply_to_comments") => self.reply_to_comments(&arguments).map(|text| vec![text_content(&text)]),
            other => Err(anyhow::anyhow!("Unknown tool {}", other.unwrap_or("(none)"))),
        };
        match outcome {
            Ok(content) => json!({ "content": content }),
            Err(err) => json!({ "content": [text_content(&format!("{err:#}"))], "isError": true }),
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
        // A branch's tip as the base would also show what it gained since: compare
        // to where the histories meet instead.
        let base = match arguments.get("base").and_then(Value::as_str).map(str::trim).filter(|b| !b.is_empty()) {
            Some(base) => Some(repo.merge_base(&repo.resolve(base)?, &commit)?),
            None => None,
        };
        // Compared to its parent, the commit needs no base.
        let base = base.filter(|base| *base != repo.parent_or_empty(&commit));
        let info = repo.commit(&commit)?;
        let text = |key: &str| arguments.get(key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
        let url = text("url");
        let code_only = match text("review") {
            None => url.is_none(),
            Some("code") => true,
            Some("product") => {
                anyhow::ensure!(
                    url.is_some(),
                    "A review of the product needs the url it runs at. Start it and pass url, or pass review: \"code\""
                );
                false
            }
            Some(other) => anyhow::bail!("review must be \"product\" or \"code\", not {other:?}"),
        };
        let queue = (self.queue_for)(&repo.root);
        let replaces = match arguments.get("replaces") {
            None | Some(Value::Null) => None,
            Some(value) => {
                let id = value.as_u64().context("replaces must be the number of a review request")?;
                anyhow::ensure!(
                    queue.requests().iter().any(|r| r.id == id),
                    "There is no review request {id} to replace"
                );
                Some(id)
            }
        };
        let request = ReviewRequest {
            id: 0,
            requested_at: crate::core::now(),
            commit,
            subject: info.subject,
            base,
            url: url.map(str::to_owned),
            code_only,
            replaces,
        };
        let request = queue.request(request).context("Could not save the review request")?;

        let mut text = format!(
            "Asked the user to review commit {} ({}) as request {}",
            request.short_id(),
            request.subject,
            request.id
        );
        if let Some(base) = &request.base {
            let since = if base == git::EMPTY_TREE { "the start" } else { base.get(..7).unwrap_or(base) };
            let _ = write!(text, ", with all the changes since {since}");
        }
        match (request.product_url(), request.code_only) {
            (Some(url), _) => {
                let _ = write!(text, ", and the product running at {url}");
            }
            (None, true) => text.push_str(", as a review of the code only"),
            (None, false) => {}
        }
        if let Some(replaced) = request.replaces {
            let _ = write!(text, ". It takes the place of request {replaced}: the user's comments carry over to it");
        }
        let _ = write!(
            text,
            ". It opens in Agent Review, where they review it, then post their comments or approve it.\n\nTo hear \
             as soon as they do, run this command in the background, if you can run one and be told when it ends: \
             {}\nIt ends once they post, saying what. Otherwise, wait until they ask you to pull their review. Then \
             call get_review. To address their comments, amend the commit or add a commit on top of it, then call \
             request_review again with replaces: {}.",
            wait::command(&repo.root),
            request.id
        );
        if repo.has_uncommitted_changes().unwrap_or(false) {
            text.push_str(
                "\n\nThere are also uncommitted changes, which this commit does not include. If they belong to it, \
                 commit them and call request_review again.",
            );
        }
        Ok(text)
    }

    /// The approvals not pulled yet and the open comments as a Markdown
    /// prompt, and the screenshots the comments name.
    fn get_review(&self, arguments: &Value) -> Result<(String, Vec<String>)> {
        let (root, queue) = self.queue(arguments)?;
        let entries = queue.load();
        let approvals: Vec<&Entry> = entries.iter().filter(|e| e.review.approved && e.is_pending()).collect();
        let open: Vec<(Option<&ReviewedCommit>, &PostedComment)> =
            entries.iter().flat_map(|e| e.open().map(|c| (e.review.commit.as_ref(), c))).collect();
        if open.is_empty() && approvals.is_empty() {
            let text = format!(
                "There are no open review comments for {}. The user has not posted any, or they are all resolved.",
                root.display()
            );
            return Ok((text, Vec::new()));
        }
        let pulled: Vec<u64> = entries.iter().filter(|e| e.is_pending()).map(|e| e.review.id).collect();
        queue.mark_pulled(&pulled, crate::core::now()).context("Could not record that the review was pulled")?;
        let mut text = render_approvals(&approvals);
        if !open.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&render(&root, &open));
        }
        let screenshots = open.iter().filter_map(|(_, c)| c.screenshot.clone()).collect();
        Ok((text, screenshots))
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

    fn reply_to_comments(&self, arguments: &Value) -> Result<String> {
        let items = arguments
            .get("replies")
            .and_then(Value::as_array)
            .context("Pass the replies as `replies`, each with the comment's `id` and your `text`")?;
        let mut replies = Vec::new();
        let mut resolve = Vec::new();
        for item in items {
            let id = item.get("id").and_then(Value::as_u64).context("Each reply needs the comment's `id`")?;
            let body = item.get("text").and_then(Value::as_str).map(str::trim).unwrap_or_default();
            anyhow::ensure!(!body.is_empty(), "The reply to comment {id} needs a `text`");
            replies.push((id, body.to_owned()));
            if item.get("resolve").and_then(Value::as_bool) == Some(true) {
                resolve.push(id);
            }
        }
        let (_, queue) = self.queue(arguments)?;
        let now = crate::core::now();
        let answered = queue.reply(&replies, now).context("Could not save the replies")?;
        let resolved = queue.resolve(&resolve, now).context("Could not save the resolved comments")?;
        let mut text = if answered.is_empty() {
            "No replies were saved.".to_owned()
        } else {
            format!("Replied to {}.", id_list(&answered))
        };
        if !resolved.is_empty() {
            let _ = write!(text, " Resolved {}.", id_list(&resolved));
        }
        let unknown: Vec<u64> = replies.iter().map(|(id, _)| *id).filter(|id| !answered.contains(id)).collect();
        if !unknown.is_empty() {
            let _ = write!(text, " Unknown: {}.", id_list(&unknown));
        }
        Ok(text)
    }
}

/// The approvals as Markdown, with what to do about them; empty without any.
fn render_approvals(approvals: &[&Entry]) -> String {
    let mut text = String::new();
    for entry in approvals {
        let what = entry.review.commit.as_ref().map_or_else(
            || "the uncommitted changes".to_owned(),
            |c| {
                let request = c.request.map_or_else(String::new, |r| format!(", review request {r}"));
                format!("commit {} ({}){request}", c.id, c.subject)
            },
        );
        let _ = writeln!(text, "The user approved {what}: it needs no more changes.");
    }
    if !text.is_empty() {
        text.push_str(
            "If they told you what to do once they approve, such as pushing the commit or opening a pull request, \
             do it now. Otherwise, tell them you saw the approval and ask what comes next.\n",
        );
    }
    text
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
             root. Line numbers refer to the files when the comment was written, or for a commit, to the files as \
             that commit left them. Address each comment, then call resolve_comments with the ids of \
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
                let _ = write!(text, "## Commit {} ({})", commit.id, commit.subject);
                if let Some(base) = &commit.base {
                    let _ = write!(text, ", with the changes since {base}");
                }
                if let Some(request) = commit.request {
                    let _ = write!(text, ", review request {request}");
                }
                text.push_str("\n\n");
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
                    let screenshot = comment.screenshot.as_deref();
                    feedback::write_page_comment(&mut text, comment.page.as_deref(), screenshot, &comment.body, &tag);
                } else {
                    feedback::write_comment(&mut text, comment.anchor.as_ref(), &comment.body, &tag);
                }
            }
            text.push('\n');
        }
    }
    text.trim_end().to_owned() + "\n"
}

fn text_content(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

/// The PNG at `path` as image content, if it can be read.
fn image_content(path: &str) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    Some(json!({ "type": "image", "data": base64(&bytes), "mimeType": "image/png" }))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0_u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
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
            "description": "Ask the user to review the commit at HEAD, and the running product if there is one: \
                work you committed (as a draft commit that is not pushed), or someone else's commit, branch or pull \
                request you checked out. The commit opens in Agent Review, which shows the changes it made to its \
                parent, or all the changes since a base you pass.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "Where the user can open the running product, such as \
                            http://localhost:5173. Make sure it is running first. Leave it out when there is \
                            nothing to run.",
                    },
                    "review": {
                        "type": "string",
                        "enum": ["product", "code"],
                        "description": "What the user reviews: \"product\" when the changes show in the \
                            running product (pages, layout, behavior the user can click through), which opens beside \
                            the code and needs url; \"code\" when they don't show (internals, a backend, tests, \
                            tooling), for a review of the code only. Defaults to \"product\" with a url, \"code\" \
                            without.",
                    },
                    "base": {
                        "type": "string",
                        "description": "What to compare the commit to, when the work spans more commits than \
                            HEAD: a commit or revision such as HEAD~3, or for a branch or pull request, the branch it \
                            goes into, such as origin/main. The commit is compared to where their histories meet.",
                    },
                    "replaces": {
                        "type": "integer",
                        "description": "The review request this commit takes the place of, given when you \
                            requested it and in get_review: usually the one whose review you addressed, when you \
                            amended its commit or added one on top. The user's comments on it carry over to the new \
                            commit.",
                    },
                    "project": project,
                },
            },
            "annotations": { "readOnlyHint": false, "idempotentHint": false },
        },
        {
            "name": "get_review",
            "title": "Get review comments",
            "description": "Get the open comments the user posted in Agent Review on the commits and uncommitted \
                changes of this repository, with the lines or product pages each one is about, and screenshots of \
                the product, and the approvals they posted since you last pulled. Address each comment, reply to \
                it with reply_to_comments and call resolve_comments with the ids of the comments you addressed, or \
                do what the user asked with the review.",
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
        {
            "name": "reply_to_comments",
            "title": "Reply to review comments",
            "description": "Answer review comments: say what you changed for one, answer a question, or explain \
                why you disagree or what is unclear. The user reads your replies under their comments in Agent \
                Review while you work. Set resolve on the replies to comments you addressed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "replies": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "integer", "description": "The comment's id, as shown by get_review." },
                                "text": { "type": "string", "description": "Your reply, in a sentence or two." },
                                "resolve": {
                                    "type": "boolean",
                                    "description": "Also mark the comment resolved: you addressed it.",
                                },
                            },
                            "required": ["id", "text"],
                        },
                    },
                    "project": project,
                },
                "required": ["replies"],
            },
            "annotations": { "readOnlyHint": false, "idempotentHint": false },
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
        PostedComment { id: 0, note_id, path: path.into(), anchor, page: None, screenshot: None, body: body.into() }
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
        assert_eq!(names, ["request_review", "get_review", "resolve_comments", "reply_to_comments"]);

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

    #[test]
    fn replies_to_comments_and_resolves_some() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        let queue = Queue::at(dir.path().to_path_buf());
        queue.post(None, [comment(1, "a.rs", None, "Rename."), comment(2, "a.rs", None, "Why?")], 1).unwrap();
        let replies = json!({ "replies": [
            { "id": 1, "text": "Renamed it to parse.", "resolve": true },
            { "id": 2, "text": "To keep it fast." },
            { "id": 7, "text": "?" },
        ] });
        let (text, error) = call(&server, "reply_to_comments", &replies);
        assert!(!error);
        assert_eq!(text, "Replied to comments 1, 2. Resolved comment 1. Unknown: comment 7.");
        let entry = &queue.load()[0];
        assert_eq!(entry.replies(&entry.review.comments[1])[0].body, "To keep it fast.");
        assert_eq!(entry.open().map(|c| c.id).collect::<Vec<_>>(), [2]);

        let (text, error) = call(&server, "reply_to_comments", &json!({ "replies": [{ "id": 2, "text": " " }] }));
        assert!(error && text.contains("needs a `text`"), "{text}");
    }

    #[test]
    fn hands_over_approvals_once() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        let queue = Queue::at(dir.path().to_path_buf());
        let commit = ReviewedCommit {
            id: "0123456789abcdef".into(),
            subject: "Add a page".into(),
            base: None,
            request: Some(2),
        };
        queue.approve(Some(commit), 1).unwrap();
        let (text, _) = call(&server, "get_review", &json!({}));
        assert!(
            text.starts_with("The user approved commit 0123456789abcdef (Add a page), review request 2: it needs no"),
            "{text}"
        );
        assert!(text.contains("tell them you saw the approval"));
        let (text, _) = call(&server, "get_review", &json!({}));
        assert!(text.starts_with("There are no open review comments"), "{text}");
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
    fn requests_a_review_of_the_commit_at_head() {
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
        assert!(!error && text.contains("(Start) as request 1, as a review of the code only. It opens"), "{text}");

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
        assert_eq!((start.base.as_deref(), page.base.as_deref()), (None, None));
        assert_eq!(page.subject, "Add a page");
        assert_eq!(page.url.as_deref(), Some("http://localhost:5173"));

        // The parent as the base is the commit alone; an earlier one, everything since.
        let (_, error) = call(&server, "request_review", &json!({ "base": "HEAD~1" }));
        assert!(!error);
        assert_eq!(queue.requests()[2].base, None);
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "Add c"]);
        let (text, error) = call(&server, "request_review", &json!({ "base": "HEAD~2" }));
        assert!(!error && text.contains(&format!("with all the changes since {}", start.short_id())), "{text}");
        assert!(text.contains("request_review again with replaces: 4."), "{text}");
        assert_eq!(queue.requests()[3].base.as_ref(), Some(&start.commit));
        let (text, error) = call(&server, "request_review", &json!({ "base": "nope" }));
        assert!(error && text.contains("nope is not a commit"), "{text}");

        let (text, error) = call(&server, "request_review", &json!({ "replaces": 4 }));
        assert!(!error && text.contains("as request 5") && text.contains("takes the place of request 4: the user's"));
        assert_eq!(queue.requests()[4].replaces, Some(4));
        let (text, error) = call(&server, "request_review", &json!({ "replaces": 9 }));
        assert!(error && text.contains("There is no review request 9 to replace"), "{text}");

        // What the user reviews: the product beside the code, or the code only.
        let (text, error) = call(&server, "request_review", &json!({}));
        assert!(!error && text.contains("as a review of the code only") && text.contains(" --wait "), "{text}");
        assert!(queue.requests()[5].code_only);
        let (text, error) = call(&server, "request_review", &json!({ "url": "http://localhost:1", "review": "code" }));
        assert!(!error && text.contains("as a review of the code only"), "{text}");
        assert_eq!(queue.requests()[6].product_url(), None);
        let (text, error) = call(&server, "request_review", &json!({ "review": "product" }));
        assert!(error && text.contains("needs the url it runs at"), "{text}");
        let (text, error) = call(&server, "request_review", &json!({ "review": "both" }));
        assert!(error && text.contains("must be \"product\" or \"code\""), "{text}");
    }

    #[test]
    fn groups_comments_on_commits_after_the_uncommitted_ones() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path());
        let queue = Queue::at(dir.path().to_path_buf());
        let commit = ReviewedCommit {
            id: "0123456789abcdef".into(),
            subject: "Add the parser".into(),
            base: None,
            request: None,
        };
        queue.post(Some(commit), [comment(1, "src/parse.rs", None, "Handle errors.")], 1).unwrap();
        queue.post(None, [comment(2, "README.md", None, "Document it.")], 2).unwrap();
        let range = ReviewedCommit {
            id: "fedcba9876543210".into(),
            subject: "Add a page".into(),
            base: Some("0".into()),
            request: Some(3),
        };
        let shot = dir.path().join("shot.png");
        std::fs::write(&shot, b"png").unwrap();
        let on_page = PostedComment {
            page: Some("http://localhost:5173/".into()),
            screenshot: Some(shot.display().to_string()),
            ..comment(4, "", None, "Bigger.")
        };
        queue.post(Some(range), [comment(3, "page.html", None, "Center it."), on_page], 3).unwrap();

        let (text, _) = call(&server, "get_review", &json!({}));
        let uncommitted = text.find("## Uncommitted changes\n\n### `README.md`\n\n- **File** (id 2)").unwrap();
        let commit = text
            .find("## Commit 0123456789abcdef (Add the parser)\n\n### `src/parse.rs`\n\n- **File** (id 1)")
            .unwrap();
        assert!(uncommitted < commit, "{text}");
        let shown = format!(
            "## Commit fedcba9876543210 (Add a page), with the changes since 0, review request 3\n\n### The running product\n\n- **Page** (id 4): \
             `http://localhost:5173/`\n  Screenshot, drawn on: `{}`\n  Bigger.\n\n### `page.html`",
            shot.display()
        );
        assert!(text.contains(&shown), "{text}");
        assert!(text.starts_with("The user reviewed changes in /repo"));

        // The screenshot follows the text as an image.
        let request = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "get_review", "arguments": {} },
        });
        let response: Value = serde_json::from_str(&server.handle(&request.to_string()).unwrap()).unwrap();
        let content = response["result"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[1], json!({ "type": "image", "data": "cG5n", "mimeType": "image/png" }));
    }

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xfe, 0x00]), "//4A");
    }
}
