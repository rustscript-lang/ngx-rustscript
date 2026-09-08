// OpenResty-compatible time hosts.
//
// OpenResty reads nginx's cached clock (`ngx_time` / `ngx_timeofday`) and
// `ngx.update_time` calls `ngx_time_update`. This host keeps a thread-local
// cached clock snapshot with the same model: `time`/`now`/`today`/`localtime`
// /`utctime` read the cache, and `update_time` performs a real OS clock read.
// It is not a no-op.

use std::cell::Cell;
use std::time::{SystemTime, UNIX_EPOCH};

use pd_host_function::pd_host_function;
use vm::VmError;

use super::{CallOutcome, IntoHostCallOutcome};
use super::super::VmResult;

#[derive(Clone, Copy, Debug)]
struct CachedClock {
    sec: i64,
    msec: u32,
}

thread_local! {
    static CLOCK: Cell<Option<CachedClock>> = const { Cell::new(None) };
}

fn ret<T: IntoHostCallOutcome>(value: T) -> VmResult<CallOutcome> {
    Ok(value.into_host_call_outcome())
}

fn snapshot_os() -> CachedClock {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    CachedClock {
        sec: duration.as_secs() as i64,
        msec: duration.subsec_millis(),
    }
}

fn read_clock() -> CachedClock {
    CLOCK.with(|cell| {
        if let Some(cached) = cell.get() {
            cached
        } else {
            let now = snapshot_os();
            cell.set(Some(now));
            now
        }
    })
}

fn refresh_clock() -> CachedClock {
    let now = snapshot_os();
    CLOCK.with(|cell| cell.set(Some(now)));
    now
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

struct Civil {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    min: u32,
    sec: u32,
    wday: u32,
}

fn broken_down(sec: i64, utc: bool) -> VmResult<Civil> {
    let tt = sec as libc::time_t;
    let mut tm = unsafe { std::mem::zeroed::<libc::tm>() };
    let ptr = if utc {
        unsafe { libc::gmtime_r(&tt, &mut tm) }
    } else {
        unsafe { libc::localtime_r(&tt, &mut tm) }
    };
    if ptr.is_null() {
        return Err(VmError::HostError(
            "ngx time conversion failed".to_string(),
        ));
    }
    Ok(Civil {
        year: tm.tm_year + 1900,
        month: u32::try_from(tm.tm_mon + 1).unwrap_or(1),
        day: u32::try_from(tm.tm_mday).unwrap_or(1),
        hour: u32::try_from(tm.tm_hour).unwrap_or(0),
        min: u32::try_from(tm.tm_min).unwrap_or(0),
        sec: u32::try_from(tm.tm_sec).unwrap_or(0),
        wday: u32::try_from(tm.tm_wday).unwrap_or(0),
    })
}

fn format_civil_date(civil: &Civil) -> String {
    format!("{:04}-{:02}-{:02}", civil.year, civil.month, civil.day)
}

fn format_civil_datetime(civil: &Civil) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        civil.year, civil.month, civil.day, civil.hour, civil.min, civil.sec
    )
}

fn http_date(sec: i64, cookie: bool) -> VmResult<String> {
    let civil = broken_down(sec, true)?;
    let wday = WEEKDAYS[civil.wday as usize % 7];
    let month = MONTHS[(civil.month.saturating_sub(1) as usize) % 12];
    if cookie {
        Ok(format!(
            "{wday}, {:02}-{month}-{:02} {:02}:{:02}:{:02} GMT",
            civil.day,
            civil.year.rem_euclid(100),
            civil.hour,
            civil.min,
            civil.sec
        ))
    } else {
        Ok(format!(
            "{wday}, {:02} {month} {} {:02}:{:02}:{:02} GMT",
            civil.day, civil.year, civil.hour, civil.min, civil.sec
        ))
    }
}

fn month_from_name(name: &str) -> Option<u32> {
    MONTHS
        .iter()
        .position(|month| month.eq_ignore_ascii_case(name))
        .map(|index| index as u32 + 1)
}

fn weekday_ok(name: &str) -> bool {
    WEEKDAYS
        .iter()
        .any(|day| name.starts_with(day) || day.eq_ignore_ascii_case(name))
        || [
            "Sunday",
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
        ]
        .iter()
        .any(|day| day.eq_ignore_ascii_case(name))
}

fn parse_hms(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split(':');
    let hour = parts.next()?.parse::<u32>().ok()?;
    let min = parts.next()?.parse::<u32>().ok()?;
    let sec = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    Some((hour, min, sec))
}

fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn ymd_hms_to_unix(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    let mut y = year;
    let mut m = month as i32;
    if m <= 2 {
        y -= 1;
        m += 9;
    } else {
        m -= 3;
    }
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400) as u32;
    let doy = (153 * m as u32 + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = i64::from(era) * 146097 + i64::from(doe) - 719468;
    Some(days * 86_400 + i64::from(hour) * 3600 + i64::from(min) * 60 + i64::from(sec))
}

