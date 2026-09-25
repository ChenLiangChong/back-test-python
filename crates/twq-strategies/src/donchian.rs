//! Donchian channel breakout (唐奇安通道 / 海龜) with ATR and channel trailing stops.

use twq_core::indicators::{Atr, Highest, Lowest};
use twq_core::{Bar, Ctx, OrderKind, Params, Side, Strategy, Tif};

use crate::common::{near_session_close, size, SessionFilter};

pub struct Donchian {
    entry_hi: Highest,
    entry_lo: Lowest,
    exit_hi: Highest,
    exit_lo: Lowest,
    atr: Atr,
    atr_stop: f64,
    qty: i64,
    risk_pct: f64,
    session: SessionFilter,
    flat_at_close: bool,
}

impl Donchian {
    pub const PARAMS: &'static [(&'static str, f64, &'static str)] = &[
        ("entry_n", 20.0, "突破通道週期"),
        ("exit_n", 10.0, "出場通道週期"),
        ("atr_n", 20.0, "ATR 週期"),
        ("atr_stop", 2.0, "初始停損 N × ATR"),
        ("qty", 1.0, "固定口數"),
        ("risk_pct", 0.0, ">0 時以權益 % 風險計算口數"),
        ("session", 0.0, "0 全部 / 1 日盤 / 2 夜盤"),
        ("flat_at_close", 0.0, "1 = 收盤前平倉"),
    ];

    pub fn new(p: &Params) -> Self {
        let en = p.usize("entry_n", 20);
        let ex = p.usize("exit_n", 10);
        Self {
            entry_hi: Highest::new(en),
            entry_lo: Lowest::new(en),
            exit_hi: Highest::new(ex),
            exit_lo: Lowest::new(ex),
            atr: Atr::new(p.usize("atr_n", 20)),
            atr_stop: p.get("atr_stop", 2.0),
            qty: p.get("qty", 1.0) as i64,
            risk_pct: p.get("risk_pct", 0.0),
            session: SessionFilter::from_params(p, 0.0),
            flat_at_close: p.flag("flat_at_close", false),
        }
    }
}

impl Strategy for Donchian {
    fn on_bar(&mut self, bar: &Bar, ctx: &mut Ctx) {
        let atr = self.atr.update(bar);
        let (eh, el) = (self.entry_hi.update(bar.high), self.entry_lo.update(bar.low));
        let (xh, xl) = (self.exit_hi.update(bar.high), self.exit_lo.update(bar.low));
        let (Some(atr), Some(eh), Some(el), Some(xh), Some(xl)) = (atr, eh, el, xh, xl) else { return };
        if self.flat_at_close && near_session_close(bar.ts, ctx.bar_end(bar), 5) {
            if !ctx.is_flat() || ctx.has_working_orders() {
                ctx.flatten();
            }
            return;
        }
        let tick = ctx.tick_size(bar.close);
        // re-quote every bar: cancel and replace (stops are engine-local in live mode,
        // so this creates no broker traffic)
        ctx.cancel_all();
        let pos = ctx.position();
        if pos > 0 {
            let stop = xl.max(ctx.avg_price() - self.atr_stop * atr);
            ctx.submit(Side::Sell, pos, OrderKind::Stop(stop - tick), Tif::Rod, 0, "dc_exit");
        } else if pos < 0 {
            let stop = xh.min(ctx.avg_price() + self.atr_stop * atr);
            ctx.submit(Side::Buy, -pos, OrderKind::Stop(stop + tick), Tif::Rod, 0, "dc_exit");
        } else if self.session.allows(ctx.bar_end(bar)) {
            let q = size(ctx, self.qty, self.risk_pct, self.atr_stop * atr, 50);
            if q > 0 {
                let oco = ctx.new_oco();
                ctx.submit(Side::Buy, q, OrderKind::Stop(eh + tick), Tif::Rod, oco, "dc_long");
                ctx.submit(Side::Sell, q, OrderKind::Stop(el - tick), Tif::Rod, oco, "dc_short");
            }
        }
    }
}
