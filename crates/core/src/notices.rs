//! Announcements and alerts: ArtCraft's messages to every launcher, read only from a file on
//! getartcraft.com signed with the hosted catalog's key (docs/adr/0002). Nothing unsigned is
//! ever shown as a notice.
//!
//! The signature proves who wrote the file, not when: a notice's signed `expires` bounds how long
//! an old copy served again can show it, and one `published` in the future waits until then.

use std::collections::HashSet;

use serde::Deserialize;

use crate::hosted_catalog::{check_signature, pinned_key};
use crate::release::{safe_web_link, truncate};
use crate::time::parse_rfc3339;

pub const NOTICES_URL: &str = "https://getartcraft.com/launcher-announcements.toml";
pub const NOTICES_SIGNATURE_URL: &str = "https://getartcraft.com/launcher-announcements.toml.sig";
pub const MAX_NOTICES_BYTES: usize = 64 << 10;
/// Of each kind; further rows are ignored.
const MAX_NOTICES: usize = 20;
const MAX_TITLE: usize = 200;
const MAX_TEXT: usize = 4000;

/// An announcement (listed in the bell) or an alert (a dialog the user acknowledges).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notice {
    /// Stable, so a dismissal sticks across launches and refreshes. Prefixed with its kind.
    pub id: String,
    pub title: String,
    pub body: String,
    /// Only `https://` links survive parsing.
    pub link: Option<String>,
    pub created_at: Option<i64>,
    /// From then on the notice is hidden, also while the launcher stays open.
    pub expires: Option<i64>,
}

impl Notice {
    /// Not yet past its `expires` at `now`.
    #[must_use]
    pub fn current(&self, now: i64) -> bool {
        self.expires.is_none_or(|e| now < e)
    }
}

/// A verified notices file, with what is not yet published or already expired at `now` left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notices {
    pub announcements: Vec<Notice>,
    pub alerts: Vec<Notice>,
}

#[derive(Deserialize)]
struct Document {
    #[serde(default)]
    announcements: Vec<toml::Value>,
    #[serde(default)]
    alerts: Vec<toml::Value>,
}

#[derive(Deserialize)]
struct Row {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    link: Option<String>,
    published: Option<String>,
    expires: Option<String>,
}

impl Notices {
    /// Checks the signature against the compiled key, then reads the file as it stands at `now`.
    pub fn verify(bytes: &[u8], signature: &[u8], now: i64) -> Result<Self, String> {
        check_signature(bytes, signature, MAX_NOTICES_BYTES, &pinned_key()?)?;
        Self::parse(bytes, now)
    }

    fn parse(bytes: &[u8], now: i64) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "notices file is not UTF-8")?;
        let doc: Document = toml::from_str(text).map_err(|e| format!("invalid notices TOML: {e}"))?;
        Ok(Self { announcements: rows(doc.announcements, "announcement", now), alerts: rows(doc.alerts, "alert", now) })
    }
}

/// The valid, current rows of one kind. A row that breaks a rule is skipped, never the file.
fn rows(values: Vec<toml::Value>, kind: &str, now: i64) -> Vec<Notice> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter_map(|v| v.try_into::<Row>().ok())
        .filter_map(|r| {
            let id_ok = !r.id.is_empty() && r.id.len() <= 80 && r.id.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'));
            // A date that is present but unreadable hides the row rather than guessing.
            let date = |s: Option<String>| s.map(|s| parse_rfc3339(&s).ok_or(())).transpose();
            let (Ok(published), Ok(expires)) = (date(r.published), date(r.expires)) else { return None };
            if published.is_some_and(|p| p > now) || expires.is_some_and(|e| e <= now) {
                return None;
            }
            let (title, body) = (r.title.trim(), r.body.trim());
            (id_ok && (!title.is_empty() || !body.is_empty()) && seen.insert(r.id.clone())).then(|| Notice {
                id: format!("{kind}:{}", r.id),
                title: truncate(title, MAX_TITLE),
                body: truncate(body, MAX_TEXT),
                link: r.link.filter(|l| safe_web_link(l)),
                created_at: published,
                expires,
            })
        })
        .take(MAX_NOTICES)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use ed25519_dalek::{Signer, SigningKey};

    const NOW: i64 = 1_795_000_000; // 2026-11-18

    fn row(id: &str, extra: &str) -> String {
        format!("[[announcements]]\nid = \"{id}\"\ntitle = \"{id} title\"\n{extra}\n")
    }

    #[test]
    fn only_a_signature_by_the_pinned_key_is_accepted() {
        let file = row("a", "");
        let other = SigningKey::from_bytes(&[7; 32]);
        let signature = STANDARD.encode(other.sign(file.as_bytes()).to_bytes());
        assert!(Notices::verify(file.as_bytes(), signature.as_bytes(), NOW).is_err());
        assert!(Notices::verify(file.as_bytes(), b"", NOW).is_err());
        let key = other.verifying_key();
        assert!(check_signature(file.as_bytes(), signature.as_bytes(), MAX_NOTICES_BYTES, &key).is_ok());
        let mut tampered = file.clone().into_bytes();
        tampered.push(b'\n');
        assert!(check_signature(&tampered, signature.as_bytes(), MAX_NOTICES_BYTES, &key).is_err());
    }

    #[test]
    fn rows_that_break_a_rule_are_skipped_and_the_rest_kept() {
        let file = [
            row("ok", "body = \"Plain text.\"\nlink = \"https://getartcraft.com/news\"\npublished = \"2026-11-01T00:00:00Z\""),
            row("expired", "expires = \"2026-11-02T00:00:00Z\""),
            row("scheduled", "published = \"2026-12-01T00:00:00Z\""),
            row("bad-date", "expires = \"soon\""),
            row("bad id", ""),
            row("ok", "body = \"a second row with a used id\""),
            row("unsafe-link", "link = \"javascript:alert(1)\""),
            "[[announcements]]\nid = \"empty\"\n".to_owned(),
            "[[announcements]]\ntitle = \"no id\"\n".to_owned(),
            "[[alerts]]\nid = \"ok\"\ntitle = \"Security update\"\nexpires = \"2026-12-01T00:00:00Z\"\n".to_owned(),
        ]
        .concat();
        let n = Notices::parse(file.as_bytes(), NOW).unwrap();
        let ids: Vec<&str> = n.announcements.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["announcement:ok", "announcement:unsafe-link"]);
        assert!(n.announcements[0].created_at.is_some() && n.announcements[0].link.is_some());
        assert_eq!(n.announcements[1].link, None);
        // Kinds keep separate ids, so dismissing one never hides the other.
        assert_eq!(n.alerts.len(), 1);
        assert_ne!(n.alerts[0].id, n.announcements[0].id);
        // A notice read before its end date stops being current once the date passes.
        let alert = &n.alerts[0];
        assert!(alert.current(NOW) && !alert.current(alert.expires.unwrap()));
    }

    #[test]
    fn a_hostile_file_is_bounded() {
        let many: String = (0..100).map(|i| row(&format!("n{i}"), &format!("body = \"{}\"", "x".repeat(5000)))).collect();
        let n = Notices::parse(many.as_bytes(), NOW).unwrap();
        assert!(n.announcements.len() < 100);
        assert!(n.announcements.iter().all(|a| a.body.len() < 5000));
        assert!(Notices::parse(b"not = [toml", NOW).is_err());
        let big = vec![b'#'; MAX_NOTICES_BYTES + 1];
        assert!(check_signature(&big, b"x", MAX_NOTICES_BYTES, &SigningKey::from_bytes(&[7; 32]).verifying_key()).is_err());
    }
}
