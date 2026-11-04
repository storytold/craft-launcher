//! Just enough calendar to show GitHub's timestamps (the conversion goes through jiff; this module
//! owns only the output format strings). Reading them is craft-update's [`parse_rfc3339`].
//!
//! # Internationalisation
//! [`format_ago`] and [`format_date`] are English-only fallbacks. Translatable UI must use
//! [`ago_kind`] plus [`month_short_en`] (or [`civil_from_unix`] directly) and render through
//! the UI crate's `tl!` / `trn` / `fmt` helpers, so this crate stays pure and wasm-safe with
//! no dependency on `ui-egui`:
//! - [`TimeAgo::JustNow`] → `tl!("just now")`
//! - [`TimeAgo::Minutes(n)`] → `trn(n, "{n} min ago", "{n} min ago")`
//! - [`TimeAgo::Hours(n)`] → `trn(n, "{n} h ago", "{n} h ago")`
//! - [`TimeAgo::Yesterday`] → `tl!("yesterday")`
//! - [`TimeAgo::Days(n)`] → `trn(n, "{n} day ago", "{n} days ago")`
//! - [`TimeAgo::Date(secs)`] → `fmt(tl!("{month} {day}, {year}"), …)` with the month from
//!   [`month_short_en`] translated via `tr_ctx("month", …)` and `{day}` / `{year}` filled in.

/// What a relative timestamp means, without any language attached.
///
/// The UI translates each variant (`tl!` / `trn` / `fmt`); see the module docs. Counts are
/// non-negative. [`TimeAgo::Date`] carries the original Unix time for the date fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimeAgo {
    /// Less than a minute old (or in the future).
    JustNow,
    /// Minutes old (1–59).
    Minutes(u64),
    /// Hours old (1–23).
    Hours(u64),
    /// Between one and two days old.
    Yesterday,
    /// Days old (2–29).
    Days(u64),
    /// Thirty days or older: show the calendar date; carries the original Unix time.
    Date(i64),
}

/// Seconds since the Unix epoch for an RFC 3339 timestamp such as `2026-10-08T14:52:50Z`. Shared
/// with every Craft app through craft-update, which tests it.
pub use craft_update::time::parse_rfc3339;

/// (year, month, day) for a Unix time (UTC). jiff's civil calendar only spans years -9999..=9999,
/// far narrower than the whole `i64` the old hand-rolled Hinnant code converted, so the instant is
/// first moved into range by whole 400-year Gregorian cycles (the calendar repeats every 400 years
/// = 146_097 days) and the real year restored afterwards. jiff still does the month/day; the cycle
/// count only shifts the year, keeping every `i64` exact (e.g. `i64::MAX` → year 292277026596,
/// `i64::MIN` → year -292277022657), with no epoch fallback for a value the old code converted.
pub fn civil_from_unix(secs: i64) -> (i64, u32, u32) {
    let epoch = jiff::civil::Date::constant(1970, 1, 1);
    let days = secs.div_euclid(86_400);
    let (cycles, rest) = (days.div_euclid(146_097), days.rem_euclid(146_097));
    // `rest` is in `0..146_097` (under one 400-year cycle), so `epoch + rest days` always lands in
    // 1970..=2369 — comfortably inside jiff's range — and `rest * 86_400` never overflows.
    let date = epoch.checked_add(jiff::SignedDuration::from_secs(rest * 86_400)).unwrap_or(epoch);
    // Restore the real year through `i128` so `cycles * 400` can't overflow at the `i64` extremes.
    let y = i128::from(date.year()) + i128::from(cycles) * 400;
    let year = i64::try_from(y).unwrap_or(if y < 0 { i64::MIN } else { i64::MAX });
    (year, u32::try_from(date.month()).unwrap_or(1), u32::try_from(date.day()).unwrap_or(1))
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// English three-letter month abbreviation for month `m` (1–12); `"?"` when out of range.
/// Source key for `tr_ctx("month", …)` at the call site; never shown untranslated in
/// non-English UI.
pub fn month_short_en(m: u32) -> &'static str {
    if (1..=12).contains(&m) { MONTHS.get((m - 1) as usize).copied().unwrap_or("?") } else { "?" }
}

/// `Oct 8, 2026`
pub fn format_date(secs: i64) -> String {
    let (y, m, d) = civil_from_unix(secs);
    let month = month_short_en(m);
    format!("{month} {d}, {y}")
}

