//! Time conversion. v1 stores instants as UTC milliseconds; the spike stored local-time text.

use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use jiff::tz::{AmbiguousOffset, TimeZone};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// An instant: milliseconds since the Unix epoch, UTC.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct UtcMs(pub i64);

/// The text was not a spike timestamp or date. Carries no input, so error text never echoes data.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum TimeError {
    #[error("that isn't a date or time")]
    Unparsable,
}

/// Parse spike local text into a civil date-time. Rejects any offset or zone suffix, which
/// jiff's civil parser would silently drop.
fn parse_civil(text: &str) -> Result<DateTime, TimeError> {
    let text = text.trim();
    // The whole-tail scan is deliberate: a spike time never contains -, +, Z, z or [ after the date.
    // After the `YYYY-MM-DD` date, no sign, `Z` or zone annotation may appear.
    let rest = text.get(10..).unwrap_or("");
    if rest.contains(['+', '-', 'Z', 'z', '[']) {
        return Err(TimeError::Unparsable);
    }
    text.parse::<DateTime>()
        .or_else(|_| {
            text.parse::<Date>()
                .map(|d| d.to_datetime(jiff::civil::time(0, 0, 0, 0)))
        })
        .map_err(|_| TimeError::Unparsable)
}

/// Spike local text (`YYYY-MM-DD HH:MM:SS`, `T` separator, optional fraction, or a date alone
/// meaning local midnight) to a UTC instant. A fold takes the earlier instant; a gap shifts forward.
pub fn spike_local_to_utc(text: &str, tz: &TimeZone) -> Result<UtcMs, TimeError> {
    let dt = parse_civil(text)?;
    let zoned = tz
        .to_ambiguous_zoned(dt)
        .compatible()
        .map_err(|_| TimeError::Unparsable)?;
    Ok(UtcMs(zoned.timestamp().as_millisecond()))
}

/// A UTC instant to spike local text: always `YYYY-MM-DD HH:MM:SS`, milliseconds truncated.
pub fn utc_to_spike_local(t: UtcMs, tz: &TimeZone) -> String {
    tz.to_datetime(timestamp(t))
        .strftime("%Y-%m-%d %H:%M:%S")
        .to_string()
}

fn timestamp(t: UtcMs) -> Timestamp {
    Timestamp::from_millisecond(t.0).unwrap_or(if t.0 < 0 {
        Timestamp::MIN
    } else {
        Timestamp::MAX
    })
}

fn utc_ms(ts: Timestamp) -> UtcMs {
    UtcMs(ts.as_millisecond())
}

/// The moment a command runs: wall clock, monotonic reading, and the brain's zone with the
/// local calendar day. The core reads no clock; callers build this from theirs.
#[derive(Clone, Debug)]
pub struct Now {
    pub wall: UtcMs,
    pub mono: Duration,
    /// The date of `wall` in `tz`, never the UTC date.
    pub local_day: Date,
    pub tz: TimeZone,
}

impl Now {
    pub fn at(wall: UtcMs, mono: Duration, tz: TimeZone) -> Now {
        let local_day = tz.to_datetime(timestamp(wall)).date();
        Now {
            wall,
            mono,
            local_day,
            tz,
        }
    }
}

/// The first instant of `day + 1` in `tz`. A midnight that doesn't exist gives the first
/// instant that does (jiff's start of day).
pub fn start_of_next_day(day: Date, tz: &TimeZone) -> UtcMs {
    let next = day.checked_add(jiff::Span::new().days(1)).unwrap_or(day);
    match next.to_zoned(tz.clone()) {
        Ok(z) => utc_ms(z.timestamp()),
        Err(_) => utc_ms(Timestamp::MAX),
    }
}

/// The same local wall time `days` earlier, computed on civil dates in `now.tz` (so a DST
/// change in between keeps the clock reading, not the elapsed hours).
pub fn days_ago(now: &Now, days: i64) -> UtcMs {
    let saturated = UtcMs(if days > 0 { i64::MIN } else { i64::MAX });
    let time = now.tz.to_datetime(timestamp(now.wall)).time();
    let earlier = jiff::Span::new()
        .try_days(days)
        .and_then(|span| now.local_day.checked_sub(span));
    let Ok(day) = earlier else {
        return saturated;
    };
    match now
        .tz
        .to_ambiguous_zoned(day.to_datetime(time))
        .compatible()
    {
        Ok(z) => utc_ms(z.timestamp()),
        Err(_) => saturated,
    }
}

/// Age of `from` at `now` as the spike printed it: `?`, `{m}m`, `{h}h` or `{d}d`; the future is `0m`.
pub fn age(from: Option<UtcMs>, now: UtcMs) -> String {
    let Some(from) = from else {
        return "?".to_string();
    };
    let ms = now.0.saturating_sub(from.0).max(0);
    let minutes = ms / 60_000;
    if minutes < 60 {
        format!("{minutes}m")
    } else if minutes < 24 * 60 {
        format!("{}h", minutes / 60)
    } else {
        format!("{}d", minutes / (24 * 60))
    }
}

