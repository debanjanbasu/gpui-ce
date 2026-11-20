//! Minimal iOS keychain credentials stub — the SecItem store lands with the
//! mobile pin; the platform trait's credential fns report unavailability
//! until then.

use anyhow::{Result, anyhow};

/// A stored credential returned by [`read_credentials`].
pub struct Credential {
    pub username: String,
    pub password: String,
}

/// Writes credentials for `url` into the iOS keychain.
pub fn write_credentials(_url: &str, _username: &str, _password: &[u8]) -> Result<()> {
    Err(anyhow!("keychain: lands with the mobile pin"))
}

/// Reads credentials for `url` from the iOS keychain.
pub fn read_credentials(_url: &str) -> Result<Option<(String, Vec<u8>)>> {
    Err(anyhow!("keychain: lands with the mobile pin"))
}

/// Deletes credentials for `url` from the iOS keychain.
pub fn delete_credentials(_url: &str) -> Result<()> {
    Err(anyhow!("keychain: lands with the mobile pin"))
}
