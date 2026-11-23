//! Server timestamps (RFC 3339, UTC) shown in the user's own time zone.

use jiff::{Timestamp, Zoned, tz::TimeZone};

/// `14:00` today, `Mon 14:00` within a week, `12 Mar 14:00` beyond; empty
/// when `rfc3339` doesn't parse.
pub fn local_clock(rfc3339: &str) -> String {
    let Ok(ts) = rfc3339.parse::<Timestamp>() else { return String::new() };
    let tz = TimeZone::system();
    local_clock_in(ts, &Zoned::now().with_time_zone(tz.clone()), &tz)
}

fn local_clock_in(ts: Timestamp, now: &Zoned, tz: &TimeZone) -> String {
    let at = ts.to_zoned(tz.clone());
    let days = (at.date() - now.date()).get_days();
    if days == 0 {
        at.strftime("%H:%M").to_string()
    } else if (-6..=6).contains(&days) {
        at.strftime("%a %H:%M").to_string()
    } else {
        at.strftime("%-d %b %H:%M").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_relative_to_today() {
        let tz = TimeZone::UTC;
        let now = "2026-11-23T09:00:00Z".parse::<Timestamp>().unwrap().to_zoned(tz.clone());
        let at = |s: &str| local_clock_in(s.parse().unwrap(), &now, &tz);
        assert_eq!(at("2026-11-23T14:00:00Z"), "14:00");
        assert_eq!(at("2026-11-24T14:00:00Z"), "Tue 14:00");
        assert_eq!(at("2026-12-12T08:05:00Z"), "12 Dec 08:05");
        assert_eq!(local_clock("not a time"), "");
    }
}
