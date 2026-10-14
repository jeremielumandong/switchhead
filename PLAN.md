# PLAN — switchhead

Work top to bottom. Tick a task when it meets the definition of done in `CLAUDE.md`, with a
one-line note. Milestones M3–M6 together are Fork feature parity.

## M0 — Workspace

- [x] M0-1 Cargo workspace on Switchyard's stack: `gpui-kit =0.7.1`, gpui-fast patch (rev
      `598306f`), Switchyard's `Cargo.lock` as the version seed, rustfmt/clippy configs, Geist
      fonts and Lucide license copied. *Pushed on `claude/scaffold`.*
- [x] M0-2 CI (`.github/workflows/ci.yml` from Switchyard: fmt, clippy, test on Linux/macOS/Windows).
      *fmt job plus clippy + tests on all three OSes; no integration jobs needed (tests use `git`).*
- [ ] M0-3 README with screenshots, install, build instructions.

## M1 — Git engine (`crates/git`)

- [x] M1-1 `cli::Git`: run / run_with (stdin, env) / run_streaming (progress lines, cancel);
      prompt-free environment; errors carry git's message. *Tests written.*
- [x] M1-2 `discover`: `classify` (repo vs folder, walking up), `scan` (parallel, depth limit,
      skip list, never inside a found repo, linked worktrees flagged). *Tests written.*
- [x] M1-3 `repo::Repo`: thread-safe gix handle, root/git dir/name, `head()`, `state()`
      (merge/rebase/...), identity, `resolve(rev)`. *Tests written.*
- [ ] M1-4 `log`: streaming walk in date order (gix topo `DateOrder` + commit-graph) over all
      refs (or HEAD / chosen branches), batches into a columnar `CommitLog` (ids, parents,
      interned authors, times, summary arena, id→row map). Cancellable. Benchmark on a big
      repo (git/git, linux) and log first-batch and full times. *`log::walk` / `CommitLog`
      (mailmap applied, authors interned, summaries in one arena), `examples/log_bench.rs`.
      git/git (85,563 commits, `--all`): with a commit-graph file 94 ms to the first batch,
      1.26 s in all (`git log --all --date-order` 2.4 s); without one 1.9 s / 3.2 s, see
      Follow-ups. linux not measured yet.*
