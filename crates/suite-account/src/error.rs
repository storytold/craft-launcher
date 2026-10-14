//! What can go wrong.

/// A failure in the shared account store.
///
/// None of these is "nobody is signed in". That is an ordinary answer. It comes back as
/// [`crate::Shared::None`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// This process reads and writes nothing: `ARTCRAFT_SUITE_DIR` is set and empty, or the system
    /// has no per-user configuration folder.
    Isolated,
    Io(String),
    /// The accounts file is in a format this build does not know, or is not an accounts file.
    Malformed(String),
    /// The operating system's keychain refused.
    Keychain(String),
    /// No account on file has that handle.
    Unknown(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Isolated => f.write_str("this process keeps no shared account"),
            Error::Io(why) | Error::Malformed(why) => f.write_str(why),
            Error::Keychain(why) => write!(f, "the keychain refused: {why}"),
            Error::Unknown(id) => write!(f, "no account {id} is signed in"),
        }
    }
}

impl std::error::Error for Error {}
