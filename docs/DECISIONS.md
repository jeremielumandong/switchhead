# Decisions log

Non-obvious calls made while building switchhead. Newest last. Each entry: context,
decision, consequences.

## 2026-10-13 — Same UI stack as Switchyard

Jeremie asked to reuse as much of Switchyard's UI as possible. switchhead depends on
`gpui-kit =0.7.1` (GPUI + gpui-component in lockstep) with the same gpui-fast
`[patch.crates-io]` rev (`598306f`) and starts from Switchyard's `Cargo.lock`, so ported
modules compile unchanged. Theme, Omarchy theme following, appearance/zoom, UI helpers,
split panes, log file and the agents crate are ported, not rewritten (CLAUDE.md table).

## 2026-10-13 — gix for reads, the git CLI for writes

Speed is the top goal, so the log, graph, refs and commit diffs read the object database in
process with gitoxide 0.89 (MIT OR Apache-2.0); no process spawn on those paths. gitoxide's
merge, rebase and push support is still incomplete, and only the real `git` honors every
hook, credential helper, signing setup, LFS filter and config the user has, so every write
goes through `git`. Working-tree status uses `git status --porcelain=v2` for its fsmonitor
and untracked-cache support. Revisit status if gix's status proves faster on big trees.

## 2026-10-13 — Assistant proposes, the user applies

Coding CLIs help with commit messages, conflicts and git commands, but never change the
repository. They run outside it (private temp dir, built-in tools off) with a read-only git
MCP server; their output is a proposal the app shows and the user applies. Same model as
Switchyard's agent safety rules.

## 2026-10-13 — Workspace folders

Opening a folder that is not inside a repository scans it for repositories (parallel, depth
4 by default, skipping dependency/build dirs and hidden dirs, never descending into a found
repo) and lists them so any can be opened as a tab.

## 2026-10-13 — License MIT; toolchain

