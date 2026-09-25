//! Event calendars (經濟數據 / 指數調整日 / 營收公布) and auxiliary time series
//! (e.g. TAIEX cumulative turnover) that strategies can consume next to the price data.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};

use crate::time::{day_of, days_from_civil, parse_datetime, time_of_day, weekday, Ts, US_PER_DAY, US_PER_HOUR};

/// 大事件 / 一般事件.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Big,
    Normal,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// Exchange-local (Taipei) timestamp of the release.
    pub ts: Ts,
    /// Leaked once at load time so it can be used as a trade tag.
    pub name: &'static str,
    pub tier: Tier,
}

/// n-th (1-based) occurrence of `wd` (0 = Monday) in a month, as a day number.
fn nth_weekday(y: i64, m: u32, wd: u32, n: u32) -> i64 {
    let first = days_from_civil(y, m, 1);
    let first_wd = ((first + 3).rem_euclid(7)) as u32;
    first + ((wd + 7 - first_wd) % 7) as i64 + 7 * (n as i64 - 1)
}

/// Convert a US-Eastern wall-clock time to Taipei wall-clock time (UTC+8, no DST).
/// US DST runs from the 2nd Sunday of March 02:00 to the 1st Sunday of November 02:00.
pub fn us_eastern_to_taipei(et: Ts) -> Ts {
    let (y, _, _) = crate::time::civil_from_days(day_of(et));
    let dst_start = nth_weekday(y, 3, 6, 2) * US_PER_DAY + 2 * US_PER_HOUR;
    let dst_end = nth_weekday(y, 11, 6, 1) * US_PER_DAY + 2 * US_PER_HOUR;
    let utc_offset_hours = if et >= dst_start && et < dst_end { -4 } else { -5 };
    et + (8 - utc_offset_hours) * US_PER_HOUR
}

/// Load an event calendar CSV:
///
/// ```text
/// datetime,name,tier,tz
/// 2026-09-04 20:30:00,NFP,big,TW
/// 2026-09-16 14:00:00,FOMC,big,ET      # converted to 2026-09-17 02:00 Taipei
/// ```
///
/// `tier`: `big`/`大` or `normal`/`一般` (default normal). `tz`: `TW` (default) or `ET`.
/// Extra columns are ignored; lines starting with `#` are comments.
pub fn load_events(path: impl AsRef<Path>) -> Result<Vec<Event>> {
    let path = path.as_ref();
    let text = fs::read_to_string(path).with_context(|| format!("reading events {}", path.display()))?;
    parse_events(&text)
}

pub fn parse_events(text: &str) -> Result<Vec<Event>> {
    let mut out = Vec::new();
    let mut header_seen = false;
    for (i, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(',').map(str::trim).collect();
        let Some(ts) = parse_datetime(f[0]) else {
            if !header_seen && out.is_empty() {
                header_seen = true; // first non-comment line may be a header
                continue;
            }
            bail!("events line {}: bad datetime '{}'", i + 1, f[0]);
        };
        let name = f
            .get(1)
            .copied()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("events line {}: missing name", i + 1))?;
        let tier = match f.get(2).map(|s| s.to_ascii_lowercase()) {
            Some(t) if t == "big" || t == "大" || t == "major" => Tier::Big,
            Some(t) if t.is_empty() || t == "normal" || t == "一般" || t == "minor" => Tier::Normal,
            None => Tier::Normal,
            Some(t) => bail!("events line {}: tier must be big/normal, got '{t}'", i + 1),
        };
        let ts = match f.get(3).map(|s| s.to_ascii_uppercase()) {
            Some(z) if z == "ET" || z == "EST" || z == "EDT" || z == "NY" => us_eastern_to_taipei(ts),
            Some(z) if z.is_empty() || z == "TW" || z == "TPE" || z == "TAIPEI" => ts,
            None => ts,
            Some(z) => bail!("events line {}: tz must be TW or ET, got '{z}'", i + 1),
        };
        out.push(Event { ts, name: Box::leak(name.to_string().into_boxed_str()), tier });
    }
    out.sort_by_key(|e| e.ts);
    Ok(out)
}

