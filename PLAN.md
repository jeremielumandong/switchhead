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
- [x] M1-11 `rebase`: interactive rebase todo model (pick, reword, edit, squash, fixup, drop,
      reorder) written through `GIT_SEQUENCE_EDITOR`; reword messages via `GIT_EDITOR` script.
      *`rebase::plan` / `run` → `RebaseOutcome`; new messages go through `exec git commit
      --amend -F` lines instead of a `GIT_EDITOR` script (see DECISIONS).*
- [x] M1-12 `blame`: `git blame --porcelain` (incremental) parser; file history (`log --follow`).
      *`blame::blame` streams `--incremental` chunks into a `Blame` (per-line commit index);
      the parser also reads `--porcelain`. `file_history` follows renames.*
- [x] M1-13 Operations API on `Repo` for every write in M3–M6, each a thin, tested wrapper.
      *`ops.rs`: commit, stage/unstage/discard/clean/ignore, index flags, checkout, branches,
      merge, rebase, cherry-pick, revert, reset, tags, stashes, remotes, fetch/pull/push
      (streaming, cancellable), submodules, worktrees, LFS, patches, bisect, `clone`,
      `init`; `cli::Git::run_lines`. Conflict continue/abort/skip follow `repo.state()`.*

## M2 — App shell (`crates/app`)

- [x] M2-1 Port from Switchyard: `theme.rs`, `omarchy.rs`, `appearance.rs`, `ui.rs`,
      `split.rs`, `log_file.rs`, `AppAssets` (see CLAUDE.md table). Theme tests pass.
      *Ported; environment colors became `deleted` / `modified` / `added`, env badges a
      generic `ui::chip`. `assets.rs` serves Lucide icons as `icons/sh-*.svg`.*
- [x] M2-2 Window: title bar with repo tabs, toolbar (Fetch, Pull, Push, Stash, Pop, Branch,
      Terminal, Assistant), 48px rail, sidebar, center, status bar; zoom; theme picker.
      *`workspace.rs`. Fetch / Pull / Push / Stash / Pop / Refresh run through `ops` on a
      thread with progress in the status bar; Branch, Terminal, Search and Assistant are
      shown disabled until their milestones. Theme and zoom in the rail's Appearance popover
      (zoom also on Ctrl/Cmd +/−/0); tabs, theme and zoom persist in `state.json`.*
- [x] M2-3 Repository manager: recent repos, Open Folder (repo → tab; plain folder → scan and
      list its repos grouped by subfolder, open any as a tab), Clone, Init; repos persist
      (`directories`, JSON).
      *`manager.rs` + `store.rs` (`SWITCHHEAD_DATA` overrides the data folder). Clone streams
      git's progress and can be cancelled. `switchhead <path>` opens a path at startup.*
- [x] M2-4 Commit graph view: virtualized list (graph lanes painted on a canvas per row, ref
      chips, summary, author, date, short id), keyboard navigation, streaming fill, jump to
      HEAD, filter by branch.
      *`repo_view.rs` + `graph_paint.rs`; the walk runs on its own thread and batches reach
      the UI over a channel. All / Current switch, double-click a ref to show only it, refs
      sidebar (virtualized). Shallow clones now walk (boundary commits become roots).*
- [x] M2-5 Commit details: header (author, committer, date, parents, refs, full message),
      changed files (list and tree), file diff (inline and side-by-side, word highlights,
      ignore whitespace, context lines, syntax highlighting via gpui-component).
      *`commit_view.rs` under the graph (resizable): header (parents jump to the commit),
      file list or folder tree (`file_list.rs`), diff with both layouts (`diff_model.rs`
      rows, tree-sitter colors cut per line off the UI thread). Diff settings are shared by
      all tabs and saved. `commit::info` in the engine. Merges show the first-parent diff;
      image previews wait for M3-5.*
- [x] M2-6 Command palette (Quick Launch) with every command; keyboard shortcuts like Fork's.
      *`palette.rs` (Ctrl/Cmd+P, ported from Switchyard): every command in
      `actions::commands()` (dispatched as actions, shortcuts read from the keymap), open
      tabs and recent repositories, the active repository's branches and tags (jump to the
      commit). Chips or a leading `>` filter. New actions for the diff settings, branch
      filter, Copy Commit ID (Ctrl/Cmd+Shift+C), Appearance (Ctrl/Cmd+,).*
- [x] M2-7 File system watcher: refresh status and refs when the work tree or `.git` changes.
  Note: `git::watch::fingerprint` polls `.git` metadata every second and when the window
  regains focus; a refs change reloads the graph. Work-tree status refresh lands with M3-1
  (it reuses the index fingerprint and window focus; `notify` awaits a decision).

