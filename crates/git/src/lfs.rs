//! Git LFS: whether a repository uses it, its pointer files, the local object store, and
//! file locks. Downloads and locks go through `git lfs` ([`Repo::lfs`] streams `fetch`,
//! `pull` and `prune`), so the user's LFS config, credentials and transfer agents apply.
//! A pointer whose object is in `.git/lfs/objects` can be diffed as its real content.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::cli::Cancel;
use crate::{Error, Repo, Result};

/// The first line of every LFS pointer file.
const POINTER_VERSION: &str = "version https://git-lfs.github.com/spec/v1";

/// Pointer files are small; anything bigger is real content.
const MAX_POINTER_BYTES: usize = 1024;

/// An LFS pointer: the stand-in git stores for a large file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pointer {
    /// SHA-256 of the content, in hex.
    pub oid: String,
    /// Size of the content in bytes.
    pub size: u64,
}

/// Parse `data` as an LFS pointer (`None` when it is anything else).
pub fn parse_pointer(data: &[u8]) -> Option<Pointer> {
    if data.len() > MAX_POINTER_BYTES {
        return None;
    }
    let text = std::str::from_utf8(data).ok()?;
    let mut lines = text.lines();
    if lines.next()?.trim_end() != POINTER_VERSION {
        return None;
    }
    let (mut oid, mut size) = (None, None);
    for line in lines {
        let (key, value) = line.split_once(' ')?;
        match key {
            "oid" => {
                let hex = value.strip_prefix("sha256:")?;
                if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return None;
                }
                oid = Some(hex.to_ascii_lowercase());
            }
            "size" => size = Some(value.parse().ok()?),
            _ => {}
        }
    }
    Some(Pointer {
        oid: oid?,
        size: size?,
    })
}

/// Whether a `git diff` of pointer files (what git shows for an LFS file) is in `text`:
/// the diff's lines are pointer lines.
pub fn is_pointer_diff_line(text: &str) -> bool {
    text.starts_with(POINTER_VERSION)
}

/// One side of a diff after looking through LFS pointers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Side {
    /// Ordinary content, or an LFS object read from the local store.
    Content(Vec<u8>),
    /// An LFS pointer whose object has not been downloaded.
    Missing(Pointer),
}

/// A lock on an LFS file, from the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lock {
    /// The server's lock id.
    pub id: String,
    /// Path in the repository.
    pub path: PathBuf,
    /// Who holds it.
    pub owner: Option<String>,
    /// When it was taken (RFC 3339, as the server sent it).
    pub locked_at: Option<String>,
}

#[derive(Deserialize)]
struct LockJson {
    id: String,
    path: String,
    owner: Option<OwnerJson>,
    locked_at: Option<String>,
}

#[derive(Deserialize)]
struct OwnerJson {
    name: String,
}

/// Parse `git lfs locks --json`.
pub fn parse_locks(json: &str) -> Result<Vec<Lock>> {
    let raw: Vec<LockJson> = serde_json::from_str(json.trim()).map_err(|e| Error::Command {
        args: "lfs locks --json".into(),
        code: None,
        message: format!("unexpected output: {e}"),
    })?;
    Ok(raw
        .into_iter()
        .map(|l| Lock {
            id: l.id,
            path: PathBuf::from(l.path),
            owner: l.owner.map(|o| o.name),
            locked_at: l.locked_at,
        })
        .collect())
}

/// The installed `git lfs` version line (`None` when git-lfs is not installed).
pub fn version() -> Option<String> {
    let out = crate::cli::Git::new(std::env::temp_dir())
        .ok()?
        .run(&["lfs", "version"])
        .ok()?;
    let text = out.text();
    let line = text.lines().next()?.trim();
    line.starts_with("git-lfs/").then(|| line.to_owned())
}

/// Ask git-lfs for progress lines although its stderr is a pipe.
pub(crate) fn force_progress() -> (&'static str, &'static OsStr) {
    ("GIT_LFS_FORCE_PROGRESS", OsStr::new("1"))
}

