//! Commit signing: the settings git signs with (`gpg.format`, `user.signingkey`,
//! `commit.gpgsign`, `tag.gpgsign`, the signing program and SSH's allowed signers file),
//! a test that signs a throwaway commit, and verifying a commit's signature.
//!
//! Signing and verifying both run the system `git`, so GnuPG's agent, `ssh-keygen`,
//! `gpgsm` and the user's own wrappers behave like the command line.

use std::ffi::OsStr;
use std::path::PathBuf;

use crate::cli::Git;
use crate::{Error, GixExt, Oid, Repo, Result};

/// How commits are signed (`gpg.format`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    /// GnuPG (`openpgp`, git's default).
    #[default]
    OpenPgp,
    /// An SSH key, signed and checked with `ssh-keygen`.
    Ssh,
    /// An X.509 certificate through `gpgsm`.
    X509,
}

impl Format {
    /// All formats, in the order the settings show them.
    pub const ALL: [Format; 3] = [Format::OpenPgp, Format::Ssh, Format::X509];

    /// What `gpg.format` says.
    pub fn config_value(self) -> &'static str {
        match self {
            Format::OpenPgp => "openpgp",
            Format::Ssh => "ssh",
            Format::X509 => "x509",
        }
    }

    /// A name for people.
    pub fn name(self) -> &'static str {
        match self {
            Format::OpenPgp => "GPG",
            Format::Ssh => "SSH",
            Format::X509 => "X.509",
        }
    }

    /// The config key naming this format's program.
    pub fn program_key(self) -> &'static str {
        match self {
            Format::OpenPgp => "gpg.program",
            Format::Ssh => "gpg.ssh.program",
            Format::X509 => "gpg.x509.program",
        }
    }

    fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "ssh" => Format::Ssh,
            "x509" => Format::X509,
            _ => Format::OpenPgp,
        }
    }

    /// The format of a signature, from its armor.
    fn of_signature(sig: &[u8]) -> Self {
        let text = String::from_utf8_lossy(&sig[..sig.len().min(64)]);
        if text.contains("BEGIN SSH SIGNATURE") {
            Format::Ssh
        } else if text.contains("BEGIN SIGNED MESSAGE") {
            Format::X509
        } else {
            Format::OpenPgp
        }
    }
}

/// The signing settings in effect for a repository.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub format: Format,
    /// `user.signingkey`: a GPG key id, an SSH public key file, or `key::<public key>`
    /// for a key in the SSH agent (empty: git picks from the committer identity).
    pub key: String,
    /// Each format's program (empty: git's default), indexed like [`Format::ALL`].
    pub programs: [String; 3],
    /// `gpg.ssh.allowedSignersFile`: who may sign, for checking SSH signatures.
    pub allowed_signers: String,
    /// `commit.gpgsign`: sign every commit.
    pub sign_commits: bool,
    /// `tag.gpgsign`: sign every annotated tag.
    pub sign_tags: bool,
    /// One of these settings is set in this repository's own config.
    pub local: bool,
}

impl Settings {
    /// The program for the chosen format.
    pub fn program(&self) -> &str {
        &self.programs[index(self.format)]
    }

    /// Read `git config --show-scope --get-regexp` output (keys are lowercased by git;
    /// later lines win, like git's own lookup).
    fn parse(out: &str) -> Self {
        let mut s = Settings::default();
        let mut openpgp_program: Option<String> = None;
        for line in out.lines() {
            let Some((scope, rest)) = line.split_once('\t') else {
                continue;
            };
            let (key, value) = rest.split_once(' ').unwrap_or((rest, ""));
            let value = value.to_owned();
            let known = match key {
                "gpg.format" => {
                    s.format = Format::parse(&value);
                    true
                }
                "user.signingkey" => {
                    s.key = value;
                    true
                }
                "gpg.program" => {
                    s.programs[0] = value;
                    true
                }
                // Newer spelling; it wins over `gpg.program`.
                "gpg.openpgp.program" => {
                    openpgp_program = Some(value);
                    true
                }
                "gpg.ssh.program" => {
                    s.programs[1] = value;
                    true
                }
                "gpg.x509.program" => {
                    s.programs[2] = value;
                    true
                }
                "gpg.ssh.allowedsignersfile" => {
                    s.allowed_signers = value;
                    true
                }
                "commit.gpgsign" => {
                    s.sign_commits = is_true(&value);
                    true
                }
                "tag.gpgsign" => {
                    s.sign_tags = is_true(&value);
                    true
                }
                _ => false,
            };
            if known && matches!(scope, "local" | "worktree") {
                s.local = true;
            }
        }
        if let Some(p) = openpgp_program {
            s.programs[0] = p;
        }
        s
    }