## M3 — Local changes (Fork parity)

- [x] M3-1 Local Changes view: unstaged / staged lists, stage / unstage file, all, selected.
  *`changes_view.rs` (sidebar "Local Changes" / "All Commits"): two virtualized lists with
  click, Ctrl/Cmd-click, Shift-click, arrows, Space/Enter, double-click, Stage/Unstage
  selected and all; the focused file's `git diff` (or new file's content). Status re-reads
  on open, Refresh, index changes and window focus. The diff body moved to `diff_view.rs`,
  shared with the commit pane. Local diffs have no syntax colors yet (Follow-ups).*
- [x] M3-2 Hunk and line staging, unstaging and discarding from the diff view.
  *Hunk headers carry Stage Hunk / Discard (unstaged) or Unstage Hunk (staged); changed
  lines are picked with click, Shift-click and Ctrl/Cmd-click (both layouts) and staged
  from the toolbar. `stage::apply_lines` applies the exact `git diff` shown; discarding
  asks first in a gpui-kit alert dialog. New and deleted files stage whole only (M1-9).*
- [x] M3-3 Commit box: subject/body, amend (loads previous message), sign-off, skip hooks,
      commit message templates, recent messages, spell check, char counters.
  *`commit_box.rs` under the staged list: subject and description, Amend (loads the last
  message, restores the draft when turned off), Sign-off, Skip hooks, `commit.template`
  (`Repo::commit_template`) in empty fields, the last 20 messages in a Recent menu
  (state.json), and a subject counter (amber past 50, red past 72). Ctrl/Cmd+Enter
  commits; Ctrl/Cmd+Shift+Enter or "Commit…" jumps to it. Hook output shows in full.
  Spell check is deferred: no approved crate (Follow-ups).*
- [x] M3-4 Discard changes, delete untracked, ignore file/pattern (`.gitignore`), assume
      unchanged / skip worktree.
  *Right-click menu on the file lists (`changes_view/menu.rs`; right-click selects the row
  first): Stage / Unstage, Discard Changes or Delete (asks; Delete or Ctrl/Cmd+Backspace
  too), Ignore (the file, `*.ext`, each parent folder; untracked files), Assume Unchanged,
  Skip Worktree and Copy Path. Hidden files come from the index in process
  (`status::hidden`) and are listed under "N hidden" in the Unstaged header to show again.*
- [x] M3-5 Image diff (side by side, swipe, onion skin) and binary file info.
  *`image_diff.rs`: binary PNG / JPEG / GIF / WebP / BMP / TIFF / ICO files show both
  versions with pixel size (read from the header) and file size; Swipe and Onion skin
  follow the pointer. Other binaries show their kind (magic bytes) and size change. Local
  binaries read HEAD / index blobs in process (`diff::read_at`). SVG stays a text diff.*
- [x] M3-6 Merge conflict helper: list conflicted files, choose ours/theirs, built-in 3-way
      merge editor (base/ours/theirs/result), open external merge tool, mark resolved,
      continue / abort banner for merge, rebase, cherry-pick, revert.
  *`merge_view.rs`: a conflicted file opens an editable result with one card per conflict
  (ours / base / theirs, pick either, both, or base), All Ours / All Theirs, Open in
  `merge.tool`, Mark Resolved (asks if markers remain). Binary and delete conflicts offer
  whole-file choices (Keep / Delete). The repo banner shows the operation, conflict count,
  Abort (confirmed), Skip and Continue. CRLF files and a MERGE_MSG prefill: Follow-ups.*
- [ ] M3-7 Diff minimap: a narrow strip beside the diff marking added / removed / changed
      lines over the whole file; click or drag to jump. Commit and local-changes diffs.
