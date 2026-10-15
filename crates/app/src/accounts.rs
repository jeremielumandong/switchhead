//! Hosting accounts in the app: the signed-in accounts (a GPUI global saved with the app
//! state; their tokens are in the keychain), the shared API client, and avatars, which
//! are downloaded once into the data folder and shown from there.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use gpui_kit::{App, BorrowAppContext as _};
use serde::{Deserialize, Serialize};
use switchhead_hosting::api::Client;
use switchhead_hosting::{Account, Provider, RepoRef, Token, remote, secrets};

/// The signed-in accounts, in the order they were added.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Accounts(pub Vec<Account>);

impl gpui_kit::Global for Accounts {}

impl Accounts {
    /// Sites of the accounts, so self-hosted servers are recognised in remote URLs.
    pub fn known_sites(&self) -> Vec<(Provider, String)> {
        self.0
            .iter()
            .map(|a| (a.provider, a.host.clone()))
            .collect()
    }

    /// The hosted repository a remote URL names.
    pub fn parse_remote(&self, url: &str) -> Option<RepoRef> {
        remote::parse(url, &self.known_sites())
    }

    /// The first account that can act on `repo`.
    pub fn for_repo(&self, repo: &RepoRef) -> Option<&Account> {
        self.0.iter().find(|a| a.serves(repo))
    }

    /// Add `account`, replacing one with the same keychain item.
    pub fn upsert(&mut self, account: Account) {
        match self.0.iter_mut().find(|a| a.key() == account.key()) {
            Some(a) => *a = account,
            None => self.0.push(account),
        }
    }
}

/// The accounts (empty before startup sets the global).
pub fn accounts(cx: &App) -> Accounts {
    cx.try_global::<Accounts>().cloned().unwrap_or_default()
}

/// The shared API client (`None` when TLS could not be set up; the error is logged).
pub fn client() -> Option<Arc<Client>> {
    static CLIENT: OnceLock<Option<Arc<Client>>> = OnceLock::new();
    CLIENT
        .get_or_init(|| match Client::new() {
            Ok(c) => Some(Arc::new(c)),
            Err(e) => {
                tracing::warn!(error = %e, "hosting client unavailable");
                None
            }
        })
        .clone()
}

/// An account's token from the keychain.
pub fn token(account: &Account) -> switchhead_hosting::Result<Token> {
    secrets::load(&account.key())?.ok_or_else(|| {
        switchhead_hosting::Error::Invalid(format!(
            "No token for {}: sign in again in Settings → Accounts",
            account.label()
        ))
    })
}

/// Where avatars are, and which ones are loading or failed.
pub struct Avatars {
    dir: PathBuf,
    images: HashMap<String, Option<PathBuf>>,
    /// Commit authors' avatar URLs by email (`None`: none found).
    authors: HashMap<String, Option<String>>,
}

impl gpui_kit::Global for Avatars {}

impl Avatars {
    /// Avatars cached in `dir`.
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            images: HashMap::new(),
            authors: HashMap::new(),
        }
    }
}

fn file_for(dir: &std::path::Path, url: &str) -> PathBuf {
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    dir.join(format!("{:016x}.img", h.finish()))
}

/// The avatar at `url` as a local file, or `None` while it downloads (the windows are
/// refreshed when it arrives) or when it could not be had.
pub fn avatar(url: &str, cx: &mut App) -> Option<PathBuf> {
    let avatars = cx.try_global::<Avatars>()?;
    if let Some(state) = avatars.images.get(url) {
        return state.clone();
    }
    let dir = avatars.dir.clone();
    // Mark it as asked for, so one download runs per URL.
    cx.update_global::<Avatars, _>(|a, _| a.images.insert(url.to_owned(), None));
    let client = client()?;
    let url = url.to_owned();
    let task = cx.background_executor().spawn(async move {
        let path = file_for(&dir, &url);
        if !path.exists() {
            let bytes = client.download(&url).ok()?;
            std::fs::create_dir_all(&dir).ok()?;
            let tmp = path.with_extension("part");
            std::fs::write(&tmp, bytes).ok()?;
            std::fs::rename(&tmp, &path).ok()?;
        }
        Some((url, path))
    });
    cx.spawn(async move |cx| {
        if let Some((url, path)) = task.await {
            cx.update_global::<Avatars, _>(|a, _| a.images.insert(url, Some(path)));
            cx.refresh();
        }
    })
    .detach();
    None
}