/// Leave pointers in the work tree on checkout (the clone pulls objects afterwards).
pub(crate) fn skip_smudge() -> (&'static str, &'static OsStr) {
    ("GIT_LFS_SKIP_SMUDGE", OsStr::new("1"))
}

/// Whether a `.gitattributes` file routes any file through the `lfs` filter.
fn attributes_use_lfs(attrs: &str) -> bool {
    attrs.lines().any(|l| {
        let l = l.trim();
        !l.starts_with('#') && l.split_whitespace().any(|w| w == "filter=lfs")
    })
}

/// What a clone did about LFS files ([`crate::ops::clone`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloneSetup {
    /// The repository does not use LFS.
    NotUsed,
    /// The LFS files were downloaded and checked out.
    Downloaded,
    /// The user's config skips LFS downloads on checkout (`--skip`, `GIT_LFS_SKIP_SMUDGE`),
    /// so the files stay pointers.
    Skipped,
    /// The repository uses LFS but git-lfs is not installed: the files are pointers.
    NotInstalled,
    /// Downloading failed (git-lfs's reason); the clone is kept with pointers.
    Failed(String),
}

impl CloneSetup {
    /// A one-line note for the user when the LFS files are not in the work tree.
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::NotUsed | Self::Downloaded => None,
            Self::Skipped => Some(
                "LFS files were not downloaded (your git config skips them): use LFS Pull to get them."
                    .into(),
            ),
            Self::NotInstalled => Some(
                "This repository uses Git LFS, which is not installed: install it from git-lfs.com, then use LFS Pull."
                    .into(),
            ),
            Self::Failed(why) => Some(format!(
                "Cloned, but LFS files were not downloaded ({why}): use LFS Pull to try again."
            )),
        }
    }
}

/// The line of a failed `git lfs` run that says why: the first that is not progress.
fn failure_reason(e: &Error) -> String {
    let Error::Command { message, .. } = e else {
        return e.to_string();
    };
    message
        .split(['\r', '\n'])
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("Downloading LFS objects"))
        .unwrap_or("git lfs pull failed")
        .to_owned()
}

impl Repo {
    /// Whether the repository uses LFS: its `.gitattributes` routes files through the
    /// `lfs` filter, or it has a local LFS store. Reads two small files.
    pub fn uses_lfs(&self) -> bool {
        let attrs = std::fs::read_to_string(self.root().join(".gitattributes")).unwrap_or_default();
        attributes_use_lfs(&attrs) || self.lfs_dir().is_dir()
    }

    /// Whether any tracked `.gitattributes` (at the root or in a folder) routes files
    /// through LFS. Lists the index, so it suits a one-off check such as after a clone.
    pub fn tracks_lfs(&self) -> Result<bool> {
        let out = self
            .git()?
            .run(&["ls-files", "-z", "--", ":(glob)**/.gitattributes"])?;
        Ok(out
            .stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .any(|p| {
                let path = self.root().join(String::from_utf8_lossy(p).as_ref());
                std::fs::read_to_string(path).is_ok_and(|a| attributes_use_lfs(&a))
            }))
    }

