//! Hosting accounts: sign in to GitHub, GitLab, Bitbucket and Azure DevOps with an access
//! token, list the account's repositories to clone, create pull requests, and fetch
//! avatars; also the update feed on switchhead's website, for the update check. Calls are
//! blocking (run them on a background thread); tokens live in the OS keychain
//! ([`secrets`]) and never appear in logs, errors or the app's state file.

pub mod api;
pub mod remote;
pub mod secrets;

use serde::{Deserialize, Serialize};

pub use remote::RepoRef;
pub use secrets::Token;

/// Errors from the hosting APIs and the keychain.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The request did not reach the server or its answer could not be read.
    #[error("{0}")]
    Network(String),
    /// The server refused the token.
    #[error("the token was refused ({0})")]
    Unauthorized(u16),
    /// The server answered with an error.
    #[error("{message} ({status})")]
    Api {
        /// HTTP status.
        status: u16,
        /// The server's message.
        message: String,
    },
    /// The keychain failed.
    #[error("keychain: {0}")]
    Keychain(String),
    /// Something the caller asked for cannot be done.
    #[error("{0}")]
    Invalid(String),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// A hosting service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// github.com or GitHub Enterprise Server.
    GitHub,
    /// gitlab.com or a self-managed GitLab.
    GitLab,
    /// Bitbucket Cloud.
    Bitbucket,
    /// Azure DevOps Services.
    AzureDevOps,
}

impl Provider {
    /// Every provider, in the app's order.
    pub const ALL: [Provider; 4] = [
        Provider::GitHub,
        Provider::GitLab,
        Provider::Bitbucket,
        Provider::AzureDevOps,
    ];

    /// Its name.
    pub fn name(self) -> &'static str {
        match self {
            Provider::GitHub => "GitHub",
            Provider::GitLab => "GitLab",
            Provider::Bitbucket => "Bitbucket",
            Provider::AzureDevOps => "Azure DevOps",
        }
    }

    /// The public service's address.
    pub fn default_host(self) -> &'static str {
        match self {
            Provider::GitHub => "https://github.com",
            Provider::GitLab => "https://gitlab.com",
            Provider::Bitbucket => "https://bitbucket.org",
            Provider::AzureDevOps => "https://dev.azure.com",
        }
    }

    /// Whether a self-hosted address can replace [`Provider::default_host`].
    pub fn self_hosted(self) -> bool {
        matches!(self, Provider::GitHub | Provider::GitLab)
    }

    /// Whether signing in needs a user name besides the token (Bitbucket's basic auth).
    pub fn needs_user(self) -> bool {
        self == Provider::Bitbucket
    }

    /// Whether signing in needs an organization (Azure DevOps).
    pub fn needs_organization(self) -> bool {
        self == Provider::AzureDevOps
    }

    /// The page where a token is created, and what it needs.
    pub fn token_help(self, host: &str) -> (String, &'static str) {
        let host = host.trim_end_matches('/');
        match self {
            Provider::GitHub => (
                format!("{host}/settings/tokens/new?scopes=repo,read:user&description=switchhead"),
                "A classic token with the repo and read:user scopes.",
            ),
            Provider::GitLab => (
                format!("{host}/-/user_settings/personal_access_tokens?name=switchhead&scopes=api"),
                "A personal access token with the api scope.",
            ),
            Provider::Bitbucket => (
                "https://id.atlassian.com/manage-profile/security/api-tokens".to_owned(),
                "An API token with repository read and pull request write access; sign in with your Atlassian email.",
            ),
            Provider::AzureDevOps => (
                format!("{host}/_usersSettings/tokens"),
                "A personal access token with Code (Read & write) access.",
            ),
        }
    }
}

/// A signed-in account (its token is in the keychain under [`Account::key`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// The service.
    pub provider: Provider,
    /// The site's address, `https://github.com` or a self-hosted one.
    pub host: String,
    /// The Azure DevOps organization (empty for the others).
    #[serde(default)]
    pub organization: String,
    /// The user name the token was given with (Bitbucket's basic auth; empty for others).
    #[serde(default)]
    pub auth_user: String,
    /// The account's login.
    pub login: String,
    /// Its display name.
    #[serde(default)]
    pub name: String,
    /// Its avatar.
    #[serde(default)]
    pub avatar_url: Option<String>,
}

impl Account {
    /// The account's keychain item name (no secret in it).
    pub fn key(&self) -> String {
        format!(
            "account:{}:{}:{}:{}",
            serde_json::to_value(self.provider)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            self.host,
            self.organization,
            self.login
        )
    }

    /// "jeremie on GitHub" style label.
    pub fn label(&self) -> String {
        let site = remote::host_of(&self.host).unwrap_or(&self.host);
        let place = if self.host == self.provider.default_host() {
            self.provider.name().to_owned()
        } else {
            format!("{} ({site})", self.provider.name())
        };
        if self.organization.is_empty() {
            format!("{} on {place}", self.login)
        } else {
            format!("{} on {place} / {}", self.login, self.organization)
        }
    }

    /// Whether this account can act on `repo`.
    pub fn serves(&self, repo: &RepoRef) -> bool {
        self.provider == repo.provider
            && remote::same_host(&self.host, &repo.host)
            && (self.provider != Provider::AzureDevOps
                || self.organization.eq_ignore_ascii_case(&repo.organization))
    }
}

/// A repository an account can clone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteRepo {
    /// `owner/name` (`project/name` on Azure DevOps).
    pub full_name: String,
    /// Its description.
    pub description: String,
    /// Private or internal.
    pub private: bool,
    /// HTTPS clone URL.
    pub https_url: String,
    /// SSH clone URL.
    pub ssh_url: String,
}

/// A pull request to create.
#[derive(Clone, Debug, Default)]
pub struct NewPullRequest {
    /// The branch with the changes, as named on the remote.
    pub head: String,
    /// The branch to merge into.
    pub base: String,
    /// Title.
    pub title: String,
    /// Description.
    pub body: String,
    /// Open it as a draft.
    pub draft: bool,
}
