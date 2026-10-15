//! Git-flow (feature, release and hotfix branches) done with plain `git`, as Fork does: the
//! `git flow` extension is not needed. Settings live in the repository's config under the
//! keys git-flow itself uses (`gitflow.branch.master`, `gitflow.prefix.feature`...), so the
//! extension and switchhead can work on the same repository.
//!
//! Finishing is a series of ordinary steps (check out, merge `--no-ff`, tag, delete). Each
//! step is skipped when it is already done, so after resolving a merge conflict the user
//! finishes again and it picks up where it stopped.

use crate::ops::MergeMode;
use crate::{Error, Repo, RepoState, Result};

/// A kind of git-flow branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// New work, branched from and merged back into the development branch.
    Feature,
    /// Preparing a version: from development, merged into production (tagged) and back.
    Release,
    /// A fix for production: from production, merged into it (tagged) and into development.
    Hotfix,
}

impl Kind {
    /// Every kind, in menu order.
    pub const ALL: [Kind; 3] = [Kind::Feature, Kind::Release, Kind::Hotfix];

    /// "Feature", "Release", "Hotfix".
    pub fn label(self) -> &'static str {
        match self {
            Kind::Feature => "Feature",
            Kind::Release => "Release",
            Kind::Hotfix => "Hotfix",
        }
    }

    /// Whether finishing tags the production branch.
    pub fn tags(self) -> bool {
        !matches!(self, Kind::Feature)
    }
}

/// A repository's git-flow settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// The production branch (`main` or `master`).
    pub master: String,
    /// The development branch (`develop`).
    pub develop: String,
    /// Feature branch prefix (`feature/`).
    pub feature: String,
    /// Release branch prefix (`release/`).
    pub release: String,
    /// Hotfix branch prefix (`hotfix/`).
    pub hotfix: String,
    /// Prefix of the tags releases and hotfixes create (often empty, or `v`).
    pub version_tag: String,
}

impl Config {
    /// git-flow's defaults, with `main` or `master` (whichever of `branches` has one,
    /// `main` first) as production.
    pub fn defaults<'a>(branches: impl IntoIterator<Item = &'a str>) -> Self {
        let branches: Vec<&str> = branches.into_iter().collect();
        let master = ["main", "master"]
            .into_iter()
            .find(|b| branches.contains(b))
            .unwrap_or("main");
        Self {
            master: master.into(),
            develop: "develop".into(),
            feature: "feature/".into(),
            release: "release/".into(),
            hotfix: "hotfix/".into(),
            version_tag: String::new(),
        }
    }

    /// The branch prefix for `kind`.
    pub fn prefix(&self, kind: Kind) -> &str {
        match kind {
            Kind::Feature => &self.feature,
            Kind::Release => &self.release,
            Kind::Hotfix => &self.hotfix,
        }
    }

    /// Where a `kind` branch starts: production for hotfixes, development otherwise.
    pub fn base(&self, kind: Kind) -> &str {
        match kind {
            Kind::Hotfix => &self.master,
            Kind::Feature | Kind::Release => &self.develop,
        }
    }

    /// The full branch name of the `kind` branch `name` (`feature/login`).
    pub fn branch(&self, kind: Kind, name: &str) -> String {
        format!("{}{name}", self.prefix(kind))
    }

    /// The tag finishing release or hotfix `name` creates.
    pub fn tag(&self, name: &str) -> String {
        format!("{}{name}", self.version_tag)
    }

    /// Which kind of branch `branch` is, and its name without the prefix.
    pub fn classify<'a>(&self, branch: &'a str) -> Option<(Kind, &'a str)> {
        Kind::ALL.into_iter().find_map(|k| {
            let prefix = self.prefix(k);
            branch
                .strip_prefix(prefix)
                .filter(|n| !prefix.is_empty() && !n.is_empty())
                .map(|n| (k, n))
        })
    }

    /// Read from `git config --get-regexp` output; `None` until both branches are set.
    fn parse(text: &str) -> Option<Self> {
        let mut cfg = Self::defaults([]);
        let (mut master, mut develop) = (None, None);
        for line in text.lines() {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            let value = value.to_owned();
            match key.to_ascii_lowercase().as_str() {
                "gitflow.branch.master" | "gitflow.branch.main" => master = Some(value),
                "gitflow.branch.develop" => develop = Some(value),
                "gitflow.prefix.feature" => cfg.feature = value,
                "gitflow.prefix.release" => cfg.release = value,
                "gitflow.prefix.hotfix" => cfg.hotfix = value,
                "gitflow.prefix.versiontag" => cfg.version_tag = value,
                _ => {}
            }
        }
        cfg.master = master.filter(|m| !m.is_empty())?;
        cfg.develop = develop.filter(|d| !d.is_empty())?;
        Some(cfg)
    }
}

