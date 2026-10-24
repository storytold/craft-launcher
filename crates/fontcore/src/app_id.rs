use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// A suite app identifier in lowercase reverse-DNS form, such as
/// `ai.storytold.photocraft`.
///
/// Valid ids have at least three dot-separated labels. Each label is 1–63
/// characters of `a-z`, `0-9` and `-`, and does not start or end with `-`.
/// The whole id is at most 253 characters. Ids compare exactly.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AppId(String);

impl AppId {
    /// Validates `id`, returning [`Error::InvalidAppId`] if it is not lowercase reverse-DNS.
    pub fn new(id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        if is_valid(&id) { Ok(Self(id)) } else { Err(Error::InvalidAppId(id)) }
    }

    /// The id as written, e.g. `ai.storytold.photocraft`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_valid(id: &str) -> bool {
    let label_ok = |label: &str| {
        (1..=63).contains(&label.len())
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    id.len() <= 253 && id.split('.').count() >= 3 && id.split('.').all(label_ok)
}

impl FromStr for AppId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

impl TryFrom<String> for AppId {
    type Error = Error;

    fn try_from(s: String) -> Result<Self> {
        Self::new(s)
    }
}

impl From<AppId> for String {
    fn from(id: AppId) -> Self {
        id.0
    }
}

impl AsRef<str> for AppId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_reverse_dns() {
        for ok in ["ai.storytold.photocraft", "ai.storytold.pdf-craft", "com.example.app2"] {
            assert!(AppId::new(ok).is_ok(), "{ok}");
        }
        let long_label = format!("ai.storytold.{}", "a".repeat(64));
        for bad in [
            "photocraft",
            "storytold.photocraft",
            "ai.storytold.PhotoCraft",
            "ai.storytold..photocraft",
            "ai.storytold.photocraft.",
            "ai.storytold.-photocraft",
            "ai.storytold.photo_craft",
            "ai.storytold.photo craft",
            long_label.as_str(),
        ] {
            assert!(matches!(AppId::new(bad), Err(Error::InvalidAppId(_))), "{bad}");
        }
    }
}
