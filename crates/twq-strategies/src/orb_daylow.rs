//! 使用者的 ORB: 破日盤 day low 放空.
//!
//! 1. 9:30 前, 日盤台指期 (08:45 起) 最高 − 最低 > `range_pts` (100 點)
//! 2. 9:30 前, 加權指數累積成交 > 昨日全天成交 × `vol_ratio` (0.45 倍)
//!
//! 9:30 前兩個條件一達標就掛單 (`at_deadline` = 0): 「當時的日盤最低點 − `offset` (1 點)」觸價空單.
//! 出場 (使用者規則): 停損 = 進場價 × 0.4% (`sl_pct`), 不設停利, 抱到尾盤 13:40 平倉.
//!
//! 條件 2 需要大盤累積成交資料: `--series taiex_vol=<csv>` (timestamp,累積成交金額或量),
//! 由 `tools/fetch_data.py` 從證交所「每5秒委託成交統計」下載.

use std::sync::Arc;

use twq_core::events::{CumulativeDaily, Series};
use twq_core::time::{hhmm, hhmm_to_min, minute_of_day};
use twq_core::{Bar, Bracket, Ctx, OrderKind, Params, Side, Strategy, Tif};

use crate::common::DayTracker;

pub struct OrbDayLow {
    range_pts: f64,
    range_pct: f64,
    sl_pct: f64,
    tp_pct: f64,
    day_open: f64,
    vol_ratio: f64,
    deadline_min: u32,
    at_deadline: bool,
    offset: f64,
    sl: f64,
    tp: f64,
    exit_hhmm: u32,
    qty: i64,
    vol: Option<CumulativeDaily>,
    days: DayTracker,
    hi: f64,
    lo: f64,
    armed: bool,
    done: bool,
    /// Calendar day of the current and of the previous day session seen.
    session_day: i64,
    prev_session_day: i64,
    /// Days on which both conditions were met (for diagnostics).
    pub signal_days: u32,
    /// Days skipped because the turnover series lacked today or the previous trading day.
    pub missing_vol_days: u32,
}

impl OrbDayLow {
    pub const PARAMS: &'static [(&'static str, f64, &'static str)] = &[
        ("range_pts", 100.0, "條件1: 9:30 前日盤高低差 > N 點"),
        ("range_pct", 0.0, ">0 時改用百分比: 高低差 > 開盤價 × N% (取代 range_pts)"),
        ("vol_ratio", 0.45, "條件2: 9:30 前大盤累積成交 > 昨量 × N"),
        ("use_vol", 1.0, "1 = 使用條件2 (需 --series taiex_vol=...), 0 = 只看條件1"),
        ("deadline", 930.0, "條件須在此時間前達成 (hhmm)"),
        ("at_deadline", 0.0, "1 = 等到 deadline (9:30) 才檢查條件並掛單; 0 = 之前任何時間達標就掛"),
        ("offset", 1.0, "觸價空單 = day low − N 點"),
        ("sl", 40.0, "停損 (點), sl_pct = 0 時才使用"),
        ("sl_pct", 0.4, "停損 = 進場價 × N% (使用者規則: 大盤點數 0.4%), 0 = 改用固定點數 sl"),
        ("tp", 0.0, "停利 (點), 0 = 不設 (使用者規則: 抱到尾盤)"),
        ("tp_pct", 0.0, ">0 時停利改用百分比: 進場價 × N% (取代 tp)"),
        ("exit_hhmm", 1340.0, "強制平倉時間"),
        ("qty", 1.0, "口數"),
    ];

    pub fn new(p: &Params, vol: Option<Arc<Series>>) -> Self {
        Self {
            range_pts: p.get("range_pts", 100.0),
            range_pct: p.get("range_pct", 0.0),
            sl_pct: p.get("sl_pct", 0.4),
            tp_pct: p.get("tp_pct", 0.0),
            day_open: f64::NAN,
            vol_ratio: p.get("vol_ratio", 0.45),
            deadline_min: hhmm_to_min(p.get("deadline", 930.0) as u32),
            at_deadline: p.flag("at_deadline", false),
            offset: p.get("offset", 1.0),
            sl: p.get("sl", 40.0),
            tp: p.get("tp", 0.0),
            exit_hhmm: p.get("exit_hhmm", 1340.0) as u32,
            qty: p.get("qty", 1.0) as i64,
            vol: vol.map(CumulativeDaily::new),
            days: DayTracker::calendar(),
            hi: f64::MIN,
            lo: f64::MAX,
            armed: false,
            done: false,
            session_day: i64::MIN,
            prev_session_day: i64::MIN,
            signal_days: 0,
            missing_vol_days: 0,
        }
    }
}

impl Strategy for OrbDayLow {
    fn on_bar(&mut self, bar: &Bar, ctx: &mut Ctx) {
        let m = minute_of_day(bar.ts);
        if !(hhmm_to_min(845)..hhmm_to_min(1345)).contains(&m) {
            return; // day session only
        }
        if self.days.is_new_day(bar.ts) {
            self.prev_session_day = self.session_day;
            self.session_day = twq_core::time::day_of(bar.ts);
            self.hi = f64::MIN;
            self.lo = f64::MAX;
            self.day_open = bar.open;
            self.armed = false;
            self.done = false;
            if !ctx.is_flat() || ctx.has_working_orders() {
                ctx.flatten();
            }
        }
        self.hi = self.hi.max(bar.high);
        self.lo = self.lo.min(bar.low);
        let end = ctx.bar_end(bar);
        if hhmm(end) >= self.exit_hhmm {
            if !ctx.is_flat() || ctx.has_working_orders() {
                ctx.flatten();
            }
            self.done = true;
            return;
        }
        if self.armed || self.done {
            return;
        }
        if minute_of_day(end) > self.deadline_min {
            self.done = true; // conditions not met before the deadline
            return;
        }
        if self.at_deadline && minute_of_day(end) < self.deadline_min {
            return; // evaluate only once, at the deadline
        }
        let threshold = if self.range_pct > 0.0 { self.day_open * self.range_pct / 100.0 } else { self.range_pts };
        let cond_range = self.hi - self.lo > threshold;
        let cond_vol = match self.vol.as_mut() {
            None => true,
            Some(v) => {
                let (today, prev) = v.at(end);
                let prev_ok = v.prev_day_for(self.session_day) == Some(self.prev_session_day);
                match (today, prev) {
                    (Some(t), Some(p)) if prev_ok && p > 0.0 => t > self.vol_ratio * p,
                    _ => {
                        // no turnover data for today or for the previous trading day:
                        // skip the day rather than compare against the wrong session
                        if minute_of_day(end) >= self.deadline_min {
                            self.missing_vol_days += 1;
                        }
                        false
                    }
                }
            }
        };
        if cond_range && cond_vol && ctx.is_flat() {
            let entry = self.lo - self.offset;
            let sl = if self.sl_pct > 0.0 { entry * self.sl_pct / 100.0 } else { self.sl };
            let tp = if self.tp_pct > 0.0 { entry * self.tp_pct / 100.0 } else { self.tp };
            let br = Bracket { stop_dist: (sl > 0.0).then_some(sl), take_dist: (tp > 0.0).then_some(tp) };
            let id = ctx.submit(Side::Sell, self.qty, OrderKind::Stop(entry), Tif::Rod, 0, "orb_daylow");
            ctx.attach_bracket(id, br);
            self.armed = true;
            self.signal_days += 1;
        }
    }
}