    /// `-c key=value` pairs that make git sign with these settings (empty values leave
    /// git's own).
    fn overrides(&self) -> Vec<String> {
        let mut args = vec![
            "-c".to_owned(),
            format!("gpg.format={}", self.format.config_value()),
        ];
        let mut set = |key: &str, value: &str| {
            if !value.trim().is_empty() {
                args.push("-c".to_owned());
                args.push(format!("{key}={}", value.trim()));
            }
        };
        set("user.signingkey", &self.key);
        if self.format == Format::OpenPgp {
            set("gpg.openpgp.program", self.program());
        } else {
            set(self.format.program_key(), self.program());
        }
        set("gpg.ssh.allowedSignersFile", &self.allowed_signers);
        args
    }
}

fn index(f: Format) -> usize {
    Format::ALL.iter().position(|x| *x == f).unwrap_or(0)
}

/// A git boolean (a key with no value is true).
pub(crate) fn is_true(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "" | "true" | "yes" | "on" | "1"
    )
}

/// What checking a signature found (git's `%G?`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A good signature from a trusted key (`G`).
    Good,
    /// A good signature, but the key is not trusted: not in the allowed signers file
    /// (SSH) or not certified (GPG) (`U`).
    Untrusted,
    /// A good signature that has expired (`X`).
    Expired,
    /// A good signature made by a key that has expired since (`Y`).
    ExpiredKey,
    /// A signature made by a revoked key (`R`).
    Revoked,
    /// The signature does not match the commit (`B`).
    Bad,
    /// The signature could not be checked: the key is missing, or SSH has no allowed
    /// signers file (`E`, or `N` on a signed commit).
    CannotCheck,
}

impl Verdict {
    fn parse(code: &str) -> Self {
        match code.trim() {
            "G" => Verdict::Good,
            "U" => Verdict::Untrusted,
            "X" => Verdict::Expired,
            "Y" => Verdict::ExpiredKey,
            "R" => Verdict::Revoked,
            "B" => Verdict::Bad,
            _ => Verdict::CannotCheck,
        }
    }
}

/// A commit's signature, checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verification {
    pub verdict: Verdict,
    /// How it was signed.
    pub format: Format,
    /// Who signed (GPG user id, or the SSH allowed signers principal; may be empty).
    pub signer: String,
    /// The signing key (GPG key id, or SSH fingerprint; may be empty).
    pub key: String,
    /// What the checking program said (or why it could not check).
    pub details: String,
}

const VERIFY_FORMAT: &str = "--format=%G?%x00%GS%x00%GK%x00%GG";

impl Repo {
    /// The signing settings in effect.
    pub fn signing_settings(&self) -> Result<Settings> {
        let out = match self.git()?.text(&[
            "config",
            "--show-scope",
            "--get-regexp",
            r"^(gpg\.|user\.signingkey$|commit\.gpgsign$|tag\.gpgsign$)",
        ]) {
            Ok(out) => out,
            // Exit code 1: nothing matches.
            Err(Error::Command { code: Some(1), .. }) => String::new(),
            Err(e) => return Err(e),
        };
        Ok(Settings::parse(&out))
    }

    /// Save `s` to this repository's config, or to the user's (`everywhere`), where it
    /// applies to every repository: then this repository's own settings are removed so
    /// the user's apply here too.
    pub fn save_signing_settings(&self, s: &Settings, everywhere: bool) -> Result<()> {
        let git = self.git()?;
        let scope = if everywhere { "--global" } else { "--local" };
        let set = |key: &str, value: &str| -> Result<()> {
            let value = value.trim();
            if value.is_empty() {
                unset(&git, scope, key)
            } else {
                git.run(&["config", scope, key, value]).map(drop)
            }
        };
        set("gpg.format", s.format.config_value())?;
        set("user.signingkey", &s.key)?;
        for f in Format::ALL {
            set(f.program_key(), &s.programs[index(f)])?;
        }
        // `gpg.program` is written; the newer spelling would win over it.
        unset(&git, scope, "gpg.openpgp.program")?;
        set("gpg.ssh.allowedSignersFile", &s.allowed_signers)?;
        set(
            "commit.gpgsign",
            if s.sign_commits { "true" } else { "false" },
        )?;
        set("tag.gpgsign", if s.sign_tags { "true" } else { "false" })?;
        if everywhere {
            for key in [
                "gpg.format",
                "user.signingkey",
                "gpg.program",
                "gpg.openpgp.program",
                "gpg.ssh.program",
                "gpg.x509.program",
                "gpg.ssh.allowedSignersFile",
                "commit.gpgsign",
                "tag.gpgsign",
            ] {
                unset(&git, "--local", key)?;
            }
        }
        Ok(())
    }

