//! Dates as the calendar on this computer reads them. GitHub's timestamps are UTC, so a release
//! published late on the 7th in UTC is the 8th in Tokyo: every date the launcher shows goes through
//! [`civil`], which adds the system time zone's offset at that instant (summer time included).
//!
//! Invariants: nothing here panics; when the system zone can't be read, dates fall back to UTC.
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

/// Seconds east of UTC in the system's time zone at `secs`; 0 when the zone can't be read.
#[cfg(not(target_arch = "wasm32"))]
fn offset(secs: i64) -> i64 {
    use chrono::{Offset, TimeZone};
    let Some(zone) = system_zone() else { return 0 };
    chrono::DateTime::from_timestamp(secs, 0).map_or(0, |at| i64::from(zone.offset_from_utc_datetime(&at.naive_utc()).fix().local_minus_utc()))
}

/// The system zone: `TZ` when it names an IANA zone, otherwise the zone name the OS is set to as
/// `iana-time-zone` reads it (`/etc/localtime`, the registry, `CFTimeZone`; it never reads `TZ`
/// itself), with `chrono-tz` turning the name into rules with summer time. Looked up again once
/// [`RECHECK`] has passed, whether it was found or not, so a failure (a bare container) is not
/// searched for on every date of every frame.
#[cfg(not(target_arch = "wasm32"))]
fn system_zone() -> Option<chrono_tz::Tz> {
    use std::sync::{Mutex, PoisonError};
    use std::time::Instant;
    type Cached = Option<(Instant, Option<chrono_tz::Tz>)>;
    static CACHE: Mutex<Cached> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, zone)) = cache.as_ref()
        && at.elapsed() < RECHECK
    {
        return *zone;
    }
    let zone = read_system_zone().map_err(|e| log::debug!("couldn't read the system time zone, showing dates in UTC: {e}")).ok();
    *cache = Some((Instant::now(), zone));
    zone
}

/// One uncached lookup of the system zone; the error says which step failed.
#[cfg(not(target_arch = "wasm32"))]
fn read_system_zone() -> Result<chrono_tz::Tz, String> {
    if let Some(zone) = zone_from_tz(std::env::var("TZ").ok().as_deref()) {
        return Ok(zone);
    }
    let name = iana_time_zone::get_timezone().map_err(|e| e.to_string())?;
    name.parse::<chrono_tz::Tz>().map_err(|e| format!("`{name}`: {e}"))
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
}