    /// Set up LFS in a clone made with [`skip_smudge`] and download its files: initialize
    /// git-lfs for this repository when the user never ran `git lfs install` (its filters
    /// and hooks), then `git lfs pull`, streaming progress to `on_line`.
    pub fn lfs_setup_clone(&self, cancel: &Cancel, on_line: impl FnMut(&str)) -> CloneSetup {
        match self.tracks_lfs() {
            Ok(true) => {}
            Ok(false) => return CloneSetup::NotUsed,
            Err(e) => return CloneSetup::Failed(failure_reason(&e)),
        }
        if version().is_none() {
            return CloneSetup::NotInstalled;
        }
        let Ok(git) = self.git() else {
            return CloneSetup::Failed("git could not be started".into());
        };
        let get = |key: &str| git.text(&["config", "--get", key]).unwrap_or_default();
        let (process, smudge) = (get("filter.lfs.process"), get("filter.lfs.smudge"));
        if process.trim().is_empty() && smudge.trim().is_empty() {
            // Without the filters git would store LFS files as plain blobs on the next
            // commit, and without the pre-push hook a push would not upload them.
            if let Err(e) = git.run(&["lfs", "install", "--local"]) {
                return CloneSetup::Failed(failure_reason(&e));
            }
        } else if process.contains("--skip")
            || smudge.contains("--skip")
            || std::env::var_os("GIT_LFS_SKIP_SMUDGE").is_some_and(|v| v != "0" && !v.is_empty())
        {
            return CloneSetup::Skipped;
        }
        match self.lfs(&["pull"], cancel, on_line) {
            Ok(()) => CloneSetup::Downloaded,
            Err(e) => CloneSetup::Failed(failure_reason(&e)),
        }
    }

    /// `.git/lfs` (shared by all worktrees).
    fn lfs_dir(&self) -> PathBuf {
        self.local().common_dir().join("lfs")
    }

    /// Where `p`'s object is in the local store, if it has been downloaded.
    pub fn lfs_object(&self, p: &Pointer) -> Option<PathBuf> {
        let path = self
            .lfs_dir()
            .join("objects")
            .join(&p.oid[..2])
            .join(&p.oid[2..4])
            .join(&p.oid);
        path.is_file().then_some(path)
    }

    /// Look through an LFS pointer: its object's bytes when downloaded, else
    /// [`Side::Missing`]. Other content comes back as it is.
    pub fn lfs_side(&self, data: Vec<u8>) -> Side {
        let Some(p) = parse_pointer(&data) else {
            return Side::Content(data);
        };
        match self.lfs_object(&p).map(std::fs::read) {
            Some(Ok(bytes)) => Side::Content(bytes),
            _ => Side::Missing(p),
        }
    }

    /// The locks on the server (`git lfs locks --json`).
    pub fn lfs_locks(&self) -> Result<Vec<Lock>> {
        let out = self.git()?.run(&["lfs", "locks", "--json"])?;
        parse_locks(&out.text())
    }

    /// Lock a file on the server.
    pub fn lfs_lock(&self, path: &Path) -> Result<()> {
        let path = path.to_string_lossy();
        self.git()?.run(&["lfs", "lock", "--", path.as_ref()])?;
        Ok(())
    }

    /// Unlock a file, by its lock's id when known (exact, where a path lookup can be
    /// ambiguous); `force` breaks someone else's lock.
    pub fn lfs_unlock(&self, path: &Path, id: Option<&str>, force: bool) -> Result<()> {
        let path = path.to_string_lossy();
        let mut args = vec!["lfs", "unlock"];
        if force {
            args.push("--force");
        }
        match id {
            Some(id) => args.extend(["--id", id]),
            None => args.extend(["--", path.as_ref()]),
        }
        self.git()?.run(&args)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    const OID: &str = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";

    fn pointer(size: u64) -> String {
        format!("{POINTER_VERSION}\noid sha256:{OID}\nsize {size}\n")
    }

    #[test]
    fn parses_pointers() {
        assert_eq!(
            parse_pointer(pointer(12345).as_bytes()),
            Some(Pointer {
                oid: OID.into(),
                size: 12345
            })
        );
        assert_eq!(parse_pointer(b"hello\n"), None);
        assert_eq!(
            parse_pointer(format!("{POINTER_VERSION}\nsize 3\n").as_bytes()),
            None
        );
        assert_eq!(
            parse_pointer(format!("{POINTER_VERSION}\noid sha256:abc\nsize 3\n").as_bytes()),
            None
        );
        let long = format!("{}{}", pointer(1), "x".repeat(2000));
        assert_eq!(parse_pointer(long.as_bytes()), None);
    }

    #[test]
    fn parses_locks() {
        let json = r#"[{"id":"7","path":"art/logo.psd","owner":{"name":"ada"},"locked_at":"2026-10-15T01:02:03Z"},{"id":"8","path":"b.bin"}]"#;
        let locks = parse_locks(json).unwrap();
        assert_eq!(locks.len(), 2);
        assert_eq!(locks[0].path, PathBuf::from("art/logo.psd"));
        assert_eq!(locks[0].owner.as_deref(), Some("ada"));
        assert_eq!(locks[1].owner, None);
        assert!(parse_locks("[]").unwrap().is_empty());
        assert!(parse_locks("Not in a git repository").is_err());
    }

    #[test]
    fn reads_objects_from_the_local_store() {
        let t = TestRepo::new();
        let repo = Repo::open(t.path()).unwrap();
        assert!(!repo.uses_lfs());
        t.write(
            ".gitattributes",
            "*.bin filter=lfs diff=lfs merge=lfs -text\n",
        );
        assert!(repo.uses_lfs());

        let p = parse_pointer(pointer(5).as_bytes()).unwrap();
        assert_eq!(
            repo.lfs_side(pointer(5).into_bytes()),
            Side::Missing(p.clone())
        );
        let dir = t
            .path()
            .join(".git/lfs/objects")
            .join(&OID[..2])
            .join(&OID[2..4]);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(OID), b"hello").unwrap();
        assert!(repo.lfs_object(&p).is_some());
        assert_eq!(
            repo.lfs_side(pointer(5).into_bytes()),
            Side::Content(b"hello".to_vec())
        );
        assert_eq!(
            repo.lfs_side(b"plain".to_vec()),
            Side::Content(b"plain".to_vec())
        );
    }

