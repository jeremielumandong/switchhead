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
