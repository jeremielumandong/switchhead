//! Git LFS: whether a repository uses it, its pointer files, the local object store, and
//! file locks. Downloads and locks go through `git lfs` ([`Repo::lfs`] streams `fetch`,
//! `pull` and `prune`), so the user's LFS config, credentials and transfer agents apply.
//! A pointer whose object is in `.git/lfs/objects` can be diffed as its real content.

use std::path::{Path, PathBuf};

use serde::Deserialize;

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

impl Repo {
    /// Whether the repository uses LFS: its `.gitattributes` routes files through the
    /// `lfs` filter, or it has a local LFS store. Reads two small files.
    pub fn uses_lfs(&self) -> bool {
        let attrs = std::fs::read_to_string(self.root().join(".gitattributes")).unwrap_or_default();
        attrs.lines().any(|l| {
            let l = l.trim();
            !l.starts_with('#') && l.split_whitespace().any(|w| w == "filter=lfs")
        }) || self.lfs_dir().is_dir()
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
}
