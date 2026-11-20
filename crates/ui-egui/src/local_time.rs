//! Dates as the calendar on this computer reads them. GitHub's timestamps are UTC, so a release
//! published late on the 7th in UTC is the 8th in Tokyo: every date the launcher shows goes through
//! [`civil`], which adds the system time zone's offset at that instant (summer time included).
//!
//! Invariants: nothing here panics; when no zone rules can be found at all, dates fall back to UTC.
//! The desktop looks the zone up at most once every [`RECHECK`], whether that worked or not (a
//! date is drawn many times a frame), so a laptop that travels keeps up; the web build asks the
//! browser.

use artcraft_launcher_core::time;

/// `(year, month, day)` of the Unix time `secs` in the system's time zone.
pub(crate) fn civil(secs: i64) -> (i64, u32, u32) {
    civil_at(secs, offset(secs))
}

/// `(year, month, day)` of `secs` seen from `offset` seconds east of UTC.
fn civil_at(secs: i64, offset: i64) -> (i64, u32, u32) {
    time::civil_from_unix(secs.saturating_add(offset))
}

/// How long a looked-up system zone (or the failure to find one) is trusted.
#[cfg(not(target_arch = "wasm32"))]
const RECHECK: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Where the desktop takes its time zone rules from.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Zone {
    /// A zone with an IANA name, from `TZ` or the OS setting; `chrono-tz` holds its rules.
    Named(chrono_tz::Tz),
    /// No usable zone name, so chrono's own `Local` reads the rules: `TZ` as a POSIX rule or a
    /// TZif path, an `/etc/localtime` that is a copied file rather than a link, and UTC only when
    /// it finds none of these.
    Local,
}

/// Seconds east of UTC in the system's time zone at `secs`; 0 when no zone rules can be found.
#[cfg(not(target_arch = "wasm32"))]
fn offset(secs: i64) -> i64 {
    use chrono::{Offset, TimeZone};
    let Some(at) = chrono::DateTime::from_timestamp(secs, 0) else { return 0 };
    let at = at.naive_utc();
    let fixed = match system_zone() {
        Zone::Named(zone) => zone.offset_from_utc_datetime(&at).fix(),
        Zone::Local => chrono::Local.offset_from_utc_datetime(&at).fix(),
    };
    i64::from(fixed.local_minus_utc())
}

/// The system zone: `TZ` when it names an IANA zone, otherwise the zone name the OS is set to as
/// `iana-time-zone` reads it (the `/etc/localtime` link, the registry, `CFTimeZone`; it never
/// reads `TZ` itself), and [`Zone::Local`] when neither gives a name `chrono-tz` knows. Looked up
/// again once [`RECHECK`] has passed, so a laptop that travels keeps up and a failed lookup is not
/// repeated on every date of every frame.
#[cfg(not(target_arch = "wasm32"))]
fn system_zone() -> Zone {
    use std::sync::{Mutex, PoisonError};
    use std::time::Instant;
    type Cached = Option<(Instant, Zone)>;
    static CACHE: Mutex<Cached> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, zone)) = cache.as_ref()
        && at.elapsed() < RECHECK
    {
        return *zone;
    }
    let zone = pick_zone(std::env::var("TZ").ok().as_deref(), || iana_time_zone::get_timezone().map_err(|e| e.to_string()));
    *cache = Some((Instant::now(), zone));
    zone
}

/// One uncached choice of zone from a `TZ` value and the OS zone name (asked for only when `TZ`
/// names no zone).
#[cfg(not(target_arch = "wasm32"))]
fn pick_zone(tz: Option<&str>, os_zone_name: impl FnOnce() -> Result<String, String>) -> Zone {
    if let Some(zone) = zone_from_tz(tz) {
        return Zone::Named(zone);
    }
    match os_zone_name().and_then(|name| name.parse::<chrono_tz::Tz>().map_err(|e| format!("`{name}`: {e}"))) {
        Ok(zone) => Zone::Named(zone),
        Err(e) => {
            log::debug!("couldn't name the system time zone ({e}); letting chrono read its rules, UTC if there are none");
            Zone::Local
        }
    }
}