/// A sparse time series: (timestamp, value) sorted by time.
pub type Series = Vec<(Ts, f64)>;

/// Load `timestamp,value` CSV (header optional).
pub fn load_series(path: impl AsRef<Path>) -> Result<Series> {
    let path = path.as_ref();
    let text = fs::read_to_string(path).with_context(|| format!("reading series {}", path.display()))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let mut it = line.split(',').map(str::trim);
        let (Some(t), Some(v)) = (it.next(), it.next()) else { continue };
        let (Some(ts), Ok(v)) = (parse_datetime(t), v.replace('_', "").parse::<f64>()) else { continue };
        out.push((ts, v));
    }
    if out.is_empty() {
        bail!("no rows in series {}", path.display());
    }
    out.sort_by_key(|x| x.0);
    Ok(out)
}

/// Intraday cumulative series (e.g. 大盤累積成交金額) with O(1) amortised lookups while
/// time moves forward, plus the previous trading day's final value (昨量).
#[derive(Clone, Debug)]
pub struct CumulativeDaily {
    data: Arc<Series>,
    idx: usize,
    cur_day: i64,
    prev_total: Option<f64>,
    prev_day: Option<i64>,
    today_last: Option<f64>,
}

impl CumulativeDaily {
    pub fn new(data: Arc<Series>) -> Self {
        Self { data, idx: 0, cur_day: i64::MIN, prev_total: None, prev_day: None, today_last: None }
    }

    /// Calendar day (day number) of the total returned as "previous day" by [`Self::at`].
    /// Callers should check it really is the previous *trading* day: a gap in the data
    /// would otherwise compare against an older session.
    pub fn prev_day_for(&self, today: i64) -> Option<i64> {
        if self.cur_day == today {
            self.prev_day
        } else if self.cur_day < today && self.cur_day != i64::MIN {
            Some(self.cur_day)
        } else {
            self.prev_day
        }
    }

    /// Advance to `now`; returns `(today's cumulative value so far, previous day's total)`.
    pub fn at(&mut self, now: Ts) -> (Option<f64>, Option<f64>) {
        let today = day_of(now);
        while self.idx < self.data.len() && self.data[self.idx].0 <= now {
            let (ts, v) = self.data[self.idx];
            let d = day_of(ts);
            if d != self.cur_day {
                if self.cur_day != i64::MIN {
                    self.prev_total = self.today_last;
                    self.prev_day = Some(self.cur_day);
                }
                self.cur_day = d;
                self.today_last = None;
            }
            self.today_last = Some(v);
            self.idx += 1;
        }
        if self.cur_day == today {
            (self.today_last, self.prev_total)
        } else {
            // no sample yet today: yesterday's last value is the previous total
            let prev = if self.cur_day < today { self.today_last } else { self.prev_total };
            (None, prev)
        }
    }
}

/// Last business day (Mon–Fri) of a month, as a day number (exchange holidays ignored).
pub fn last_weekday_of_month(y: i64, m: u32) -> i64 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    let mut d = days_from_civil(ny, nm, 1) - 1;
    while weekday(d * US_PER_DAY) >= 5 {
        d -= 1;
    }
    d
}

/// MSCI quarterly / semi-annual review implementation days (last business day of
/// Feb / May / Aug / Nov) and FTSE quarterly review days (3rd Friday of Mar / Jun /
/// Sep / Dec), at `at_hhmm` Taipei time. Exchange holidays are not modelled: check
/// the generated dates against the official announcements.
pub fn index_review_events(from_year: i64, to_year: i64, at_hhmm: u32) -> Vec<Event> {
    let t = (at_hhmm / 100) as i64 * US_PER_HOUR + (at_hhmm % 100) as i64 * 60_000_000;
    let mut out = Vec::new();
    for y in from_year..=to_year {
        for m in [2, 5, 8, 11] {
            out.push(Event { ts: last_weekday_of_month(y, m) * US_PER_DAY + t, name: "MSCI", tier: Tier::Normal });
        }
        for m in [3, 6, 9, 12] {
            out.push(Event { ts: nth_weekday(y, m, 4, 3) * US_PER_DAY + t, name: "FTSE", tier: Tier::Normal });
        }
    }
    out.sort_by_key(|e| e.ts);
    out
}