    #[test]
    fn tracked_files_commit_as_pointers() {
        if version().is_none() {
            eprintln!("git-lfs is not installed; skipping");
            return;
        }
        let t = TestRepo::new();
        t.git(&["lfs", "install", "--local"]);
        t.git(&["lfs", "track", "*.bin"]);
        t.write("a.bin", "large content\n");
        t.git(&["add", ".gitattributes", "a.bin"]);
        t.git(&["commit", "-qm", "Add a.bin"]);
        let repo = Repo::open(t.path()).unwrap();
        assert!(repo.uses_lfs());
        let blob = crate::diff::read_at(&repo, Path::new("a.bin"), crate::diff::Source::Head)
            .unwrap()
            .unwrap();
        let p = parse_pointer(&blob).expect("a pointer in the commit");
        assert_eq!(p.size, 14);
        assert_eq!(
            repo.lfs_side(blob),
            Side::Content(b"large content\n".to_vec())
        );
    }

    /// A `file://` URL for `path` (git-lfs reads and writes such remotes directly).
    fn file_url(path: &Path) -> String {
        let p = path.to_string_lossy().replace('\\', "/");
        if p.starts_with('/') {
            format!("file://{p}")
        } else {
            format!("file:///{p}")
        }
    }

    /// A bare remote whose `a.bin` is in LFS, with the object uploaded (`upload: false`
    /// leaves the remote's LFS store empty).
    fn lfs_remote(upload: bool) -> (TestRepo, tempfile::TempDir, String) {
        let t = TestRepo::new();
        t.git(&["lfs", "install", "--local"]);
        t.git(&["lfs", "track", "*.bin"]);
        t.write("a.bin", "large content\n");
        t.write("docs/readme.txt", "hi\n");
        t.git(&["add", "."]);
        t.git(&["commit", "-qm", "Add a.bin"]);
        let bare = tempfile::tempdir().unwrap();
        let url = file_url(&bare.path().join("origin.git"));
        t.git(&[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            &bare.path().join("origin.git").to_string_lossy(),
        ]);
        if upload {
            t.git(&["lfs", "push", "--all", &url]);
        }
        t.git(&[
            "-c",
            "lfs.allowincompletepush=true",
            "push",
            "-q",
            "--no-verify",
            &url,
            "main",
        ]);
        (t, bare, url)
    }

