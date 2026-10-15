//! Who commits: the repository's `user.name`, `user.email` and `user.signingkey`, and
//! where each comes from. Identity profiles (saved sets of the three) are applied by
//! writing them to the repository's local config, so the command line sees them too.

use crate::{Error, Repo, Result};

/// A name, email and optional signing key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub email: String,
    /// `user.signingkey` (a GPG key id or an SSH key); `None` leaves signing to the
    /// user's other config.
    pub signing_key: Option<String>,
}

/// Where a config value was set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// This repository's `.git/config` (or a worktree's config).
    Local,
    /// The user's or system config, or an included file.
    Elsewhere,
}

/// One config value and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Value {
    pub value: String,
    pub scope: Scope,
}

/// The identity commits get now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Current {
    pub name: Option<Value>,
    pub email: Option<Value>,
    pub signing_key: Option<Value>,
    /// `commit.gpgsign` is on: commits are signed.
    pub signs_commits: bool,
}

impl Current {
    /// Whether the repository sets its own name or email.
    pub fn is_local(&self) -> bool {
        [&self.name, &self.email]
            .into_iter()
            .flatten()
            .any(|v| v.scope == Scope::Local)
    }

    /// Whether this is `id` (name, email, and the signing key when `id` has one).
    pub fn matches(&self, id: &Identity) -> bool {
        fn get(v: &Option<Value>) -> Option<&str> {
            v.as_ref().map(|v| v.value.as_str())
        }
        get(&self.name) == Some(id.name.as_str())
            && get(&self.email) == Some(id.email.as_str())
            && id
                .signing_key
                .as_deref()
                .is_none_or(|k| get(&self.signing_key) == Some(k))
    }
}

const KEYS: [&str; 3] = ["user.name", "user.email", "user.signingkey"];

impl Repo {
    /// The identity in effect for this repository.
    pub fn current_identity(&self) -> Result<Current> {
        let git = self.git()?;
        let get = |key: &str| -> Result<Option<Value>> {
            match git.text(&["config", "--show-scope", "--get", key]) {
                Ok(out) => Ok(parse_scoped(&out)),
                // Exit code 1: the key is not set.
                Err(Error::Command { code: Some(1), .. }) => Ok(None),
                Err(e) => Err(e),
            }
        };
        Ok(Current {
            name: get(KEYS[0])?,
            email: get(KEYS[1])?,
            signing_key: get(KEYS[2])?,
            signs_commits: get("commit.gpgsign")?
                .is_some_and(|v| crate::signing::is_true(&v.value)),
        })
    }

    /// Write `id` to the repository's local config. Without a signing key, a local
    /// `user.signingkey` is removed so the user's own setting applies.
    pub fn set_identity(&self, id: &Identity) -> Result<()> {
        let git = self.git()?;
        git.run(&["config", "--local", KEYS[0], id.name.trim()])?;
        git.run(&["config", "--local", KEYS[1], id.email.trim()])?;
        match id.signing_key.as_deref().map(str::trim) {
            Some(k) if !k.is_empty() => {
                git.run(&["config", "--local", KEYS[2], k])?;
            }
            _ => unset_local(self, KEYS[2])?,
        }
        Ok(())
    }

    /// Remove the repository's own identity, so the user's global one applies.
    pub fn clear_identity(&self) -> Result<()> {
        for key in KEYS {
            unset_local(self, key)?;
        }
        Ok(())
    }
}

fn unset_local(repo: &Repo, key: &str) -> Result<()> {
    match repo.git()?.run(&["config", "--local", "--unset-all", key]) {
        // Exit code 5: the key was not set.
        Ok(_) | Err(Error::Command { code: Some(5), .. }) => Ok(()),
        Err(e) => Err(e),
    }
}

/// `local\tJane Doe` from `git config --show-scope --get`.
fn parse_scoped(out: &str) -> Option<Value> {
    let line = out.lines().next()?;
    let (scope, value) = line.split_once('\t')?;
    Some(Value {
        value: value.to_owned(),
        scope: match scope {
            "local" | "worktree" => Scope::Local,
            _ => Scope::Elsewhere,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    #[test]
    fn parses_scoped_values() {
        let v = parse_scoped("local\tJane Doe\n").unwrap();
        assert_eq!(v.value, "Jane Doe");
        assert_eq!(v.scope, Scope::Local);
        assert_eq!(
            parse_scoped("global\tj@x\n").unwrap().scope,
            Scope::Elsewhere
        );
        assert!(parse_scoped("").is_none());
    }

    #[test]
    fn sets_and_clears_the_local_identity() {
        let t = TestRepo::new();
        let repo = Repo::open(t.path()).unwrap();
        let cur = repo.current_identity().unwrap();
        assert_eq!(cur.name.as_ref().unwrap().value, "Test");
        assert!(cur.is_local());

        let work = Identity {
            name: "Jane Work".into(),
            email: "jane@work.example".into(),
            signing_key: Some("ABC123".into()),
        };
        repo.set_identity(&work).unwrap();
        let cur = repo.current_identity().unwrap();
        assert!(cur.matches(&work));
        assert_eq!(cur.signing_key.unwrap().value, "ABC123");

        // A profile without a key drops the local one.
        let home = Identity {
            name: "Jane".into(),
            email: "jane@home.example".into(),
            signing_key: None,
        };
        repo.set_identity(&home).unwrap();
        let cur = repo.current_identity().unwrap();
        assert!(cur.matches(&home));
        assert!(cur.signing_key.is_none_or(|k| k.scope == Scope::Elsewhere));

        repo.clear_identity().unwrap();
        let cur = repo.current_identity().unwrap();
        assert!(!cur.is_local());
        // Clearing twice is fine.
        repo.clear_identity().unwrap();
    }
}
