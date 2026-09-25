//! Bollinger band mean reversion (布林通道均值回歸) with a KD filter — a codified
//! version of "range scalping": fade excursions outside the band, exit at the mid band.

use twq_core::indicators::{Atr, Bollinger, Kd};
use twq_core::{Bar, Bracket, Ctx, OrderKind, Params, Side, Strategy, Tif};

use crate::common::{near_session_close, SessionFilter};

pub struct BbRevert {
    bb: Bollinger,
    kd: Kd,
    atr: Atr,
    prev_close: f64,
    prev_lower: f64,
    prev_upper: f64,
    kd_lo: f64,
    kd_hi: f64,
    atr_stop: f64,
    qty: i64,
    session: SessionFilter,
    flat_at_close: bool,
}

impl BbRevert {
    pub const PARAMS: &'static [(&'static str, f64, &'static str)] = &[
        ("n", 20.0, "布林週期"),
        ("k", 2.0, "標準差倍數"),
        ("kd_lo", 25.0, "做多需 K < 此值"),
        ("kd_hi", 75.0, "做空需 K > 此值"),
        ("atr_n", 14.0, "ATR 週期"),
        ("atr_stop", 2.0, "停損 N × ATR"),
        ("qty", 1.0, "口數"),
        ("session", 1.0, "0 全部 / 1 日盤 / 2 夜盤"),
        ("flat_at_close", 1.0, "1 = 收盤前平倉"),
    ];

    pub fn new(p: &Params) -> Self {
        Self {
            bb: Bollinger::new(p.usize("n", 20), p.get("k", 2.0)),
            kd: Kd::new(9, 3, 3),
            atr: Atr::new(p.usize("atr_n", 14)),
            prev_close: f64::NAN,
            prev_lower: f64::NAN,
            prev_upper: f64::NAN,
            kd_lo: p.get("kd_lo", 25.0),
            kd_hi: p.get("kd_hi", 75.0),
            atr_stop: p.get("atr_stop", 2.0),
            qty: p.get("qty", 1.0) as i64,
            session: SessionFilter::from_params(p, 1.0),
            flat_at_close: p.flag("flat_at_close", true),
        }
    }
}

impl Strategy for BbRevert {
    fn on_bar(&mut self, bar: &Bar, ctx: &mut Ctx) {
        let atr = self.atr.update(bar);
        let kd = self.kd.update(bar);
        let Some(b) = self.bb.update(bar.close) else { return };
        let (pc, pl, pu) = (self.prev_close, self.prev_lower, self.prev_upper);
        self.prev_close = bar.close;
        self.prev_lower = b.lower;
        self.prev_upper = b.upper;
        let (Some(atr), Some((k, _d))) = (atr, kd) else { return };
        if self.flat_at_close && near_session_close(bar.ts, ctx.bar_end(bar), 5) {
            if !ctx.is_flat() || ctx.has_working_orders() {
                ctx.flatten();
            }
            return;
        }
        let pos = ctx.position();
        if pos != 0 {
            // move the take-profit to the current mid band
            let tp_id = ctx.working_orders().iter().find(|o| o.tag == "bb_tp").map(|o| o.id);
            if let Some(id) = tp_id {
                ctx.cancel(id);
            }
            let oco = ctx.working_orders().iter().find(|o| o.tag == "sl").map(|o| o.oco).unwrap_or(0);
            let side = if pos > 0 { Side::Sell } else { Side::Buy };
            ctx.submit(side, pos.abs(), OrderKind::Limit(b.mid), Tif::Rod, oco, "bb_tp");
            return;
        }
        if !self.session.allows(ctx.bar_end(bar)) || ctx.has_working_orders() || pc.is_nan() {
            return;
        }
        let br = Bracket { stop_dist: Some(self.atr_stop * atr), take_dist: None };
        if pc < pl && bar.close > b.lower && k < self.kd_lo {
            ctx.enter(Side::Buy, self.qty, OrderKind::Market, br, "bb_long");
        } else if pc > pu && bar.close < b.upper && k > self.kd_hi {
            ctx.enter(Side::Sell, self.qty, OrderKind::Market, br, "bb_short");
        }
    }
}