/// How [`Repo::flow_finish`] ends.
#[derive(Clone, Debug, Default)]
pub struct FinishOptions {
    /// The tag's message for a release or hotfix (default "Release 1.2" / "Hotfix 1.2.1").
    pub tag_message: Option<String>,
    /// Keep the branch instead of deleting it.
    pub keep_branch: bool,
}

/// The repository's git-flow settings, `None` when it was never initialized.
pub fn read(repo: &Repo) -> Result<Option<Config>> {
    match repo.git()?.text(&["config", "--get-regexp", r"^gitflow\."]) {
        Ok(text) => Ok(Config::parse(&text)),
        // No key matched.
        Err(Error::Command { code: Some(1), .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

impl Repo {
    fn branch_exists(&self, name: &str) -> Result<bool> {
        Ok(self
            .git()?
            .run(&["show-ref", "--verify", "-q", &format!("refs/heads/{name}")])
            .is_ok())
    }

    fn set_config(&self, key: &str, value: &str) -> Result<()> {
        self.git()?.run(&["config", key, value])?;
        Ok(())
    }

    /// Set up git-flow: write `cfg` to the repository's config and create the development
    /// branch from production when it does not exist.
    pub fn flow_init(&self, cfg: &Config) -> Result<()> {
        if cfg.master == cfg.develop {
            return Err(Error::Unsupported(
                "The production and development branches must differ".into(),
            ));
        }
        if !self.branch_exists(&cfg.master)? {
            return Err(Error::Unsupported(format!(
                "The production branch {} does not exist",
                cfg.master
            )));
        }
        if !self.branch_exists(&cfg.develop)? {
            self.create_branch(&cfg.develop, &cfg.master, false)?;
        }
        for (key, value) in [
            ("gitflow.branch.master", &cfg.master),
            ("gitflow.branch.develop", &cfg.develop),
            ("gitflow.prefix.feature", &cfg.feature),
            ("gitflow.prefix.release", &cfg.release),
            ("gitflow.prefix.hotfix", &cfg.hotfix),
            ("gitflow.prefix.versiontag", &cfg.version_tag),
        ] {
            self.set_config(key, value)?;
        }
        Ok(())
    }

    /// Start the `kind` branch `name` from its base (or `base`) and check it out. Returns
    /// the branch's full name.
    pub fn flow_start(
        &self,
        cfg: &Config,
        kind: Kind,
        name: &str,
        base: Option<&str>,
    ) -> Result<String> {
        let branch = cfg.branch(kind, name);
        self.create_branch(&branch, base.unwrap_or(cfg.base(kind)), true)?;
        Ok(branch)
    }

    /// Finish the `kind` branch `name`: merge it (`--no-ff`) into development (a feature)
    /// or into production, tag it and merge it into development (a release or hotfix; a
    /// hotfix goes into a running release branch instead, as git-flow does), then delete
    /// it. Ends on the development branch. A merge conflict stops here with the merge in
    /// progress; finishing again after committing the resolution carries on.
    pub fn flow_finish(
        &self,
        cfg: &Config,
        kind: Kind,
        name: &str,
        opts: &FinishOptions,
    ) -> Result<()> {
        let branch = cfg.branch(kind, name);
        if !self.branch_exists(&branch)? {
            return Err(Error::Unsupported(format!("There is no branch {branch}")));
        }
        if self.state() != RepoState::Clean {
            return Err(Error::Unsupported(format!(
                "Finish the {} in progress first",
                self.state().label().to_lowercase()
            )));
        }
        let dirty = self
            .git()?
            .text(&["status", "--porcelain", "--untracked-files=no"])?;
        if !dirty.trim().is_empty() {
            return Err(Error::Unsupported(
                "Commit or stash your changes before finishing".into(),
            ));
        }
        if kind.tags() {
            self.merge_into(&cfg.master, &branch)?;
            let tag = cfg.tag(name);
            let exists = self
                .git()?
                .run(&["show-ref", "--verify", "-q", &format!("refs/tags/{tag}")])
                .is_ok();
            if !exists {
                let message = opts
                    .tag_message
                    .clone()
                    .filter(|m| !m.trim().is_empty())
                    .unwrap_or_else(|| format!("{} {name}", kind.label()));
                self.create_tag(&tag, &cfg.master, Some(&message))?;
            }
        }
        let into = match kind {
            Kind::Hotfix => self
                .release_branches(cfg)?
                .into_iter()
                .next()
                .unwrap_or_else(|| cfg.develop.clone()),
            Kind::Feature | Kind::Release => cfg.develop.clone(),
        };
        self.merge_into(&into, &branch)?;
        // `-d` checks the branch is merged into HEAD, so delete before leaving `into`.
        if !opts.keep_branch {
            self.delete_branch(&branch, false)?;
        }
        if into != cfg.develop {
            self.checkout(&cfg.develop)?;
        }
        Ok(())
    }

    /// Local release branches in progress.
    fn release_branches(&self, cfg: &Config) -> Result<Vec<String>> {
        if cfg.release.is_empty() {
            return Ok(Vec::new());
        }
        let text = self.git()?.text(&[
            "for-each-ref",
            "--format=%(refname:short)",
            &format!("refs/heads/{}", cfg.release),
        ])?;
        Ok(text.lines().map(str::to_owned).collect())
    }

    /// Check out `target` and merge `branch` into it with a merge commit (nothing when it
    /// is already merged).
    fn merge_into(&self, target: &str, branch: &str) -> Result<()> {
        self.checkout(target)?;
        self.merge(branch, MergeMode::NoFastForward)
            .map_err(|e| match self.state() {
                RepoState::Merge => Error::Unsupported(format!(
                    "Merging {branch} into {target} stopped on conflicts. Resolve them and \
                     commit, then finish {branch} again."
                )),
                _ => e,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    fn setup() -> (TestRepo, Repo, Config) {
        let t = TestRepo::new();
        t.commit_file("a.txt", "a\n", "one");
        let r = Repo::open(t.path()).unwrap();
        let cfg = Config::defaults(["main"]);
        r.flow_init(&cfg).unwrap();
        (t, r, cfg)
    }

    fn branches(t: &TestRepo) -> Vec<String> {
        t.git(&["for-each-ref", "--format=%(refname:short)", "refs/heads"])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn defaults_pick_main_or_master() {
        assert_eq!(Config::defaults(["master", "x"]).master, "master");
        assert_eq!(Config::defaults(["main", "master"]).master, "main");
        assert_eq!(Config::defaults([]).master, "main");
    }

    #[test]
    fn classifies_branches() {
        let cfg = Config::defaults(["main"]);
        assert_eq!(
            cfg.classify("feature/login"),
            Some((Kind::Feature, "login"))
        );
        assert_eq!(cfg.classify("hotfix/1.0.1"), Some((Kind::Hotfix, "1.0.1")));
        assert_eq!(cfg.classify("feature/"), None);
        assert_eq!(cfg.classify("develop"), None);
    }

    #[test]
    fn parses_config() {
        assert_eq!(Config::parse("gitflow.prefix.feature f/\n"), None);
        let cfg = Config::parse(
            "gitflow.branch.master trunk\ngitflow.branch.develop dev\n\
             gitflow.prefix.feature f/\ngitflow.prefix.versiontag v\n",
        )
        .unwrap();
        assert_eq!(
            (
                cfg.master.as_str(),
                cfg.develop.as_str(),
                cfg.feature.as_str()
            ),
            ("trunk", "dev", "f/")
        );
        assert_eq!(
            (cfg.release.as_str(), cfg.tag("1.0")),
            ("release/", "v1.0".into())
        );
        // An empty value (git prints the key alone) is kept empty.
        let cfg = Config::parse(
            "gitflow.branch.master main\ngitflow.branch.develop develop\ngitflow.prefix.versiontag\n",
        )
        .unwrap();
        assert_eq!(cfg.version_tag, "");
    }

    #[test]
    fn init_writes_config_and_creates_develop() {
        let t = TestRepo::new();
        let r = Repo::open(t.path()).unwrap();
        assert_eq!(read(&r).unwrap(), None);
        t.commit_file("a.txt", "a\n", "one");
        let mut cfg = Config::defaults(["main"]);
        cfg.version_tag = "v".into();
        r.flow_init(&cfg).unwrap();
        assert_eq!(read(&r).unwrap(), Some(cfg.clone()));
        assert!(branches(&t).contains(&"develop".to_owned()));
        cfg.master = "nope".into();
        assert!(r.flow_init(&cfg).is_err());
    }

    #[test]
    fn feature_start_and_finish() {
        let (t, r, cfg) = setup();
        assert_eq!(
            r.flow_start(&cfg, Kind::Feature, "login", None).unwrap(),
            "feature/login"
        );
        t.commit_file("login.txt", "l\n", "Add login");
        r.flow_finish(&cfg, Kind::Feature, "login", &FinishOptions::default())
            .unwrap();
        assert_eq!(r.head().unwrap().branch(), Some("develop"));
        assert!(!branches(&t).contains(&"feature/login".to_owned()));
        // A merge commit on develop; main is untouched.
        assert_eq!(
            t.git(&["log", "-1", "--format=%P", "develop"])
                .split(' ')
                .count(),
            2
        );
        assert!(
            !t.git(&["ls-tree", "main", "--name-only"])
                .contains("login.txt")
        );
    }

    #[test]
    fn release_merges_tags_and_returns_to_develop() {
        let (t, r, cfg) = setup();
        r.flow_start(&cfg, Kind::Release, "1.0", None).unwrap();
        t.commit_file("version.txt", "1.0\n", "Bump version");
        r.flow_finish(
            &cfg,
            Kind::Release,
            "1.0",
            &FinishOptions {
                tag_message: Some("First release".into()),
                keep_branch: true,
            },
        )
        .unwrap();
        assert_eq!(r.head().unwrap().branch(), Some("develop"));
        assert!(branches(&t).contains(&"release/1.0".to_owned()));
        assert_eq!(
            t.git(&["rev-parse", "1.0^{commit}"]),
            t.git(&["rev-parse", "main"])
        );
        assert_eq!(
            t.git(&["tag", "-l", "-n1", "1.0"])
                .split_whitespace()
                .nth(1),
            Some("First")
        );
        assert!(
            t.git(&["ls-tree", "develop", "--name-only"])
                .contains("version.txt")
        );
    }

    #[test]
    fn hotfix_goes_into_a_running_release() {
        let (t, r, cfg) = setup();
        r.flow_start(&cfg, Kind::Release, "2.0", None).unwrap();
        r.flow_start(&cfg, Kind::Hotfix, "1.0.1", None).unwrap();
        t.commit_file("fix.txt", "f\n", "Fix");
        r.flow_finish(&cfg, Kind::Hotfix, "1.0.1", &FinishOptions::default())
            .unwrap();
        assert!(
            t.git(&["ls-tree", "main", "--name-only"])
                .contains("fix.txt")
        );
        assert!(
            t.git(&["ls-tree", "release/2.0", "--name-only"])
                .contains("fix.txt")
        );
        assert!(
            !t.git(&["ls-tree", "develop", "--name-only"])
                .contains("fix.txt")
        );
        assert_eq!(
            t.git(&["tag", "-l", "-n1", "1.0.1"])
                .split_whitespace()
                .nth(1),
            Some("Hotfix")
        );
        assert_eq!(r.head().unwrap().branch(), Some("develop"));
    }

    #[test]
    fn finishing_again_after_a_conflict_carries_on() {
        let (t, r, cfg) = setup();
        r.flow_start(&cfg, Kind::Feature, "x", None).unwrap();
        t.commit_file("a.txt", "feature\n", "Feature edit");
        t.git(&["checkout", "-q", "develop"]);
        t.commit_file("a.txt", "develop\n", "Develop edit");
        let err = r
            .flow_finish(&cfg, Kind::Feature, "x", &FinishOptions::default())
            .unwrap_err();
        assert!(err.to_string().contains("stopped on conflicts"), "{err}");
        // Refused while the merge is open.
        assert!(
            r.flow_finish(&cfg, Kind::Feature, "x", &FinishOptions::default())
                .is_err()
        );
        t.write("a.txt", "both\n");
        t.git(&["commit", "-qam", "Resolve"]);
        r.flow_finish(&cfg, Kind::Feature, "x", &FinishOptions::default())
            .unwrap();
        assert!(!branches(&t).contains(&"feature/x".to_owned()));
    }

    #[test]
    fn finish_refuses_uncommitted_changes() {
        let (t, r, cfg) = setup();
        r.flow_start(&cfg, Kind::Feature, "y", None).unwrap();
        t.write("a.txt", "dirty\n");
        assert!(
            r.flow_finish(&cfg, Kind::Feature, "y", &FinishOptions::default())
                .is_err()
        );
    }
}
