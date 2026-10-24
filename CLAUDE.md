# CLAUDE.md — switchhead

switchhead is a very fast, native, cross-platform Git client (a Fork-class GUI) written in
Rust with GPUI + gpui-component, on the same stack and design system as Switchyard
(github.com/jeremielumandong/switchyard). Goals, in order:

1. **Speed everywhere**: opening a repo, the commit graph, diffs, staging, switching repos.
2. **Full Fork feature parity** (the checklist in `PLAN.md`).
3. **Coding-CLI assistant** (Claude Code, Codex CLI, Gemini CLI, custom) that helps with git:
   generating commit messages, resolving merge conflicts, explaining and suggesting git
   commands, reviewing branches.
4. **Workspace folders**: open a folder that is not a repo and get every repo under it.

- Task plan: `PLAN.md` (work through it in order; it holds the Fork parity checklist)
- Decisions log: `docs/DECISIONS.md` (append when you make or need a non-obvious call)

## How to work in this repo

1. Open `PLAN.md`, take the first unchecked task in the current milestone.
2. Before coding, state a short plan: files you will touch, types you will add, how you will test.
3. Implement only that task. No drive-by refactors; note them in `PLAN.md` under "Follow-ups".
4. Run the checks below. All must pass.
5. Tick the task in `PLAN.md` and add a one-line note (what landed, anything deferred).
6. If a task is ambiguous or conflicts with a rule here, stop and ask. Record the answer in
   `docs/DECISIONS.md`.
7. When you learn something non-obvious (a crate quirk, a platform trap), add it to "Gotchas".

## Commands

```bash
cargo build --workspace
cargo run -p switchhead-app            # binary: `switchhead`
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                 # unit tests; they build throwaway repos with `git`
```

Definition of done: builds on stable, fmt clean, clippy clean with `-D warnings`, tests pass,
PLAN.md updated.

## Workspace layout

```
switchhead/
  Cargo.toml              # workspace; shared [workspace.dependencies]; vendored GPUI Fast patch
  rust-toolchain.toml     # 1.98.1, matching Switchyard / gpui-fast
  crates/
    git/     switchhead-git     Git engine: gix reads, `git` CLI writes, graph layout, parsers
    hosting/ switchhead-hosting GitHub / GitLab / Bitbucket / Azure DevOps APIs, tokens in the keychain
    agents/  switchhead-agents  (M5) AgentAdapter + Claude Code / Codex / Gemini / custom, ported from switchyard-agents
    mcp/     switchhead-mcp     (M5) read-only git MCP server, served by `switchhead mcp`
    app/     switchhead-app     GPUI binary `switchhead`
  vendor/gpui-base, vendor/gpui-fast   # application-patched GPUI Fast stack (from AgentOpsChat)
  docs/DECISIONS.md
```

Dependency direction: `app → {git, hosting, agents, mcp}`, `mcp → git`. Nothing depends on `app`.

## Architecture rules (do not break these)

- **Reads in process, writes through `git`.** Log, graph, refs, commit diffs and blob diffs
  use gitoxide (`gix`) in process: no process spawn on hot paths. Anything that changes a
  repository (commit, stage, checkout, merge, rebase, push, stash, tag...) runs the system
  `git` through `switchhead_git::cli::Git`, so hooks, credential helpers, SSH agents, GPG/SSH
  signing, LFS and the user's config behave like the command line. Working-tree status and
  index diffs also use `git` (`status --porcelain=v2 -z`, `diff`): it has fsmonitor and the
  untracked cache.
- **The UI thread never does I/O.** All git work runs on GPUI's background executor (or
  dedicated threads for long operations); results come back to entities. No blocking calls in
  render or event handlers.
- **Stream, don't materialize.** The log walk sends batches (first batch ~300 commits, then
  ~2,000) with their graph rows; the commit list renders as soon as the first batch arrives.
  Never wait for the full history before showing anything.
- **Columnar commit store.** Ids, times, author indices (interned), and summaries in a string
  arena; graph rows as segments in one arena with per-row offsets. No per-row `String` or
  `Vec` allocations in the log; no `Vec<Vec<..>>`.