/// Forget which authors had no avatar (an account was added or removed, so asking again
/// may find one).
pub fn forget_author_lookups(cx: &mut App) {
    if cx.has_global::<Avatars>() {
        cx.update_global::<Avatars, _>(|a, _| a.authors.clear());
    }
}

/// The avatar of a commit's author: from a GitHub no-reply address, or asked of the
/// service of an account signed in for the repository's remote. Looked up once per email.
pub fn author_avatar(
    email: &str,
    sha: &str,
    remote_url: Option<&str>,
    cx: &mut App,
) -> Option<PathBuf> {
    let email = email.trim().to_ascii_lowercase();
    if email.is_empty() {
        return None;
    }
    let known = cx.try_global::<Avatars>()?.authors.get(&email).cloned();
    match known {
        Some(Some(url)) => return avatar(&url, cx),
        Some(None) => return None,
        None => {}
    }
    if let Some(url) = switchhead_hosting::api::noreply_avatar(&email) {
        cx.update_global::<Avatars, _>(|a, _| a.authors.insert(email.clone(), Some(url.clone())));
        return avatar(&url, cx);
    }
    let all = accounts(cx);
    let repo = remote_url.and_then(|u| all.parse_remote(u));
    // GitHub answers per commit (needs the repository); GitLab by email (any account).
    let account = repo
        .as_ref()
        .and_then(|r| all.for_repo(r))
        .or_else(|| all.0.iter().find(|a| a.provider == Provider::GitLab))
        .cloned();
    let (Some(account), Some(client)) = (account, client()) else {
        cx.update_global::<Avatars, _>(|a, _| a.authors.insert(email.clone(), None));
        return None;
    };
    // Mark it as looked up, so one lookup runs per email.
    cx.update_global::<Avatars, _>(|a, _| a.authors.insert(email.clone(), None));
    let sha = sha.to_owned();
    let task = cx.background_executor().spawn({
        let email = email.clone();
        async move {
            let token = token(&account).ok()?;
            client
                .author_avatar(&email, &sha, repo.as_ref(), Some((&account, &token)))
                .ok()
                .flatten()
        }
    });
    cx.spawn(async move |cx| {
        if let Some(url) = task.await {
            cx.update_global::<Avatars, _>(|a, _| a.authors.insert(email, Some(url)));
            cx.refresh();
        }
    })
    .detach();
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(provider: Provider, host: &str, login: &str) -> Account {
        Account {
            provider,
            host: host.into(),
            organization: String::new(),
            auth_user: String::new(),
            login: login.into(),
            name: String::new(),
            avatar_url: None,
        }
    }

    #[test]
    fn finds_the_account_for_a_remote() {
        let mut all = Accounts::default();
        all.upsert(account(Provider::GitHub, "https://github.com", "jl"));
        all.upsert(account(Provider::GitLab, "https://git.corp.example", "jl"));
        all.upsert(account(Provider::GitHub, "https://github.com", "jl"));
        assert_eq!(all.0.len(), 2);
        let repo = all
            .parse_remote("git@git.corp.example:team/app.git")
            .unwrap();
        assert_eq!(repo.provider, Provider::GitLab);
        assert_eq!(
            all.for_repo(&repo).map(|a| a.host.as_str()),
            Some("https://git.corp.example")
        );
        let repo = all.parse_remote("https://gitlab.com/team/app.git").unwrap();
        assert!(all.for_repo(&repo).is_none());
    }

    #[test]
    fn avatar_files_are_stable() {
        let dir = std::path::Path::new("/tmp/a");
        assert_eq!(file_for(dir, "https://x/1"), file_for(dir, "https://x/1"));
        assert_ne!(file_for(dir, "https://x/1"), file_for(dir, "https://x/2"));
    }
}
