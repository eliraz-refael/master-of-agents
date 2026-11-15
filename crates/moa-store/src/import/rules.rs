//! The general import rules G1 to G6 as small pure functions. Each returns the value to
//! store and, when the input was odd, a fixed reason to warn with. Every table importer is
//! built from these.

use std::str::FromStr;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use moa_core::UtcMs;
use moa_core::time::{parse_date, spike_local_to_utc};

use super::report::*;

pub type Ruled<T> = (T, Option<&'static str>);

fn blank(text: Option<&str>) -> Option<&str> {
    text.filter(|t| !t.trim().is_empty())
}

/// G1, nullable column: NULL or empty is none, with no note; unparsable is none with a warning.
pub fn instant(text: Option<&str>, tz: &TimeZone) -> Ruled<Option<UtcMs>> {
    match blank(text) {
        None => (None, None),
        Some(t) => match spike_local_to_utc(t, tz) {
            Ok(ms) => (Some(ms), None),
            Err(_) => (None, Some(R_INSTANT_BAD)),
        },
    }
}

/// G1, `NOT NULL` column: missing is `fallback` with no note; unparsable is `fallback` with a warning.
pub fn instant_or(text: Option<&str>, tz: &TimeZone, fallback: UtcMs) -> Ruled<UtcMs> {
    match blank(text) {
        None => (fallback, None),
        Some(t) => match spike_local_to_utc(t, tz) {
            Ok(ms) => (ms, None),
            Err(_) => (fallback, Some(R_INSTANT_BAD_NOW)),
        },
    }
}

/// G2: NULL or empty is none with no note; unparsable is none with a warning.
pub fn date(text: Option<&str>) -> Ruled<Option<Date>> {
    match blank(text) {
        None => (None, None),
        Some(t) => match parse_date(t) {
            Ok(d) => (Some(d), None),
            Err(_) => (None, Some(R_DATE_BAD)),
        },
    }
}

/// G3: a missing column or NULL is `default` with no note; an unknown value is `default` with
/// a warning (`reason`, which names the column).
pub fn closed<T: FromStr>(text: Option<&str>, default: T, reason: &'static str) -> Ruled<T> {
    match text {
        None => (default, None),
        Some(t) => match t.parse() {
            Ok(v) => (v, None),
            Err(_) => (default, Some(reason)),
        },
    }
}

/// G4: a reference to something that exists is kept; one that doesn't is none with a warning.
pub fn reference(id: Option<i64>, exists: impl Fn(i64) -> bool) -> Ruled<Option<i64>> {
    match id {
        None => (None, None),
        Some(i) if exists(i) => (Some(i), None),
        Some(_) => (None, Some(R_REF_GONE)),
    }
}

/// G5: NULL in a `NOT NULL` text column is empty with a warning.
pub fn text_not_null(text: Option<String>) -> Ruled<String> {
    match text {
        Some(t) => (t, None),
        None => (String::new(), Some(R_TEXT_NULL)),
    }
}

/// Trims `/` and drops empty segments (`A//B/` becomes `A/B`), warning when that changed the path.
pub fn group_path(raw: &str) -> Ruled<String> {
    let clean = raw
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    let note = (clean != raw).then_some(R_GROUP_PATH_FIXED);
    (clean, note)
}

#[cfg(test)]
mod tests {
    use super::*;
    use moa_core::model::{ActivityWho, ProjectStatus};

    fn tz() -> TimeZone {
        TimeZone::posix("UTC0").unwrap()
    }

    #[test]
    fn g1_instants() {
        let t = tz();
        assert_eq!(instant(None, &t), (None, None));
        assert_eq!(instant(Some(""), &t), (None, None));
        assert_eq!(
            instant(Some("1970-01-01 00:00:01"), &t),
            (Some(UtcMs(1000)), None)
        );
        assert_eq!(instant(Some("soon"), &t), (None, Some(R_INSTANT_BAD)));
        assert_eq!(instant_or(None, &t, UtcMs(7)), (UtcMs(7), None));
        assert_eq!(instant_or(Some(" "), &t, UtcMs(7)), (UtcMs(7), None));
        assert_eq!(
            instant_or(Some("1970-01-01 00:00:02"), &t, UtcMs(7)),
            (UtcMs(2000), None)
        );
        assert_eq!(
            instant_or(Some("soon"), &t, UtcMs(7)),
            (UtcMs(7), Some(R_INSTANT_BAD_NOW))
        );
    }

    #[test]
    fn g2_dates() {
        let d = jiff::civil::date(2026, 11, 15);
        assert_eq!(date(None), (None, None));
        assert_eq!(date(Some("")), (None, None));
        assert_eq!(date(Some("2026-11-15")), (Some(d), None));
        assert_eq!(date(Some("2026-11-15 10:00:00")), (Some(d), None));
        assert_eq!(date(Some("tomorrow")), (None, Some(R_DATE_BAD)));
    }

    #[test]
    fn g3_closed_enums() {
        let d = ProjectStatus::Active;
        assert_eq!(closed(None, d, R_PROJECT_STATUS), (d, None));
        assert_eq!(
            closed(Some("paused"), d, R_PROJECT_STATUS),
            (ProjectStatus::Paused, None)
        );
        assert_eq!(
            closed(Some("someday"), d, R_PROJECT_STATUS),
            (d, Some(R_PROJECT_STATUS))
        );
        assert_eq!(
            closed(Some("bot"), ActivityWho::Brain, R_ACTIVITY_WHO),
            (ActivityWho::Brain, Some(R_ACTIVITY_WHO))
        );
    }

    #[test]
    fn g4_references() {
        let has = |i: i64| i == 1;
        assert_eq!(reference(None, has), (None, None));
        assert_eq!(reference(Some(1), has), (Some(1), None));
        assert_eq!(reference(Some(2), has), (None, Some(R_REF_GONE)));
    }

    #[test]
    fn g5_text() {
        assert_eq!(text_not_null(Some("x".into())), ("x".into(), None));
        assert_eq!(text_not_null(Some(String::new())), (String::new(), None));
        assert_eq!(text_not_null(None), (String::new(), Some(R_TEXT_NULL)));
    }

    #[test]
    fn group_paths_are_cleaned() {
        assert_eq!(group_path("A/B"), ("A/B".into(), None));
        for raw in ["A/B/", "/A/B", "A//B", "//A///B//"] {
            assert_eq!(group_path(raw), ("A/B".into(), Some(R_GROUP_PATH_FIXED)));
        }
        assert_eq!(group_path("/"), (String::new(), Some(R_GROUP_PATH_FIXED)));
    }

    #[test]
    fn no_reason_is_built_from_data() {
        // Reasons are consts; the type is &'static str, so this only documents the intent.
        for r in [
            R_INSTANT_BAD,
            R_DATE_BAD,
            R_ENUM_BAD,
            R_REF_GONE,
            R_TEXT_NULL,
        ] {
            assert!(!r.contains('{'));
        }
    }
}
