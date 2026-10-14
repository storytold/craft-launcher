//! The token, in the operating system's own keychain.
//!
//! Windows Credential Manager, the macOS Keychain, or the Secret Service on Linux and the BSDs.
//!
//! A bearer token in a JSON file beside an application's settings is what this replaces. Such a
//! file is readable by anything running as the user. It survives in backups. It leaves nothing for
//! an administrator to revoke. A keychain entry is scoped to the one user. The platform encrypts it
//! at rest. It can be deleted one entry at a time from the platform's own window.
//!
//! One service name covers the whole suite. Every entry the suite ever writes is therefore findable
//! and removable in one place.
//!
//! ```text
//! service  ai.storyteller.artcraft.suite-account
//! user     the account handle (crate::accounts::handle: a digest of the deployment and the subject)
//! secret   the CollabCraft session token, as returned by POST /auth/device/poll
//! ```
//!
//! On Windows that is a generic credential whose target name is
//! `<handle>.ai.storyteller.artcraft.suite-account`, with machine-local persistence. A bearer token
//! minted for this machine therefore stays on it. On macOS it is a generic password in the user's
//! login keychain. On Linux and the BSDs it is a Secret Service item in the user's default
//! collection. That collection is locked with the login session.
//!
//! [`TokenStore`] is a trait so everything above it can be tested without a keychain. [`Memory`] is
//! the stand-in, the way `craft-nextcloud` drives a canned transport.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::error::Error;
use crate::secret::Secret;

/// The keychain service name every Craft App uses. One namespace, one place to revoke.
pub const KEYCHAIN_SERVICE: &str = "ai.storyteller.artcraft.suite-account";

/// Reading and writing one token per account handle.
pub trait TokenStore {
    /// Whether this machine has a keychain this process can use.
    ///
    /// A headless Linux box with no Secret Service answers `Err`. The suite then behaves as though
    /// nobody is signed in. No token goes to a file.
    fn available(&self) -> Result<(), String>;
    /// The token for `handle`, or `Ok(None)` when there is no entry.
    fn get(&self, handle: &str) -> Result<Option<Secret>, String>;
    fn put(&self, handle: &str, token: &Secret) -> Result<(), String>;
    /// Removes the entry. Already gone is success.
    fn remove(&self, handle: &str) -> Result<(), String>;
}

/// The platform's keychain.
#[derive(Clone, Copy, Debug, Default)]
pub struct Keychain;

impl Keychain {
    fn entry(handle: &str) -> Result<keyring_core::Entry, Error> {
        // Installs the platform store on the first call, once per process.
        if let Err(e) = keyring::Entry::store_status() {
            return Err(Error::Keychain(e.to_string()));
        }
        #[cfg(target_os = "windows")]
        {
            // Local, where the store's own default is Enterprise. An Enterprise credential is part
            // of the roaming profile. A bearer token minted for this machine has no business
            // travelling to another one.
            let modifiers = std::collections::HashMap::from([("persistence", "Local")]);
            keyring_core::Entry::new_with_modifiers(KEYCHAIN_SERVICE, handle, &modifiers).map_err(|e| Error::Keychain(e.to_string()))
        }
        #[cfg(not(target_os = "windows"))]
        keyring_core::Entry::new(KEYCHAIN_SERVICE, handle).map_err(|e| Error::Keychain(e.to_string()))
    }
}

impl TokenStore for Keychain {
    fn available(&self) -> Result<(), String> {
        match keyring::Entry::store_status() {
            Ok(()) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn get(&self, handle: &str) -> Result<Option<Secret>, String> {
        let entry = Keychain::entry(handle).map_err(|e| e.to_string())?;
        match entry.get_password() {
            Ok(token) => Ok(Some(Secret::new(token))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn put(&self, handle: &str, token: &Secret) -> Result<(), String> {
        let entry = Keychain::entry(handle).map_err(|e| e.to_string())?;
        entry.set_password(token.expose()).map_err(|e| e.to_string())
    }

    fn remove(&self, handle: &str) -> Result<(), String> {
        let entry = Keychain::entry(handle).map_err(|e| e.to_string())?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// A keychain stand-in that keeps entries in memory.
///
/// For tests. It also covers the answers a real keychain gives that are awkward to arrange: an
/// entry is there, there is no keychain on this machine, the keychain is there and locked.
#[derive(Clone, Debug, Default)]
pub struct Memory {
    entries: Arc<Mutex<BTreeMap<String, Secret>>>,
    /// Why there is no usable keychain here, when there is not one.
    missing: Option<String>,
    /// Why every read and write fails, for a keychain that exists and refuses.
    refusing: Option<String>,
}

impl Memory {
    pub fn new() -> Memory {
        Memory::default()
    }

    /// A machine with no keychain at all.
    pub fn missing(why: impl Into<String>) -> Memory {
        Memory { missing: Some(why.into()), ..Memory::default() }
    }

    /// A keychain that is there and refuses, the way a locked Secret Service does.
    pub fn refusing(why: impl Into<String>) -> Memory {
        Memory { refusing: Some(why.into()), ..Memory::default() }
    }

    /// How many entries are held. Tests use it to show a sign-out removed one.
    pub fn count(&self) -> usize {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    pub fn has(&self, handle: &str) -> bool {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).contains_key(handle)
    }
}

impl TokenStore for Memory {
    fn available(&self) -> Result<(), String> {
        match &self.missing {
            Some(why) => Err(why.clone()),
            None => Ok(()),
        }
    }

    fn get(&self, handle: &str) -> Result<Option<Secret>, String> {
        self.available()?;
        if let Some(why) = &self.refusing {
            return Err(why.clone());
        }
        Ok(self.entries.lock().unwrap_or_else(PoisonError::into_inner).get(handle).cloned())
    }

    fn put(&self, handle: &str, token: &Secret) -> Result<(), String> {
        self.available()?;
        if let Some(why) = &self.refusing {
            return Err(why.clone());
        }
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).insert(handle.to_owned(), token.clone());
        Ok(())
    }

    fn remove(&self, handle: &str) -> Result<(), String> {
        self.available()?;
        if let Some(why) = &self.refusing {
            return Err(why.clone());
        }
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).remove(handle);
        Ok(())
    }
}
