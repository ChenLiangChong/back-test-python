//! Timestamps are `i64` microseconds since 1970-01-01 00:00:00 **in exchange-local
//! time** (Asia/Taipei, no DST). Keeping wall-clock local time avoids any timezone
//! math in the hot path: session rules like "08:45–13:45" are plain integer compares.

pub type Ts = i64;

pub const US_PER_MS: i64 = 1_000;
pub const US_PER_SEC: i64 = 1_000_000;
pub const US_PER_MIN: i64 = 60 * US_PER_SEC;
pub const US_PER_HOUR: i64 = 60 * US_PER_MIN;
pub const US_PER_DAY: i64 = 24 * US_PER_HOUR;

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
#[inline]
pub const fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`].
#[inline]
pub const fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[inline]
pub const fn make_ts(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32, us: u32) -> Ts {
    days_from_civil(y, mo, d) * US_PER_DAY
        + h as i64 * US_PER_HOUR
        + mi as i64 * US_PER_MIN
        + s as i64 * US_PER_SEC
        + us as i64
}

/// Day number (days since epoch) of a timestamp.
#[inline]
pub const fn day_of(ts: Ts) -> i64 {
    ts.div_euclid(US_PER_DAY)
}

/// Microseconds since local midnight.
#[inline]
pub const fn time_of_day(ts: Ts) -> i64 {
    ts.rem_euclid(US_PER_DAY)
}

/// Minutes since local midnight (0..1440).
#[inline]
pub const fn minute_of_day(ts: Ts) -> u32 {
    (time_of_day(ts) / US_PER_MIN) as u32
}

/// `hhmm` as an integer, e.g. 13:45 -> 1345. Handy for session filters.
#[inline]
pub const fn hhmm(ts: Ts) -> u32 {
    let m = minute_of_day(ts);
    (m / 60) * 100 + m % 60
}

/// Minutes since midnight for an `hhmm` literal (e.g. 845 -> 525).
#[inline]
pub const fn hhmm_to_min(hhmm: u32) -> u32 {
    (hhmm / 100) * 60 + hhmm % 100
}

/// 0 = Monday … 6 = Sunday.
#[inline]
pub const fn weekday(ts: Ts) -> u32 {
    // 1970-01-01 was a Thursday (3).
    ((day_of(ts) + 3).rem_euclid(7)) as u32
}

/// TAIFEX trading day of a timestamp, as a day number.
///
/// The night session (15:00–05:00) belongs to the *next* business day, e.g. Monday
/// 15:00 → Tuesday; Friday 15:00 and Saturday 02:00 → Monday. Exchange holidays are
/// not modelled (acceptable for bar-level research; use a calendar file for production).
pub const fn trading_day(ts: Ts) -> i64 {
    let day = day_of(ts);
    let tod = time_of_day(ts);
    let base = if tod >= 14 * US_PER_HOUR + 50 * US_PER_MIN { day + 1 } else { day };
    // roll weekend onto Monday
    match (base + 3).rem_euclid(7) {
        5 => base + 2, // Saturday
        6 => base + 1, // Sunday
        _ => base,
    }
}

#[inline]
fn digits(b: &[u8]) -> Option<u32> {
    let mut v: u32 = 0;
    for &c in b {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (c - b'0') as u32;
    }
    Some(v)
}

/// Fast parser for the common timestamp layouts found in broker / exchange CSVs:
///
/// * `2024-01-02 08:45:00`, `2024/01/02 08:45`, `2024-01-02T08:45:00.123456`
/// * `20240102 084500`, `20240102084500`, `20240102` (date only)
/// * integer epoch seconds (10 digits) or milliseconds (13 digits), treated as local time
pub fn parse_datetime(s: &str) -> Option<Ts> {
    let b = s.trim().as_bytes();
    if b.is_empty() {
        return None;
    }
    // epoch
    if b.iter().all(|c| c.is_ascii_digit()) {
        return match b.len() {
            8 => Some(make_ts(digits(&b[0..4])? as i64, digits(&b[4..6])?, digits(&b[6..8])?, 0, 0, 0, 0)),
            10 => Some(digits(b)? as i64 * US_PER_SEC),
            13 => s.trim().parse::<i64>().ok().map(|ms| ms * US_PER_MS),
            14 => Some(make_ts(
                digits(&b[0..4])? as i64,
                digits(&b[4..6])?,
                digits(&b[6..8])?,
                digits(&b[8..10])?,
                digits(&b[10..12])?,
                digits(&b[12..14])?,
                0,
            )),
            _ => None,
        };
    }
    let padded = b.len() >= 10
        && (b[4] == b'-' || b[4] == b'/')
        && (b[7] == b'-' || b[7] == b'/')
        && b[8].is_ascii_digit()
        && b[9].is_ascii_digit()
        && (b.len() == 10 || b[10] == b' ' || b[10] == b'T');
    if !padded && b.len() > 4 && (b[4] == b'-' || b[4] == b'/') {
        return parse_unpadded(s.trim());
    }
    let (y, mo, d, rest) = if padded {
        (digits(&b[0..4])?, digits(&b[5..7])?, digits(&b[8..10])?, &b[10..])
    } else if b.len() >= 8 {
        (digits(&b[0..4])?, digits(&b[4..6])?, digits(&b[6..8])?, &b[8..])
    } else {
        return None;
    };
    let rest = if !rest.is_empty() && (rest[0] == b' ' || rest[0] == b'T') { &rest[1..] } else { rest };
    let (mut h, mut mi, mut sec, mut us) = (0, 0, 0, 0);
    if !rest.is_empty() {
        if rest.len() >= 5 && rest[2] == b':' {
            h = digits(&rest[0..2])?;
            mi = digits(&rest[3..5])?;
            let mut r = &rest[5..];
            if r.len() >= 3 && r[0] == b':' {
                sec = digits(&r[1..3])?;
                r = &r[3..];
            }
            if !r.is_empty() && r[0] == b'.' {
                let frac = &r[1..];
                let n = frac.len().min(6);
                let mut v = digits(&frac[..n])?;
                for _ in n..6 {
                    v *= 10;
                }
                us = v;
            }
        } else if rest.len() >= 4 && rest[..4].iter().all(|c| c.is_ascii_digit()) {
            h = digits(&rest[0..2])?;
            mi = digits(&rest[2..4])?;
            if rest.len() >= 6 {
                sec = digits(&rest[4..6])?;
            }
        } else {
            return None;
        }
    }
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    Some(make_ts(y as i64, mo, d, h, mi, sec, us))
}

/// Slow path for dates without zero padding, e.g. `1998/7/22 9:01:00`.
fn parse_unpadded(s: &str) -> Option<Ts> {
    let (date, time) = match s.find([' ', 'T']) {
        Some(i) => (&s[..i], s[i + 1..].trim()),
        None => (s, ""),
    };
    let mut dp = date.split(['-', '/']);
    let (y, mo, d) =
        (dp.next()?.parse::<i64>().ok()?, dp.next()?.parse::<u32>().ok()?, dp.next()?.parse::<u32>().ok()?);
    let (mut h, mut mi, mut sec, mut us) = (0u32, 0u32, 0u32, 0u32);
    if !time.is_empty() {
        let mut tp = time.split(':');
        h = tp.next()?.parse().ok()?;
        mi = tp.next().unwrap_or("0").parse().ok()?;
        if let Some(sp) = tp.next() {
            let (whole, frac) = sp.split_once('.').unwrap_or((sp, ""));
            sec = whole.parse().ok()?;
            if !frac.is_empty() {
                let n = frac.len().min(6);
                us = frac[..n].parse::<u32>().ok()? * 10u32.pow(6 - n as u32);
            }
        }
    }
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    Some(make_ts(y, mo, d, h, mi, sec, us))
}

/// `YYYY-MM-DD HH:MM:SS` (plus `.ffffff` when there are sub-second parts).
pub fn fmt_ts(ts: Ts) -> String {
    let (y, m, d) = civil_from_days(day_of(ts));
    let tod = time_of_day(ts);
    let (h, mi, s, us) = (tod / US_PER_HOUR, (tod / US_PER_MIN) % 60, (tod / US_PER_SEC) % 60, tod % US_PER_SEC);
    if us == 0 {
        format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
    } else {
        format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}.{us:06}")
    }
}

pub fn fmt_day(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Which TAIFEX session a timestamp falls into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FuturesSession {
    /// 一般交易時段 08:45–13:45
    Day,
    /// 盤後交易時段 15:00–05:00(+1)
    Night,
    Closed,
}

pub fn futures_session(ts: Ts) -> FuturesSession {
    let m = minute_of_day(ts);
    if (hhmm_to_min(845)..hhmm_to_min(1345)).contains(&m) {
        FuturesSession::Day
    } else if m >= hhmm_to_min(1500) || m < hhmm_to_min(500) {
        FuturesSession::Night
    } else {
        FuturesSession::Closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_roundtrip() {
        for z in [-1000_i64, 0, 1, 19_000, 20_000, 20_721] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(days_from_civil(2026, 9, 25)), (2026, 9, 25));
    }

    #[test]
    fn parse_formats() {
        let t = make_ts(2024, 1, 2, 8, 45, 0, 0);
        assert_eq!(parse_datetime("2024-01-02 08:45:00"), Some(t));
        assert_eq!(parse_datetime("2024/01/02 08:45"), Some(t));
        assert_eq!(parse_datetime("20240102 084500"), Some(t));
        assert_eq!(parse_datetime("20240102084500"), Some(t));
        assert_eq!(parse_datetime("2024-01-02T08:45:00.5"), Some(t + 500_000));
        assert_eq!(parse_datetime("2024-13-02 08:45:00"), None);
        assert_eq!(parse_datetime("2024/1/2 8:45:00"), Some(t));
        assert_eq!(parse_datetime("1998/7/22 09:01:00"), Some(make_ts(1998, 7, 22, 9, 1, 0, 0)));
        assert_eq!(parse_datetime("1998/10/1 09:01:00"), Some(make_ts(1998, 10, 1, 9, 1, 0, 0)));
        assert_eq!(parse_datetime("1998/10/1"), Some(make_ts(1998, 10, 1, 0, 0, 0, 0)));
        assert_eq!(fmt_ts(t), "2024-01-02 08:45:00");
    }

    #[test]
    fn weekday_and_trading_day() {
        let fri = make_ts(2026, 9, 25, 15, 30, 0, 0); // Friday night session
        assert_eq!(weekday(fri), 4);
        let mon = days_from_civil(2026, 9, 28);
        assert_eq!(trading_day(fri), mon);
        assert_eq!(trading_day(make_ts(2026, 9, 26, 2, 0, 0, 0)), mon); // Sat 02:00
        let tue = make_ts(2026, 9, 29, 9, 0, 0, 0);
        assert_eq!(trading_day(tue), days_from_civil(2026, 9, 29));
        assert_eq!(trading_day(make_ts(2026, 9, 28, 20, 0, 0, 0)), days_from_civil(2026, 9, 29));
        assert_eq!(futures_session(tue), FuturesSession::Day);
        assert_eq!(futures_session(make_ts(2026, 9, 29, 14, 0, 0, 0)), FuturesSession::Closed);
        assert_eq!(futures_session(make_ts(2026, 9, 29, 3, 0, 0, 0)), FuturesSession::Night);
        assert_eq!(hhmm(make_ts(2026, 9, 29, 13, 45, 0, 0)), 1345);
    }
}
