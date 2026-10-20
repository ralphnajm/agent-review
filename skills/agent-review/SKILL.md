---
name: agent-review
description: Hand your work to the user for review in Agent Review, a desktop app in which they click through the running product and read the code changes, then pull their comments back and address them. Use it whenever the agent-review MCP server's tools (request_review, get_review, resolve_comments) are available and you finish a feature, fix or other set of changes the user asked for, and whenever the user mentions Agent Review, their review, review comments, "pull my review", or asks you to get something ready for them to look at, even if they don't name the app.
---

# Agent Review

Agent Review is a desktop app in which the user reviews your work. You ask for
a review of a *version*: a commit of yours, together with the running product
if there is one. The user clicks through the product inside the app, reads the
code changes, and leaves comments on lines, files and pages of the product.
When they post the review, you pull the comments, address them, and ask for a
review of the next version. Nothing reaches you unless you pull it: the app
never interrupts you.

You talk to the app through its MCP server, `agent-review --mcp`, which gives
you three tools: `request_review`, `get_review` and `resolve_comments`. If they
are missing, see [Setup](#setup).

## Hand over a finished set of changes

Do this once the work the user asked for is done and checked, not after every
edit: each request puts a banner in front of the user, so it should mean
"ready for you".

1. **Commit the work as a draft.** The version under review is a commit, so
   commit your changes, on the current branch, with a message that says what
   the user is about to review: the subject is the version's title in the app,
   and the body is shown beside it. Do not push: the commit stays a draft you
   can still replace until the user is happy. If the user does not want you to
   commit, skip the request; they can still review the uncommitted changes in
   the app.
2. **Make sure the product runs**, if it has one (a web app, a site, a
   service with pages). Start it the way the project does (its README,
   `package.json` scripts, Makefile, …), in the background so it keeps running
   after your command returns, and check that the address answers, for example
   with `curl -sf <url>`. If it was already running, make sure it serves the
   committed code (dev servers usually reload by themselves).
3. **Call `request_review`** with `url`: the address of the page the change is
   about, such as `http://localhost:5173/pricing`, so the review opens right
   where it matters. Leave `url` out when there is nothing to run, such as a
   library or a command-line tool. By default the version covers everything
   since the branch left its upstream, or the commit alone when there is no
   upstream; pass `base` (a commit or revision such as `HEAD~3`) when the work
   spans more commits than that.
4. **Tell the user** in a sentence that the version is ready in Agent Review,
   then stop. Don't wait or poll for the review: the user will ask you to pull
   it when they have posted it.

If `request_review` warns about uncommitted changes, they are not part of the
version: commit them if they belong to it and request again.

## Address a review

When the user asks you to pull, fetch or check their review:

1. **Call `get_review`.** It returns the open comments as Markdown, grouped by
   what was reviewed (a version, a commit, or the uncommitted changes), each
   with an id:
   - Comments on **code** name a file and the lines they are about, quoted as
     they were when the comment was written. Line numbers may have moved
     since: find the lines by their content.
   - Comments on **the running product**, under "The running product", name
     the page they were written on. Find the code behind that page from its
     route, its visible text and the components it renders.
2. **Address each comment.** A comment is the user's request in their own
   words: make the change it asks for, answer it if it is a question, and if
   you disagree or something is unclear, say so instead of guessing. Fix the
   root cause rather than the one spot the comment points at when the same
   problem appears elsewhere.
3. **Call `resolve_comments`** with the ids of the comments you addressed. The
   user sees them as resolved. Leave out the ones you did not address, and tell
   the user why.
4. **Hand over the next version.** Amend the draft commit (`git commit
   --amend`), or add commits on top of it if the user prefers to keep the
   history; make sure the product runs the new code; and call `request_review`
   again with the same `url` or the page the changes are about. The user's
   comments and reviewed files carry over to the new version, and files that
   changed open again for another look.

## Setup

If the tools are not available, Agent Review's MCP server is not configured
for you. Tell the user, and point them to how to add it: the server is the
command `agent-review --mcp` (stdio), for example
`claude mcp add --scope user agent-review -- agent-review --mcp` in Claude
Code, or an `mcpServers` entry with `"command": "agent-review", "args":
["--mcp"]` in agents configured with JSON. The app itself is at
https://github.com/ralphnajm/agent-review. Without the server, the user can
still copy their review from the app and paste it to you: address it the same
way.