The repository was created with an MIT license, so the workspace is MIT (Switchyard is
Apache-2.0; ported code is the same author's). `rust-toolchain.toml` pins 1.98.1 like
Switchyard; the workspace `rust-version` is 1.97 because the dev container cannot download
1.98.1 (see Gotchas).

## 2026-10-14 — Rebase messages through `exec` lines

PLAN M1-11 suggested a `GIT_EDITOR` script for reworded messages. A script has to tell
which commit git is asking about, and must be a shell script on every platform. Instead,
`rebase::run` writes each new message to a file and follows the commit with
`exec git commit --amend --allow-empty --cleanup=strip -q -F '<file>'` (after the last
squash/fixup of a group). `GIT_SEQUENCE_EDITOR` is just `cp '<todo>'`. Hooks still run, and
a failing amend stops the rebase like any `exec`.

## 2026-10-14 — Ported design-system modules keep their unused parts

`theme.rs`, `appearance.rs`, `ui.rs`, `split.rs` and `omarchy.rs` are ported whole from
Switchyard so the two apps keep one design system and later fixes port cleanly. Parts that
switchhead does not use yet (split panes, terminal colors, font choices) stay, with
`#![allow(dead_code)]` at the top of those modules only; switchhead's own modules get no
such allowance.

## 2026-10-14 — Worker threads talk to the UI over tokio channels

Log walks, git operations and clones run on their own threads (they can take seconds) and
send results to a GPUI task over `tokio::sync::mpsc` (the `sync` feature only, no runtime;
tokio is already approved "sync only"). Short one-off reads (classify a path, scan a
folder) use GPUI's background executor.

## 2026-10-14 — Syntax colors from gpui-kit's bundled grammars

M2-5 asks for syntax highlighting "via gpui-component". gpui-kit only compiles its
highlighter with a `tree-sitter*` feature, so the workspace enables
`tree-sitter-languages` (35 grammars, the same set Switchyard can pick from). No new crate
is added by hand; the grammars come with the approved gpui-kit. The diff view highlights
the old and new file once on a background thread and cuts the spans per diff line
(`diff_model::highlight`); files over 1 MB show without colors.

## 2026-10-14 — Polling `.git` metadata instead of a file watcher (for now)

M2-7 needs changes made in a terminal to show up. No watcher crate is approved, so
`switchhead_git::watch::fingerprint` hashes the size and mtime of HEAD, the reflogs,
packed and loose refs, operation markers and the index (metadata only, no reads), and the
repo tab polls it every second off the UI thread and again when the window is focused.
That is cheap for `.git` (tens of files) but would not scale to a large work tree, so
work-tree status refresh will either use `notify` (asked; pending Jeremie's answer) or
refresh on focus and on index changes only.

## 2026-10-14 — GPUI Kit 0.7.3 on the application-patched GPUI Fast stack

Jeremie asked to move to GPUI Kit 0.7.3 on GPUI Fast, as AgentOpsChat already does.
switchhead now depends on `gpui-kit =0.7.3` and vendors AgentOpsChat's `vendor/gpui-base`
(Kit 0.7.3's base with AgentOpsChat's input, selection and accessibility contributions) and
`vendor/gpui-fast` (GPUI Fast at `598306f` with its keyboard, accessibility, retained
rendering and Linux patches, plus `compat/*` facades that publish it as `gpui-pre` 0.3.9).
`[patch.crates-io]` points the `gpui-base` and `gpui-pre*` names at those paths, and
`[patch."https://github.com/longbridge/gpui-fast"]` points `gpui` / `gpui_linux` at the
local copies, so the graph has one GPUI core. All of it is Apache-2.0. Switchyard stays on
0.7.1 with upstream GPUI Fast for now, so modules ported from it may need small API fixes.
The copy is kept byte-identical to AgentOpsChat's; changes to the stack land there first.

## 2026-10-14 — When Local Changes re-reads status

Without an approved file watcher (`notify` is pending Jeremie's answer), status is re-read
when the repo tab loads or is refreshed, after every stage / unstage, when the index
fingerprint from `git::watch` changes (staging from a terminal), and when the window comes
back to the front (files edited in another app). Work-tree edits made while switchhead stays
in front show on Refresh. `git status` is not polled: on a large tree it costs far more
than the `.git` metadata check.

## 2026-10-14 — Commit box: recent messages and the template

Recent commit messages are kept once for the whole app (20, newest first, in state.json),
not per repository, so a message reused across a monorepo split is still there; they are
stored as git keeps them (`--cleanup=strip`). The repository's `commit.template` fills the
box when it is empty, and an untouched template does not count as a message. Commit runs
`git commit -F -` with the message on stdin, so hooks and signing are
git's own. Spell check needs a crate outside the approved list and is a follow-up.

## 2026-10-14 — notify for file events

Jeremie approved `notify` (CC0-1.0 / MIT, already in the tree through gpui-component, so no
new download). `git::watch::Watcher` watches the work tree and the git dir recursively with
native events (inotify, FSEvents, ReadDirectoryChangesW). Events are debounced (150 ms, at
most 1 s while they keep coming) and reported as "something in `.git`" (the app compares
`watch::fingerprint` to tell refs from the index) and "something in the work tree" (status
is re-read). Reads (access events), `.git/objects` and `*.lock` files are ignored. The
`.git` poll stays as a safety net, every 10 s while events arrive and every second when the
OS refuses the watch (on Linux, a tree with more folders than
`fs.inotify.max_user_watches`). Ignored folders (`target/`, `node_modules/`) still send
events, so a build re-reads status about once a second; filtering them is a follow-up.

## 2026-10-15 — Git-flow without the git-flow extension

Like Fork, switchhead does git-flow with plain `git` (`switchhead_git::flow`) instead of
calling the `git flow` extension, which most installs do not have. The settings go in the
repository config under the extension's own keys (`gitflow.branch.master`,
`gitflow.branch.develop`, `gitflow.prefix.feature|release|hotfix|versiontag`), so a
repository set up by either tool works in the other. Finishing merges with `--no-ff`, tags
releases and hotfixes (annotated, default message "Release 1.0"), merges a hotfix into a
release branch in progress instead of develop (as git-flow does), and deletes the branch
with `-d`. Each step is skipped when already done, so after a conflict the user resolves,
commits and finishes again. Pushing after a finish, support branches and bugfix branches
are left out.

## 2026-10-15 — Custom commands run through the shell, with quoted variables

Custom commands are shell commands (`sh -c` in the repository's folder; `cmd /S /C` on
Windows), so pipes, `&&` and scripts work as users expect from Fork. Variables (`${repo}`
`${branch}` `${sha}` `${ref}` `${file}` `${remote}`) are inserted as they are when they
hold only plain characters, else quoted for the shell (single quotes on Unix), so a branch
or file name cannot add commands; on Windows a value with `"` or `%` is refused because cmd
has no safe way to quote it. A variable with no value where the command runs (`${file}` on
a commit) stops the run with a message. Output goes to the activity log line by line, like
git operations. They are the user's own commands and run without the agent rules (those
apply to the coding CLI, not to the user).