/// `YYYY-MM-DD HH:MM:SS` in `tz`, as the spike printed it.
pub fn local_text(t: UtcMs, tz: &TimeZone) -> String {
    utc_to_spike_local(t, tz)
}

/// `YYYY-MM-DD`, or the date part of a timestamp text.
pub fn parse_date(text: &str) -> Result<Date, TimeError> {
    let text = text.trim();
    let head = text.get(..10).ok_or(TimeError::Unparsable)?;
    // Anything after the date must be a time, as in a spike timestamp.
    if !text[10..].is_empty() && (!text[10..].starts_with([' ', 'T']) || parse_civil(text).is_err())
    {
        return Err(TimeError::Unparsable);
    }
    head.parse::<Date>().map_err(|_| TimeError::Unparsable)
}

/// True when the local text falls in a fold or a gap of `tz`.
pub fn is_ambiguous_local(text: &str, tz: &TimeZone) -> bool {
    match parse_civil(text) {
        Ok(dt) => !matches!(
            tz.to_ambiguous_zoned(dt).offset(),
            AmbiguousOffset::Unambiguous { .. }
        ),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::TimeZone;
    use proptest::prelude::*;

    const ZONES: &[&str] = &[
        "EST5EDT,M3.2.0,M11.1.0",
        "AEST-10AEDT,M10.1.0,M4.1.0/3",
        "LHST-10:30LHDT-11,M10.1.0,M4.1.0",
        "UTC0",
    ];
    fn tz(s: &str) -> TimeZone {
        TimeZone::posix(s).unwrap()
    }
    fn ny() -> TimeZone {
        tz(ZONES[0])
    }
    fn utc(s: &str) -> UtcMs {
        UtcMs(s.parse::<jiff::Timestamp>().unwrap().as_millisecond())
    }

    fn date(y: i16, m: i8, d: i8) -> Date {
        jiff::civil::date(y, m, d)
    }
    fn jerusalem() -> TimeZone {
        tz("IST-2IDT,M3.4.5/2,M10.5.0/2")
    }

    #[test]
    fn now_takes_the_local_day_not_the_utc_one() {
        let n = Now::at(utc("2026-11-16T04:30:00Z"), Duration::ZERO, ny());
        assert_eq!(n.local_day, date(2026, 11, 15));
        // The other direction: 01:30 on the 16th in Jerusalem is still the 15th in UTC.
        let n = Now::at(utc("2026-11-15T23:30:00Z"), Duration::ZERO, jerusalem());
        assert_eq!(n.local_day, date(2026, 11, 16));
        assert_eq!(n.wall, utc("2026-11-15T23:30:00Z"));
    }
    #[test]
    fn start_of_next_day_skips_a_missing_midnight() {
        let brt = tz("BRT3BRST,M10.3.0/0,M2.3.0/0");
        assert_eq!(
            start_of_next_day(date(2026, 10, 17), &brt),
            utc("2026-10-18T03:00:00Z")
        );
        assert_eq!(
            start_of_next_day(date(2026, 11, 15), &ny()),
            utc("2026-11-16T05:00:00Z")
        );
    }
    #[test]
    fn start_of_next_day_across_dst_is_a_23_or_25_hour_day() {
        let start = |d| start_of_next_day(d, &ny()).0;
        // 2026-03-08 is 23 hours long in New York, 2026-11-01 is 25.
        assert_eq!(
            start(date(2026, 3, 8)) - start(date(2026, 3, 7)),
            23 * 3_600_000
        );
        assert_eq!(
            start(date(2026, 11, 1)) - start(date(2026, 10, 31)),
            25 * 3_600_000
        );
    }
    #[test]
    fn ages_like_the_spike() {
        let now = utc("2026-11-15T12:00:00Z");
        assert_eq!(age(Some(UtcMs(now.0 - 59_000)), now), "0m");
        assert_eq!(age(Some(UtcMs(now.0 - 61 * 60_000)), now), "1h");
        assert_eq!(age(Some(UtcMs(now.0 - 25 * 3_600_000)), now), "1d");
        assert_eq!(age(Some(UtcMs(now.0 + 5_000)), now), "0m");
        assert_eq!(age(None, now), "?");
        assert_eq!(age(Some(UtcMs(now.0 - 59 * 60_000)), now), "59m");
        assert_eq!(
            age(Some(UtcMs(now.0 - 23 * 3_600_000 - 59 * 60_000)), now),
            "23h"
        );
        assert_eq!(age(Some(UtcMs(now.0 - 49 * 3_600_000)), now), "2d");
    }
    #[test]
    fn days_ago_keeps_the_local_wall_time_across_dst() {
        // 12:00 EDT on 2026-03-10; seven days earlier is 12:00 EST on 2026-03-03 (5 h offset, not 4).
        let now = Now::at(utc("2026-03-10T16:00:00Z"), Duration::ZERO, ny());
        assert_eq!(days_ago(&now, 7), utc("2026-03-03T17:00:00Z"));
        assert_eq!(days_ago(&now, 0), now.wall);
        // Near midnight: 23:30 on the 15th in New York; one day ago is 23:30 on the 14th.
        let late = Now::at(utc("2026-11-16T04:30:00Z"), Duration::ZERO, ny());
        assert_eq!(days_ago(&late, 1), utc("2026-11-15T04:30:00Z"));
        assert_eq!(local_text(days_ago(&late, 1), &ny()), "2026-11-14 23:30:00");
    }
    #[test]
    fn days_ago_out_of_range_does_not_panic() {
        let now = Now::at(utc("2026-11-15T12:00:00Z"), Duration::ZERO, ny());
        assert_eq!(days_ago(&now, i64::MAX), UtcMs(i64::MIN));
        assert_eq!(days_ago(&now, i64::MIN), UtcMs(i64::MAX));
    }
    #[test]
    fn local_text_matches_the_spike_format() {
        assert_eq!(
            local_text(utc("2026-07-01T16:00:00.900Z"), &ny()),
            "2026-07-01 12:00:00"
        );
    }

    #[test]
    fn summer_and_winter() {
        assert_eq!(
            spike_local_to_utc("2026-07-01 12:00:00", &ny()).unwrap(),
            utc("2026-07-01T16:00:00Z")
        );
        assert_eq!(
            spike_local_to_utc("2026-12-01 12:00:00", &ny()).unwrap(),
            utc("2026-12-01T17:00:00Z")
        );
    }
    #[test]
    fn t_separator_fraction_and_date_only() {
        assert_eq!(
            spike_local_to_utc("2026-07-01T12:00:00.250", &ny()).unwrap(),
            utc("2026-07-01T16:00:00.250Z")
        );
        assert_eq!(
            spike_local_to_utc("2026-07-01", &ny()).unwrap(),
            utc("2026-07-01T04:00:00Z")
        );
    }
    #[test]
    fn fold_takes_the_earlier_instant() {
        assert_eq!(
            spike_local_to_utc("2026-11-01 01:30:00", &ny()).unwrap(),
            utc("2026-11-01T05:30:00Z")
        );
    }
    #[test]
    fn gap_shifts_forward() {
        assert_eq!(
            spike_local_to_utc("2026-03-08 02:30:00", &ny()).unwrap(),
            utc("2026-03-08T07:30:00Z")
        );
    }
    #[test]
    fn garbage_and_offsets_are_errors() {
        for bad in [
            "",
            "next tuesday",
            "2026-07-01 12:00:00+02:00",
            "2026-07-01T12:00:00Z",
            "2026-13-01",
            "2026-07-01T12:00:00-05:00",
            "2026-07-01T12:00:00z",
            "2026-07-01T12:00:00[America/New_York]",
        ] {
            assert!(spike_local_to_utc(bad, &ny()).is_err(), "{bad}");
        }
    }
    #[test]
    fn parse_date_accepts_timestamp_text() {
        assert_eq!(
            parse_date("2026-07-01 12:00:00").unwrap(),
            jiff::civil::date(2026, 7, 1)
        );
    }
    #[test]
    fn parse_date_rejects_trailing_junk() {
        assert!(parse_date("2026-07-01garbage").is_err());
        assert!(parse_date("2026-07-01+02:00").is_err());
        assert!(parse_date("2026-07-01 garbage").is_err());
        assert!(parse_date("2026-07-01T12:00:00+02:00").is_err());
        assert_eq!(
            parse_date("2026-07-01T12:00:00").unwrap(),
            jiff::civil::date(2026, 7, 1)
        );
    }
    #[test]
    fn error_text_never_echoes_input() {
        let e = spike_local_to_utc("secret-title", &ny()).unwrap_err();
        assert!(!format!("{e} {e:?}").contains("secret"));
    }

    proptest! {
        #[test]
        fn local_text_round_trips(ms in 1_600_000_000_000i64..2_000_000_000_000i64, z in 0..ZONES.len()) {
            let zone = tz(ZONES[z]);
            let t = UtcMs(ms);
            let text = utc_to_spike_local(t, &zone);
            let back = spike_local_to_utc(&text, &zone).unwrap();
            prop_assert_eq!(utc_to_spike_local(back, &zone), text.clone());
            prop_assert!(back <= t);
            if !is_ambiguous_local(&text, &zone) {
                prop_assert_eq!(back.0, ms.div_euclid(1000) * 1000);
            }
        }
        #[test]
        fn never_panics(s in ".*", z in 0..ZONES.len()) {
            let _ = spike_local_to_utc(&s, &tz(ZONES[z]));
            let _ = parse_date(&s);
        }
    }
}
