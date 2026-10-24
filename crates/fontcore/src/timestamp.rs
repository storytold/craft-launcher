use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// An RFC 3339 date-time such as `2026-10-22T02:30:00Z`.
///
/// Parsing is strict: `T` and `Z` must be uppercase, an explicit `Z` or
/// `±HH:MM` offset is required, and fractional seconds are allowed. The
/// original text is kept so a manifest round-trips unchanged. FontCore
/// itself writes whole seconds in UTC.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Timestamp {
    text: String,
    unix: i64,
}

impl Timestamp {
    /// The current time in UTC, in whole seconds.
    pub fn now() -> Self {
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        Self::from_unix(secs)
    }

    /// Formats whole seconds since the Unix epoch as UTC.
    pub fn from_unix(secs: i64) -> Self {
        let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
        let (y, m, d) = civil_from_days(days);
        let text = format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60);
        Self { text, unix: secs }
    }

    /// Parses a strict RFC 3339 date-time, keeping `text` exactly as written.
    pub fn parse(text: &str) -> Result<Self> {
        parse_unix(text).map(|unix| Self { text: text.to_owned(), unix }).ok_or_else(|| Error::InvalidTimestamp(text.to_owned()))
    }

    /// The timestamp as written in the manifest.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Whole seconds since the Unix epoch; fractional seconds are dropped.
    pub fn unix_seconds(&self) -> i64 {
        self.unix
    }
}

impl TryFrom<String> for Timestamp {
    type Error = Error;

    fn try_from(text: String) -> Result<Self> {
        Self::parse(&text)
    }
}

impl From<Timestamp> for String {
    fn from(t: Timestamp) -> Self {
        t.text
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// `YYYY-MM-DDTHH:MM:SS[.fraction](Z|±HH:MM)`
fn parse_unix(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    let num = |at: usize, len: usize| -> Option<i64> {
        let digits = b.get(at..at + len)?;
        digits.iter().all(u8::is_ascii_digit).then(|| digits.iter().fold(0, |n, d| n * 10 + i64::from(d - b'0')))
    };
    let seps = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
    if !seps.iter().all(|&(i, c)| b.get(i) == Some(&c)) {
        return None;
    }
    let (year, month, day) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (hour, minute, second) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);

    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return None;
        }
    }
    let offset = match &b[i..] {
        b"Z" => 0,
        [sign @ (b'+' | b'-'), _, _, b':', _, _] => {
            let (oh, om) = (num(i + 1, 2)?, num(i + 4, 2)?);
            if oh > 23 || om > 59 {
                return None;
            }
            let secs = oh * 3600 + om * 60;
            if *sign == b'-' { -secs } else { secs }
        }
        _ => return None,
    };

    let valid = (1..=12).contains(&month) && (1..=days_in_month(year, month)).contains(&day) && hour <= 23 && minute <= 59 && second <= 60; // 60 allows a leap second
    valid.then(|| days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

// Proleptic Gregorian day counts, from Howard Hinnant's `chrono`-compatible
// date algorithms (http://howardhinnant.github.io/date_algorithms.html).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc() {
        assert_eq!(Timestamp::from_unix(0).as_str(), "1970-01-01T00:00:00Z");
        assert_eq!(Timestamp::from_unix(1_792_636_200).as_str(), "2026-10-22T02:30:00Z");
        assert_eq!(Timestamp::from_unix(951_782_400).as_str(), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn parses_and_round_trips() {
        for secs in [0, 1, 951_782_399, 1_792_636_200, 4_102_444_800] {
            let t = Timestamp::from_unix(secs);
            assert_eq!(Timestamp::parse(t.as_str()).unwrap().unix_seconds(), secs);
        }
        let cases = [
            ("2026-10-22T02:30:00Z", 1_792_636_200),
            ("2026-10-22T02:30:00.250Z", 1_792_636_200),
            ("2026-10-22T04:30:00+02:00", 1_792_636_200),
            ("2026-10-21T21:30:00-05:00", 1_792_636_200),
        ];
        for (text, secs) in cases {
            let t = Timestamp::parse(text).unwrap();
            assert_eq!((t.as_str(), t.unix_seconds()), (text, secs));
        }
    }

    #[test]
    fn rejects_non_rfc3339() {
        for bad in [
            "",
            "1792636200",
            "2026-10-22",
            "2026-10-22 02:30:00Z",
            "2026-10-22t02:30:00z",
            "2026-10-22T02:30:00",
            "2026-10-22T02:30:00.Z",
            "2026-10-22T02:30:00+0200",
            "2026-13-01T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2026-10-22T24:00:00Z",
            "2026-10-22T02:30:00+24:00",
            "2026-10-22T02:30:00Z ",
        ] {
            assert!(Timestamp::parse(bad).is_err(), "{bad:?}");
        }
    }
}