- **Virtualize every long list**: commit graph, file lists, diff lines, blame, sidebar trees,
  repo manager.
- **Everything cancellable**: log walks, fetch/pull/push/clone (`cli::Cancel`), scans, agent runs.
- **Graph layout is pure and incremental** (`switchhead_git::graph`): it consumes commits in
  display order and emits rows; the UI only paints rows. Unit-test it on synthetic histories.
- **Workspace folders**: `discover::classify` decides repo vs folder; `discover::scan` finds
  repos under a folder in parallel (depth limit, skips `node_modules`, `target`, hidden dirs,
  never descends into a found repo).
- **One design system with Switchyard.** Port Switchyard's UI modules rather than inventing
  new ones (list under "Reuse from Switchyard"). Fixed sizes use `appearance::rpx`, text
  sizes `appearance::ts::*`.

## Performance budgets

Cold start < 300 ms · open a repo to first graph rows < 100 ms (and < 500 ms on a
1M-commit repo with a commit-graph file) · scroll the graph of a 1M-commit repo without
dropped frames · switching commits shows its file list < 30 ms and a typical file diff
< 50 ms · status of a 100k-file work tree < 300 ms (git with fsmonitor) · switching repo
tabs < 16 ms (state stays in memory) · folder scan of ~1,000 dirs < 200 ms · no spinner for
operations under 200 ms. Add a timing log line (`tracing::debug!` with `ms`) to every git
operation; `cli::Git` already does.

## Reuse from Switchyard

Port these from `switchyard/crates/app/src` (same owner; switchhead is MIT, keep the license
headers of bundled assets):

| Switchyard file | What it gives switchhead | Changes needed |
| --- | --- | --- |
| `theme.rs` | Palette, 10 themes, gpui-component theme bridge, contrast test | Drop `EnvironmentLabel`; map `prod/stg/dev` to deleted/modified/added colors |
| `omarchy.rs` | "Follow Omarchy" theme from `~/.config/omarchy` / `~/.local/state/omarchy` | None |
| `appearance.rs` | Zoom, `rpx`, `ts::*` text sizes, editor font | Env var `SWITCHHEAD_ZOOM` |
| `ui.rs` | Buttons, icon buttons, kbd chips, dots, segmented control, shimmer, formatting helpers | Drop env badges |
| `split.rs` | Split panes with drag | None |
| `log_file.rs` | Rotating log file writer | File name `switchhead.log` |
| `rail.rs` (`AppAssets`, `render_rail`) | 48px activity rail, asset source | New rail items (Repos, Branches, Search, Assistant, Settings) |
| `workspace.rs` (`render_title_bar`, `render_tab_strip`, `render_status_bar`) | Window chrome | Repo tabs instead of connection tabs |
| `palette.rs` | Command palette | Git commands |
| `assistant_panel.rs`, `chat_markdown.rs`, `coding_clis.rs`, `assistant_settings.rs` | Assistant UI | Git prompts |
| `crates/agents` (whole crate) | Coding-CLI adapters and runner | `MCP_SERVER_NAME = "switchhead"`, git system prompt |
| `crates/cli/src/mcp/server.rs` | Stdio MCP JSON-RPC server | Blocking I/O is fine; git tools |
| `assets/fonts` (Geist, OFL) | Fonts | Already copied |
| `packaging/`, `.github/workflows/` | AppImage / macOS / Windows packaging, CI | Rename |

## Agent safety rules (assistant and MCP server)

- The coding CLI runs in a private empty temp dir (switchyard-agents `WorkDir`), never in the
  user's repository, with its built-in shell and file-edit tools disabled. Its only tools are
  switchhead's MCP server, which is **read-only** (status, log, show, diff, branches, blame,
  conflicted file contents) and scoped to the one repository the run is for.
