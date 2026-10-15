//! The hosting services' REST APIs, as far as switchhead uses them: who the token belongs
//! to, the account's repositories, creating a pull request, and avatars. Calls block;
//! run them off the UI thread.

use std::time::{Duration, Instant};

use reqwest::blocking::{RequestBuilder, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::remote::encode;
use crate::{Account, Error, NewPullRequest, Provider, RemoteRepo, RepoRef, Result, Token};

/// Pages of repositories read at most (100 each).
const MAX_PAGES: usize = 20;

/// A client for every provider.
pub struct Client {
    http: reqwest::blocking::Client,
}

/// The API's address for a site.
pub fn api_base(provider: Provider, host: &str, organization: &str) -> String {
    let host = host.trim_end_matches('/');
    match provider {
        Provider::GitHub if host == "https://github.com" => "https://api.github.com".into(),
        Provider::GitHub => format!("{host}/api/v3"),
        Provider::GitLab => format!("{host}/api/v4"),
        Provider::Bitbucket if host == "https://bitbucket.org" => {
            "https://api.bitbucket.org/2.0".into()
        }
        Provider::Bitbucket => format!("{host}/2.0"),
        Provider::AzureDevOps => format!("{host}/{}", encode(organization)),
    }
}

/// A GitHub no-reply address's avatar (`12345+login@users.noreply.github.com`), no API
/// call needed.
pub fn noreply_avatar(email: &str) -> Option<String> {
    let local = email
        .to_ascii_lowercase()
        .strip_suffix("@users.noreply.github.com")?
        .to_owned();
    Some(match local.split_once('+') {
        Some((id, _)) if id.bytes().all(|b| b.is_ascii_digit()) => {
            format!("https://avatars.githubusercontent.com/u/{id}?s=64")
        }
        _ => format!("https://github.com/{local}.png?size=64"),
    })
}

/// git's `rel="next"` link from a `Link` header.
fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        params
            .split(';')
            .any(|p| p.trim() == "rel=\"next\"")
            .then(|| {
                url.trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_owned()
            })
    })
}

/// The message in an API error body, whatever the provider's shape.
fn error_message(body: &str) -> String {
    let short = || body.chars().take(200).collect::<String>();
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return short();
    };
    let text = |v: &Value| -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Array(a) => Some(
                a.iter()
                    .filter_map(|x| {
                        x.as_str()
                            .map(str::to_owned)
                            .or_else(|| x.get("message")?.as_str().map(str::to_owned))
                    })
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            Value::Object(o) => Some(
                o.iter()
                    .map(|(k, v)| format!("{k}: {}", v.to_string().trim_matches('"')))
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            _ => None,
        }
    };
    let mut parts = Vec::new();
    for key in ["message", "error_description"] {
        if let Some(t) = v.get(key).and_then(text) {
            parts.push(t);
        }
    }
    if let Some(e) = v.get("error") {
        parts.push(
            e.get("message")
                .and_then(text)
                .or_else(|| text(e))
                .unwrap_or_default(),
        );
    }
    if let Some(errors) = v.get("errors").and_then(text) {
        parts.push(errors);
    }
    parts.retain(|p| !p.is_empty());
    if parts.is_empty() {
        short()
    } else {
        parts.join(": ")
    }
}

impl Client {
    /// A client using the system's proxy settings and certificate store.
    pub fn new() -> Result<Self> {
        Self::build(reqwest::blocking::Client::builder())
    }

