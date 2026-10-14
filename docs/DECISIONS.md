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
