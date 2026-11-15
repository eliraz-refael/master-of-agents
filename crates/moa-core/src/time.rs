//! Time conversion. v1 stores instants as UTC milliseconds; the spike stored local-time text.

use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use jiff::tz::{AmbiguousOffset, TimeZone};
use serde::{Deserialize, Serialize};

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
    // The whole-tail scan is deliberate: a spike time never contains -, +, Z or [ after the date.
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
    let ts = Timestamp::from_millisecond(t.0).unwrap_or(if t.0 < 0 {
        Timestamp::MIN
    } else {
        Timestamp::MAX
    });
    tz.to_datetime(ts).strftime("%Y-%m-%d %H:%M:%S").to_string()
}

/// `YYYY-MM-DD`, or the date part of a timestamp text.
pub fn parse_date(text: &str) -> Result<Date, TimeError> {
    let text = text.trim();
    let head = text.get(..10).ok_or(TimeError::Unparsable)?;
    if !text[10..].is_empty() && !text[10..].starts_with([' ', 'T']) {
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
