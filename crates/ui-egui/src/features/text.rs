//! Wording for the AI tools screen. Plain functions so tests can check the sentences.

use craft_ai_features::Blocker;

use crate::i18n::fmt;

/// A size for people: decimal units, one decimal place from gigabytes up.
pub fn bytes(n: u64) -> String {
    const KB: u64 = 1_000;
    const MB: u64 = 1_000_000;
    const GB: u64 = 1_000_000_000;
    if n >= GB {
        // Tenths of a gigabyte, rounded to nearest, without floating point.
        let tenths = n.saturating_add(GB / 20) / (GB / 10);
        format!("{}.{} GB", tenths / 10, tenths % 10)
    } else if n >= MB {
        format!("{} MB", (n + MB / 2) / MB)
    } else if n >= KB {
        format!("{} KB", (n + KB / 2) / KB)
    } else {
        format!("{n} B")
    }
}

pub(super) fn blocker_text(blocker: Option<&Blocker>) -> String {
    match blocker {
        Some(Blocker::NotEnoughVram { needed_mb, have_mb }) => {
            fmt(tl!("Needs {needed} GB of graphics memory; this computer has {have} GB"), &[("needed", &gb(*needed_mb)), ("have", &gb(*have_mb))])
        }
        Some(Blocker::NotEnoughDisk { needed_bytes, free_bytes }) => {
            fmt(tl!("Needs {needed} of free disk space; {free} is free"), &[("needed", &bytes(*needed_bytes)), ("free", &bytes(*free_bytes))])
        }
        None => tl!("Cannot run on this computer").to_owned(),
    }
}

fn gb(mb: u32) -> String {
    let tenths = (u64::from(mb) * 10 + 512) / 1024;
    if tenths % 10 == 0 { (tenths / 10).to_string() } else { format!("{}.{}", tenths / 10, tenths % 10) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_the_way_download_pages_show_them() {
        assert_eq!(bytes(3_449_860_632), "3.4 GB");
        assert_eq!(bytes(24_000_000_000), "24.0 GB");
        assert_eq!(bytes(999_999_999), "1000 MB");
        assert_eq!(bytes(820_400_000), "820 MB");
        assert_eq!(bytes(12_345), "12 KB");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(u64::MAX), "18446744073.7 GB");
    }

    #[test]
    fn memory_is_shown_in_whole_gigabytes_when_it_is_whole() {
        assert_eq!(gb(24_576), "24");
        assert_eq!(gb(8_192), "8");
        assert_eq!(gb(12_000), "11.7");
    }
}