    fn build(builder: reqwest::blocking::ClientBuilder) -> Result<Self> {
        let http = builder
            .user_agent(concat!("switchhead/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| Error::Network(e.to_string()))?;
        Ok(Self { http })
    }

    fn auth(
        &self,
        req: RequestBuilder,
        provider: Provider,
        user: &str,
        token: &Token,
    ) -> RequestBuilder {
        match provider {
            Provider::GitHub => req
                .bearer_auth(token.expose())
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28"),
            Provider::GitLab => req.bearer_auth(token.expose()),
            Provider::Bitbucket => req.basic_auth(user, Some(token.expose())),
            Provider::AzureDevOps => req.basic_auth("", Some(token.expose())),
        }
    }

    /// Send, and turn error statuses into [`Error`]s.
    fn send(&self, req: RequestBuilder, what: &str) -> Result<Response> {
        let started = Instant::now();
        let resp = req
            .send()
            .map_err(|e| Error::Network(network_message(&e)))?;
        let status = resp.status().as_u16();
        tracing::debug!(
            what,
            status,
            ms = started.elapsed().as_millis() as u64,
            "hosting api"
        );
        if resp.status().is_success() {
            return Ok(resp);
        }
        if status == 401 {
            return Err(Error::Unauthorized(status));
        }
        let body = resp.text().unwrap_or_default();
        Err(Error::Api {
            status,
            message: error_message(&body),
        })
    }

    fn json<T: for<'de> Deserialize<'de>>(resp: Response) -> Result<T> {
        resp.json()
            .map_err(|e| Error::Network(format!("unexpected answer: {e}")))
    }

    /// Check a token and return the account it belongs to.
    pub fn sign_in(
        &self,
        provider: Provider,
        host: &str,
        organization: &str,
        auth_user: &str,
        token: &Token,
    ) -> Result<Account> {
        if token.is_empty() {
            return Err(Error::Invalid("Enter a token".into()));
        }
        if provider.needs_organization() && organization.trim().is_empty() {
            return Err(Error::Invalid("Enter the organization".into()));
        }
        if provider.needs_user() && auth_user.trim().is_empty() {
            return Err(Error::Invalid("Enter your user name or email".into()));
        }
        let host = host.trim().trim_end_matches('/').to_owned();
        let organization = organization.trim().to_owned();
        let auth_user = auth_user.trim().to_owned();
        let base = api_base(provider, &host, &organization);
        let mut account = Account {
            provider,
            host,
            organization,
            auth_user,
            login: String::new(),
            name: String::new(),
            avatar_url: None,
        };
        let get = |path: &str| {
            self.auth(
                self.http.get(format!("{base}{path}")),
                provider,
                &account.auth_user,
                token,
            )
        };
        match provider {
            Provider::GitHub | Provider::GitLab => {
                #[derive(Deserialize)]
                struct User {
                    #[serde(alias = "username")]
                    login: String,
                    #[serde(default)]
                    name: Option<String>,
                    #[serde(default)]
                    avatar_url: Option<String>,
                }
                let u: User = Self::json(self.send(get("/user"), "user")?)?;
                account.login = u.login;
                account.name = u.name.unwrap_or_default();
                account.avatar_url = u.avatar_url;
            }
            Provider::Bitbucket => {
                let v: Value = Self::json(self.send(get("/user"), "user")?)?;
                account.login = str_at(&v, &["username"])
                    .or_else(|| str_at(&v, &["nickname"]))
                    .unwrap_or_else(|| account.auth_user.clone());
                account.name = str_at(&v, &["display_name"]).unwrap_or_default();
                account.avatar_url = str_at(&v, &["links", "avatar", "href"]);
            }
            Provider::AzureDevOps => {
                let v: Value = Self::json(self.send(get("/_apis/connectionData"), "user")?)?;
                let user = v.get("authenticatedUser").cloned().unwrap_or(Value::Null);
                account.name = str_at(&user, &["providerDisplayName"]).unwrap_or_default();
                account.login = str_at(&user, &["properties", "Account", "$value"])
                    .unwrap_or_else(|| account.name.clone());
                if account.login.is_empty() {
                    return Err(Error::Unauthorized(203));
                }
            }
        }
        Ok(account)
    }

    /// The repositories `account` can see, most recently updated first.
    pub fn repositories(&self, account: &Account, token: &Token) -> Result<Vec<RemoteRepo>> {
        let base = api_base(account.provider, &account.host, &account.organization);
        let get = |url: &str| {
            self.auth(
                self.http.get(url),
                account.provider,
                &account.auth_user,
                token,
            )
        };
        let mut out = Vec::new();
        match account.provider {
            Provider::GitHub => {
                let mut url = Some(format!(
                    "{base}/user/repos?per_page=100&sort=updated&affiliation=owner,collaborator,organization_member"
                ));
                for _ in 0..MAX_PAGES {
                    let Some(u) = url.take() else { break };
                    let resp = self.send(get(&u), "repos")?;
                    url = resp
                        .headers()
                        .get("link")
                        .and_then(|h| h.to_str().ok())
                        .and_then(next_link);
                    let page: Vec<Value> = Self::json(resp)?;
                    out.extend(page.iter().map(|r| RemoteRepo {
                        full_name: str_at(r, &["full_name"]).unwrap_or_default(),
                        description: str_at(r, &["description"]).unwrap_or_default(),
                        private: r.get("private").and_then(Value::as_bool).unwrap_or(false),
                        https_url: str_at(r, &["clone_url"]).unwrap_or_default(),
                        ssh_url: str_at(r, &["ssh_url"]).unwrap_or_default(),
                    }));
                }
            }
            Provider::GitLab => {
                let mut page = Some("1".to_owned());
                for _ in 0..MAX_PAGES {
                    let Some(p) = page.take() else { break };
                    let resp = self.send(
                        get(&format!(
                            "{base}/projects?membership=true&simple=true&per_page=100&order_by=last_activity_at&page={p}"
                        )),
                        "projects",
                    )?;
                    page = resp
                        .headers()
                        .get("x-next-page")
                        .and_then(|h| h.to_str().ok())
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_owned);
                    let items: Vec<Value> = Self::json(resp)?;
                    out.extend(items.iter().map(|r| RemoteRepo {
                        full_name: str_at(r, &["path_with_namespace"]).unwrap_or_default(),
                        description: str_at(r, &["description"]).unwrap_or_default(),
                        private: str_at(r, &["visibility"]).is_none_or(|v| v != "public"),
                        https_url: str_at(r, &["http_url_to_repo"]).unwrap_or_default(),
                        ssh_url: str_at(r, &["ssh_url_to_repo"]).unwrap_or_default(),
                    }));
                }
            }
            Provider::Bitbucket => {
                let mut url = Some(format!(
                    "{base}/repositories?role=member&pagelen=100&sort=-updated_on"
                ));
                for _ in 0..MAX_PAGES {
                    let Some(u) = url.take() else { break };
                    let v: Value = Self::json(self.send(get(&u), "repositories")?)?;
                    url = str_at(&v, &["next"]);
                    let items = v
                        .get("values")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    out.extend(items.iter().map(|r| {
                        let clone = |name: &str| {
                            r.pointer("/links/clone")
                                .and_then(Value::as_array)
                                .and_then(|a| {
                                    a.iter().find(|c| {
                                        c.get("name").and_then(Value::as_str) == Some(name)
                                    })
                                })
                                .and_then(|c| str_at(c, &["href"]))
                                .unwrap_or_default()
                        };
                        RemoteRepo {
                            full_name: str_at(r, &["full_name"]).unwrap_or_default(),
                            description: str_at(r, &["description"]).unwrap_or_default(),
                            private: r
                                .get("is_private")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            // Bitbucket puts the user name in the HTTPS URL; git asks for the
                            // password itself, so leave it out.
                            https_url: strip_userinfo(&clone("https")),
                            ssh_url: clone("ssh"),
                        }
                    }));
                }
            }
            Provider::AzureDevOps => {
                let v: Value = Self::json(self.send(
                    get(&format!("{base}/_apis/git/repositories?api-version=7.1")),
                    "repositories",
                )?)?;
                let items = v
                    .get("value")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                out.extend(items.iter().map(|r| RemoteRepo {
                    full_name: format!(
                        "{}/{}",
                        str_at(r, &["project", "name"]).unwrap_or_default(),
                        str_at(r, &["name"]).unwrap_or_default()
                    ),
                    description: String::new(),
                    private: str_at(r, &["project", "visibility"]).is_none_or(|v| v != "public"),
                    https_url: strip_userinfo(&str_at(r, &["remoteUrl"]).unwrap_or_default()),
                    ssh_url: str_at(r, &["sshUrl"]).unwrap_or_default(),
                }));
            }
        }
        out.retain(|r| !r.full_name.is_empty());
        Ok(out)
    }

    /// Open a pull request on `repo`; returns its web page.
    pub fn create_pull_request(
        &self,
        account: &Account,
        token: &Token,
        repo: &RepoRef,
        pr: &NewPullRequest,
    ) -> Result<String> {
        if !account.serves(repo) {
            return Err(Error::Invalid(format!(
                "{} cannot open pull requests on {}",
                account.label(),
                repo.web_url()
            )));
        }
        let base = api_base(account.provider, &account.host, &account.organization);
        let post = |url: String, body: Value| {
            self.auth(
                self.http.post(url),
                account.provider,
                &account.auth_user,
                token,
            )
            .json(&body)
        };
        let created: Value = match account.provider {
            Provider::GitHub => Self::json(self.send(
                post(
                    format!("{base}/repos/{}/pulls", repo.path),
                    json!({
                        "title": pr.title, "body": pr.body,
                        "head": pr.head, "base": pr.base, "draft": pr.draft,
                    }),
                ),
                "create pull request",
            )?)?,
            Provider::GitLab => {
                let title = if pr.draft {
                    format!("Draft: {}", pr.title)
                } else {
                    pr.title.clone()
                };
                Self::json(self.send(
                    post(
                        format!("{base}/projects/{}/merge_requests", encode(&repo.path)),
                        json!({
                            "title": title, "description": pr.body,
                            "source_branch": pr.head, "target_branch": pr.base,
                        }),
                    ),
                    "create merge request",
                )?)?
            }
            Provider::Bitbucket => Self::json(self.send(
                post(
                    format!("{base}/repositories/{}/pullrequests", repo.path),
                    json!({
                        "title": pr.title, "description": pr.body, "draft": pr.draft,
                        "source": { "branch": { "name": pr.head } },
                        "destination": { "branch": { "name": pr.base } },
                    }),
                ),
                "create pull request",
            )?)?,
            Provider::AzureDevOps => Self::json(self.send(
                post(
                    format!(
                        "{base}/{}/_apis/git/repositories/{}/pullrequests?api-version=7.1",
                        encode(&repo.project),
                        encode(&repo.path)
                    ),
                    json!({
                        "title": pr.title, "description": pr.body, "isDraft": pr.draft,
                        "sourceRefName": format!("refs/heads/{}", pr.head),
                        "targetRefName": format!("refs/heads/{}", pr.base),
                    }),
                ),
                "create pull request",
            )?)?,
        };
        let url = match account.provider {
            Provider::GitHub => str_at(&created, &["html_url"]),
            Provider::GitLab => str_at(&created, &["web_url"]),
            Provider::Bitbucket => str_at(&created, &["links", "html", "href"]),
            Provider::AzureDevOps => created
                .get("pullRequestId")
                .and_then(Value::as_u64)
                .map(|id| format!("{}/pullrequest/{id}", repo.web_url())),
        };
        url.ok_or_else(|| {
            Error::Network("the pull request was created but its address is missing".into())
        })
    }

    /// The avatar URL of a commit's author: GitHub no-reply addresses directly, else from
    /// the account's service (GitHub: the commit's author; GitLab: by email).
    pub fn author_avatar(
        &self,
        email: &str,
        sha: &str,
        repo: Option<&RepoRef>,
        account: Option<(&Account, &Token)>,
    ) -> Result<Option<String>> {
        if let Some(url) = noreply_avatar(email) {
            return Ok(Some(url));
        }
        let Some((account, token)) = account else {
            return Ok(None);
        };
        let base = api_base(account.provider, &account.host, &account.organization);
        let get = |url: String| {
            self.auth(
                self.http.get(url),
                account.provider,
                &account.auth_user,
                token,
            )
        };
        Ok(match account.provider {
            Provider::GitHub => {
                let Some(repo) = repo.filter(|r| account.serves(r)) else {
                    return Ok(None);
                };
                let v: Value = Self::json(self.send(
                    get(format!("{base}/repos/{}/commits/{sha}", repo.path)),
                    "commit author",
                )?)?;
                str_at(&v, &["author", "avatar_url"])
            }
            Provider::GitLab => {
                let v: Value = Self::json(self.send(
                    get(format!("{base}/avatar?email={}&size=64", encode(email))),
                    "avatar",
                )?)?;
                str_at(&v, &["avatar_url"])
            }
            Provider::Bitbucket | Provider::AzureDevOps => None,
        })
    }

    /// The latest release announced by an update feed: a public JSON file such as
    /// `{"version": "0.2.0", "url": "https://…/download", "notes": "…"}`. No token is sent.
    /// `None` when there is no feed yet (404).
    pub fn latest_release(&self, feed: &str) -> Result<Option<Release>> {
        let v: Value = match self.send(self.http.get(feed), "update feed") {
            Ok(resp) => Self::json(resp)?,
            Err(Error::Api { status: 404, .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        let version = str_at(&v, &["version"])
            .ok_or_else(|| Error::Network("the update feed has no version".into()))?;
        Ok(Some(Release {
            version: version.trim_start_matches('v').to_owned(),
            url: str_at(&v, &["url"]).unwrap_or_default(),
            notes: str_at(&v, &["notes"]).unwrap_or_default(),
        }))
    }

    /// Download an image (an avatar). Refuses anything over 2 MB.
    pub fn download(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self.send(self.http.get(url), "download")?;
        if resp.content_length().is_some_and(|n| n > 2 << 20) {
            return Err(Error::Invalid("image too large".into()));
        }
        let bytes = resp.bytes().map_err(|e| Error::Network(e.to_string()))?;
        if bytes.len() > 2 << 20 {
            return Err(Error::Invalid("image too large".into()));
        }
        Ok(bytes.to_vec())
    }
}

/// A published release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// Its version, without a leading `v` (`1.2.0`).
    pub version: String,
    /// Where to download it.
    pub url: String,
    /// A line about what changed (may be empty).
    pub notes: String,
}

/// Whether version `candidate` is newer than `current` (`major.minor.patch`, compared as
/// numbers; a pre-release suffix such as `-beta.1` sorts before the release itself).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<([u64; 3], bool)> {
        let v = v.trim().trim_start_matches('v');
        let (core, pre) = match v.split_once(['-', '+']) {
            Some((core, rest)) => (core, !rest.is_empty() && v.as_bytes()[core.len()] == b'-'),
            None => (v, false),
        };
        let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
        let n = [
            parts.next()??,
            parts.next().unwrap_or(Some(0))?,
            parts.next().unwrap_or(Some(0))?,
        ];
        parts.next().is_none().then_some((n, pre))
    }
    match (parse(candidate), parse(current)) {
        (Some((a, a_pre)), Some((b, b_pre))) => a > b || (a == b && b_pre && !a_pre),
        _ => false,
    }
}

/// A string at a JSON path.
fn str_at(v: &Value, path: &[&str]) -> Option<String> {
    let mut cur = v;
    for key in path {
        cur = cur.get(key)?;
    }
    cur.as_str().map(str::to_owned)
}

/// `https://user@host/...` → `https://host/...`.
fn strip_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = authority.rsplit('@').next().unwrap_or(authority);
    format!("{scheme}://{host}/{path}")
}

