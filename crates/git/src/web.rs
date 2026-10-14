//! Web pages of a hosted repository (GitHub, GitLab, Bitbucket) from a remote URL.

/// A forge the app knows how to link to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forge {
    /// github.com or GitHub Enterprise.
    GitHub,
    /// gitlab.com or a self-hosted GitLab.
    GitLab,
    /// bitbucket.org.
    Bitbucket,
}

impl Forge {
    /// Its name for menus.
    pub fn name(self) -> &'static str {
        match self {
            Forge::GitHub => "GitHub",
            Forge::GitLab => "GitLab",
            Forge::Bitbucket => "Bitbucket",
        }
    }
}

/// The repository's web address (`https://github.com/owner/repo`) and forge, from a
/// remote URL in any of git's forms. Credentials in the URL are dropped. Hosts that are
/// not recognisably GitHub, GitLab or Bitbucket give `None`.
pub fn repo_page(remote_url: &str) -> Option<(Forge, String)> {
    let url = remote_url.trim();
    // scheme://[user[:pass]@]host[:port]/path, or scp-like [user@]host:path.
    let (host, path) = if let Some((_, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?;
        let host = host.split(':').next()?;
        (host, path)
    } else {
        let (left, path) = url.split_once(':')?;
        if left.contains('/') {
            return None; // a local path
        }
        (left.rsplit('@').next()?, path)
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || path.is_empty() {
        return None;
    }
    let h = host.to_ascii_lowercase();
    let forge = if h.contains("github") {
        Forge::GitHub
    } else if h.contains("gitlab") {
        Forge::GitLab
    } else if h.contains("bitbucket") {
        Forge::Bitbucket
    } else {
        return None;
    };
    // GitHub's ssh host aliases (`ssh.github.com`) serve the same site.
    let host = if forge == Forge::GitHub && h == "ssh.github.com" {
        "github.com"
    } else {
        host
    };
    Some((forge, format!("https://{host}/{path}")))
}

/// A commit's page.
pub fn commit_page(remote_url: &str, id: &str) -> Option<(Forge, String)> {
    let (forge, base) = repo_page(remote_url)?;
    let url = match forge {
        Forge::GitHub => format!("{base}/commit/{id}"),
        Forge::GitLab => format!("{base}/-/commit/{id}"),
        Forge::Bitbucket => format!("{base}/commits/{id}"),
    };
    Some((forge, url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_url_form() {
        let gh = Some((Forge::GitHub, "https://github.com/o/r".to_owned()));
        for url in [
            "git@github.com:o/r.git",
            "https://github.com/o/r.git",
            "https://user:token@github.com/o/r",
            "ssh://git@github.com/o/r.git",
            "ssh://git@ssh.github.com:443/o/r.git",
            "github.com:o/r",
        ] {
            assert_eq!(repo_page(url), gh, "{url}");
        }
        assert_eq!(
            repo_page("git@gitlab.example.com:group/sub/r.git"),
            Some((
                Forge::GitLab,
                "https://gitlab.example.com/group/sub/r".into()
            ))
        );
        assert_eq!(repo_page("/srv/git/r.git"), None);
        assert_eq!(repo_page("../up"), None);
        assert_eq!(repo_page("https://example.com/o/r"), None);
    }

    #[test]
    fn links_commits() {
        assert_eq!(
            commit_page("git@github.com:o/r.git", "abc").map(|c| c.1),
            Some("https://github.com/o/r/commit/abc".into())
        );
        assert_eq!(
            commit_page("https://gitlab.com/o/r.git", "abc").map(|c| c.1),
            Some("https://gitlab.com/o/r/-/commit/abc".into())
        );
        assert_eq!(
            commit_page("git@bitbucket.org:o/r.git", "abc").map(|c| c.1),
            Some("https://bitbucket.org/o/r/commits/abc".into())
        );
    }
}
