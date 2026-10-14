//! The Craft App Suite's shared account store: the suite's keychain.
//!
//! Somebody signs in once and every Craft App acts as them. Before this, each application kept its
//! own session file. Taken across the suite that meant a sign-in per application, a copy of a
//! bearer token on disk per application, and a separate place to revoke per application.
//!
//! The store has two halves, kept deliberately apart:
//!
//! * The list of accounts is a small JSON file in one suite-level folder ([`paths`]). It names who
//!   has signed in, at which deployment, and which account is current. It holds no credential.
//! * The token is in the operating system's keychain ([`store`]). Windows Credential Manager, the
//!   macOS Keychain, or the Secret Service.
//!
//! Nothing here is mandatory. An application asks [`Store::shared`] what the suite has to offer and
//! gets one of three answers: somebody is signed in, nobody is signed in, or there is an account on
//! file whose token could not be read. The second and third leave the application exactly as it was
//! before this crate existed, offering its own sign-in. A machine with no keychain, a locked
//! keychain, a missing folder and an empty suite all land on "nobody is signed in". None of them is
//! an error that stops an application starting.
//!
//! [`Store::sign_out`] ends the session at the deployment and then clears the keychain entry. The
//! deployment drops every session it holds for that user. The one action therefore covers the
//! suite.
//!
//! An application upgrading from its own session file calls [`migrate::adopt`] once at startup.
//!
//! ```no_run
//! # fn main() -> Result<(), artcraft_suite_account::Error> {
//! use artcraft_suite_account::{Shared, Store};
//!
//! let store = Store::open();
//! match store.shared() {
//!     Shared::Session { account, token } => {
//!         // Use `token` as a bearer against `account.service`.
//!         let _ = (account, token);
//!     }
//!     // Offer this application's own sign-in, exactly as before.
//!     Shared::None => {}
//!     Shared::Unreadable { why, .. } => eprintln!("the shared sign-in could not be read: {why}"),
//! }
//! # Ok(()) }
//! ```

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod accounts;
mod error;
pub mod migrate;
pub mod paths;
mod secret;
pub mod service;
pub mod store;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

pub use accounts::{Account, Accounts};
pub use error::Error;
pub use migrate::Adopted;
pub use secret::Secret;
pub use service::{Flow, Identity, Polled, Service};
pub use store::{KEYCHAIN_SERVICE, Keychain, Memory, TokenStore};

/// What the suite store has to offer.
#[derive(Debug)]
pub enum Shared {
    /// Somebody is signed in and the token came back.
    Session { account: Account, token: Secret },
    /// Nobody is signed in here. Offer a sign-in.
    None,
    /// An account is on file and its token could not be read: a locked keychain, a keychain this
    /// process may not reach, or an entry somebody deleted by hand.
    ///
    /// Offer a sign-in. Say why the shared one did not come through. Quietly showing a signed-out
    /// window would leave somebody wondering where their account went.
    Unreadable { account: Account, why: String },
}

/// A sign-in about to be recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewAccount {
    /// The deployment's base URL.
    pub service: String,
    /// The deployment's identifier for the account: the OIDC subject.
    pub user: String,
    pub display: String,
    pub email: Option<String>,
    /// Whether cloud storage is linked. Recorded for a user interface; the deployment is the
    /// authority and is asked again on each start.
    pub linked: bool,
}

/// How a sign-out ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignedOut {
    /// The deployment ended the session and the keychain entry is gone.
    Everywhere,
    /// The keychain entry is gone and the deployment could not be told. The session may still be
    /// live there. Worth saying out loud. The user asked to be signed out and half of it happened.
    LocalOnly { why: String },
}

/// The suite's account store.
pub struct Store<S: TokenStore = Keychain> {
    /// The suite folder. `None` when this process keeps nothing.
    dir: Option<PathBuf>,
    tokens: S,
}

impl Store<Keychain> {
    /// The real store: the suite folder for this platform and the platform's keychain.
    pub fn open() -> Store<Keychain> {
        Store::at(paths::resolve(&paths::Probe::default()).dir, Keychain)
    }
}

impl<S: TokenStore> Store<S> {
    /// A store over a given folder and token store. `None` keeps nothing.
    pub fn at(dir: Option<PathBuf>, tokens: S) -> Store<S> {
        Store { dir, tokens }
    }

    /// The suite folder, when there is one.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// The accounts file, when there is one.
    pub fn path(&self) -> Option<PathBuf> {
        self.dir.as_deref().map(paths::file)
    }

    /// Whether this store can hold a session. It needs a folder. The machine needs a keychain.
    ///
    /// Checked before a sign-in and before a migration. Neither then half-completes.
    pub fn usable(&self) -> Result<(), Error> {
        let _ = self.dir.as_deref().ok_or(Error::Isolated)?;
        self.tokens.available().map_err(Error::Keychain)
    }