/// Parses RFC 822 / RFC 850 / asctime HTTP dates used by `ngx.parse_http_time`.
fn parse_http_date(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // RFC 822: Thu, 18 Nov 2010 11:27:35 GMT
    if let Some((wday, rest)) = text.split_once(", ") {
        if !weekday_ok(wday) {
            return None;
        }
        let mut parts = rest.split_whitespace();
        if let (Some(day), Some(month), Some(year), Some(hms), Some(zone)) =
            (parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
        {
            if parts.next().is_some() || !zone.eq_ignore_ascii_case("GMT") {
                return None;
            }
            let day = day.parse::<u32>().ok()?;
            let month = month_from_name(month)?;
            let year = parse_year(year)?;
            let (hour, min, sec) = parse_hms(hms)?;
            return ymd_hms_to_unix(year, month, day, hour, min, sec);
        }
        // RFC 850: Thursday, 18-Nov-10 11:27:35 GMT
        let mut dash_parts = rest.split_whitespace();
        let date = dash_parts.next()?;
        let hms = dash_parts.next()?;
        let zone = dash_parts.next()?;
        if dash_parts.next().is_some() || !zone.eq_ignore_ascii_case("GMT") {
            return None;
        }
        let mut date_parts = date.split('-');
        let day = date_parts.next()?.parse::<u32>().ok()?;
        let month = month_from_name(date_parts.next()?)?;
        let year = parse_year(date_parts.next()?)?;
        if date_parts.next().is_some() {
            return None;
        }
        let (hour, min, sec) = parse_hms(hms)?;
        return ymd_hms_to_unix(year, month, day, hour, min, sec);
    }
    // asctime: Thu Nov 18 11:27:35 2010
    let mut parts = text.split_whitespace();
    let wday = parts.next()?;
    let month = parts.next()?;
    let day = parts.next()?;
    let hms = parts.next()?;
    let year = parts.next()?;
    if parts.next().is_some() || !weekday_ok(wday) {
        return None;
    }
    let month = month_from_name(month)?;
    let day = day.parse::<u32>().ok()?;
    let year = parse_year(year)?;
    let (hour, min, sec) = parse_hms(hms)?;
    ymd_hms_to_unix(year, month, day, hour, min, sec)
}

fn parse_year(text: &str) -> Option<i32> {
    let year = text.parse::<i32>().ok()?;
    if text.len() == 2 {
        Some(if year < 70 { 2000 + year } else { 1900 + year })
    } else if text.len() == 4 {
        Some(year)
    } else {
        None
    }
}

/// Local date `yyyy-mm-dd` from the cached clock.
#[pd_host_function(name = "ngx::today")]
pub fn today() -> VmResult<CallOutcome> {
    let clock = read_clock();
    ret(format_civil_date(&broken_down(clock.sec, false)?))
}

/// Cached unix seconds as an integer. Distinct from [`now`].
#[pd_host_function(name = "ngx::time")]
pub fn time() -> VmResult<CallOutcome> {
    ret(read_clock().sec)
}

/// Cached unix time as seconds plus milliseconds (`float`).
#[pd_host_function(name = "ngx::now")]
pub fn now() -> VmResult<CallOutcome> {
    let clock = read_clock();
    ret(clock.sec as f64 + f64::from(clock.msec) / 1000.0)
}

/// Refreshes the cached clock from the OS. This is a real side effect.
#[pd_host_function(name = "ngx::update_time")]
pub fn update_time() -> VmResult<CallOutcome> {
    refresh_clock();
    ret(())
}

/// Local `yyyy-mm-dd hh:mm:ss` from the cached clock.
#[pd_host_function(name = "ngx::localtime")]
pub fn localtime() -> VmResult<CallOutcome> {
    let clock = read_clock();
    ret(format_civil_datetime(&broken_down(clock.sec, false)?))
}

/// UTC `yyyy-mm-dd hh:mm:ss` from the cached clock.
#[pd_host_function(name = "ngx::utctime")]
pub fn utctime() -> VmResult<CallOutcome> {
    let clock = read_clock();
    ret(format_civil_datetime(&broken_down(clock.sec, true)?))
}

/// Cookie date for `sec` (`Thu, 18-Nov-10 11:27:35 GMT`).
#[pd_host_function(name = "ngx::cookie_time")]
pub fn cookie_time(sec: i64) -> VmResult<CallOutcome> {
    ret(http_date(sec, true)?)
}

/// HTTP date for `sec` (`Thu, 18 Nov 2010 11:27:35 GMT`).
#[pd_host_function(name = "ngx::http_time")]
pub fn http_time(sec: i64) -> VmResult<CallOutcome> {
    ret(http_date(sec, false)?)
}

/// Parses an HTTP date into unix seconds, or `null` when invalid.
#[pd_host_function(name = "ngx::parse_http_time")]
pub fn parse_http_time(text: &str) -> VmResult<CallOutcome> {
    if text.len() > 128 {
        return Err(VmError::HostError(
            "ngx::parse_http_time input exceeds 128 bytes".to_string(),
        ));
    }
    ret(parse_http_date(text))
}
