//! Opening Range Breakout (開盤區間突破), by default for the TAIFEX day session
//! (08:45–13:45; set `open_hhmm` / `session_end` / `exit_hhmm` for other markets).
//!
//! The first `range_min` minutes after 08:45 define a range. OCO stop orders sit just
//! beyond both sides; the first to trigger enters and cancels the other. The protective
//! stop is the opposite side of the range (scaled by `stop_frac`), the target `rr`×risk,
//! and everything is flattened at `exit_hhmm`.

use twq_core::time::{hhmm, hhmm_to_min, minute_of_day};
use twq_core::{Bar, Bracket, Ctx, Fill, OrderKind, Params, Side, Strategy, Tif};

use crate::common::{size, DayTracker};

pub struct Orb {
    open_min: u32,
    session_end_min: u32,
    range_min: u32,
    buffer: f64,
    stop_frac: f64,
    rr: f64,
    exit_hhmm: u32,
    min_range: f64,
    max_range: f64,
    max_trades: u32,
    qty: i64,
    risk_pct: f64,
    // state
    days: DayTracker,
    hi: f64,
    lo: f64,
    armed: bool,
    done: bool,
    trades: u32,
    rearm: bool,
}

impl Orb {
    pub const PARAMS: &'static [(&'static str, f64, &'static str)] = &[
        ("open_hhmm", 845.0, "區間起點 = 盤開 (08:45)"),
        ("session_end", 1345.0, "盤收時間 (13:45)"),
        ("range_min", 15.0, "區間長度 (分鐘)"),
        ("buffer", 2.0, "突破緩衝 (點)"),
        ("stop_frac", 1.0, "停損 = 區間寬 × N (1 = 區間另一側)"),
        ("rr", 2.0, "停利 = 風險 × N (0 = 持有到出場時間)"),
        ("exit_hhmm", 1340.0, "強制平倉時間"),
        ("min_range", 20.0, "區間太窄不做 (點)"),
        ("max_range", 250.0, "區間太寬不做 (點)"),
        ("max_trades", 1.0, "每日最多交易次數"),
        ("qty", 1.0, "固定口數"),
        ("risk_pct", 0.0, ">0 時以權益 % 風險計算口數"),
    ];

    pub fn new(p: &Params) -> Self {
        Self {
            open_min: hhmm_to_min(p.get("open_hhmm", 845.0) as u32),
            session_end_min: hhmm_to_min(p.get("session_end", 1345.0) as u32),
            range_min: p.usize("range_min", 15) as u32,
            buffer: p.get("buffer", 2.0),
            stop_frac: p.get("stop_frac", 1.0),
            rr: p.get("rr", 2.0),
            exit_hhmm: p.get("exit_hhmm", 1340.0) as u32,
            min_range: p.get("min_range", 20.0),
            max_range: p.get("max_range", 250.0),
            max_trades: p.usize("max_trades", 1) as u32,
            qty: p.get("qty", 1.0) as i64,
            risk_pct: p.get("risk_pct", 0.0),
            days: DayTracker::calendar(),
            hi: f64::MIN,
            lo: f64::MAX,
            armed: false,
            done: false,
            trades: 0,
            rearm: false,
        }
    }

    fn arm(&mut self, ctx: &mut Ctx) {
        let width = self.hi - self.lo;
        if width < self.min_range || width > self.max_range {
            self.done = true;
            return;
        }
        let risk = (width + 2.0 * self.buffer) * self.stop_frac;
        let q = size(ctx, self.qty, self.risk_pct, risk, 50);
        if q <= 0 {
            self.done = true;
            return;
        }
        let br = Bracket { stop_dist: Some(risk), take_dist: (self.rr > 0.0).then_some(risk * self.rr) };
        let oco = ctx.new_oco();
        let l = ctx.submit(Side::Buy, q, OrderKind::Stop(self.hi + self.buffer), Tif::Rod, oco, "orb_long");
        ctx.attach_bracket(l, br);
        let s = ctx.submit(Side::Sell, q, OrderKind::Stop(self.lo - self.buffer), Tif::Rod, oco, "orb_short");
        ctx.attach_bracket(s, br);
        self.armed = true;
    }
}

impl Strategy for Orb {
    fn on_bar(&mut self, bar: &Bar, ctx: &mut Ctx) {
        if self.days.is_new_day(bar.ts) {
            self.hi = f64::MIN;
            self.lo = f64::MAX;
            self.armed = false;
            self.done = false;
            self.trades = 0;
            self.rearm = false;
            // a position left over from an early-close day is never carried into a new range
            if !ctx.is_flat() || ctx.has_working_orders() {
                ctx.flatten();
            }
        }
        let m = minute_of_day(bar.ts);
        let end = ctx.bar_end(bar);
        // only the regular (day) session matters to this strategy
        if !(self.open_min..self.session_end_min).contains(&m) {
            return;
        }
        if hhmm(end) >= self.exit_hhmm {
            if !ctx.is_flat() || ctx.has_working_orders() {
                ctx.flatten();
            }
            self.done = true;
            return;
        }
        if self.done {
            return;
        }
        let range_end = self.open_min + self.range_min;
        if m >= self.open_min && m < range_end {
            self.hi = self.hi.max(bar.high);
            self.lo = self.lo.min(bar.low);
        }
        if !self.armed && minute_of_day(end) >= range_end && self.hi > f64::MIN && ctx.is_flat() {
            self.arm(ctx);
        } else if self.rearm && ctx.is_flat() && !ctx.has_working_orders() {
            self.rearm = false;
            self.arm(ctx);
        }
    }

    fn on_fill(&mut self, _fill: &Fill, ctx: &mut Ctx) {
        if ctx.is_flat() && self.armed {
            self.trades += 1;
            if self.trades >= self.max_trades {
                self.done = true;
                ctx.cancel_all();
            } else {
                self.rearm = true;
            }
        }
    }
}