/// Seconds-of-day helper for formatting.
pub fn is_same_minute(a: Ts, b: Ts) -> bool {
    day_of(a) == day_of(b) && time_of_day(a) / 60_000_000 == time_of_day(b) / 60_000_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::make_ts;

    #[test]
    fn et_to_taipei() {
        // 2026-09-16 14:00 EDT (FOMC) -> 2026-09-17 02:00 Taipei
        assert_eq!(us_eastern_to_taipei(make_ts(2026, 9, 16, 14, 0, 0, 0)), make_ts(2026, 9, 17, 2, 0, 0, 0));
        // 2026-01-09 08:30 EST -> 21:30 Taipei
        assert_eq!(us_eastern_to_taipei(make_ts(2026, 1, 9, 8, 30, 0, 0)), make_ts(2026, 1, 9, 21, 30, 0, 0));
        // DST starts 2026-03-08, ends 2026-11-01
        assert_eq!(us_eastern_to_taipei(make_ts(2026, 3, 9, 8, 30, 0, 0)), make_ts(2026, 3, 9, 20, 30, 0, 0));
        assert_eq!(us_eastern_to_taipei(make_ts(2026, 11, 2, 8, 30, 0, 0)), make_ts(2026, 11, 2, 21, 30, 0, 0));
    }

    #[test]
    fn parse_calendar() {
        let ev = parse_events(
            "datetime,name,tier,tz\n# comment\n2026-09-16 14:00:00,FOMC,big,ET\n2026-09-04 20:30,NFP,大\n2026-09-18 13:25,FTSE,normal,TW # trailing\n",
        )
        .unwrap();
        assert_eq!(ev.len(), 3);
        assert_eq!(ev[0].name, "NFP");
        assert_eq!(ev[0].tier, Tier::Big);
        assert_eq!(ev[1].ts, make_ts(2026, 9, 17, 2, 0, 0, 0));
        assert_eq!(ev[2].tier, Tier::Normal);
        assert!(parse_events("x\n2026-09-04 20:30,NFP,huge\n").is_err());
    }

    #[test]
    fn index_review_dates() {
        let ev = index_review_events(2026, 2026, 1325);
        let msci_aug = ev.iter().find(|e| e.name == "MSCI" && day_of(e.ts) == days_from_civil(2026, 8, 31));
        let ftse_sep = ev.iter().find(|e| e.name == "FTSE" && day_of(e.ts) == days_from_civil(2026, 9, 18));
        assert!(msci_aug.is_some(), "MSCI Aug 2026 should be Mon 08-31");
        assert!(ftse_sep.is_some(), "FTSE Sep 2026 should be Fri 09-18");
        assert_eq!(time_of_day(msci_aug.unwrap().ts), 13 * US_PER_HOUR + 25 * 60_000_000);
    }

    #[test]
    fn cumulative_daily() {
        let d1 = make_ts(2026, 9, 1, 9, 0, 0, 0);
        let d2 = make_ts(2026, 9, 2, 9, 0, 0, 0);
        let s = vec![
            (d1, 10.0),
            (d1 + US_PER_HOUR, 50.0),
            (d1 + 4 * US_PER_HOUR, 100.0),
            (d2, 5.0),
            (d2 + 1_800_000_000, 40.0),
        ];
        let mut c = CumulativeDaily::new(Arc::new(s));
        assert_eq!(c.at(d1 + 10), (Some(10.0), None));
        assert_eq!(c.at(d2 - 1), (None, Some(100.0)));
        assert_eq!(c.at(d2 + 1_800_000_000), (Some(40.0), Some(100.0)));
    }
}
