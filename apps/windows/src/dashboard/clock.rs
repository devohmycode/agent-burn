//! Local wall-clock time from the Windows time zone settings.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::Win32::{
    Foundation::{FILETIME, SYSTEMTIME},
    System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime},
};

/// 100-nanosecond intervals between 1601-01-01 and 1970-01-01.
const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `time` as `HH:MM:SS` in the user's time zone.
pub fn local_time(time: SystemTime) -> String {
    local(time).map_or_else(
        || "unknown time".into(),
        |local| {
            format!(
                "{:02}:{:02}:{:02}",
                local.wHour, local.wMinute, local.wSecond
            )
        },
    )
}

/// `time` as `Oct 14, 10:42` in the user's time zone.
pub fn local_date_time(time: SystemTime) -> String {
    local(time).map_or_else(
        || "unknown time".into(),
        |local| {
            format!(
                "{} {}, {:02}:{:02}",
                MONTHS[usize::from(local.wMonth.clamp(1, 12)) - 1],
                local.wDay,
                local.wHour,
                local.wMinute
            )
        },
    )
}

/// The start of the local day of `time`, in milliseconds since the Unix
/// epoch.
pub fn local_midnight_ms(time: SystemTime) -> Option<i64> {
    let local = local(time)?;
    let since_midnight = i64::from(local.wHour) * 3_600_000
        + i64::from(local.wMinute) * 60_000
        + i64::from(local.wSecond) * 1000
        + i64::from(local.wMilliseconds);
    let now = i64::try_from(time.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()?;
    Some(now - since_midnight)
}

/// Milliseconds since the Unix epoch as a `SystemTime`.
pub fn from_millis(millis: i64) -> Option<SystemTime> {
    let millis = u64::try_from(millis).ok()?;
    UNIX_EPOCH.checked_add(Duration::from_millis(millis))
}

fn local(time: SystemTime) -> Option<SYSTEMTIME> {
    let ticks = UNIX_EPOCH_AS_FILETIME
        + u64::try_from(time.duration_since(UNIX_EPOCH).ok()?.as_nanos() / 100).ok()?;
    let file_time = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    unsafe {
        FileTimeToSystemTime(&file_time, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    Some(local)
}