    /// Sign a throwaway commit with `s` and check it, to show the settings work before
    /// they are used. The commit is written to a temporary object folder (the
    /// repository's objects are only read), so nothing is left behind.
    pub fn test_signing(&self, s: &Settings) -> Result<Verification> {
        let tmp = TempObjects::new()?;
        let objects = self.local().common_dir().join("objects");
        let env: [(&str, &OsStr); 2] = [
            ("GIT_OBJECT_DIRECTORY", tmp.0.as_os_str()),
            ("GIT_ALTERNATE_OBJECT_DIRECTORIES", objects.as_os_str()),
        ];
        let git = self.git()?;
        let tree = git.run_with(&["mktree"], Some(b""), &env)?.text();
        let tree = tree.trim();
        let mut args = s.overrides();
        args.extend(
            ["commit-tree", "-S", tree, "-m", "switchhead signing test"].map(str::to_owned),
        );
        let commit = git.run_with(&args, None, &env)?.text();
        let mut args = s.overrides();
        args.extend(["show", "-s", VERIFY_FORMAT, commit.trim()].map(str::to_owned));
        let out = git.run_with(&args, None, &env)?;
        Ok(parse_verification(&out.text(), &out.err_text(), s.format))
    }

    /// Check commit `id`'s signature (`None`: it is not signed). Unsigned commits are
    /// told apart in process; only signed ones run git.
    pub fn verify_commit(&self, id: Oid) -> Result<Option<Verification>> {
        let format = {
            let r = self.local();
            let commit = r.find_commit(id).gix()?;
            let decoded = commit.decode().gix()?;
            match decoded.extra_headers().pgp_signature() {
                Some(sig) => Format::of_signature(sig),
                None => return Ok(None),
            }
        };
        let out = self
            .git()?
            .run(&["show", "-s", VERIFY_FORMAT, &id.to_string()])?;
        Ok(Some(parse_verification(
            &out.text(),
            &out.err_text(),
            format,
        )))
    }
}

