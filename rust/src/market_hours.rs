//! Market-hours calendars shared by the nx-rates seat and the keepers guard (broker-attest spec N10): one copy, pinned by
//! golden vectors on both sides. Pure: the clock comes in.
//! - [`weekend_closed`]: THE closure rule for signing and attest judgement (owner 11-12: broker classes are priced and
//!   signed through every weekday, daily breaks included; only the weekend is closed).
//! - [`us_equity_session_open`]: the NYSE regular session, a wire LABEL only (`session_closed` exclusion reason).

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Timelike, Utc, Weekday};
use mitch::common::AssetClass;

/// New York local time (US DST rule) of a UTC instant.
fn new_york(now: DateTime<Utc>) -> DateTime<Utc> {
    let sunday = |m: u32, n: u32| {
        let first = NaiveDate::from_ymd_opt(now.year(), m, 1).expect("valid month");
        let off = (7 - first.weekday().num_days_from_sunday()) % 7;
        first + chrono::Days::new(u64::from(off + 7 * (n - 1)))
    };
    let dst_on = sunday(3, 2).and_hms_opt(7, 0, 0).expect("valid").and_utc();
    let dst_off = sunday(11, 1).and_hms_opt(6, 0, 0).expect("valid").and_utc();
    now - chrono::Duration::hours(if now >= dst_on && now < dst_off { 4 } else { 5 })
}

/// Weekend window of a class in New York local minutes: (Friday close, Sunday reopen). `None` = never closed (crypto
/// only); every class without its own row takes the FX window.
// ponytail: one window per class, exchange holidays not modelled (a holiday reads open); upgrade = a per-class holiday
// list in the same table, pinned by the same golden vector.
fn weekend_window(class: AssetClass) -> Option<(u32, u32)> {
    match class {
        AssetClass::FX => Some((17 * 60, 17 * 60)),
        AssetClass::CM => Some((17 * 60, 18 * 60)),
        AssetClass::EQ | AssetClass::IP => Some((16 * 60, 18 * 60)),
        AssetClass::CR => None,
        _ => Some((17 * 60, 17 * 60)),
    }
}

/// A class is in its weekend closure: from Friday close to Sunday reopen, New York time. Weekday daily breaks are never
/// closed. An unrepresentable instant reads open.
pub fn weekend_closed(class: AssetClass, now_ms: i64) -> bool {
    let (Some((close, reopen)), Some(now)) = (weekend_window(class), Utc.timestamp_millis_opt(now_ms).single()) else {
        return false;
    };
    let ny = new_york(now);
    let mins = ny.hour() * 60 + ny.minute();
    match ny.weekday() {
        Weekday::Fri => mins >= close,
        Weekday::Sat => true,
        Weekday::Sun => mins < reopen,
        _ => false,
    }
}

/// US regular equity session: NYSE 09:30-16:00 New York, Mon-Fri, US DST rule. Exchange holidays are not modelled: a
/// holiday reads open. An unrepresentable instant reads open.
pub fn us_equity_session_open(now_ms: i64) -> bool {
    let Some(now) = Utc.timestamp_millis_opt(now_ms).single() else {
        return true;
    };
    let ny = new_york(now);
    let mins = ny.hour() * 60 + ny.minute();
    !matches!(ny.weekday(), Weekday::Sat | Weekday::Sun) && (9 * 60 + 30..16 * 60).contains(&mins)
}

/// Golden vector: (UTC instant, open). The same table is pinned in keepers; a change here re-pins both sides.
pub const GOLDEN: &[(&str, bool)] = &[
    ("2026-11-02T14:00:00Z", false), // 09:00 EST
    ("2026-11-02T14:30:00Z", true),  // 09:30 EST
    ("2026-11-02T20:59:00Z", true),
    ("2026-11-02T21:00:00Z", false), // 16:00 EST
    ("2026-07-06T13:30:00Z", true),  // 09:30 EDT
    ("2026-07-06T13:29:00Z", false),
    ("2026-07-06T20:00:00Z", false), // 16:00 EDT
    ("2026-03-08T07:00:00Z", false), // DST starts, Sunday
    ("2026-03-09T13:30:00Z", true),  // first EDT Monday
    ("2026-11-01T06:00:00Z", false), // DST ends, Sunday
    ("2026-11-07T16:00:00Z", false), // Saturday
    ("2026-11-08T16:00:00Z", false), // Sunday
    ("2026-11-06T20:59:59Z", true),  // Friday close - 1 s
];

