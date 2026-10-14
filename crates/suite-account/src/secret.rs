//! A credential held in memory.

use zeroize::Zeroizing;

/// A bearer token, wrapped so it cannot be printed by accident.
///
/// There is no `Display` and no `Serialize`. [`Debug`] reports the length and nothing else. A token
/// therefore never reaches a log line through a `{:?}` on some struct that happens to carry one.
/// The bytes are overwritten when the value is dropped.
///
/// [`Secret::expose`] is the one way out. The only caller that needs it is the code building an
/// `Authorization` header.
#[derive(Clone)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(Zeroizing::new(value.into()))
    }

    /// The bytes themselves. Send them. Never log them.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// How many bytes the token is. A report names this and never the value.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret({} bytes)", self.0.len())
    }
}

impl PartialEq for Secret {
    fn eq(&self, other: &Secret) -> bool {
        *self.0 == *other.0
    }
}

impl Eq for Secret {}