- [ ] M3-8 Git identity profiles: saved name / email / signing key sets, one picked per repo
      (written to the repo's local config); current identity shown in the commit box.

## M4 — Branches, remotes, history (Fork parity)

- [x] M4-1 Sidebar: branches (tree by `/`), remotes, tags, stashes, submodules, worktrees;
      filter box; ahead/behind badges; checkout by double-click.
  *`repo_view/sidebar.rs` + `sidebar_model.rs` (tree and filter, tested): branches and each
  remote's branches fold by `/`; the filter keeps matching refs with their folders open.
  Double-click checks out a branch, a remote branch (tracking branch if new) or a tag
  (asks: detached), and opens a submodule or linked worktree in a tab
  (`worktrees.rs`, via `git`). "Show only this ref" moved from double-click to Alt-click.*
- [x] M4-2 Branch ops: create (from any commit), rename, delete (local/remote), checkout
      (with tracking), set upstream, merge into current (ff / no-ff / squash), rebase current
      onto, compare branches, push/delete remote branch.
  *`repo_view/branch_ops.rs`: the sidebar's right-click menu on branches, remote branches
  and tags (check out, merge ▸ modes, rebase, new branch, rename, delete with force and
  upstream options, push, set upstream, compare, show only, copy name), and the toolbar's
  Branch button / Ctrl+Shift+B at the selected commit. Names are checked like
  `check-ref-format --branch`. Compare walks `left...right` (`Scope::Compare`, ends at the
  merge bases); it does not yet mark which side a commit is on (Follow-ups).*
- [x] M4-3 Fetch (all remotes, prune), Pull (merge / rebase, autostash), Push (force-with-lease,
      tags, set upstream) with progress, cancel and errors in the activity panel.
  *`repo_view/remote_ops.rs`: Fetch / Pull / Push open an options dialog (`form.rs`, shared
  with branch ops). Progress shows in the status bar with Cancel; every operation lands in
  the activity panel (`activity.rs`, `repo_view/activity_panel.rs`) with git's output,
  progress counters collapsed, and errors in full; the status bar shows the telling line
  (`activity::summary`). Cancel kills git's process group (`cli::run_streaming`). The
  dialogs don't remember choices or read `pull.rebase` yet (Follow-ups).*
- [x] M4-4 Commit context menu: checkout, create branch/tag here, cherry-pick, revert, reset
      (soft / mixed / hard), interactive rebase from here, copy id/message, create patch,
      open on GitHub/GitLab/Bitbucket.
  *`repo_view/commit_menu.rs`: right-click a graph row (it selects it) for check out
  (asks: detached), new branch / tag here, cherry-pick, revert, reset ▸ soft / mixed / hard
  (hard asks), copy id / short id / summary / message, save as patch (folder prompt,
  `format-patch -1`), and Open on GitHub / GitLab / Bitbucket (`git::web`, from the
  upstream's remote or origin). Cherry-pick and revert are off for merge commits (no `-m`
  choice yet, Follow-ups). "Interactive rebase from here" comes with its editor in M4-5.*
- [x] M4-5 Interactive rebase editor (drag to reorder, pick/reword/edit/squash/fixup/drop).
  *Commit menu ▸ "Interactive Rebase main from Here…" replaces the graph with
  `rebase_editor.rs`: the commits after the selected one, newest first; drag or Alt+↑/↓
  to reorder, P R E S F D or each row's menu for the action, a message area for reword
  (starts from the old message) and squash (the combined commit's), autostash, and a
  warning when merges in the range would be flattened. Start is off until something
  changed and while the plan has a problem (`rebase_model.rs`, tested). It runs
  `git::rebase::run`; a stop (conflict, `edit`) shows "Rebase paused: …" and the banner.
  `rebase::plan` now refuses a base outside HEAD's history, a squash message survives a
  later fixup in its group, and old todo/message folders are cleared.*
- [x] M4-6 Tags: create (lightweight / annotated), delete, push, checkout.
  *`repo_view/tag_ops.rs`: New Tag… (commit menu, palette, at the selection) takes a name
  and an optional message (annotated when given) and can push it; the sidebar's tag menu
  adds Push Tag and Delete… (optionally on the remote too); palette "Push All Tags";
  checkout was already there (asks, detached). The dialog form gained a message field.*
- [x] M4-7 Stashes: create (with untracked, staged only, keep index), apply, pop, drop,
      view diff, create branch from stash.
  *`repo_view/stash_ops.rs`: Stash (toolbar, palette) opens a dialog with a message and
  the three options; clicking a stash in the sidebar shows it in the details pane; its
  menu has Apply, Pop, New Branch from Stash…, Drop… (asks) and Copy Message. Each
  index-based command first checks `stash@{n}` is still the stash that was shown. The
  untracked part of a stash is not listed yet (Follow-ups).*
- [x] M4-8 Remotes: add / edit / remove, fetch one, prune.
  *Right-click a remote's folder in the sidebar: Fetch <remote>, Prune Deleted Branches,
  Edit… (rename and URL), Remove… (asks), Copy URL. Add Remote… (name, URL, fetch now)
  is on the Remotes header and in the palette. Every section header has a menu now
  (New Branch…, Add Remote…, New Tag… / Push All Tags, Stash Changes…, Collapse), and
  remotes nothing was fetched from still show their folder (`repo_view/section_menu.rs`,
  `remote_ops.rs`).*
- [x] M4-9 Search: commits by message, author, id, file path; content search (`-S`/`-G`);
      results highlighted in the graph.
  *Ctrl/Cmd+F, the rail's Search or "Find Commits…" opens a bar over the graph: Message,
  Author, Commit ID, File Path, Content (`-S`) or Content (regex, `-G`). `git::search`
  streams matches from `git log` over the graph's scope (tested); ids match the loaded
  graph. Other rows dim, "3 of 17" counts, Enter / Shift+Enter or the arrows step
  through the matches, Esc closes; typing searches again after a 250 ms pause and a
  scope change re-runs it (`repo_view/search.rs`).*
- [x] M4-10 Blame view and file history (follow renames), jump to commit.
  File History / Blame from the commit file list's and Local Changes' menus open a view in place of the graph: history lists versions (renames shown) beside the commit's details; blame streams in with an age bar, Blame Before This Commit, Blame This Version, and Show Commit (or double-click) selects it in the graph.
- [x] M4-11 Reflog view and restore.
  "Show Reflog" (palette, branch menu) opens HEAD's or a branch's reflog in place of the graph (`git::reflog`, read with gix; tested), with the entry's commit beside it. Reset Here (HEAD / current branch, asks), Move Branch Here (`branch -f`, asks), New Branch Here, Check Out and Show in Graph; the list reloads when refs change.
- [ ] M4-12 Repository file tree at any commit; open / save file at revision.
- [ ] M4-13 Undo: before a hard reset, discard or clean, snapshot the work tree and index
      (`git stash create`, kept under `refs/switchhead/undo/*`); ref moves undo through the
      reflog. Undo (Ctrl/Cmd+Z outside text inputs) restores the last risky action; old
      snapshots expire.

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
- [ ] M6-10 Bisect UI (good / bad / skip, highlighted in the graph), guided: a step panel
      with the current candidate, steps left and the remaining range, ending on the first
      bad commit.

## M7 — Release

- [ ] M7-1 Packaging (AppImage, macOS .app/.pkg, Windows installer) from Switchyard's scripts.
- [ ] M7-2 Performance CI: open-repo and graph-scroll timings on git/git and linux.
- [ ] M7-3 Updates, crash logs, docs site.

## Follow-ups

- Blame: no syntax colors yet, and file history (`log --follow`) is read in one go rather than streamed or cancellable.
- Watcher: events from ignored folders (`target/`, `node_modules/`) still re-read status during a build; filter them with gix's excludes before reporting.
- Search: a pickaxe search that prints nothing for a long time is only stopped at its next match (`run_lines` checks cancel between lines); kill it from a watcher like `run_streaming`.

- Stash details: also list the untracked files a stash holds (its third parent).

- Cherry-pick and revert of merge commits: ask which parent is the mainline (`-m`).

- Fetch / Pull / Push dialogs: remember the last choices per repository and default Pull's
  rebase option from `pull.rebase`.

- Branch compare: mark each commit's side (left / right) in the graph, like
  `git log --left-right`.

- Merge helper: keep CRLF line endings when writing the result; prefill the commit box
  from `MERGE_MSG` during a merge.

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
- Graph column width follows the widest row loaded, so one busy stretch widens the whole
  list; size it to the visible rows (or compact lanes, above).
- Recent repositories whose folder is gone still show; mark them and offer to remove.
- Rebase message files stay in `.git/switchhead/` after a stopped rebase that the user
  then aborts; sweep old `rebase-*` dirs when a repo opens.
- Side-by-side diff clips long lines (the inline layout scrolls sideways); give each side
  its own horizontal scroll.
- Word highlights mark changed indentation even with "Ignore all whitespace"; skip
  whitespace-only word ranges when whitespace is ignored.
- Merge commits: offer a diff against each parent (and the combined diff), not only the
  first parent.
- Local Changes diffs have no syntax colors or word highlights: read the index blob and
  the work-tree file to highlight them, and pair runs as for commit diffs.
- Local Changes ignores the whitespace setting (`git diff` without `-b` / `-w`); offer it
  for viewing while keeping hunk staging on the real lines.
- Fork shows an "Uncommitted changes" row at the top of the graph; add one that opens
  Local Changes.
- Work-tree edits made while switchhead is in front show only on Refresh or the next
  focus change; a watcher (`notify`, pending approval) would catch them live.
- Spell check in the commit box (M3-3): needs a dictionary crate (e.g. `spellbook`, MIT,
  Hunspell dictionaries) and a squiggle decoration on the input; ask before adding it.
- Image diffs: hide the text-diff settings (context, inline / side by side) in the
  toolbar, show small images at 1:1 instead of scaled up, and a checkerboard behind
  transparent pixels.