/// Golden vector: (class, UTC instant, closed). The same table is pinned in keepers.
pub const GOLDEN_WEEKEND: &[(AssetClass, &str, bool)] = &[
    (AssetClass::FX, "2026-11-06T21:59:00Z", false), // Fri 16:59 EST
    (AssetClass::FX, "2026-11-06T22:00:00Z", true),  // Fri 17:00 EST
    (AssetClass::FX, "2026-11-08T21:59:00Z", true),  // Sun 16:59 EST
    (AssetClass::FX, "2026-11-08T22:00:00Z", false), // Sun 17:00 EST
    (AssetClass::FX, "2026-07-10T21:00:00Z", true),  // Fri 17:00 EDT
    (AssetClass::FX, "2026-07-12T21:00:00Z", false), // Sun 17:00 EDT
    (AssetClass::CM, "2026-11-08T22:30:00Z", true),  // Sun 17:30 EST: metals reopen 18:00
    (AssetClass::CM, "2026-11-08T23:00:00Z", false),
    (AssetClass::CM, "2026-11-04T22:30:00Z", false), // Wed daily break: never closed
    (AssetClass::EQ, "2026-11-06T21:00:00Z", true),  // Fri 16:00 EST
    (AssetClass::EQ, "2026-11-06T20:59:00Z", false),
    (AssetClass::EQ, "2026-11-09T03:00:00Z", false), // Sun 22:00 EST: open
    (AssetClass::EQ, "2026-11-03T02:00:00Z", false), // Mon 21:00 EST, after hours: open (weekday)
    (AssetClass::IP, "2026-11-07T12:00:00Z", true),  // Saturday
    (AssetClass::CR, "2026-11-07T12:00:00Z", false), // crypto never closed
    (AssetClass::SD, "2026-11-07T12:00:00Z", true),  // no own row: the FX window
    (AssetClass::SD, "2026-11-06T21:59:00Z", false),
    // DST starts Sun 2026-03-08 07:00Z: Friday is still EST, Sunday's reopen is EDT
    (AssetClass::FX, "2026-03-06T21:59:00Z", false), // Fri 16:59 EST
    (AssetClass::FX, "2026-03-06T22:00:00Z", true),  // Fri 17:00 EST
    (AssetClass::FX, "2026-03-08T20:59:00Z", true),  // Sun 16:59 EDT
    (AssetClass::FX, "2026-03-08T21:00:00Z", false), // Sun 17:00 EDT
    (AssetClass::CM, "2026-03-08T21:59:00Z", true),  // Sun 17:59 EDT
    (AssetClass::CM, "2026-03-08T22:00:00Z", false), // Sun 18:00 EDT
    (AssetClass::EQ, "2026-03-06T20:59:00Z", false), // Fri 15:59 EST
    (AssetClass::EQ, "2026-03-06T21:00:00Z", true),  // Fri 16:00 EST
    (AssetClass::EQ, "2026-03-08T22:00:00Z", false), // Sun 18:00 EDT
    // DST ends Sun 2026-11-01 06:00Z: Friday is EDT, Sunday's reopen is EST
    (AssetClass::FX, "2026-10-30T20:59:00Z", false), // Fri 16:59 EDT
    (AssetClass::FX, "2026-10-30T21:00:00Z", true),  // Fri 17:00 EDT
    (AssetClass::FX, "2026-11-01T21:59:00Z", true),  // Sun 16:59 EST
    (AssetClass::FX, "2026-11-01T22:00:00Z", false), // Sun 17:00 EST
    (AssetClass::CM, "2026-11-01T22:59:00Z", true),  // Sun 17:59 EST
    (AssetClass::CM, "2026-11-01T23:00:00Z", false), // Sun 18:00 EST
    (AssetClass::EQ, "2026-10-30T19:59:00Z", false), // Fri 15:59 EDT
    (AssetClass::EQ, "2026-10-30T20:00:00Z", true),  // Fri 16:00 EDT
    (AssetClass::EQ, "2026-11-01T23:00:00Z", false), // Sun 18:00 EST
    // New York midnight edges
    (AssetClass::FX, "2026-11-07T05:00:00Z", true),  // Sat 00:00 EST
    (AssetClass::FX, "2026-11-07T04:59:00Z", true),  // Fri 23:59 EST
    (AssetClass::FX, "2026-11-09T05:00:00Z", false), // Mon 00:00 EST
    (AssetClass::EQ, "2026-11-09T04:59:00Z", false), // Sun 23:59 EST
    (AssetClass::EQ, "2026-11-09T05:00:00Z", false), // Mon 00:00 EST
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_weekend_closed() {
        for (class, at, closed) in GOLDEN_WEEKEND {
            let ms = chrono::DateTime::parse_from_rfc3339(at).unwrap().timestamp_millis();
            assert_eq!(weekend_closed(*class, ms), *closed, "{class:?} {at}");
        }
        assert!(!weekend_closed(AssetClass::FX, i64::MAX), "unrepresentable reads open");
    }

    #[test]
    fn golden_us_equity_session() {
        for (at, open) in GOLDEN {
            let ms = chrono::DateTime::parse_from_rfc3339(at).unwrap().timestamp_millis();
            assert_eq!(us_equity_session_open(ms), *open, "{at}");
        }
        assert!(us_equity_session_open(i64::MAX), "unrepresentable reads open");
    }
}