/// `%G?\0%GS\0%GK\0%GG` and git's stderr, as a [`Verification`].
fn parse_verification(out: &str, err: &str, format: Format) -> Verification {
    let mut parts = out.splitn(4, '\0');
    let mut next = || parts.next().unwrap_or_default().trim().to_owned();
    let verdict = Verdict::parse(&next());
    let signer = next();
    let key = next();
    let mut details = next();
    if details.is_empty() {
        // Nothing checked it: git says why on stderr ("error: gpg.ssh.allowedSignersFile
        // needs to be configured...").
        details = err
            .lines()
            .map(|l| l.strip_prefix("error: ").unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned();
    }
    Verification {
        verdict,
        format,
        signer,
        key,
        details,
    }
}

fn unset(git: &Git, scope: &str, key: &str) -> Result<()> {
    match git.run(&["config", scope, "--unset-all", key]) {
        // Exit code 5: the key was not set.
        Ok(_) | Err(Error::Command { code: Some(5), .. }) => Ok(()),
        Err(e) => Err(e),
    }
}

/// A temporary object folder, removed when dropped.
struct TempObjects(PathBuf);

impl TempObjects {
    fn new() -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        let dir =
            std::env::temp_dir().join(format!("switchhead-sign-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for TempObjects {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    #[test]
    fn parses_settings() {
        let s = Settings::parse(
            "global\tgpg.format ssh\n\
             global\tuser.signingkey ~/.ssh/id.pub\n\
             global\tcommit.gpgsign true\n\
             local\tgpg.ssh.allowedsignersfile /a/b\n\
             global\tgpg.program gpg2\n\
             global\tgpg.openpgp.program /opt/gpg\n\
             local\ttag.gpgsign\n",
        );
        assert_eq!(s.format, Format::Ssh);
        assert_eq!(s.key, "~/.ssh/id.pub");
        assert!(s.sign_commits);
        assert!(s.sign_tags);
        assert_eq!(s.allowed_signers, "/a/b");
        assert_eq!(s.programs[0], "/opt/gpg");
        assert!(s.local);
        let s = Settings::parse("global\tcommit.gpgsign false\nsystem\tgpg.format x509\n");
        assert!(!s.sign_commits);
        assert_eq!(s.format, Format::X509);
        assert!(!s.local);
    }

    #[test]
    fn parses_verification() {
        let v = parse_verification(
            "G\0t@x\0SHA256:abc\0Good \"git\" signature for t@x\n",
            "",
            Format::Ssh,
        );
        assert_eq!(v.verdict, Verdict::Good);
        assert_eq!(v.signer, "t@x");
        assert_eq!(v.key, "SHA256:abc");
        let v = parse_verification(
            "N\0\0\0",
            "error: gpg.ssh.allowedSignersFile needs to be configured\n",
            Format::Ssh,
        );
        assert_eq!(v.verdict, Verdict::CannotCheck);
        assert_eq!(
            v.details,
            "gpg.ssh.allowedSignersFile needs to be configured"
        );
        assert_eq!(
            Format::of_signature(b"-----BEGIN PGP SIGNATURE-----\n"),
            Format::OpenPgp
        );
    }

    #[test]
    fn saves_settings_locally() {
        let t = TestRepo::new();
        let repo = Repo::open(t.path()).unwrap();
        let mut s = Settings {
            format: Format::Ssh,
            key: "/keys/id.pub".into(),
            allowed_signers: "/keys/allowed".into(),
            sign_commits: true,
            ..Settings::default()
        };
        s.programs[1] = "ssh-keygen".into();
        repo.save_signing_settings(&s, false).unwrap();
        let read = repo.signing_settings().unwrap();
        assert_eq!(read.format, Format::Ssh);
        assert_eq!(read.key, "/keys/id.pub");
        assert_eq!(read.program(), "ssh-keygen");
        assert_eq!(read.allowed_signers, "/keys/allowed");
        assert!(read.sign_commits && !read.sign_tags && read.local);
        // Clearing a field removes it.
        s.allowed_signers.clear();
        repo.save_signing_settings(&s, false).unwrap();
        let git = repo.git().unwrap();
        assert!(
            git.run(&["config", "--local", "--get", "gpg.ssh.allowedSignersFile"])
                .is_err()
        );
    }

    /// A repository signing with a fresh SSH key; `None` without `ssh-keygen`.
    fn ssh_repo() -> Option<(TestRepo, Settings)> {
        let t = TestRepo::new();
        let key = t.join("key");
        let made = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "signer", "-f"])
            .arg(&key)
            .output()
            .ok()?;
        if !made.status.success() {
            return None;
        }
        let public = std::fs::read_to_string(t.join("key.pub")).ok()?;
        let allowed = t.join("allowed");
        std::fs::write(&allowed, format!("test@example.com {public}")).ok()?;
        let mut s = Settings {
            format: Format::Ssh,
            key: t.join("key.pub").display().to_string(),
            allowed_signers: allowed.display().to_string(),
            sign_commits: true,
            ..Settings::default()
        };
        // The user's global config may name another program.
        s.programs[1] = "ssh-keygen".into();
        Some((t, s))
    }

    #[test]
    fn signs_and_verifies_with_ssh() {
        let Some((t, s)) = ssh_repo() else {
            eprintln!("ssh-keygen is not available; skipping");
            return;
        };
        let repo = Repo::open(t.path()).unwrap();
        let unsigned: Oid = t.commit_file("a.txt", "a\n", "unsigned").parse().unwrap();
        assert_eq!(repo.verify_commit(unsigned).unwrap(), None);

        // The test signs without touching the repository's objects or config.
        let objects = || {
            t.git(&["count-objects", "-v"])
                .lines()
                .find(|l| l.starts_with("count:"))
                .map(str::to_owned)
        };
        let before = objects();
        let v = repo.test_signing(&s).unwrap();
        assert_eq!(v.verdict, Verdict::Good, "{}", v.details);
        assert_eq!(v.signer, "test@example.com");
        assert_eq!(objects(), before);

        repo.save_signing_settings(&s, false).unwrap();
        t.write("b.txt", "b\n");
        t.git(&["add", "b.txt"]);
        repo.git()
            .unwrap()
            .run(&["commit", "-q", "-m", "signed"])
            .unwrap();
        let head = repo.resolve("HEAD").unwrap();
        let v = repo.verify_commit(head).unwrap().unwrap();
        assert_eq!(v.verdict, Verdict::Good, "{}", v.details);
        assert_eq!(v.format, Format::Ssh);
        assert!(v.key.starts_with("SHA256:"));

        // Someone else's key: a good signature from an unknown signer.
        std::fs::write(t.join("allowed"), "").unwrap();
        let v = repo.verify_commit(head).unwrap().unwrap();
        assert_eq!(v.verdict, Verdict::Untrusted);

        // No allowed signers file: it cannot be checked, and git says why.
        t.git(&["config", "--unset", "gpg.ssh.allowedSignersFile"]);
        let v = repo.verify_commit(head).unwrap().unwrap();
        assert_eq!(v.verdict, Verdict::CannotCheck);
        assert!(v.details.contains("allowedSignersFile"), "{}", v.details);
    }

    #[test]
    fn a_failing_test_signature_is_an_error() {
        let Some((t, mut s)) = ssh_repo() else {
            return;
        };
        let repo = Repo::open(t.path()).unwrap();
        s.key = t.join("missing.pub").display().to_string();
        assert!(repo.test_signing(&s).is_err());
    }
}
