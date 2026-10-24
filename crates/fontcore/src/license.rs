use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The license of a font in the vault.
///
/// In `fonts.json` an SPDX expression is a plain string (`"OFL-1.1"`), and a
/// license with no SPDX identifier is wrapped in an object
/// (`{"custom": "Licensed to Acme for internal use"}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum License {
    /// An SPDX license expression such as `OFL-1.1` or `MIT OR Apache-2.0`.
    ///
    /// Manifests load even when this is not a valid expression (for example
    /// an id added to SPDX after this build); see [`License::is_recognized`].
    Spdx(String),
    Custom(CustomLicense),
}

/// Free-text license description for fonts without an SPDX identifier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomLicense {
    pub custom: String,
    /// Fields this build does not know about, preserved on rewrite.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl License {
    /// Classifies user input. Valid SPDX expressions, and single SPDX ids
    /// written in the wrong case (`ofl-1.1`), become [`License::Spdx`] in
    /// canonical form; anything else becomes [`License::Custom`]. Returns
    /// `None` for blank input.
    pub fn parse(input: &str) -> Option<Self> {
        let input = input.trim();
        if input.is_empty() {
            return None;
        }
        if spdx::Expression::parse(input).is_ok() {
            return Some(Self::Spdx(input.to_owned()));
        }
        let canonical = spdx::identifiers::LICENSES.iter().find(|l| l.name.eq_ignore_ascii_case(input)).map(|l| l.name);
        Some(match canonical {
            Some(name) => Self::Spdx(name.to_owned()),
            None => Self::custom(input),
        })
    }

    /// A free-text license, stored as `{"custom": text}`.
    pub fn custom(text: impl Into<String>) -> Self {
        Self::Custom(CustomLicense { custom: text.into(), extra: Map::new() })
    }

    /// Whether this is an SPDX expression this build can parse.
    pub fn is_recognized(&self) -> bool {
        self.expression().is_some()
    }

    /// Whether the license is open source: an SPDX expression that can be
    /// satisfied using only OSI-approved or FSF free/libre licenses. Custom
    /// licenses are never considered open source.
    pub fn is_open_source(&self) -> bool {
        self.expression().is_some_and(|expr| expr.evaluate(|req| req.license.id().is_some_and(|id| id.is_osi_approved() || id.is_fsf_free_libre())))
    }

    fn expression(&self) -> Option<spdx::Expression> {
        match self {
            Self::Spdx(s) => spdx::Expression::parse(s).ok(),
            Self::Custom(_) => None,
        }
    }
}

impl std::fmt::Display for License {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spdx(s) => f.write_str(s),
            Self::Custom(c) => f.write_str(&c.custom),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spdx(s: &str) -> License {
        License::Spdx(s.into())
    }

    #[test]
    fn parse_classifies_input() {
        assert_eq!(License::parse("  "), None);
        assert_eq!(License::parse("OFL-1.1"), Some(spdx("OFL-1.1")));
        assert_eq!(License::parse("ofl-1.1"), Some(spdx("OFL-1.1")));
        assert_eq!(License::parse("MIT OR Apache-2.0"), Some(spdx("MIT OR Apache-2.0")));
        assert_eq!(License::parse("Licensed for Acme only"), Some(License::custom("Licensed for Acme only")));
    }

    #[test]
    fn open_source_check() {
        assert!(spdx("OFL-1.1").is_open_source());
        assert!(spdx("Apache-2.0").is_open_source());
        assert!(spdx("MIT OR LicenseRef-Proprietary").is_open_source());
        assert!(!spdx("LicenseRef-Proprietary").is_open_source());
        assert!(spdx("LicenseRef-Proprietary").is_recognized());
        assert!(!spdx("CC-BY-NC-4.0").is_open_source());
        assert!(!spdx("Not A License").is_recognized());
        assert!(!License::custom("EULA").is_open_source());
    }

    #[test]
    fn serializes_as_string_or_wrapper() {
        assert_eq!(serde_json::to_string(&spdx("OFL-1.1")).unwrap(), r#""OFL-1.1""#);
        let custom = License::custom("EULA");
        assert_eq!(serde_json::to_string(&custom).unwrap(), r#"{"custom":"EULA"}"#);
        assert_eq!(serde_json::from_str::<License>(r#"{"custom":"EULA"}"#).unwrap(), custom);
        assert!(serde_json::from_str::<License>("42").is_err());
    }
}
