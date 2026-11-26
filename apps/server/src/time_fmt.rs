use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// `IsoDateTime` as t3code emits it: UTC with millisecond precision, e.g. `2026-11-13T15:27:30.705Z`.
pub fn iso(dt: OffsetDateTime) -> String {
    let dt = dt.to_offset(time::UtcOffset::UTC);
    let ms = dt.millisecond();
    let base = dt.replace_nanosecond(0).expect("0 is a valid nanosecond").format(&Rfc3339).expect("a UTC date within RFC 3339 years");
    // Rfc3339 renders `...:30Z`; splice in millis.
    format!("{}.{ms:03}Z", base.trim_end_matches('Z'))
}

pub fn iso_now() -> String {
    iso(OffsetDateTime::now_utc())
}

pub fn now_millis() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

pub fn iso_from_millis(ms: i64) -> String {
    // Out of range (a bad client value): the nearest date that is not.
    let day = |y, m, d| OffsetDateTime::new_utc(time::Date::from_calendar_date(y, m, d).expect("a valid date"), time::Time::MIDNIGHT).unix_timestamp_nanos();
    let nanos = (i128::from(ms) * 1_000_000).clamp(day(0, time::Month::January, 1), day(9999, time::Month::December, 31));
    iso(OffsetDateTime::from_unix_timestamp_nanos(nanos).expect("clamped into range"))
}

/// Unix milliseconds of an RFC 3339 time (None if it doesn't parse).
pub fn millis_from_iso(s: &str) -> Option<i64> {
    OffsetDateTime::parse(s, &Rfc3339).ok().map(|dt| (dt.unix_timestamp_nanos() / 1_000_000) as i64)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_millisecond_time_out_of_range_formats_as_the_nearest_date() {
        assert_eq!(super::iso_from_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(super::iso_from_millis(i64::MAX), "9999-12-31T00:00:00.000Z");
        assert_eq!(super::iso_from_millis(i64::MIN), "0000-01-01T00:00:00.000Z");
    }

    #[test]
    fn formats_like_t3code() {
        assert_eq!(super::iso_from_millis(1794583650705), "2026-11-13T15:27:30.705Z");
        assert_eq!(super::iso_from_millis(1794583650000), "2026-11-13T15:27:30.000Z");
        assert_eq!(super::millis_from_iso("2026-11-13T15:27:30.705Z"), Some(1794583650705));
        assert_eq!(super::millis_from_iso("soon"), None);
    }
}