    #[test]
    fn finds_lfs_in_nested_attributes() {
        let t = TestRepo::new();
        t.commit_file("a.txt", "a\n", "one");
        let repo = Repo::open(t.path()).unwrap();
        assert!(!repo.tracks_lfs().unwrap());
        t.write("art/.gitattributes", "# none here\n*.txt text\n");
        t.git(&["add", "."]);
        assert!(!repo.tracks_lfs().unwrap());
        t.write(
            "art/.gitattributes",
            "*.psd filter=lfs diff=lfs merge=lfs -text\n",
        );
        assert!(repo.tracks_lfs().unwrap());
        // Untracked attributes do not count.
        t.write(".gitattributes", "*.bin filter=lfs\n");
        t.write("art/.gitattributes", "");
        assert!(!repo.tracks_lfs().unwrap());
    }

    #[test]
    fn failure_reasons_skip_progress() {
        let e = Error::Command {
            args: "lfs pull".into(),
            code: Some(2),
            message: "Downloading LFS objects:   0% (0/1)\rbatch response: Authentication required\nerror: failed to fetch some objects".into(),
        };
        assert_eq!(
            failure_reason(&e),
            "batch response: Authentication required"
        );
        assert_eq!(CloneSetup::NotUsed.warning(), None);
        assert_eq!(CloneSetup::Downloaded.warning(), None);
        assert!(
            CloneSetup::NotInstalled
                .warning()
                .unwrap()
                .contains("not installed")
        );
    }

    // The user's own git config decides between downloading and skipping (`--skip`), so
    // both outcomes are accepted; run with `GIT_CONFIG_NOSYSTEM=1 HOME=<empty dir>` to
    // check a git-lfs that was never initialized.
    #[test]
    fn clone_downloads_lfs_files() {
        if version().is_none() {
            eprintln!("git-lfs is not installed; skipping");
            return;
        }
        let (_t, _bare, url) = lfs_remote(true);
        let work = tempfile::tempdir().unwrap();
        let mut lines = Vec::new();
        let cloned = crate::ops::clone(&url, &work.path().join("c"), &Cancel::default(), |l| {
            lines.push(l.to_owned())
        })
        .unwrap();
        let root = cloned.repo.root().to_path_buf();
        assert!(root.join("docs/readme.txt").is_file());
        match cloned.lfs {
            CloneSetup::Downloaded => {
                assert_eq!(
                    std::fs::read(root.join("a.bin")).unwrap(),
                    b"large content\n"
                );
                assert!(
                    lines
                        .iter()
                        .any(|l| l.starts_with("Downloading LFS objects"))
                );
                // Set up for the next commit and push.
                let git = cloned.repo.git().unwrap();
                assert!(
                    !git.text(&["config", "filter.lfs.process"])
                        .unwrap()
                        .is_empty()
                );
                assert!(
                    cloned
                        .repo
                        .local()
                        .common_dir()
                        .join("hooks/pre-push")
                        .is_file()
                );
                assert!(git.text(&["status", "--porcelain"]).unwrap().is_empty());
            }
            CloneSetup::Skipped => {
                let data = std::fs::read(root.join("a.bin")).unwrap();
                assert!(parse_pointer(&data).is_some());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn clone_keeps_the_work_tree_when_lfs_fails() {
        if version().is_none() {
            eprintln!("git-lfs is not installed; skipping");
            return;
        }
        let (_t, _bare, url) = lfs_remote(false);
        let work = tempfile::tempdir().unwrap();
        let cloned =
            crate::ops::clone(&url, &work.path().join("c"), &Cancel::default(), |_| {}).unwrap();
        let root = cloned.repo.root().to_path_buf();
        assert!(root.join("docs/readme.txt").is_file());
        let data = std::fs::read(root.join("a.bin")).unwrap();
        assert!(parse_pointer(&data).is_some());
        match cloned.lfs {
            CloneSetup::Failed(why) => assert!(!why.is_empty(), "{why}"),
            CloneSetup::Skipped => {}
            other => panic!("unexpected {other:?}"),
        }
    }
}
