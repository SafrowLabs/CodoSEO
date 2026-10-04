//! Display formatting shared by every screen: thousands separators, "12m ago", durations.

use time::{Duration, OffsetDateTime};

/// `1284` -> `1,284`
pub fn thousands(n: impl Into<i64>) -> String {
    let n: i64 = n.into();
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A short "how long ago": `just now`, `12m ago`, `3h ago`, `2d ago`, then a date.
pub fn ago(at: OffsetDateTime) -> String {
    ago_from(at, OffsetDateTime::now_utc())
}

pub fn ago_from(at: OffsetDateTime, now: OffsetDateTime) -> String {
    let secs = (now - at).whole_seconds();
    match secs {
        s if s < 45 => "just now".to_owned(),
        s if s < 3600 => format!("{}m ago", (s + 30) / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s if s < 7 * 86_400 => format!("{}d ago", s / 86_400),
        _ => date(at),
    }
}

/// `Oct 3`
pub fn date(at: OffsetDateTime) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!("{} {}", MONTHS[at.month() as usize - 1], at.day())
}

/// `Oct 3, 14:05 UTC`
pub fn datetime(at: OffsetDateTime) -> String {
    format!("{}, {:02}:{:02} UTC", date(at), at.hour(), at.minute())
}

/// `42s`, `1m 42s`, `2h 05m`
pub fn duration(d: Duration) -> String {
    let s = d.whole_seconds().max(0);
    match s {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// `duration` for a millisecond count.
pub fn millis(ms: u64) -> String {
    duration(Duration::milliseconds(ms.min(i64::MAX as u64) as i64))
}

/// One decimal place percentage of `part / whole`: `90.2`.
pub fn pct1(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "0".to_owned();
    }
    let tenths = (part * 1000 + whole / 2) / whole;
    format!("{}.{}", tenths / 10, tenths % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1284), "1,284");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(thousands(-50_000), "-50,000");
    }

    #[test]
    fn ago_buckets() {
        let now = OffsetDateTime::UNIX_EPOCH + Duration::days(400);
        assert_eq!(ago_from(now - Duration::seconds(10), now), "just now");
        assert_eq!(ago_from(now - Duration::minutes(12), now), "12m ago");
        assert_eq!(ago_from(now - Duration::hours(3), now), "3h ago");
        assert_eq!(ago_from(now - Duration::days(2), now), "2d ago");
        assert!(ago_from(now - Duration::days(30), now).contains(' '));
    }

    #[test]
    fn durations() {
        assert_eq!(duration(Duration::seconds(42)), "42s");
        assert_eq!(duration(Duration::seconds(102)), "1m 42s");
        assert_eq!(duration(Duration::seconds(7500)), "2h 05m");
        assert_eq!(pct1(1158, 1284), "90.2");
        assert_eq!(pct1(1, 0), "0");
    }
}
