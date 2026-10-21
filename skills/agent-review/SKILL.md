---
name: agent-review
description: Hand work to the user for review in Agent Review, a desktop app in which they click through the running product, draw on screenshots of it and read the code changes, then pull their review back and act on it. Use it whenever the agent-review MCP server's tools (request_review, get_review, resolve_comments) are available and you finish a feature, fix or other set of changes the user asked for; whenever the user wants to review someone else's work, such as a colleague's commit, a branch or a pull request ("I want to review John's last commit", "let me review PR 42"); and whenever the user mentions Agent Review, their review, review comments, "pull my review", or asks you to get something ready for them to look at, even if they don't name the app.
---

# Agent Review

Agent Review is a desktop app in which the user reviews *versions*: a commit,
together with the product running from it if there is one, compared to a base
commit. The user clicks through the product inside the app, draws on
screenshots of it (marker, arrows, boxes, sticky notes, text) and comments on
the code's lines and files. When they post the review, you pull it and do what
they ask with it: address it yourself, or pass it on, for example to the pull
request it is about. Nothing reaches you unless you pull it, and the user can
only review what you ask them to review.

You talk to the app through its MCP server, `agent-review --mcp`, which gives
you three tools: `request_review`, `get_review` and `resolve_comments`. If they
are missing, see [Setup](#setup).

## Hand over your own work

Do this once the work the user asked for is done and checked, not after every
edit: each request puts a banner in front of the user, so it should mean
"ready for you".

1. **Commit the work as a draft.** The version under review is a commit, so
   commit your changes on the current branch, with a message that says what
   the user is about to review: the subject is the version's title in the app,
   and the body is shown beside it. Do not push: the commit stays a draft you
   can still replace until the user is happy.
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
   spans more commits than that. The reply gives the request's number: you
   pass it as `replaces` when you hand over the next version.
4. **Tell the user** in a sentence that the version is ready in Agent Review,
   then stop. Don't wait or poll for the review: the user will ask you to pull
   it when they have posted it.

If `request_review` warns about uncommitted changes, they are not part of the
version: commit them if they belong to it and request again.

## Hand over someone else's work

The user can review anything you can check out: a colleague's commit, a
branch, a pull request. You prepare it like your own work, without committing.

1. **Keep the user's work safe.** Checking something out changes the working
   tree. If `git status` shows uncommitted changes, ask the user whether to
   stash them (and restore them afterwards) before going on. Note the branch
   they are on, to come back to it.
2. **Check the work out:**
   - a commit: `git fetch` if needed, then `git checkout <commit>`;
   - a branch: `git fetch origin <branch>` and `git checkout <branch>`;
   - a GitHub pull request: `gh pr checkout <number>`, or
     `git fetch origin pull/<number>/head:pr-<number>` and check that out.
     Note the branch it goes into (`gh pr view <number> --json baseRefName`).
3. **Make sure the product runs** from that checkout, as for your own work
   (install dependencies if the work changed them).
4. **Call `request_review`** with:
   - `title`: whose work it is and what, such as `"PR #42 by John: Add
     pricing"` or `"John's last commit: Fix the login redirect"`, since the
     commit's own subject doesn't say;
   - `base`: what the work is compared to. For a pull request or a branch,
     the branch it goes into, such as `origin/main`: the version is compared
     to where their histories meet, so it shows only that work's changes. For
     a single commit, leave it out: the commit is compared to its parent.
   - `url`, as for your own work.
5. **Tell the user** it is ready to review, and stop.

Once they have posted their review and you have done what they asked with it,
go back to the branch the user was on, and restore what you stashed.

## Pull a review and act on it

When the user asks you to pull, fetch or check their review:

1. **Call `get_review`.** It returns the open comments as Markdown, grouped by
   the version they are about (its commit and title), each with an id, and
   then the screenshots as images:
   - Comments on **code** name a file and the lines they are about, quoted as
     they were when the comment was written. "Line 12" is line 12 of the file
     as the version left it; "Removed line 12" is line 12 of the file before.
     Line numbers may have moved since: find the lines by their content.
   - Comments on **the running product**, under "The running product", name
     the page they were written on and the screenshot the user drew on, whose
     file path is given and whose image follows the text. Look at the image:
     the boxes, arrows and marker show what the comment is about, and sticky
     notes and text on it are repeated in the comment. Find the code behind a
     page from its route, its visible text and the components it renders.
2. **Do what the user asks with the review.** When they don't say, and the
   version is your own work, address it:
   - Make the change each comment asks for, answer it if it is a question, and
     if you disagree or something is unclear, say so instead of guessing. Fix
     the root cause rather than the one spot a comment points at when the same
     problem appears elsewhere.
   - Then hand over the next version: amend the draft commit (`git commit
     --amend`), or add commits on top of it if the user prefers to keep the
     history; make sure the product runs the new code; and call
     `request_review` again with `replaces`: the number of the review request
     you addressed, which `get_review` gives with each version ("review
     request 3"). The new version takes the old one's place: if the user has
     it open, Agent Review switches to it by itself. Their comments and
     reviewed files carry over, and files that changed open again for another
     look.

   For someone else's work, the user usually wants the review passed on. Do
   that with the tools you have, for example:
   - **A GitHub pull request:** post it as one review with `gh api
     repos/{owner}/{repo}/pulls/{number}/reviews`, with the general comments
     in `body` and each code comment in `comments` as `path`, `line` and
     `side` (`RIGHT` for "Line N", `LEFT` for "Removed line N"); use `event`
     `COMMENT`, or `REQUEST_CHANGES` or `APPROVE` if the user says so.
     GitHub's API can't attach images: describe what a screenshot shows in the
     comment, and offer to share the image files some other way.
   - **A message to the author**, on Slack or by email, through the tools you
     have, with the comments grouped by file and page.
   - **Changes yourself**, on the author's branch, if the user asks you to fix
     things rather than report them.
3. **Call `resolve_comments`** with the ids of the comments you dealt with,
   whether you addressed them or passed them on. The user sees them as
   resolved. Leave out the ones you didn't deal with, and tell the user why.

## Setup

If the tools are not available, Agent Review's MCP server is not configured
for you. Tell the user, and point them to how to add it: the server is the
command `agent-review --mcp` (stdio), for example
`claude mcp add --scope user agent-review -- agent-review --mcp` in Claude
Code, or an `mcpServers` entry with `"command": "agent-review", "args":
["--mcp"]` in agents configured with JSON. The app itself is at
https://github.com/ralphnajm/agent-review. Without the server, the user can
still copy their review from the app and paste it to you: act on it the same
way.