/// The zone a `TZ` value names, when it is an IANA zone name (`Asia/Tokyo`, or `:Asia/Tokyo` in
/// the POSIX spelling). `None` when `TZ` is unset or empty, a POSIX rule string
/// (`EST5EDT,M3.2.0,M11.1.0`) or a file path, so the zone the OS is set to is used instead.
#[cfg(not(target_arch = "wasm32"))]
fn zone_from_tz(tz: Option<&str>) -> Option<chrono_tz::Tz> {
    let tz = tz?;
    tz.strip_prefix(':').unwrap_or(tz).parse().ok()
}

/// Seconds east of UTC in the browser's time zone at `secs`; 0 when it gives nothing sensible.
#[cfg(target_arch = "wasm32")]
fn offset(secs: i64) -> i64 {
    // Date's offset is in minutes *west* of UTC. Real zones are within a day of it.
    let at = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(secs as f64 * 1000.0));
    let west = at.get_timezone_offset();
    if west.is_finite() && west.abs() <= 24.0 * 60.0 { -(west.round() as i64) * 60 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::civil_at;

    #[test]
    fn a_date_follows_the_zone_it_is_read_in() {
        // 2026-10-07 22:30 UTC.
        let at = 1_791_412_200;
        assert_eq!(civil_at(at, 0), (2026, 10, 7));
        // Tokyo, UTC+9: already the 8th.
        assert_eq!(civil_at(at, 9 * 3600), (2026, 10, 8));
        // New York in summer, UTC−4: still the 7th.
        assert_eq!(civil_at(at, -4 * 3600), (2026, 10, 7));
        // Just after midnight UTC on the 8th, Los Angeles (UTC−7) reads the 7th.
        assert_eq!(civil_at(1_791_417_660, -7 * 3600), (2026, 10, 7));
        // Out-of-range offsets saturate rather than overflow.
        assert_eq!(civil_at(i64::MAX, 3600), civil_at(i64::MAX, 0));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn tz_wins_when_it_names_a_zone() {
        use super::zone_from_tz;
        use chrono_tz::Tz;
        assert_eq!(zone_from_tz(Some("Asia/Tokyo")), Some(Tz::Asia__Tokyo));
        // POSIX lets `TZ` start with a colon.
        assert_eq!(zone_from_tz(Some(":America/Chicago")), Some(Tz::America__Chicago));
        // A rule string, a path or an empty value leaves it to the zone the OS is set to.
        assert_eq!(zone_from_tz(Some("EST5EDT,M3.2.0,M11.1.0")), None);
        assert_eq!(zone_from_tz(Some("/usr/share/zoneinfo/Asia/Tokyo")), None);
        assert_eq!(zone_from_tz(Some("")), None);
        assert_eq!(zone_from_tz(None), None);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_zone_the_os_cannot_name_is_read_by_chrono_not_shown_as_utc() {
        use super::{Zone, pick_zone};
        use chrono_tz::Tz;
        // A copied `/etc/localtime` with no `/etc/timezone` beside it: rules, but no name.
        assert_eq!(pick_zone(None, || Err("no zone name".into())), Zone::Local);
        // A name chrono-tz doesn't know is no better than none.
        assert_eq!(pick_zone(None, || Ok("Mars/Olympus".into())), Zone::Local);
        // A named zone stays named, and a `TZ` that names one still wins.
        assert_eq!(pick_zone(None, || Ok("Asia/Tokyo".into())), Zone::Named(Tz::Asia__Tokyo));
        assert_eq!(pick_zone(Some("America/Chicago"), || Ok("Asia/Tokyo".into())), Zone::Named(Tz::America__Chicago));
    }
}