/// Classify a timestamp for translation-neutral rendering (no allocation, no English).
/// Thresholds match [`format_ago`]: <60s → [`TimeAgo::JustNow`], <1h → minutes,
/// <1d → hours, <2d → yesterday, <30d → days, older → [`TimeAgo::Date`].
pub fn ago_kind(secs: i64, now: i64) -> TimeAgo {
    let dt = now.saturating_sub(secs);
    if dt < 60 {
        TimeAgo::JustNow
    } else if dt < 3600 {
        TimeAgo::Minutes(u64::try_from(dt / 60).unwrap_or(0))
    } else if dt < 86_400 {
        TimeAgo::Hours(u64::try_from(dt / 3600).unwrap_or(0))
    } else if dt < 172_800 {
        TimeAgo::Yesterday
    } else if dt < 2_592_000 {
        TimeAgo::Days(u64::try_from(dt / 86_400).unwrap_or(0))
    } else {
        TimeAgo::Date(secs)
    }
}

/// `just now`, `5 min ago`, `3 h ago`, `yesterday`, `4 days ago`, or the date.
///
/// English-only fallback; translatable UI should match on [`ago_kind`] instead.
pub fn format_ago(secs: i64, now: i64) -> String {
    match ago_kind(secs, now) {
        TimeAgo::JustNow => "just now".into(),
        TimeAgo::Minutes(n) => format!("{n} min ago"),
        TimeAgo::Hours(n) => format!("{n} h ago"),
        TimeAgo::Yesterday => "yesterday".into(),
        TimeAgo::Days(n) => format!("{n} days ago"),
        TimeAgo::Date(s) => format_date(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_dates_people_read() {
        assert_eq!(format_date(1_791_471_170), "Oct 8, 2026");
        assert_eq!(civil_from_unix(951_782_400), (2000, 2, 29));
        // Year 10000 is past jiff's own calendar range, but the 400-year cycle reduction keeps the
        // old conversion (jiff spans only -9999..=9999).
        assert_eq!(civil_from_unix(253_402_300_800), (10000, 1, 1));
        // The `i64` extremes stay the dates the old Hinnant code produced, not the epoch.
        assert_eq!(civil_from_unix(i64::MAX), (292_277_026_596, 12, 4));
        assert_eq!(civil_from_unix(i64::MIN), (-292_277_022_657, 1, 27));
        assert_eq!(format_date(i64::MAX), "Dec 4, 292277026596");
        assert_eq!(format_date(i64::MIN), "Jan 27, -292277022657");
        let now = 1_791_471_170;
        assert_eq!(format_ago(now - 5, now), "just now");
        assert_eq!(format_ago(now - 600, now), "10 min ago");
        assert_eq!(format_ago(now - 7200, now), "2 h ago");
        assert_eq!(format_ago(now - 90_000, now), "yesterday");
        assert_eq!(format_ago(now - 5 * 86_400, now), "5 days ago");
        assert_eq!(format_ago(now - 90 * 86_400, now), "Jul 10, 2026");
    }

    #[test]
    fn ago_kind_matches_the_english_fallback() {
        let now = 1_791_471_170;
        let cases: [(i64, TimeAgo, &str); 7] = [
            (5, TimeAgo::JustNow, "just now"),
            (600, TimeAgo::Minutes(10), "10 min ago"),
            (7200, TimeAgo::Hours(2), "2 h ago"),
            (90_000, TimeAgo::Yesterday, "yesterday"),
            (5 * 86_400, TimeAgo::Days(5), "5 days ago"),
            (90 * 86_400, TimeAgo::Date(now - 90 * 86_400), "Jul 10, 2026"),
            (-30, TimeAgo::JustNow, "just now"),
        ];
        for (delta, kind, english) in cases {
            assert_eq!(ago_kind(now - delta, now), kind, "{english}");
            assert_eq!(format_ago(now - delta, now), english);
        }
        // Boundaries.
        assert_eq!(ago_kind(now - 59, now), TimeAgo::JustNow);
        assert_eq!(ago_kind(now - 60, now), TimeAgo::Minutes(1));
        assert_eq!(ago_kind(now - 3599, now), TimeAgo::Minutes(59));
        assert_eq!(ago_kind(now - 3600, now), TimeAgo::Hours(1));
        assert_eq!(ago_kind(now - 86_399, now), TimeAgo::Hours(23));
        assert_eq!(ago_kind(now - 86_400, now), TimeAgo::Yesterday);
        assert_eq!(ago_kind(now - 172_799, now), TimeAgo::Yesterday);
        assert_eq!(ago_kind(now - 172_800, now), TimeAgo::Days(2));
        assert_eq!(ago_kind(now - 2_591_999, now), TimeAgo::Days(29));
        assert!(matches!(ago_kind(now - 2_592_000, now), TimeAgo::Date(_)));
    }

    #[test]
    fn month_keys_cover_the_calendar() {
        let names = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        for (i, name) in names.iter().enumerate() {
            let m = u32::try_from(i + 1).unwrap_or(1);
            assert_eq!(month_short_en(m), *name);
        }
        assert_eq!(month_short_en(0), "?");
        assert_eq!(month_short_en(13), "?");
    }
}
