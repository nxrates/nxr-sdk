//! Market-hours calendar shared by the nx-rates seat and the keepers guard (broker-attest spec N10): one copy, pinned by
//! a golden vector on both sides. Pure: the clock comes in.

use chrono::{Datelike, NaiveDate, TimeZone, Timelike, Utc, Weekday};

/// US regular equity session: NYSE 09:30-16:00 New York, Mon-Fri, US DST rule. Exchange holidays are not modelled: a
/// holiday reads open. An unrepresentable instant reads open.
pub fn us_equity_session_open(now_ms: i64) -> bool {
    let Some(now) = Utc.timestamp_millis_opt(now_ms).single() else {
        return true;
    };
    // n-th Sunday of a month (n 1-based) as a date
    let sunday = |m: u32, n: u32| {
        let first = NaiveDate::from_ymd_opt(now.year(), m, 1).expect("valid month");
        let off = (7 - first.weekday().num_days_from_sunday()) % 7;
        first + chrono::Days::new(u64::from(off + 7 * (n - 1)))
    };
    // DST from the 2nd Sunday of March 02:00 EST (07:00 UTC) to the 1st Sunday of November 02:00 EDT (06:00 UTC)
    let dst_on = sunday(3, 2).and_hms_opt(7, 0, 0).expect("valid").and_utc();
    let dst_off = sunday(11, 1).and_hms_opt(6, 0, 0).expect("valid").and_utc();
    let offset_h = if now >= dst_on && now < dst_off { 4 } else { 5 };
    let ny = now - chrono::Duration::hours(offset_h);
    let mins = ny.hour() * 60 + ny.minute();
    !matches!(ny.weekday(), Weekday::Sat | Weekday::Sun) && (9 * 60 + 30..16 * 60).contains(&mins)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn golden_us_equity_session() {
        for (at, open) in GOLDEN {
            let ms = chrono::DateTime::parse_from_rfc3339(at).unwrap().timestamp_millis();
            assert_eq!(us_equity_session_open(ms), *open, "{at}");
        }
        assert!(us_equity_session_open(i64::MAX), "unrepresentable reads open");
    }
}
