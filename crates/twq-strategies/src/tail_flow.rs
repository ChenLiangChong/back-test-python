//! 尾盤當沖強平順勢 (13:30 forced-liquidation flow), from third-party notes of 馬克羊's
//! 2026-02 群益期貨 lecture: day-trade (當沖) futures positions must be closed by 13:30, so
//! on strong trend days losing day traders are forced out *with* the trend.
//!
//! Rule (third-party spec): trend = close(13:29) − open(08:45); if |trend| ≥ `min_trend`
//! enter with the trend at 13:30 and exit at 13:44. The third party found the
//! continuation leg not robust across regimes, so treat this as a hypothesis to test.

use twq_core::time::{hhmm, hhmm_to_min, minute_of_day};
use twq_core::{Bar, Ctx, Params, Strategy};

use crate::common::DayTracker;

pub struct TailFlow {
    min_trend: f64,
    entry_hhmm: u32,
    exit_hhmm: u32,
    qty: i64,
    days: DayTracker,
    day_open: Option<f64>,
    done: bool,
}

impl TailFlow {
    pub const PARAMS: &'static [(&'static str, f64, &'static str)] = &[
        ("min_trend", 150.0, "當日趨勢門檻 (點)"),
        ("entry_hhmm", 1330.0, "進場時間 (K棒收盤)"),
        ("exit_hhmm", 1344.0, "出場時間 (K棒收盤)"),
        ("qty", 1.0, "口數"),
    ];

    pub fn new(p: &Params) -> Self {
        Self {
            min_trend: p.get("min_trend", 150.0),
            entry_hhmm: p.get("entry_hhmm", 1330.0) as u32,
            exit_hhmm: p.get("exit_hhmm", 1344.0) as u32,
            qty: p.get("qty", 1.0) as i64,
            days: DayTracker::calendar(),
            day_open: None,
            done: false,
        }
    }
}

impl Strategy for TailFlow {
    fn on_bar(&mut self, bar: &Bar, ctx: &mut Ctx) {
        if self.days.is_new_day(bar.ts) {
            self.day_open = None;
            self.done = false;
        }
        let m = minute_of_day(bar.ts);
        if !(hhmm_to_min(845)..hhmm_to_min(1345)).contains(&m) {
            return;
        }
        if self.day_open.is_none() {
            self.day_open = Some(bar.open);
        }
        let t = hhmm(ctx.bar_end(bar));
        if t >= self.exit_hhmm {
            if !ctx.is_flat() {
                ctx.flatten();
            }
            self.done = true;
            return;
        }
        if self.done || t < self.entry_hhmm {
            return;
        }
        self.done = true;
        let trend = bar.close - self.day_open.unwrap_or(bar.close);
        if trend >= self.min_trend {
            ctx.target_position(self.qty);
        } else if trend <= -self.min_trend {
            ctx.target_position(-self.qty);
        }
    }
}
