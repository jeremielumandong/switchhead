//! Paths other programs can use.
//!
//! On Windows `std::fs::canonicalize` returns verbatim paths (`\\?\C:\repo`,
//! `\\?\UNC\server\share`). Windows' own APIs accept them, but most programs do not:
//! Windows Terminal, PowerShell and `cmd` refuse one as a starting directory, and editors
//! show it as typed. [`canonicalize`] returns the ordinary form whenever it means the
//! same file.

use std::io;
use std::path::{Path, PathBuf};

/// `std::fs::canonicalize` without the verbatim prefix where it is not needed.
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    std::fs::canonicalize(path).map(|p| simplified(&p))
}

/// `path` without a verbatim `\\?\` prefix when the ordinary form names the same file
/// (on Windows; elsewhere `path` itself).
pub fn simplified(path: &Path) -> PathBuf {
    if !cfg!(windows) {
        return path.to_path_buf();
    }
    match path.to_str().and_then(strip_verbatim) {
        Some(plain) => PathBuf::from(plain),
        None => path.to_path_buf(),
    }
}

/// The ordinary form of a verbatim Windows path, when there is one: `\\?\C:\a` → `C:\a`,
/// `\\?\UNC\server\share\a` → `\\server\share\a`. `None` for other paths and for ones
/// only the verbatim form can name (too long, `.` or `..` parts, names Windows would
/// change or treat as devices).
fn strip_verbatim(path: &str) -> Option<String> {
    let rest = path.strip_prefix(r"\\?\")?;
    let (plain, tail) = if let Some(unc) = rest
        .strip_prefix(r"UNC\")
        .or_else(|| rest.strip_prefix(r"unc\"))
    {
        (format!(r"\\{unc}"), unc)
    } else {
        let b = rest.as_bytes();
        if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || b[2] != b'\\' {
            return None;
        }
        (rest.to_owned(), &rest[3..])
    };
    // MAX_PATH, counting the terminating NUL.
    if plain.len() >= 260 || !tail.split('\\').all(plain_component) {
        return None;
    }
    Some(plain)
}

/// Whether a path component means the same thing outside a verbatim path.
fn plain_component(c: &str) -> bool {
    const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    if c == "." || c == ".." || c.ends_with('.') || c.ends_with(' ') || c.contains('/') {
        return false;
    }
    let stem = c.split('.').next().unwrap_or(c).trim_end();
    let upper = stem.to_ascii_uppercase();
    let device = RESERVED.contains(&upper.as_str())
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && upper.as_bytes()[3].is_ascii_digit());
    !device
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_drive_and_unc_prefixes() {
        assert_eq!(
            strip_verbatim(r"\\?\C:\development\AI\agentopschat").as_deref(),
            Some(r"C:\development\AI\agentopschat")
        );
        assert_eq!(strip_verbatim(r"\\?\D:\").as_deref(), Some(r"D:\"));
        assert_eq!(
            strip_verbatim(r"\\?\UNC\server\share\repo").as_deref(),
            Some(r"\\server\share\repo")
        );
    }

    #[test]
    fn keeps_paths_only_the_verbatim_form_names() {
        assert_eq!(strip_verbatim(r"C:\repo"), None);
        assert_eq!(strip_verbatim(r"\\?\Volume{1234}\repo"), None);
        assert_eq!(strip_verbatim(r"\\?\C:\repo\nul"), None);
        assert_eq!(strip_verbatim(r"\\?\C:\repo\com1.txt"), None);
        assert_eq!(strip_verbatim(r"\\?\C:\repo\trailing."), None);
        assert_eq!(strip_verbatim(r"\\?\C:\repo\..\x"), None);
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        assert_eq!(strip_verbatim(&long), None);
        // Not devices.
        assert!(strip_verbatim(r"\\?\C:\console\com10\nullable").is_some());
    }

    #[test]
    fn canonicalizes_existing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let p = canonicalize(dir.path()).unwrap();
        assert!(p.is_absolute());
        assert!(!p.to_string_lossy().starts_with(r"\\?\"));
    }
}
