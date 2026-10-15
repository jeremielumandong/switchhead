//! Which hosted repository a remote URL names, in any of git's URL forms, and the web
//! pages for it.

use crate::Provider;

/// A repository on a hosting service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoRef {
    /// The service.
    pub provider: Provider,
    /// The site, `https://github.com`.
    pub host: String,
    /// Azure DevOps organization (empty elsewhere).
    pub organization: String,
    /// Azure DevOps project (empty elsewhere).
    pub project: String,
    /// `owner/repo` (GitLab: `group/sub/repo`; Azure DevOps: the repository's name).
    pub path: String,
}

impl RepoRef {
    /// The repository's web page.
    pub fn web_url(&self) -> String {
        match self.provider {
            Provider::AzureDevOps => format!(
                "{}/{}/{}/_git/{}",
                self.host,
                encode(&self.organization),
                encode(&self.project),
                encode(&self.path)
            ),
            _ => format!("{}/{}", self.host, self.path),
        }
    }

    /// The site's page for opening a pull request from `head` into `base` (used when no
    /// account is signed in for it).
    pub fn new_pull_request_page(&self, head: &str, base: &str) -> String {
        let web = self.web_url();
        let (h, b) = (encode(head), encode(base));
        match self.provider {
            Provider::GitHub => format!("{web}/compare/{b}...{h}?expand=1"),
            Provider::GitLab => format!(
                "{web}/-/merge_requests/new?merge_request%5Bsource_branch%5D={h}&merge_request%5Btarget_branch%5D={b}"
            ),
            Provider::Bitbucket => format!("{web}/pull-requests/new?source={h}&dest={b}"),
            Provider::AzureDevOps => {
                format!("{web}/pullrequestcreate?sourceRef={h}&targetRef={b}")
            }
        }
    }
}

/// The host name in `https://host[:port]/...` (`None` without a scheme).
pub fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split('/').next()?;
    authority.rsplit('@').next()
}

