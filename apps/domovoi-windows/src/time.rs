//! The time of day on this machine, for when a turn ran.

use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

/// Between 1601 (where Windows counts from) and 1970, in milliseconds.
const EPOCH_GAP_MS: i64 = 11_644_473_600_000;

/// `unix_ms` as this machine's clock shows it, `14:02`, with the daylight
/// saving of that day. `None` for no time (0) or one Windows cannot place.
pub fn local_clock(unix_ms: i64) -> Option<String> {
    if unix_ms <= 0 {
        return None;
    }
    let ticks = unix_ms.checked_add(EPOCH_GAP_MS)?.checked_mul(10_000)? as u64;
    let file = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    // SAFETY: plain value conversions into structs that live on this stack.
    let local = unsafe {
        let (mut utc, mut local): (SYSTEMTIME, SYSTEMTIME) = (std::mem::zeroed(), std::mem::zeroed());
        if FileTimeToSystemTime(&raw const file, &raw mut utc) == 0 || SystemTimeToTzSpecificLocalTime(std::ptr::null(), &raw const utc, &raw mut local) == 0 {
            return None;
        }
        local
    };
    Some(format!("{:02}:{:02}", local.wHour, local.wMinute))
}

/// The clock time of a moment given as RFC 3339 (`2026-11-26T14:02:00Z`), as `local_clock` has it.
pub fn clock_of(rfc3339: &str) -> Option<String> {
    local_clock(rfc3339.parse::<jiff::Timestamp>().ok()?.as_millisecond())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_reads_as_hours_and_minutes_and_none_is_none() {
        assert_eq!(local_clock(0), None);
        assert_eq!(clock_of("2023-11-14T22:13:20Z"), local_clock(1_700_000_000_000));
        assert_eq!(clock_of("soon"), None);
        let clock = local_clock(1_700_000_000_000).expect("a time in 2023");
        assert_eq!(clock.len(), 5);
        assert_eq!(clock.as_bytes()[2], b':');
        // Two minutes on, the minutes are two on (whatever the time zone).
        let later = local_clock(1_700_000_120_000).expect("two minutes later");
        let minutes = |c: &str| c[3..].parse::<u32>().expect("minutes");
        assert_eq!((minutes(&clock) + 2) % 60, minutes(&later));
    }
}
