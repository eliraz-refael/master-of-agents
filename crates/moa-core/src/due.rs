//! `--due` text to a local calendar date. Pure: `today` arrives from `Now.local_day`.

use jiff::Span;
use jiff::civil::{Date, Weekday};

/// A parsed `--due` value: no due date, or a local calendar date.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Due {
    Clear,
    On(Date),
}

/// The text wasn't a due date. Displays the offending text, as the spike's `--due` did.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
#[error("--due: '{0}' isn't YYYY-MM-DD, today, tomorrow, +N or a weekday (fri, friday)")]
pub struct DueError(String);

const WEEKDAYS: [(&str, &str, Weekday); 7] = [
    ("mon", "monday", Weekday::Monday),
    ("tue", "tuesday", Weekday::Tuesday),
    ("wed", "wednesday", Weekday::Wednesday),
    ("thu", "thursday", Weekday::Thursday),
    ("fri", "friday", Weekday::Friday),
    ("sat", "saturday", Weekday::Saturday),
    ("sun", "sunday", Weekday::Sunday),
];

fn plus_days(today: Date, n: i64) -> Option<Date> {
    today.checked_add(Span::new().try_days(n).ok()?).ok()
}

/// `""` clears; `today`, `tomorrow`, `+N`, a weekday (the next one, never today) or `YYYY-MM-DD`.
pub fn parse_due(text: &str, today: Date) -> Result<Due, DueError> {
    let err = || DueError(text.to_string());
    let t = text.trim();
    if t.is_empty() {
        return Ok(Due::Clear);
    }
    let lower = t.to_ascii_lowercase();
    let date = match lower.as_str() {
        "today" => Some(today),
        "tomorrow" => plus_days(today, 1),
        w => {
            if let Some(n) = w.strip_prefix('+') {
                if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(err());
                }
                n.parse::<i64>().ok().and_then(|n| plus_days(today, n))
            } else if let Some((_, _, wd)) = WEEKDAYS.iter().find(|(s, l, _)| w == *s || w == *l) {
                let ahead = (i64::from(wd.to_monday_zero_offset())
                    - i64::from(today.weekday().to_monday_zero_offset()))
                .rem_euclid(7);
                plus_days(today, if ahead == 0 { 7 } else { ahead })
            } else {
                let b = w.as_bytes();
                let shaped = b.len() == 10
                    && b.iter().enumerate().all(|(i, c)| {
                        if i == 4 || i == 7 {
                            *c == b'-'
                        } else {
                            c.is_ascii_digit()
                        }
                    });
                if !shaped {
                    return Err(err());
                }
                w.parse::<Date>().ok()
            }
        }
    };
    date.map(Due::On).ok_or_else(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;
    use proptest::prelude::*;

    fn on(d: i8) -> Due {
        Due::On(date(2026, 11, d))
    }

    // today = 2026-11-15, a Sunday
    #[test]
    fn due_words() {
        let t = date(2026, 11, 15);
        assert_eq!(t.weekday(), Weekday::Sunday);
        for (s, want) in [
            ("", Due::Clear),
            ("today", on(15)),
            ("Tomorrow", on(16)),
            ("+0", on(15)),
            ("+3", on(18)),
            ("fri", on(20)),
            ("Friday", on(20)),
            ("MON", on(16)),
            ("sun", on(22)),
            ("2026-12-01", Due::On(date(2026, 12, 1))),
        ] {
            assert_eq!(parse_due(s, t).unwrap(), want, "{s}");
        }
        assert_eq!(
            parse_due("next week", t).unwrap_err().to_string(),
            "--due: 'next week' isn't YYYY-MM-DD, today, tomorrow, +N or a weekday (fri, friday)"
        );
        for bad in [
            "+x",
            "-1",
            "2026-13-01",
            "fr",
            "+",
            "2026-11-31",
            "+99999999999999999999",
        ] {
            assert!(parse_due(bad, t).is_err(), "{bad}");
        }
    }
    #[test]
    fn every_weekday_name_resolves() {
        let t = date(2026, 11, 15);
        for (i, (s, l, wd)) in WEEKDAYS.iter().enumerate() {
            let Due::On(a) = parse_due(s, t).unwrap() else {
                panic!()
            };
            assert_eq!(a.weekday(), *wd);
            assert_eq!(parse_due(l, t).unwrap(), Due::On(a));
            assert_eq!(a, date(2026, 11, 16 + i as i8));
        }
    }
    #[test]
    fn overflow_is_an_error_not_a_panic() {
        assert!(parse_due("+3650000", date(9999, 12, 31)).is_err());
        assert!(parse_due("tomorrow", date(9999, 12, 31)).is_err());
    }

    proptest! {
        #[test]
        fn plus_n_is_n_days_on(n in 0i64..3650, d in 0i32..3000) {
            let t = date(2000, 1, 1) + Span::new().days(d);
            let want = t + Span::new().days(n);
            prop_assert_eq!(parse_due(&format!("+{n}"), t).unwrap(), Due::On(want));
        }
        #[test]
        fn a_weekday_is_one_to_seven_days_ahead(d in 0i32..3000, w in 0usize..7) {
            let t = date(2000, 1, 1) + Span::new().days(d);
            for name in [WEEKDAYS[w].0, WEEKDAYS[w].1] {
                let Due::On(got) = parse_due(name, t).unwrap() else { panic!() };
                prop_assert_eq!(got.weekday(), WEEKDAYS[w].2);
                let gap = t.until(got).unwrap().get_days();
                prop_assert!((1..=7).contains(&gap), "{}", gap);
            }
        }
        #[test]
        fn never_panics(s in ".*", d in 0i32..3000) {
            let _ = parse_due(&s, date(2000, 1, 1) + Span::new().days(d));
        }
    }
}
