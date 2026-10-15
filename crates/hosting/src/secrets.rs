//! Account tokens in the OS keychain (macOS Keychain, Windows Credential Manager, the
//! Secret Service on Linux). Where there is no keychain (a Linux desktop without a Secret
//! Service), a token is kept in memory for the session only and must be entered again
//! after a restart; it is never written to disk by switchhead.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, OnceLock};

use crate::{Error, Result};

/// The keychain service name.
pub const SERVICE: &str = "dev.switchhead.app";

/// An access token. Its `Debug` output is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// A token from its text (trimmed).
    pub fn new(text: &str) -> Self {
        Token(text.trim().to_owned())
    }

    /// The token's text, for an `Authorization` header.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether it is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(***)")
    }
}

/// Session-only tokens, when there is no keychain.
fn memory() -> &'static Mutex<HashMap<String, Token>> {
    static MEMORY: OnceLock<Mutex<HashMap<String, Token>>> = OnceLock::new();
    MEMORY.get_or_init(Mutex::default)
}

/// Whether the OS keychain can be used.
pub fn keychain_available() -> bool {
    keyring::Entry::store_status().is_ok()
}

fn entry(key: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, key).map_err(|e| Error::Keychain(e.to_string()))
}

/// Store the token for `key`. Returns `false` when it is kept for this session only (no
/// keychain).
pub fn store(key: &str, token: &Token) -> Result<bool> {
    if !keychain_available() {
        if let Ok(mut m) = memory().lock() {
            m.insert(key.to_owned(), token.clone());
        }
        return Ok(false);
    }
    entry(key)?
        .set_password(token.expose())
        .map_err(|e| Error::Keychain(e.to_string()))?;
    Ok(true)
}

/// The token for `key`, if one is stored.
pub fn load(key: &str) -> Result<Option<Token>> {
    if let Some(t) = memory().lock().ok().and_then(|m| m.get(key).cloned()) {
        return Ok(Some(t));
    }
    if !keychain_available() {
        return Ok(None);
    }
    match entry(key)?.get_password() {
        Ok(v) => Ok(Some(Token(v))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(Error::Keychain(e.to_string())),
    }
}

/// Forget the token for `key` (missing is fine).
pub fn delete(key: &str) -> Result<()> {
    if let Ok(mut m) = memory().lock() {
        m.remove(key);
    }
    if !keychain_available() {
        return Ok(());
    }
    match entry(key)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(Error::Keychain(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_redacted() {
        let t = Token::new("  ghp_secret \n");
        assert_eq!(t.expose(), "ghp_secret");
        assert_eq!(format!("{t:?}"), "Token(***)");
    }
}