- [ ] M1-5 `graph`: incremental lane layout (lanes hold the next expected commit; merges join,
      branches fork; colors follow first-parent lines); rows as top/bottom segments in one
      arena. Tests on linear, branch/merge, octopus, criss-cross, and out-of-order histories.
      *`graph::Layout` / `GraphRows`; each `log::Batch` carries its rows. Tests check that
      every row's bottom edge meets the next row's top edge. git/git's widest row is 282
      lanes (git's own graph ~190), see Follow-ups.*
- [ ] M1-6 `refs`: local branches (upstream, ahead/behind via `for-each-ref --format
      %(upstream:track)`), remotes, tags (annotated peeled), stashes (reflog), HEAD;
      decorations map id → labels for the graph. *`refs::load` (gix, tags peeled, stash
      reflog) and `refs::load_tracking` (one `for-each-ref`, can arrive later);
      `Refs::decorations`.*
- [ ] M1-7 `status`: `git status --porcelain=v2 -z --branch` parser (ordinary, renamed,
      unmerged, untracked, ignored), staged vs unstaged lists, branch header.
      *`status::status` / `parse`; paths are `PathBuf` (not always UTF-8).*
- [x] M1-8 `diff`: commit file list (gix tree diff, renames), per-file blob diff (imara
      histogram + word diff), unified-diff parser for `git diff` / `git diff --cached`;
      binary and large-file detection; image file detection for image diff. *`diff::commit_files`
      / `tree_files` (gix, `-M` renames), `diff_text` / `diff_blobs` (histogram, context,
      `-b` / `-w`, word ranges), `parse_unified` + `worktree_diff` / `index_diff`; one
      `TextDiff` arena for both. `git diff` output gets no word ranges yet (Follow-ups).*
- [x] M1-9 Partial staging: build a patch from selected hunks or lines and apply with
      `git apply --cached --recount` (stage) or `--reverse` (unstage), and to the work tree
      (discard lines). Round-trip tests. *`stage::build_patch` / `apply_lines` /
      `apply_hunks` for Stage, Unstage, Discard; `-U0` diffs too. New and deleted files
      stage only whole.*
- [x] M1-10 `conflict`: parse conflict markers (diff3 and zdiff3 styles) into regions;
      resolve per region (ours / theirs / both / custom); read `:1:` `:2:` `:3:` stages.
      *`conflict::parse` (merge, diff3, zdiff3, any marker size, CRLF kept),
      `ConflictFile::resolve`, `stages` (`ls-files -u`).*
- [ ] M1-11 `rebase`: interactive rebase todo model (pick, reword, edit, squash, fixup, drop,
      reorder) written through `GIT_SEQUENCE_EDITOR`; reword messages via `GIT_EDITOR` script.
- [ ] M1-12 `blame`: `git blame --porcelain` (incremental) parser; file history (`log --follow`).
- [ ] M1-13 Operations API on `Repo` for every write in M3–M6, each a thin, tested wrapper.

## M2 — App shell (`crates/app`)

- [ ] M2-1 Port from Switchyard: `theme.rs`, `omarchy.rs`, `appearance.rs`, `ui.rs`,
      `split.rs`, `log_file.rs`, `AppAssets` (see CLAUDE.md table). Theme tests pass.
- [ ] M2-2 Window: title bar with repo tabs, toolbar (Fetch, Pull, Push, Stash, Pop, Branch,
      Terminal, Assistant), 48px rail, sidebar, center, status bar; zoom; theme picker.
- [ ] M2-3 Repository manager: recent repos, Open Folder (repo → tab; plain folder → scan and
      list its repos grouped by subfolder, open any as a tab), Clone, Init; repos persist
      (`directories`, JSON).
- [ ] M2-4 Commit graph view: virtualized list (graph lanes painted on a canvas per row, ref
      chips, summary, author, date, short id), keyboard navigation, streaming fill, jump to
      HEAD, filter by branch.
- [ ] M2-5 Commit details: header (author, committer, date, parents, refs, full message),
      changed files (list and tree), file diff (inline and side-by-side, word highlights,
      ignore whitespace, context lines, syntax highlighting via gpui-component).
- [ ] M2-6 Command palette (Quick Launch) with every command; keyboard shortcuts like Fork's.
- [ ] M2-7 File system watcher: refresh status and refs when the work tree or `.git` changes.

## M3 — Local changes (Fork parity)

- [ ] M3-1 Local Changes view: unstaged / staged lists, stage / unstage file, all, selected.
- [ ] M3-2 Hunk and line staging, unstaging and discarding from the diff view.
- [ ] M3-3 Commit box: subject/body, amend (loads previous message), sign-off, skip hooks,
      commit message templates, recent messages, spell check, char counters.
- [ ] M3-4 Discard changes, delete untracked, ignore file/pattern (`.gitignore`), assume
      unchanged / skip worktree.
- [ ] M3-5 Image diff (side by side, swipe, onion skin) and binary file info.
- [ ] M3-6 Merge conflict helper: list conflicted files, choose ours/theirs, built-in 3-way
      merge editor (base/ours/theirs/result), open external merge tool, mark resolved,
      continue / abort banner for merge, rebase, cherry-pick, revert.

## M4 — Branches, remotes, history (Fork parity)

- [ ] M4-1 Sidebar: branches (tree by `/`), remotes, tags, stashes, submodules, worktrees;
      filter box; ahead/behind badges; checkout by double-click.
- [ ] M4-2 Branch ops: create (from any commit), rename, delete (local/remote), checkout
      (with tracking), set upstream, merge into current (ff / no-ff / squash), rebase current
      onto, compare branches, push/delete remote branch.
- [ ] M4-3 Fetch (all remotes, prune), Pull (merge / rebase, autostash), Push (force-with-lease,
      tags, set upstream) with progress, cancel and errors in the activity panel.
- [ ] M4-4 Commit context menu: checkout, create branch/tag here, cherry-pick, revert, reset
      (soft / mixed / hard), interactive rebase from here, copy id/message, create patch,
      open on GitHub/GitLab/Bitbucket.
- [ ] M4-5 Interactive rebase editor (drag to reorder, pick/reword/edit/squash/fixup/drop).
- [ ] M4-6 Tags: create (lightweight / annotated), delete, push, checkout.
- [ ] M4-7 Stashes: create (with untracked, staged only, keep index), apply, pop, drop,
      view diff, create branch from stash.
- [ ] M4-8 Remotes: add / edit / remove, fetch one, prune.
- [ ] M4-9 Search: commits by message, author, id, file path; content search (`-S`/`-G`);
      results highlighted in the graph.
- [ ] M4-10 Blame view and file history (follow renames), jump to commit.
- [ ] M4-11 Reflog view and restore.
- [ ] M4-12 Repository file tree at any commit; open / save file at revision.

## M5 — Assistant (coding CLIs)

- [ ] M5-1 Port `switchyard-agents` as `switchhead-agents` (Claude Code, Codex CLI, Gemini
      CLI, custom; private temp dir; cancel; stream parsing). Git system prompt.
- [ ] M5-2 `switchhead-mcp`: read-only git MCP server (`switchhead mcp`, hidden argument
      checked in `main` before GPUI starts): `status`, `log`, `show`, `diff` (staged /
      unstaged / range), `branches`, `blame`, `file_at`, `conflicts`. Scoped to one repo.
- [ ] M5-3 Generate commit message from the staged diff (button in the commit box; follows
      the repo's recent message style; conventional commits option).
- [ ] M5-4 Resolve conflicts with AI: per file, proposes merged content; shown as a diff
      against the conflicted file; user accepts / edits / rejects.
- [ ] M5-5 Git command helper: "what do you want to do?" → explanation plus commands in a
      ```bash block; each command runs only when the user clicks Run (shown with its output).
- [ ] M5-6 Branch review (summary and findings for a branch vs its base) and explain-commit.
- [ ] M5-7 Assistant panel (port Switchyard's), settings (which CLI, model, extra args).

## M6 — Advanced (Fork parity)

- [ ] M6-1 Git LFS: detect, pull/fetch/prune, lock/unlock, LFS file diffs.
- [ ] M6-2 Submodules: add, update (init/recursive), open as tab, show changes.
- [ ] M6-3 Worktrees: list, add, remove, open as tab.
- [ ] M6-4 Git-flow (init, start/finish feature, release, hotfix).
- [ ] M6-5 Custom commands (user-defined, with variables like `${ref}` `${sha}` `${file}`).
- [ ] M6-6 Hosting accounts (GitHub, GitLab, Bitbucket, Azure DevOps): clone from account,
      create pull request, avatars.
- [ ] M6-7 Patches: create from commits, apply patch / `git am`.
- [ ] M6-8 GPG / SSH commit signing settings and verified badges.
- [ ] M6-9 Open in terminal / editor / file manager; external diff and merge tools.
- [ ] M6-10 Bisect UI (good / bad / skip, highlighted in the graph).

## M7 — Release

- [ ] M7-1 Packaging (AppImage, macOS .app/.pkg, Windows installer) from Switchyard's scripts.
- [ ] M7-2 Performance CI: open-repo and graph-scroll timings on git/git and linux.
- [ ] M7-3 Updates, crash logs, docs site.

## Follow-ups

- Log without a commit-graph file: gix's topo walk computes in-degrees over the whole
  history before the first commit (1.9 s on git/git). Offer to run `git commit-graph write
  --reachable` in the background on open, or start with a time-ordered walk and re-sort.
- Graph width: lanes keep their slot, so busy histories get wider than git's own graph.
  Compact lanes (shift right lanes left with diagonal segments) if wide graphs look bad.
- README (M0-3) waits for the app shell (M2) for screenshots.
- Word highlights for `git diff` output (`parse_unified`): pair each hunk's removed and
  added runs and reuse `diff.rs`'s word diff, as `diff_text` does.
- Partial staging of a new (intent-to-add) or deleted file: build the patch against an
  empty file instead of refusing a partial selection.
