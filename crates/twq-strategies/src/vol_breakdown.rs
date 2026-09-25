//! 爆量紅K破低 → 放空 (馬克羊, 夜盤當沖影片 vIk4yxI_ZTw 的說明欄:
//! 「爆大量紅K低點 -1tick 跌破 → 空單進場 停損紅K」).
//!
//! Sourced from the creator: the trigger (a large-volume up bar, sell when its low is
//! broken by one tick) and the stop (the red bar). Everything else is our
//! interpretation and exposed as parameters: the volume threshold, how long the setup
//! stays valid, the target / trailing exit, session filter and sizing. `mirror=1` adds
//! the symmetric long setup (large-volume down bar, buy above its high), which the
//! creator did not state.

use twq_core::indicators::{Highest, Lowest, Sma};
use twq_core::{Bar, Bracket, Ctx, Fill, OrderKind, Params, Side, Strategy, Tif};

use crate::common::{near_session_close, size, SessionFilter};

pub struct VolBreakdown {
    vol_sma: Sma,
    hh: Option<Highest>,
    ll: Option<Lowest>,
    vol_mult: f64,
    confirm_n: usize,
    valid_bars: u32,
    rr: f64,
    trail_n: usize,
    trail_hi: Option<Highest>,
    trail_lo: Option<Lowest>,
    max_stop: f64,
    qty: i64,
    risk_pct: f64,
    mirror: bool,
    session: SessionFilter,
    // state
    pending_age: u32,
    prev_hh: Option<f64>,
    prev_ll: Option<f64>,
    trail_ready: (Option<f64>, Option<f64>),
}

impl VolBreakdown {
    pub const PARAMS: &'static [(&'static str, f64, &'static str)] = &[
        ("vol_n", 20.0, "均量週期 N"),
        ("vol_mult", 3.0, "爆量 = 量 ≥ N 根均量 × 倍數"),
        ("confirm_n", 0.0, ">0: 紅K高點需創 N 根新高 (力竭確認)"),
        ("valid_bars", 5.0, "破低單有效 K 棒數"),
        ("rr", 2.0, "停利 = 風險 × N (0 = 不設)"),
        ("trail_n", 0.0, ">0: 以最近 N 根高點移動停損"),
        ("max_stop", 80.0, "紅K太長 (停損點數過大) 不做"),
        ("qty", 1.0, "固定口數"),
        ("risk_pct", 0.0, ">0 時以權益 % 風險計算口數"),
        ("mirror", 0.0, "1 = 也做對稱多單 (爆量黑K破高)"),
        ("session", 2.0, "0 全部 / 1 日盤 / 2 夜盤"),
    ];

    pub fn new(p: &Params) -> Self {
        let confirm_n = p.usize("confirm_n", 0);
        let trail_n = p.usize("trail_n", 0);
        Self {
            vol_sma: Sma::new(p.usize("vol_n", 20)),
            hh: (confirm_n > 0).then(|| Highest::new(confirm_n)),
            ll: (confirm_n > 0).then(|| Lowest::new(confirm_n)),
            vol_mult: p.get("vol_mult", 3.0),
            confirm_n,
            valid_bars: p.usize("valid_bars", 5) as u32,
            rr: p.get("rr", 2.0),
            trail_n,
            trail_hi: (trail_n > 0).then(|| Highest::new(trail_n)),
            trail_lo: (trail_n > 0).then(|| Lowest::new(trail_n)),
            max_stop: p.get("max_stop", 80.0),
            qty: p.get("qty", 1.0) as i64,
            risk_pct: p.get("risk_pct", 0.0),
            mirror: p.flag("mirror", false),
            session: SessionFilter::from_params(p, 2.0),
            pending_age: 0,
            prev_hh: None,
            prev_ll: None,
            trail_ready: (None, None),
        }
    }

    fn entry_pending(ctx: &Ctx) -> bool {
        ctx.working_orders().iter().any(|o| o.tag == "vbd_short" || o.tag == "vbd_long")
    }
}

impl Strategy for VolBreakdown {
    fn on_bar(&mut self, bar: &Bar, ctx: &mut Ctx) {
        let avg_vol = self.vol_sma.value();
        self.vol_sma.update(bar.volume);
        let (phh, pll) = (self.prev_hh, self.prev_ll);
        self.prev_hh = self.hh.as_mut().and_then(|h| h.update(bar.high));
        self.prev_ll = self.ll.as_mut().and_then(|l| l.update(bar.low));
        if let (Some(h), Some(l)) = (self.trail_hi.as_mut(), self.trail_lo.as_mut()) {
            self.trail_ready = (h.update(bar.high), l.update(bar.low));
        }

        let end = ctx.bar_end(bar);
        if near_session_close(bar.ts, end, 5) {
            if !ctx.is_flat() || ctx.has_working_orders() {
                ctx.flatten();
            }
            return;
        }

        // expire a stale breakout order
        if Self::entry_pending(ctx) {
            self.pending_age += 1;
            if self.pending_age > self.valid_bars && ctx.is_flat() {
                ctx.cancel_all();
            }
        }

        // trailing stop for an open position (replaces the bracket stop)
        let pos = ctx.position();
        if pos != 0 && self.trail_n > 0 {
            let tick = ctx.tick_size(bar.close);
            let (side, level) = if pos < 0 {
                (Side::Buy, self.trail_ready.0.map(|h| h + tick))
            } else {
                (Side::Sell, self.trail_ready.1.map(|l| l - tick))
            };
            if let Some(level) = level {
                let sl = ctx.working_orders().iter().find(|o| o.tag == "sl").copied();
                if let Some(sl) = sl {
                    if let OrderKind::Stop(cur) = sl.kind {
                        let tighter = if pos < 0 { level < cur } else { level > cur };
                        if tighter {
                            ctx.cancel(sl.id);
                            ctx.submit(side, pos.abs(), OrderKind::Stop(level), Tif::Rod, sl.oco, "sl");
                        }
                    }
                }
            }
            return;
        }

        if pos != 0 || Self::entry_pending(ctx) || !self.session.allows(end) {
            return;
        }
        let Some(avg) = avg_vol else { return };
        if avg <= 0.0 || bar.volume < self.vol_mult * avg {
            return;
        }
        let tick = ctx.tick_size(bar.close);
        let range = bar.high - bar.low;
        let risk = range + 2.0 * tick;
        if risk > self.max_stop || range <= 0.0 {
            return;
        }
        let q = size(ctx, self.qty, self.risk_pct, risk, 50);
        if q <= 0 {
            return;
        }
        let br = Bracket { stop_dist: Some(risk), take_dist: (self.rr > 0.0).then_some(risk * self.rr) };
        let red = bar.close > bar.open;
        let black = bar.close < bar.open;
        let exhausted_up = self.confirm_n == 0 || phh.is_some_and(|h| bar.high >= h);
        let exhausted_dn = self.confirm_n == 0 || pll.is_some_and(|l| bar.low <= l);
        if red && exhausted_up {
            let id = ctx.submit(Side::Sell, q, OrderKind::Stop(bar.low - tick), Tif::Rod, 0, "vbd_short");
            ctx.attach_bracket(id, br);
            self.pending_age = 0;
        } else if self.mirror && black && exhausted_dn {
            let id = ctx.submit(Side::Buy, q, OrderKind::Stop(bar.high + tick), Tif::Rod, 0, "vbd_long");
            ctx.attach_bracket(id, br);
            self.pending_age = 0;
        }
    }

    fn on_fill(&mut self, _fill: &Fill, _ctx: &mut Ctx) {
        self.pending_age = 0;
    }
}
