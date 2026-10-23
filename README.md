# switchhead

**A very fast, native Git client for Linux, macOS and Windows.**

switchhead is a Fork-class Git GUI written in Rust on [GPUI](https://www.gpui.rs/) and
gpui-component. It opens big repositories in milliseconds, streams the commit graph while
history is still being read, and does everything that changes a repository through your own
`git`, so hooks, credential helpers, SSH agents, signing and LFS behave exactly like the
command line.

**[Website](https://jeremielumandong.github.io/switchhead/)** · [Plan](PLAN.md)

![Commit graph in the dark theme](docs/screenshots/commit-graph-dark.png)

> switchhead is in active development: everything below works today, and
> [`PLAN.md`](PLAN.md) lists what comes next.

## Why switchhead

- **Fast where it matters.** Log, graph, refs and commit diffs are read in process with
  [gitoxide](https://github.com/GitoxideLabs/gitoxide): no process spawn on hot paths. On
  git/git (85,000+ commits, all refs) the first rows appear in **94 ms** and the whole
  history is laid out in 1.3 s, about twice as fast as `git log --all --date-order`.
- **Never waits for the full history.** Commits arrive in batches with their graph rows,
  and every long list (graph, files, diff lines, sidebar) is virtualized.
- **Behaves like your terminal.** Commits, staging, checkouts, merges and rebases run the
  system `git`, with your config, hooks and credentials.
- **Native and light.** One Rust binary with GPU-rendered UI, no Electron, no web view.

## Features

### Commit graph

A virtualized commit list with lanes painted per row, ref chips for branches, remote
branches and tags, author, date and short id. Switch between **All** refs and the
**Current** branch, Alt-click a ref to show only it, jump to HEAD (Ctrl/Cmd+Shift+H), and
move with the keyboard. The status bar shows how long the first rows took.

![Commit graph with the Dracula theme at 110 % zoom](docs/screenshots/commit-graph-dracula-110.png)

Changes made outside switchhead (a commit in a terminal, a fetch from your editor) are
picked up on their own: switchhead watches `.git` and reloads refs and status when they
change or when the window regains focus.

![A commit made in a terminal shows up in the graph](docs/screenshots/outside-commit-picked-up.png)

### Commit details and diffs

Select a commit to see its header (author, committer, date, parents you can jump to, refs,
full message, the signature checked as **Verified**, **Unverified** or **Bad**), its changed files as a list or a folder tree, and each
file's diff with syntax highlighting and word-level highlights. Diffs come **inline** or
**side by side**, with adjustable context lines and an ignore-whitespace switch shared by
all tabs.

![Commit details with a syntax-highlighted inline diff](docs/screenshots/commit-details-dark.png)

![Side-by-side diff in the light theme](docs/screenshots/commit-details-light-side-by-side.png)

### Image diffs

Changed images (PNG, JPEG, GIF, WebP, BMP, TIFF, ICO) show both versions with their pixel
and file sizes, **side by side**, as a **swipe** or as an **onion skin** that follows the
pointer. Other binary files show their kind and size change.

![Image diff side by side](docs/screenshots/image-diff-side-by-side.png)

![Image diff in swipe mode](docs/screenshots/image-diff-swipe.png)

### Local changes and staging

Unstaged and staged lists with mouse and keyboard selection (Shift- and Ctrl/Cmd-click,
arrows, Space or Enter to stage, Tab to switch lists). Stage, unstage or discard whole
files, single **hunks**, or **individual lines** picked in the diff.

![Local Changes view](docs/screenshots/local-changes.png)

![Picking lines to stage in the diff](docs/screenshots/line-staging.png)

Right-click a file to discard or delete it, add it (or its extension, or a parent folder)
to `.gitignore`, mark it assume-unchanged or skip-worktree, or copy its path. Destructive
actions always ask first.

![File context menu with ignore options](docs/screenshots/ignore-menu.png)

![Discard confirmation](docs/screenshots/discard-confirm.png)

### Commit box

Subject and description fields with a subject counter (amber past 50 characters, red past
72), **Amend** (loads the last message and restores your draft when turned off),
**Sign-off**, **Skip hooks**, your `commit.template`, and your last 20 messages under
**Recent**. Ctrl/Cmd+Enter commits.

![Commit box](docs/screenshots/commit-box.png)

When a hook rejects a commit, its full output is shown right under the box.

![A pre-commit hook's error output](docs/screenshots/commit-hook-error.png)

### Merge conflict helper

Conflicted files open in a built-in three-way helper: one card per conflict with **ours**,
**base** and **theirs**, and buttons to take either side, both (in either order) or the
base. Edit the result directly, take **All Ours** / **All Theirs**, open your configured
`merge.tool`, then **Mark Resolved**. Binary and delete conflicts get whole-file choices.

![Merge conflict helper](docs/screenshots/merge-helper.png)

While a merge, rebase, cherry-pick or revert is in progress, a banner shows the operation
and the number of conflicts left, with **Abort**, **Skip** and **Continue**.

![Operation banner](docs/screenshots/operation-banner.png)

### Sidebar, branches and remotes

Branches and each remote's branches fold into folders by `/`, next to tags, stashes,
submodules and worktrees, with ahead/behind badges and a filter box. Double-click to check
out a branch (a remote branch creates its tracking branch) or a tag, or to open a submodule
or linked worktree in its own tab.

![Sidebar with branch folders, submodules and worktrees](docs/screenshots/sidebar.png)

Right-click a branch, remote branch or tag to check it out, merge it into the current
branch (fast-forward, no-ff or squash), rebase onto it, create a branch from it, rename,
delete (local and remote), push, set its upstream, compare it with the current branch, or
copy its name. Ctrl/Cmd+Shift+B creates a branch at the selected commit.

![Branch context menu](docs/screenshots/branch-menu.png)

The toolbar runs **Fetch**, **Pull**, **Push**, **Stash** and **Pop** with git's progress
in the status bar.

### Quick Launch

Ctrl/Cmd+P opens a command palette with every command and its shortcut, your open tabs,
recent repositories, and the current repository's branches and tags (pick one to jump to
its commit).

![Quick Launch](docs/screenshots/quick-launch.png)

### Repositories and workspace folders

Open a repository, **clone** one (with progress and cancel), or create a new one. Open a
plain folder and switchhead scans it for every repository inside, in parallel, so a whole
workspace is one click away. Repositories open as tabs, and tabs, recent repositories,
theme and zoom are remembered.

![Repository manager in the light theme](docs/screenshots/repository-manager-light.png)

### History tools

Right-click a commit to cherry-pick, revert, reset (soft, mixed or hard), create a branch
or tag there, save or apply patches, or open it on GitHub, GitLab or Bitbucket. Rebase
interactively by dragging commits and choosing pick, reword, edit, squash, fixup or drop.
Search commits by message, author, id or content, read blame and file history, browse the
file tree at any revision, look through the reflog, and undo risky actions.

![Interactive rebase editor](docs/screenshots/rebase-editor.png)

![Blame](docs/screenshots/blame.png)

### Assistant

Claude Code, Codex CLI, Gemini CLI or a custom coding CLI writes commit messages, proposes
conflict resolutions, explains git commands and reviews branches. It runs in an empty
temporary folder and works through switchhead's read-only git MCP server, so it never
changes your repository itself: you review and apply every proposal. Each CLI uses its own
login.

![The assistant proposes a conflict resolution](docs/screenshots/assistant-conflict-proposal.png)

![The assistant answers a git question with commands to run](docs/screenshots/assistant-answer.png)

### Advanced

Git LFS (locks, fetch and pull, pointer-aware diffs), submodules and worktrees, git-flow,
custom commands, hosting accounts for GitHub, GitLab, Bitbucket and Azure DevOps (clone
from your account, create pull requests, author avatars), commit signing settings with
GPG, SSH or X.509, identity profiles, a guided **bisect**, a diff minimap, and Open in
Terminal, Editor, File Manager or your external diff and merge tools.

![A guided bisect found the first bad commit](docs/screenshots/bisect-found.png)

![A verified SSH signature](docs/screenshots/signature-verified.png)

### Themes and zoom

Ten themes: switchhead Dark and Light, Nord, Dracula, Catppuccin Mocha and Latte, Tokyo
Night, Gruvbox Dark, Solarized Light and High Contrast, plus **Follow Omarchy** to match
your [Omarchy](https://omarchy.org) theme. Zoom the whole UI with Ctrl/Cmd +, − and 0.

## Keyboard shortcuts

Ctrl on Linux and Windows, Cmd on macOS.

| Shortcut | Action |
| --- | --- |
| Ctrl/Cmd+P | Quick Launch |
| Ctrl/Cmd+O | Open repository |
| Ctrl/Cmd+N | Clone repository |
| Ctrl/Cmd+Shift+M | Repository manager |
| Ctrl/Cmd+W | Close tab |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab |
| Ctrl/Cmd+R | Refresh |
| Ctrl/Cmd+Shift+F | Fetch |
| Ctrl/Cmd+Shift+L | Pull |
| Ctrl/Cmd+Shift+P | Push |
| Ctrl/Cmd+Shift+S | Stash |
| Ctrl/Cmd+Shift+B | New branch at the selected commit |
| Ctrl/Cmd+Shift+H | Jump to HEAD |
| Ctrl/Cmd+Shift+C | Copy commit id |
| Ctrl/Cmd+B | Toggle sidebar |
| Space / Enter | Stage or unstage the selected files |
| Ctrl/Cmd+Backspace, Delete | Discard the selected files |
| Ctrl/Cmd+Enter | Commit |
| Ctrl/Cmd+Shift+Enter | Jump to the commit message |
| Ctrl/Cmd+, | Appearance |
| Ctrl/Cmd + / − / 0 | Zoom in / out / reset |

## Install

Each release on [switchhead's website](https://jeremielumandong.github.io/switchhead/)
comes as an AppImage for Linux (x86_64), a universal disk image for macOS 11 or newer
([`switchhead-macos-universal.dmg`](https://github.com/jeremielumandong/switchhead/releases/latest/download/switchhead-macos-universal.dmg):
drag switchhead to Applications), and an installer for Windows 10 or newer that needs no
administrator rights ([`switchhead-windows-x64-setup.exe`](https://github.com/jeremielumandong/switchhead/releases/latest/download/switchhead-windows-x64-setup.exe)).
switchhead uses the `git` on your `PATH`, so install git first.

### Linux

Install the latest published Linux x86_64 release:

```sh
curl -fsSL https://raw.githubusercontent.com/jeremielumandong/switchhead/main/install.sh | sh
```

The installer ([`install.sh`](install.sh)) downloads the AppImage from this repository's
GitHub Releases, checks its SHA-256 checksum, and installs it for your user:

| What | Where |
| --- | --- |
| The app | `~/Applications/switchhead.AppImage` |
| Launcher command | `~/.local/bin/switchhead` |
| Desktop entry and icon | `~/.local/share/applications/switchhead.desktop`, `~/.local/share/icons/hicolor` |

No Rust compiler or sudo is needed, but the AppImage uses the system's graphics stack:
a working GPU driver with the Vulkan loader, plus `libxkbcommon`, `libxkbcommon-x11`,
`libwayland-client`, `libxcb`, `fontconfig` and `freetype`. Most desktops already have
them. On Ubuntu or Debian, install any that are missing with
`sudo apt install libvulkan1 libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 libxcb1 libfontconfig1 libfreetype6`.
The release is built on Ubuntu 22.04, so it needs glibc 2.35 or newer. Make sure
`~/.local/bin` is on your `PATH` so the `switchhead` command resolves; the
installer warns if it is not.

- **Update:** quit switchhead and run the same command again. The previous version is kept
  as `switchhead.AppImage.bak`. The installer won't replace an AppImage that is running
  unless you add `--stop-running`: `curl -fsSL …/install.sh | sh -s -- --stop-running`.
- **Pick a release:** `curl -fsSL …/install.sh | SWITCHHEAD_VERSION=v0.1.2 sh`.
- **Install somewhere else:** set `SWITCHHEAD_APPIMAGE=/absolute/path/switchhead.AppImage`.
- **Uninstall:** `curl -fsSL …/install.sh | sh -s -- --uninstall`, or
  `./install.sh --uninstall` from a checkout. Your repository list and settings in
  `~/.local/share/switchhead` are kept (delete that folder yourself to remove them).

To build and install from a source checkout instead (needs Rust and the build packages
under [Building from source](#building-from-source)):

```sh
./install.sh --build                         # build the AppImage, then install it
./install.sh dist/switchhead-<version>-x86_64.AppImage   # install an AppImage you already have
./install.sh --uninstall
```

## Building from source

switchhead needs Rust (the toolchain is pinned in `rust-toolchain.toml`; 1.97 or newer)
and `git` on your `PATH`.

On Linux, install the GPUI build dependencies first (Debian/Ubuntu):

```bash
sudo apt-get install -y libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libxcb1-dev libvulkan-dev libfontconfig-dev libfreetype-dev libssl-dev pkg-config
```

Then:

```bash
git clone https://github.com/jeremielumandong/switchhead
cd switchhead
cargo run --release -p switchhead-app            # or: cargo run -p switchhead-app -- <path>
```

The binary is `switchhead`; pass a repository or folder path to open it at startup.

Development checks:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace      # builds throwaway repositories with the real `git`
```

## Packages

`packaging/package.sh` builds the installer for the platform it runs on, into `dist/`:

| Platform | Script | Output |
| --- | --- | --- |
| Linux | `packaging/linux/build-appimage.sh` | `switchhead-<version>-x86_64.AppImage` |
| macOS | `packaging/macos/build-macos.sh` | `switchhead-<version>-macos-universal.dmg` and `.pkg` |
| Windows | `packaging/windows/build-windows.ps1` | `switchhead-<version>-windows-x64-setup.exe` (per-user, no administrator rights) |

The AppImage uses the host's `git`, glibc and GPUI system libraries, so build it on the
oldest distribution you want to support. The macOS build is ad-hoc signed unless
`MACOS_SIGN_IDENTITY` (and `MACOS_NOTARY_PROFILE` for notarization) is set; the Windows
installer is signed with `WINDOWS_SIGN_THUMBPRINT` or `WINDOWS_SIGN_PFX`, or with Azure
Trusted Signing through `-Sign`. Each script's header lists its options. The icons are
rendered from `packaging/icons/*.svg` by `packaging/icons/generate.py`.

Releases are built by hand: the Linux, macOS and Windows release workflows (Actions → Run
workflow, on `main` after CI passed) each attach their package to one draft release for the
workspace version, which is published once all three are there. macOS signing uses the
`macos-release` environment's secrets and Windows signing the `windows-release`
environment's Azure variables; without them the packages are built unsigned and kept as
workflow artifacts (or uploaded with the `upload_unsigned` input).

At startup switchhead reads the update feed on its website (`site/latest.json`, published
with the site; no account or token) and shows a newer version in the status bar with a
link to download it; turn this off under
Settings → Check at startup. If switchhead crashes it writes a report to the `crashes`
folder of its data folder and offers to show it at the next start; reports are never sent.

## How it is built

| Crate | What it does |
| --- | --- |
| `crates/git` (`switchhead-git`) | Git engine: gitoxide reads, `git` CLI writes, streaming log, incremental graph layout, status, diff, partial staging, conflicts, rebase, blame |
| `crates/hosting` (`switchhead-hosting`) | GitHub, GitLab, Bitbucket and Azure DevOps APIs, tokens in the keychain, the update feed |
| `crates/agents` (`switchhead-agents`) | Coding-CLI adapters and runner for the assistant |
| `crates/mcp` (`switchhead-mcp`) | The read-only git MCP server the assistant uses (`switchhead mcp`) |
| `crates/app` (`switchhead-app`) | The GPUI application |

switchhead shares its design system with
[Switchyard](https://github.com/jeremielumandong/switchyard), a native database, SSH and
file-transfer client on the same stack.

## License

[MIT](LICENSE). Bundled fonts (Geist) and icons (Lucide) keep their own licenses.