/// Whether two site addresses are the same site (scheme, case and a trailing `/` aside).
pub fn same_host(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        host_of(s)
            .unwrap_or(s)
            .trim_end_matches('/')
            .to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

/// The hosted repository `url` names. `known` lists sites of signed-in accounts, so a
/// self-hosted GitHub or GitLab whose name says neither is still recognised.
pub fn parse(url: &str, known: &[(Provider, String)]) -> Option<RepoRef> {
    let url = url.trim();
    let (host, port, path) = split_url(url)?;
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let h = host.to_ascii_lowercase();

    // Azure DevOps: dev.azure.com/org/project/_git/repo, ssh.dev.azure.com:v3/org/project/repo,
    // org.visualstudio.com/[DefaultCollection/]project/_git/repo.
    if h == "dev.azure.com" || h == "ssh.dev.azure.com" || h.ends_with(".visualstudio.com") {
        let parts: Vec<String> = path.split('/').map(decode).collect();
        let (org, project, repo) = if h == "ssh.dev.azure.com" {
            match parts.as_slice() {
                [v3, org, project, repo] if v3 == "v3" => (org.clone(), project, repo),
                _ => return None,
            }
        } else if h == "dev.azure.com" {
            match parts.as_slice() {
                [org, project, git, repo] if git == "_git" => (org.clone(), project, repo),
                _ => return None,
            }
        } else {
            let org = h.strip_suffix(".visualstudio.com")?.to_owned();
            let parts: Vec<&String> = parts
                .iter()
                .filter(|p| !p.eq_ignore_ascii_case("DefaultCollection"))
                .collect();
            match parts.as_slice() {
                [project, git, repo] if *git == "_git" => (org, *project, *repo),
                _ => return None,
            }
        };
        return Some(RepoRef {
            provider: Provider::AzureDevOps,
            host: "https://dev.azure.com".into(),
            organization: org,
            project: project.clone(),
            path: repo.clone(),
        });
    }

    if path.is_empty() || !path.contains('/') {
        return None;
    }
    let known_provider = known
        .iter()
        .find(|(_, site)| {
            host_of(site).is_some_and(|k| {
                // A web port is part of the site's name; an ssh port never is.
                let k = k.to_ascii_lowercase();
                k == h || port.is_some_and(|p| k == format!("{h}:{p}"))
            })
        })
        .map(|(p, site)| (*p, site.trim_end_matches('/').to_owned()));
    let (provider, site) = if let Some(k) = known_provider {
        k
    } else if h == "github.com" || h == "ssh.github.com" {
        (Provider::GitHub, "https://github.com".to_owned())
    } else if h == "bitbucket.org" || h.ends_with(".bitbucket.org") {
        (Provider::Bitbucket, "https://bitbucket.org".to_owned())
    } else if h.contains("github") {
        (Provider::GitHub, format!("https://{host}"))
    } else if h.contains("gitlab") {
        // A web port (`https://gitlab.local:8443/...`) belongs to the site; an ssh one does not.
        let site = match port.filter(|_| url.starts_with("http")) {
            Some(p) => format!("https://{host}:{p}"),
            None => format!("https://{host}"),
        };
        (Provider::GitLab, site)
    } else {
        return None;
    };
    Some(RepoRef {
        provider,
        host: site,
        organization: String::new(),
        project: String::new(),
        path: path.to_owned(),
    })
}

/// `(host, port, path)` of a URL in git's forms: `scheme://[user@]host[:port]/path` or
/// scp-like `[user@]host:path`.
fn split_url(url: &str) -> Option<(&str, Option<&str>, &str)> {
    if let Some((_, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        let hostport = authority.rsplit('@').next()?;
        let (host, port) = match hostport.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        };
        Some((host, port, path))
    } else {
        let (left, path) = url.split_once(':')?;
        if left.contains('/') || left.len() < 2 {
            return None; // a local path (or a Windows drive)
        }
        Some((left.rsplit('@').next()?, None, path))
    }
}

/// Percent-encode a URL path segment or query value.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Undo percent-encoding (invalid escapes stay as they are).
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(url: &str) -> Option<(Provider, String, String)> {
        parse(url, &[]).map(|r| (r.provider, r.host, r.path))
    }

    #[test]
    fn parses_forges() {
        let gh = Some((
            Provider::GitHub,
            "https://github.com".to_owned(),
            "owner/repo".to_owned(),
        ));
        assert_eq!(p("https://github.com/owner/repo.git"), gh);
        assert_eq!(p("git@github.com:owner/repo.git"), gh);
        assert_eq!(p("ssh://git@ssh.github.com:443/owner/repo"), gh);
        assert_eq!(p("https://user:tok@github.com/owner/repo/"), gh);
        assert_eq!(
            p("git@gitlab.com:group/sub/repo.git"),
            Some((
                Provider::GitLab,
                "https://gitlab.com".into(),
                "group/sub/repo".into()
            ))
        );
        assert_eq!(
            p("https://gitlab.example.com:8443/team/app.git").map(|r| r.1),
            Some("https://gitlab.example.com:8443".into())
        );
        assert_eq!(
            p("git@bitbucket.org:team/app.git").map(|r| r.0),
            Some(Provider::Bitbucket)
        );
        assert_eq!(p("/srv/git/repo.git"), None);
        assert_eq!(p("C:\\src\\repo"), None);
        assert_eq!(p("https://example.com/owner/repo.git"), None);
    }

    #[test]
    fn known_sites_win() {
        let known = [(Provider::GitHub, "https://code.corp.example/".to_owned())];
        let r = parse("git@code.corp.example:team/app.git", &known).unwrap();
        assert_eq!(r.provider, Provider::GitHub);
        assert_eq!(r.host, "https://code.corp.example");
    }

    #[test]
    fn parses_azure_devops() {
        let want = RepoRef {
            provider: Provider::AzureDevOps,
            host: "https://dev.azure.com".into(),
            organization: "contoso".into(),
            project: "Web Shop".into(),
            path: "shop".into(),
        };
        for url in [
            "https://contoso@dev.azure.com/contoso/Web%20Shop/_git/shop",
            "git@ssh.dev.azure.com:v3/contoso/Web%20Shop/shop",
            "https://contoso.visualstudio.com/DefaultCollection/Web%20Shop/_git/shop",
        ] {
            assert_eq!(parse(url, &[]).as_ref(), Some(&want), "{url}");
        }
        assert_eq!(
            want.web_url(),
            "https://dev.azure.com/contoso/Web%20Shop/_git/shop"
        );
    }

    #[test]
    fn new_pull_request_pages() {
        let r = parse("git@github.com:o/r.git", &[]).unwrap();
        assert_eq!(
            r.new_pull_request_page("feature/x", "main"),
            "https://github.com/o/r/compare/main...feature%2Fx?expand=1"
        );
        let r = parse("git@bitbucket.org:o/r.git", &[]).unwrap();
        assert_eq!(
            r.new_pull_request_page("f", "main"),
            "https://bitbucket.org/o/r/pull-requests/new?source=f&dest=main"
        );
    }

    #[test]
    fn compares_hosts() {
        assert!(same_host("https://GitHub.com/", "https://github.com"));
        assert!(!same_host("https://github.com", "https://gitlab.com"));
        assert_eq!(host_of("https://a@b.example:1/x"), Some("b.example:1"));
    }
}