    /// Every account on file. An isolated store has none.
    pub fn accounts(&self) -> Result<Accounts, Error> {
        match self.path() {
            Some(path) => accounts::read(&path),
            None => Ok(Accounts::default()),
        }
    }

    /// What the suite has to offer. Never fails: absence of every kind is [`Shared::None`].
    ///
    /// This is the call an application makes at startup. The answer decides whether it offers its
    /// own sign-in.
    pub fn shared(&self) -> Shared {
        let list = match self.accounts() {
            Ok(list) => list,
            Err(Error::Isolated) => return Shared::None,
            Err(e) => {
                log::warn!("the suite account list could not be read: {e}");
                return Shared::None;
            }
        };
        let Some(account) = list.current().cloned() else { return Shared::None };
        match self.tokens.get(&account.id) {
            Ok(Some(token)) => Shared::Session { account, token },
            // On file with no entry behind it. Somebody deleted the entry, or a sign-out got half
            // way. Either way nobody is signed in. The next sign-in tidies the row up.
            Ok(None) => Shared::None,
            Err(why) => Shared::Unreadable { account, why },
        }
    }

    /// Records a sign-in: the token into the keychain, then the account onto the list.
    ///
    /// The token goes first. A failed list write then leaves the previous state intact and one
    /// unreferenced keychain entry. The next sign-in for the same account overwrites that entry,
    /// because the handle is a digest of the deployment and the subject. Nothing fresh is minted
    /// each time. The other order would leave an account on the list with no token behind it.
    pub fn sign_in(&self, new: &NewAccount, token: &Secret, now: i64) -> Result<Account, Error> {
        self.usable()?;
        let path = self.path().ok_or(Error::Isolated)?;
        let service = accounts::normalise(&new.service);
        let id = accounts::handle(&service, &new.user);
        self.tokens.put(&id, token).map_err(Error::Keychain)?;
        let mut list = accounts::read(&path)?;
        let added = list.get(&id).map_or(now, |a| a.added);
        let account = Account { id, service, user: new.user.clone(), display: new.display.clone(), email: new.email.clone(), added, last_used: now };
        list.put(account.clone());
        accounts::write(&path, &list)?;
        Ok(account)
    }

    /// Makes an account the current one.
    pub fn set_active(&self, id: &str) -> Result<(), Error> {
        let path = self.path().ok_or(Error::Isolated)?;
        let mut list = accounts::read(&path)?;
        if list.get(id).is_none() {
            return Err(Error::Unknown(id.to_owned()));
        }
        list.active = Some(id.to_owned());
        accounts::write(&path, &list)
    }

    /// Records that an application used the account, for a list that can show the most recent one.
    pub fn touch(&self, id: &str, now: i64) -> Result<(), Error> {
        let path = self.path().ok_or(Error::Isolated)?;
        let mut list = accounts::read(&path)?;
        let Some(account) = list.accounts.iter_mut().find(|a| a.id == id) else {
            return Err(Error::Unknown(id.to_owned()));
        };
        account.last_used = now;
        accounts::write(&path, &list)
    }

    /// Ends a session everywhere: at the deployment, then here.
    ///
    /// The deployment is told first, while the token can still be read. It drops every session it
    /// holds for that user. This one action therefore signs the person out of the whole suite and
    /// of any other machine they left a session on.
    ///
    /// A deployment that cannot be reached does not block the local half. The user asked to be
    /// signed out. Clearing the entry and saying the server was not told beats leaving a readable
    /// token in the keychain.
    pub fn sign_out(&self, service: &dyn Service, id: &str) -> Result<SignedOut, Error> {
        let path = self.path().ok_or(Error::Isolated)?;
        let account = accounts::read(&path)?.get(id).cloned().ok_or_else(|| Error::Unknown(id.to_owned()))?;
        let told = match self.tokens.get(id) {
            Ok(Some(token)) => service.revoke(&account.service, &token).err(),
            // No token to present. There is nothing the deployment would accept. Clearing the row
            // is the whole of the job.
            Ok(None) => None,
            Err(why) => Some(why),
        };
        self.discard(id)?;
        Ok(match told {
            None => SignedOut::Everywhere,
            Some(why) => SignedOut::LocalOnly { why },
        })
    }

    /// Clears an account here without telling the deployment: the keychain entry, then the row.
    ///
    /// For a session the deployment has already stopped honouring. There is nothing left to revoke
    /// and the entry should still go.
    pub fn discard(&self, id: &str) -> Result<(), Error> {
        let path = self.path().ok_or(Error::Isolated)?;
        self.forget(id)?;
        let mut list = accounts::read(&path)?;
        list.remove(id);
        accounts::write(&path, &list)
    }

    /// Clears the keychain entry for an account, leaving the list alone.
    pub fn forget(&self, id: &str) -> Result<(), Error> {
        self.tokens.remove(id).map_err(Error::Keychain)
    }
}

impl<S: TokenStore + std::fmt::Debug> std::fmt::Debug for Store<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("dir", &self.dir).field("tokens", &self.tokens).finish()
    }
}