- The assistant never changes a repository. It returns proposals: a commit message, resolved
  file contents for a conflict, git commands in a ```bash block. The app shows each proposal;
  the user applies it (the app runs the command or writes the file) after reviewing it.
- Never read, copy, store or proxy a coding CLI's credentials. Each CLI uses its own login.

## Commits

- Author every commit as the repository owner: `Jeremie Lumandong <jeremie.lumandong@outlook.com>`.
  Before the first commit in a fresh clone run
  `git config user.name "Jeremie Lumandong"` and
  `git config user.email "jeremie.lumandong@outlook.com"` (the container's default identity
  is Claude's). Jeremie is the only author: no `Co-Authored-By: Claude ...` or
  `Claude-Session:` trailers in commit messages, and no "Generated with Claude Code" footer
  or session link in PR descriptions (the attribution header comment at the top of a PR body
  stays).

## Coding conventions

- Edition 2024, stable toolchain.
- Errors: `thiserror` enums per library crate (`switchhead_git::Error`); `anyhow` only in the
  app's top level. gitoxide errors convert with the crate's `GixExt::gix()`.
- No `unwrap()` / `expect()` outside tests and one-time startup code.
- Logging with `tracing`; never log credentials or remote URLs with userinfo.
- Public types and functions get doc comments. Keep modules small.
- Tests: unit tests next to code. Git tests build throwaway repos with the real `git`
  (`switchhead_git::testutil::TestRepo`, fixed identity and dates).

## Approved dependencies

Ask before adding anything else. Pin versions in `[workspace.dependencies]`.

| Purpose | Crates |
| --- | --- |
| UI | `gpui-kit =0.7.3` (facade over `gpui-pre` + `gpui-component`, kept in lockstep) on the vendored GPUI Fast stack |
| Git | `gix` 0.89 (features: max-performance-safe, sha1, basic, status, mailmap, parallel) |
| Async (agents) | `tokio` (sync only) |
| Serialization | `serde`, `serde_json` |
| Errors / logs | `thiserror`, `anyhow`, `tracing`, `tracing-subscriber` |
| Platform | `directories`, `chrono` (`clock` only), `notify` 8 (file events; already in the tree via gpui-component) |
| Hosting accounts | `reqwest` 0.12 (`blocking`, `json`, `rustls-tls-native-roots`), `keyring` 4 |
| Testing | `tempfile` |

## Licensing

MIT. GPUI and gpui-component are Apache-2.0 and fine. Zed's editor, git panel and other app
crates are GPL-3: do not copy or port code from them. gitoxide is MIT OR Apache-2.0.

## gix 0.89 notes (verified against the source)

- Errors are `gix_error::Error` (`gix::Result`); map with `.gix()`.
- Thread safety: keep a `gix::ThreadSafeRepository` (`Repo`) and call `.to_thread_local()`
  per worker (`Repo::local`, which also sets a 32 MB object cache).
- Log order like `git log --date-order`, incremental with the commit-graph file:
  `gix::traverse::commit::topo::Builder::from_iters(&repo.objects, tips, None::<Vec<Oid>>)
  .sorting(topo::Sorting::DateOrder).with_commit_graph(repo.commit_graph_if_enabled()?)
  .build()?` yields `Info { id, parent_ids, generation, commit_time }`. Decode author and
  summary with `repo.find_commit(id)?.decode()?` (`CommitRef.author()` → `SignatureRef`,
  `.seconds()`; `CommitRef::message().summary()`).
- Refs: `repo.references()?.all()?.peeled()?`; `Reference::name()` → `FullNameRef`
  (`shorten()`, `category()`); reflog via `log_iter()` (stashes: `refs/stash`).
- Tree diff: `repo.diff_tree_to_tree(old, new, None)` → `Vec<ChangeDetached>`.
- Blob diff: `gix::diff::blob` re-exports `gix-imara-diff` 0.3: `InternedInput::new(a, b)`
  (`&[u8]` tokenizes by line), `Diff::compute(Algorithm::Histogram, &input)`,
  `diff.postprocess_lines(&input)`, `diff.hunks()` → `Hunk { before, after }` ranges;
  `Hunk::latin_word_diff` gives word-level highlights.
- State: `repo.state()` → `gix::state::InProgress` (merge, rebase, cherry-pick...).

## Gotchas

- Some dev containers cannot reach `static.rust-lang.org`, so rustup cannot install 1.98.1:
  build there with `RUSTUP_TOOLCHAIN=stable` (1.97). The workspace `rust-version` is 1.97.
- Linking the app on Linux needs `libxkbcommon-dev` and `libxkbcommon-x11-dev`. GPUI renders
  through Vulkan: under Xvfb install `mesa-vulkan-drivers` (lavapipe) or the window fails with
  "Failed to create surface".
- The repository here was cloned single-branch: add
  `+refs/heads/*:refs/remotes/origin/*` to `remote.origin.fetch` before tracking new branches.
- git prints progress with `\r`; `cli::Git::run_streaming` splits on `\r` and `\n`.
- `git fetch --all`, ssh and transport helpers are child processes that keep git's pipes
  open after git is killed. `run_streaming` starts git in its own process group, kills the
  group on cancel, and stops reading a pipe 500 ms after git exits.
- Background `git` runs with `GIT_TERMINAL_PROMPT=0`, `LC_ALL=C`, `GIT_EDITOR=true`,
  `GIT_OPTIONAL_LOCKS=0` and `-c color.ui=false -c core.quotepath=false`.
- GPUI: a `uniform_list` row sizes to its content, so rows with a `flex_1` column need
  `.w_full()`. Text in a flex row needs `min_w_0()` to truncate. (From Switchyard.)
- gix's topo walk (`traverse::commit::topo`) computes in-degrees over the whole history
  before yielding anything unless the commit-graph file gives generation numbers: on git/git
  the first batch takes 1.9 s without it and 94 ms with it.
- `gix::path::from_bstr` returns a `Result` (non-UTF-8 paths fail on Windows); status paths
  fall back to a lossy conversion.
- gix's topo walk does not read `.git/shallow`: on a shallow clone it fails with "object
  could not be found" at the boundary. `log::walk` wraps the object store (`ShallowCut`) so
  boundary commits read as having no parents.
- inotify (and so `notify` on Linux) reports opening a file as an event: the watcher drops
  access events except close-after-write, or reading status would trigger another status.
  A plain `git status` may rewrite the index (an index-refresh write, a real `.git` change);
  the app's status runs with `GIT_OPTIONAL_LOCKS=0`, which does not.
- The app reads `SWITCHHEAD_DATA` (data folder), `SWITCHHEAD_THEME` and `SWITCHHEAD_ZOOM`;
  screenshots: `Xvfb :99 &`, then `DISPLAY=:99 SWITCHHEAD_DATA=<tmp> switchhead <repo>` and
  `import -window root shot.png` (ImageMagick); `xdotool` drives clicks and keys.
- A `claude` started from inside a Claude Code session (an app launched from one, for a
  manual check) inherits its `CLAUDE_CODE_*` environment and reports that session's id, so
  `--resume` then fails ("error_during_execution"). Launch the app with those variables
  unset (every `CLAUDE*` variable, `env -u ...`) to check follow-ups. With a clean environment
  Claude Code resumes from any working directory, so each run's fresh temp dir is fine.
- Claude's cloud containers route HTTPS through a proxy that adds the owner's GitHub
  credentials, so api.github.com accepts any token there; check sign-in against a local
  stand-in server (any `http://127.0.0.1:<port>` site works as a GitHub Enterprise host).
  Those containers have no Secret Service either: tokens are kept for the session only.
- Escape typed in a gpui-kit `Input` reaches containers as the app's global `actions::Dismiss`
  (bound with no context), not as a key-down or the input's own `Escape`: a bar that closes
  on Escape handles `Dismiss` (`repo_view/search.rs`).
- gpui-kit compiles `component::highlighter::SyntaxHighlighter` only with a `tree-sitter*`
  feature (the workspace enables `tree-sitter-languages`); without one the module is a stub
  and `SyntaxHighlighter` does not exist. Unknown names fall back to `"text"` (no colors).
- Git for Windows sets `core.autocrlf=true` in its system config, so `git apply` to the work
  tree writes CRLF there. `TestRepo` turns it off; tests that compare file bytes rely on that.
- `git lfs unlock <path>` looks the lock up by path on the server and fails with "multiple
  locks found; ambiguous" when the server's answer has more than one; the app unlocks by
  `--id`. `git lfs fetch` only fetches the current branch's objects, so older diff sides
  stay pointers until `fetch --all`.
- git-lfs prints progress only to a terminal unless `GIT_LFS_FORCE_PROGRESS=1`, and then
  on stdout, not stderr (`run_streaming_with(.., stdout_lines: true, ..)`). A clone whose
  LFS download fails exits 128 "Clone succeeded, but checkout failed" with no work tree,
  so `ops::clone` skips the smudge and pulls afterwards. git-lfs reads and writes
  `file://` remotes itself (tests use one); a plain path remote is not uploaded to.
- `git submodule add` / `update` clone with `GIT_PROTOCOL_FROM_USER=0`, so a local path or
  `file://` URL fails ("transport 'file' not allowed") unless `protocol.file.allow=always`;
  tests pass `-c protocol.file.allow=always`, and a manual check can launch the app with
  `GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=protocol.file.allow GIT_CONFIG_VALUE_0=always`.
- GPUI Kit 0.7.3 runs on the application-patched GPUI Fast stack vendored in `vendor/`
  (a copy of AgentOpsChat's): `compat/*` facades publish GPUI Fast's core under the
  `gpui-pre` 0.3.9 names Kit pins, `gpui`, `gpui_linux` and `gpui_windows` are patched
  locally, and the other GPUI Fast crates stay on git rev `598306f`. Do not enable gpui-kit's
  own `gpui-fast` feature (that pulls the unpatched published runtime) and update `vendor/`
  from AgentOpsChat rather than editing it here. The one exception is the backport of
  gpui-fast #84 in `gpui_windows` (end of `vendor/gpui-fast/PARITY-PATCH.md`): carry it over
  until AgentOpsChat's copy includes it.
- SSH signatures: `%G?` is `N` (as for an unsigned commit) when `gpg.ssh.allowedSignersFile`
  is not set, with the reason only on stderr, and `U` when the key is not listed in it;
  `signing::verify_commit` tells signed from unsigned in process (the `gpgsig` header).
  Cloud containers set a global `gpg.ssh.program` and `commit.gpgsign`: tests and manual
  checks set `gpg.ssh.program=ssh-keygen` locally. gpg-agent's socket lives in
  `GNUPGHOME`, so a long scratchpad path there fails with "No agent running".
- `difftool.<name>.cmd` runs in a shell where `$LOCAL` / `$REMOTE` are plain (not exported)
  variables: pass them as arguments to a nested `sh -c`, or it sees them empty. Without
  `diff.tool` (or `merge.tool`) git difftool falls back to terminal tools such as vimdiff,
  which hang with no terminal, so the app refuses to run it until a tool is chosen.
- gix's topo walk, like git's, computes in-degrees down to the lowest generation among its
  tips before yielding, so an old tag as a tip makes the first batch walk almost the whole
  history even with a commit-graph (linux: 2.7 s with its tags, git's `--branches` 17 ms).
  `crates/git/examples/perf.rs` (and the Performance workflow) measures it.
- Release builds on Windows are GUI-subsystem binaries, so every console child (git, `cmd`,
  `taskkill`) opens its own console window that flashes until it exits. Start them with
  `CREATE_NO_WINDOW` (`cli::no_window`, which `cli::Git` and custom commands already use);
  their own children (ssh, hooks, credential helpers) inherit the hidden console.
- `std::fs::canonicalize` on Windows returns verbatim paths (`\\?\C:\repo`), which
  Windows Terminal, PowerShell and `cmd` refuse as a starting folder: use
  `switchhead_git::paths::canonicalize`. `Path::join("")` adds a trailing separator, and
  a trailing `\` before a closing quote escapes it (`wt -d "C:\repo\"` gets `C:\repo"`).
