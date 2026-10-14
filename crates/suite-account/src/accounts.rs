//! The accounts file: who has signed in and at which deployment.
//!
//! No token is in here. Every field is something a user interface shows, or something needed to
//! find the token in the keychain. See [`crate::store`] for the token itself.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::Error;

/// The format this code writes. A reader that does not recognise the number leaves the file alone.
pub const FORMAT: u32 = 1;

/// The largest accounts file that will be read. A suite account list is a few hundred bytes. A
/// megabyte is already far past anything legitimate.
const MAX_BYTES: u64 = 1 << 20;

/// One account, as the suite remembers it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    /// The handle for this account inside the suite. It is also the keychain entry's user name.
    ///
    /// A digest of `service` and `user` (see [`handle`]). Signing in again as the same person at
    /// the same deployment therefore lands on the same keychain entry. No stale entry is left
    /// behind.
    pub id: String,
    /// The CollabCraft deployment, as a base URL with no trailing slash.
    pub service: String,
    /// The deployment's own identifier for this account: the OIDC subject, never an address.
    pub user: String,
    /// What to show a person. Never an identity: two people can share a display name.
    pub display: String,
    pub email: Option<String>,
    /// Unix seconds when this account was first stored here.
    pub added: i64,
    /// Unix seconds when an application last used it.
    pub last_used: i64,
}

/// The whole file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Accounts {
    pub format: u32,
    pub accounts: Vec<Account>,
    /// The account an application should use when it has no reason to prefer another.
    pub active: Option<String>,
}

impl Default for Accounts {
    fn default() -> Accounts {
        Accounts { format: FORMAT, accounts: Vec::new(), active: None }
    }
}

impl Accounts {
    pub fn get(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    /// The active account, when one is named and still on the list.
    ///
    /// An `active` naming an account that is gone gives the ordinary answer of `None`. A
    /// half-finished sign-out on another machine must not stop an application starting.
    pub fn current(&self) -> Option<&Account> {
        self.active.as_deref().and_then(|id| self.get(id))
    }

    /// Adds or replaces an account. The account then becomes the active one.
    pub fn put(&mut self, account: Account) {
        self.active = Some(account.id.clone());
        match self.accounts.iter_mut().find(|a| a.id == account.id) {
            Some(existing) => *existing = account,
            None => self.accounts.push(account),
        }
    }

    /// Drops an account. Whoever is left over, if anybody, becomes active.
    pub fn remove(&mut self, id: &str) {
        self.accounts.retain(|a| a.id != id);
        if self.active.as_deref() == Some(id) {
            self.active = self.accounts.first().map(|a| a.id.clone());
        }
    }
}

/// The handle for an account at a deployment.
///
/// A digest, with the address and the subject kept out of it. The digest is stable. One account
/// therefore stays on one keychain entry however many times somebody signs in. The handle is also
/// opaque. A credential manager window puts the handle on screen. Nobody's address belongs in that
/// list.
pub fn handle(service: &str, user: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(normalise(service).as_bytes());
    hash.update(b"\n");
    hash.update(user.as_bytes());
    let digest = hash.finalize();
    digest.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

/// A deployment's base URL with any trailing slashes taken off. One deployment is then one account
/// however the URL was typed.
pub fn normalise(service: &str) -> String {
    service.trim().trim_end_matches('/').to_owned()
}

/// Reads the file. A missing file is an empty list. That is what "nobody has signed in" looks like.
pub fn read(path: &Path) -> Result<Accounts, Error> {
    let bytes = match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_BYTES => {
            return Err(Error::Malformed(format!("{} is {} bytes, too large to be an account list", path.display(), meta.len())));
        }
        Ok(_) => std::fs::read(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Accounts::default()),
        Err(e) => return Err(Error::Io(format!("couldn't read {} ({e})", path.display()))),
    };
    let bytes = bytes.map_err(|e| Error::Io(format!("couldn't read {} ({e})", path.display())))?;
    let parsed: Accounts = serde_json::from_slice(&bytes).map_err(|e| Error::Malformed(format!("{} didn't parse ({e})", path.display())))?;
    if parsed.format > FORMAT {
        return Err(Error::Malformed(format!("{} is format {}. This build understands {FORMAT}.", path.display(), parsed.format)));
    }
    Ok(parsed)
}

/// Writes the file through a temporary file and a rename. A crash then never leaves half of one.
///
/// Readable by this user alone where the platform has file modes. The file holds no token. It does
/// say who is signed in. That is nobody else's business.
pub fn write(path: &Path, accounts: &Accounts) -> Result<(), Error> {
    let dir = path.parent().ok_or_else(|| Error::Io(format!("{} has no parent folder", path.display())))?;
    std::fs::create_dir_all(dir).map_err(|e| Error::Io(format!("couldn't create {} ({e})", dir.display())))?;
    let json = serde_json::to_vec_pretty(accounts).map_err(|e| Error::Malformed(e.to_string()))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &json).map_err(|e| Error::Io(format!("couldn't write {} ({e})", tmp.display())))?;
    restrict(&tmp)?;
    std::fs::rename(&tmp, path).map_err(|e| Error::Io(format!("couldn't replace {} ({e})", path.display())))
}

#[cfg(unix)]
fn restrict(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| Error::Io(format!("couldn't set the mode on {} ({e})", path.display())))
}

#[cfg(not(unix))]
fn restrict(_path: &Path) -> Result<(), Error> {
    // Windows gives a file in `%APPDATA%` the profile's own access rules. Those are this user's.
    Ok(())
}