/// A network error's message (never the token: tokens travel in headers).
fn network_message(e: &reqwest::Error) -> String {
    let what = if e.is_timeout() {
        "the server did not answer in time"
    } else if e.is_connect() {
        "could not connect to the server"
    } else {
        "the request failed"
    };
    let mut msg = what.to_owned();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        msg = format!("{msg}: {s}");
        source = s.source();
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A request the test server saw.
    #[derive(Clone, Debug)]
    struct Seen {
        method: String,
        path: String,
        auth: String,
        body: String,
    }

    type Route = Box<dyn Fn(&Seen) -> (u16, Vec<(String, String)>, String) + Send>;

    /// A tiny HTTP/1.1 server answering every request with `route`.
    fn serve(route: Route) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut parts = line.split_whitespace();
                let (method, path) = (
                    parts.next().unwrap_or("").to_owned(),
                    parts.next().unwrap_or("").to_owned(),
                );
                let (mut len, mut auth) = (0usize, String::new());
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    let (k, v) = h.split_once(':').unwrap();
                    match k.to_ascii_lowercase().as_str() {
                        "content-length" => len = v.trim().parse().unwrap(),
                        "authorization" => auth = v.trim().to_owned(),
                        _ => {}
                    }
                }
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                let req = Seen {
                    method,
                    path,
                    auth,
                    body: String::from_utf8(body).unwrap(),
                };
                let (status, headers, body) = route(&req);
                log.lock().unwrap().push(req);
                let mut out = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
                    body.len()
                );
                for (k, v) in headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("\r\n");
                out.push_str(&body);
                stream.write_all(out.as_bytes()).unwrap();
            }
        });
        (addr, seen)
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("v1.10.0", "1.9.3"));
        assert!(is_newer("1.0.0", "1.0.0-beta.2"));
        assert!(!is_newer("1.0.0-beta.2", "1.0.0"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("0.9", "1.0.0"));
        assert!(!is_newer("nightly", "1.0.0"));
    }

    #[test]
    fn reads_the_update_feed() {
        let (host, seen) = serve(Box::new(|r: &Seen| match r.path.as_str() {
            "/latest.json" => (
                200,
                vec![],
                r#"{"version":"v0.3.0","url":"https://h/download","notes":"Bisect"}"#.into(),
            ),
            _ => (404, vec![], "Not Found".into()),
        }));
        let c = client();
        let r = c
            .latest_release(&format!("{host}/latest.json"))
            .unwrap()
            .unwrap();
        assert_eq!(
            (r.version.as_str(), r.url.as_str(), r.notes.as_str()),
            ("0.3.0", "https://h/download", "Bisect")
        );
        assert_eq!(seen.lock().unwrap()[0].auth, "", "no token is sent");
        // No feed published yet.
        assert_eq!(
            c.latest_release(&format!("{host}/none.json")).unwrap(),
            None
        );
    }

    fn client() -> Client {
        Client::build(reqwest::blocking::Client::builder().no_proxy()).unwrap()
    }

    #[test]
    fn github_sign_in_list_and_pull_request() {
        let (host, seen) = serve(Box::new(|r: &Seen| {
            let base = r.path.trim_start_matches("/api/v3");
            match (r.method.as_str(), base) {
                ("GET", "/user") => (200, vec![], r#"{"login":"jl","name":"Jeremie","avatar_url":"http://a/1.png"}"#.into()),
                ("GET", p) if p.starts_with("/user/repos") && !p.contains("page=2") => (
                    200,
                    vec![("link".into(), "<http://x/api/v3/user/repos?page=2>; rel=\"next\", <http://x/last>; rel=\"last\"".into())],
                    r#"[{"full_name":"jl/app","description":null,"private":true,"clone_url":"https://h/jl/app.git","ssh_url":"git@h:jl/app.git"}]"#.into(),
                ),
                ("GET", _) => (200, vec![], r#"[{"full_name":"org/lib","description":"A lib","private":false,"clone_url":"c","ssh_url":"s"}]"#.into()),
                ("POST", "/repos/jl/app/pulls") => (201, vec![], r#"{"html_url":"https://h/jl/app/pull/7"}"#.into()),
                _ => (404, vec![], r#"{"message":"Not Found"}"#.into()),
            }
        }));
        let c = client();
        let token = Token::new("t0k");
        let account = c.sign_in(Provider::GitHub, &host, "", "", &token).unwrap();
        assert_eq!(
            (account.login.as_str(), account.name.as_str()),
            ("jl", "Jeremie")
        );
        assert_eq!(seen.lock().unwrap()[0].auth, "Bearer t0k");

        // The next page's link points at an unreachable host: the error does not carry
        // the token.
        let err = c.repositories(&account, &token).unwrap_err().to_string();
        assert!(!err.contains("t0k"), "{err}");

        let repo = crate::remote::parse(
            &format!("{host}/jl/app.git"),
            &[(Provider::GitHub, host.clone())],
        )
        .unwrap();
        let url = c
            .create_pull_request(
                &account,
                &token,
                &repo,
                &NewPullRequest {
                    head: "feature".into(),
                    base: "main".into(),
                    title: "Add it".into(),
                    body: "Why".into(),
                    draft: true,
                },
            )
            .unwrap();
        assert_eq!(url, "https://h/jl/app/pull/7");
        let post = seen.lock().unwrap().last().cloned().unwrap();
        let body: Value = serde_json::from_str(&post.body).unwrap();
        assert_eq!(body["head"], "feature");
        assert_eq!(body["draft"], true);
    }

    #[test]
    fn github_follows_pages() {
        let addr: Arc<Mutex<String>> = Arc::default();
        let a = addr.clone();
        let (host, _) = serve(Box::new(move |r: &Seen| {
            if r.path.contains("page=2") {
                (
                    200,
                    vec![],
                    r#"[{"full_name":"org/lib","clone_url":"c","ssh_url":"s"}]"#.into(),
                )
            } else {
                let next = format!(
                    "<{}/api/v3/user/repos?page=2>; rel=\"next\"",
                    a.lock().unwrap()
                );
                (
                    200,
                    vec![("link".into(), next)],
                    r#"[{"full_name":"jl/app","private":true,"clone_url":"c","ssh_url":"s"}]"#
                        .into(),
                )
            }
        }));
        *addr.lock().unwrap() = host.clone();
        let account = Account {
            provider: Provider::GitHub,
            host,
            organization: String::new(),
            auth_user: String::new(),
            login: "jl".into(),
            name: String::new(),
            avatar_url: None,
        };
        let repos = client().repositories(&account, &Token::new("t")).unwrap();
        let names: Vec<_> = repos.iter().map(|r| r.full_name.as_str()).collect();
        assert_eq!(names, ["jl/app", "org/lib"]);
        assert!(repos[0].private);
    }

    #[test]
    fn gitlab_merge_request_and_errors() {
        let (host, seen) = serve(Box::new(|r: &Seen| {
            match (r.method.as_str(), r.path.as_str()) {
            ("GET", "/api/v4/user") => (200, vec![], r#"{"username":"jl","name":"J"}"#.into()),
            ("GET", p) if p.starts_with("/api/v4/projects") && p.ends_with("page=1") => (
                200,
                vec![("x-next-page".into(), "2".into())],
                r#"[{"path_with_namespace":"g/a","visibility":"private","http_url_to_repo":"h","ssh_url_to_repo":"s"}]"#.into(),
            ),
            ("GET", p) if p.starts_with("/api/v4/projects") => (
                200,
                vec![("x-next-page".into(), "".into())],
                r#"[{"path_with_namespace":"g/b","visibility":"public","http_url_to_repo":"h","ssh_url_to_repo":"s"}]"#.into(),
            ),
            ("POST", "/api/v4/projects/g%2Fsub%2Fa/merge_requests") => (
                409,
                vec![],
                r#"{"message":["Another open merge request already exists for this source branch: !3"]}"#.into(),
            ),
            _ => (404, vec![], r#"{"message":"404 Not Found"}"#.into()),
        }
        }));
        let c = client();
        let token = Token::new("glpat");
        let account = c.sign_in(Provider::GitLab, &host, "", "", &token).unwrap();
        assert_eq!(account.login, "jl");
        let repos = c.repositories(&account, &token).unwrap();
        assert_eq!(repos.len(), 2);
        assert!(repos[0].private && !repos[1].private);

        let repo = crate::remote::parse(
            &format!("{host}/g/sub/a.git"),
            &[(Provider::GitLab, host.clone())],
        )
        .unwrap();
        let err = c
            .create_pull_request(
                &account,
                &token,
                &repo,
                &NewPullRequest {
                    head: "f".into(),
                    base: "main".into(),
                    title: "T".into(),
                    draft: true,
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("Another open merge request"),
            "{err}"
        );
        let body: Value = serde_json::from_str(&seen.lock().unwrap().last().unwrap().body).unwrap();
        assert_eq!(body["title"], "Draft: T");
    }

    #[test]
    fn refused_tokens() {
        let (host, _) = serve(Box::new(|_| {
            (401, vec![], r#"{"message":"Bad credentials"}"#.into())
        }));
        let err = client()
            .sign_in(Provider::GitHub, &host, "", "", &Token::new("x"))
            .unwrap_err();
        assert!(matches!(err, Error::Unauthorized(401)));
        let err = client()
            .sign_in(Provider::Bitbucket, &host, "", "", &Token::new("x"))
            .unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
    }

    #[test]
    fn bitbucket_and_azure() {
        let (host, seen) = serve(Box::new(|r: &Seen| {
            match r.path.as_str() {
            "/2.0/user" => (200, vec![], r#"{"username":"jl","display_name":"J L","links":{"avatar":{"href":"http://a"}}}"#.into()),
            p if p.starts_with("/2.0/repositories?") => (200, vec![], r#"{"values":[{"full_name":"t/a","is_private":true,"links":{"clone":[{"name":"https","href":"https://jl@bitbucket.org/t/a.git"},{"name":"ssh","href":"git@bitbucket.org:t/a.git"}]}}]}"#.into()),
            "/contoso/_apis/connectionData" => (200, vec![], r#"{"authenticatedUser":{"providerDisplayName":"Jeremie","properties":{"Account":{"$value":"j@example.com"}}}}"#.into()),
            p if p.starts_with("/contoso/_apis/git/repositories") => (200, vec![], r#"{"value":[{"name":"shop","project":{"name":"Web"},"remoteUrl":"https://contoso@dev.azure.com/contoso/Web/_git/shop","sshUrl":"git@ssh.dev.azure.com:v3/contoso/Web/shop"}]}"#.into()),
            "/contoso/Web%20Shop/_apis/git/repositories/shop/pullrequests?api-version=7.1" => (201, vec![], r#"{"pullRequestId":12}"#.into()),
            _ => (404, vec![], "{}".into()),
        }
        }));
        let c = client();
        let token = Token::new("pw");
        let bb = c
            .sign_in(Provider::Bitbucket, &host, "", "me@example.com", &token)
            .unwrap();
        assert_eq!(
            (bb.login.as_str(), bb.auth_user.as_str()),
            ("jl", "me@example.com")
        );
        assert!(seen.lock().unwrap()[0].auth.starts_with("Basic "));
        let repos = c.repositories(&bb, &token).unwrap();
        assert_eq!(repos[0].https_url, "https://bitbucket.org/t/a.git");

        let az = c
            .sign_in(Provider::AzureDevOps, &host, "contoso", "", &token)
            .unwrap();
        assert_eq!(az.login, "j@example.com");
        let repos = c.repositories(&az, &token).unwrap();
        assert_eq!(repos[0].full_name, "Web/shop");
        assert_eq!(
            repos[0].https_url,
            "https://dev.azure.com/contoso/Web/_git/shop"
        );
        let repo = RepoRef {
            provider: Provider::AzureDevOps,
            host: host.clone(),
            organization: "contoso".into(),
            project: "Web Shop".into(),
            path: "shop".into(),
        };
        let url = c
            .create_pull_request(
                &az,
                &token,
                &repo,
                &NewPullRequest {
                    head: "f".into(),
                    base: "main".into(),
                    title: "T".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(url.ends_with("/_git/shop/pullrequest/12"), "{url}");
        let body: Value = serde_json::from_str(&seen.lock().unwrap().last().unwrap().body).unwrap();
        assert_eq!(body["sourceRefName"], "refs/heads/f");
    }

    #[test]
    fn helpers() {
        assert_eq!(
            noreply_avatar("123+jl@users.noreply.github.com").as_deref(),
            Some("https://avatars.githubusercontent.com/u/123?s=64")
        );
        assert_eq!(
            noreply_avatar("jl@users.noreply.github.com").as_deref(),
            Some("https://github.com/jl.png?size=64")
        );
        assert_eq!(noreply_avatar("jl@example.com"), None);
        assert_eq!(
            next_link("<https://a/p2>; rel=\"next\", <https://a/p9>; rel=\"last\"").as_deref(),
            Some("https://a/p2")
        );
        assert_eq!(next_link("<https://a/p1>; rel=\"prev\""), None);
        assert_eq!(
            error_message(
                r#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists"}]}"#
            ),
            "Validation Failed: A pull request already exists"
        );
        assert_eq!(
            error_message(r#"{"type":"error","error":{"message":"Bad branch"}}"#),
            "Bad branch"
        );
        assert_eq!(error_message("<html>oops"), "<html>oops");
        assert_eq!(
            api_base(Provider::GitHub, "https://github.com/", ""),
            "https://api.github.com"
        );
        assert_eq!(
            api_base(Provider::GitHub, "https://ghe.corp", ""),
            "https://ghe.corp/api/v3"
        );
        assert_eq!(strip_userinfo("https://u@h/x.git"), "https://h/x.git");
    }
}
