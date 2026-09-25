//! Dual moving-average crossover (golden / death cross) — the baseline trend strategy.

use twq_core::indicators::{Atr, Cross, CrossEvent, Ema, Sma};
use twq_core::{Bar, Bracket, Ctx, OrderKind, Params, Side, Strategy};

use crate::common::{near_session_close, SessionFilter};

enum Ma {
    S(Sma),
    E(Ema),
}

impl Ma {
    fn new(n: usize, ema: bool) -> Self {
        if ema {
            Ma::E(Ema::new(n))
        } else {
            Ma::S(Sma::new(n))
        }
    }
    #[inline]
    fn update(&mut self, x: f64) -> Option<f64> {
        match self {
            Ma::S(m) => m.update(x),
            Ma::E(m) => m.update(x),
        }
    }
}

pub struct MaCross {
    fast: Ma,
    slow: Ma,
    cross: Cross,
    atr: Atr,
    qty: i64,
    atr_stop: f64,
    long_only: bool,
    session: SessionFilter,
    flat_at_close: bool,
}

impl MaCross {
    pub const PARAMS: &'static [(&'static str, f64, &'static str)] = &[
        ("fast", 10.0, "快線週期"),
        ("slow", 30.0, "慢線週期"),
        ("ema", 0.0, "1 = 用 EMA, 0 = SMA"),
        ("qty", 1.0, "口數"),
        ("atr_n", 14.0, "ATR 週期"),
        ("atr_stop", 0.0, "停損 = N × ATR (0 = 不設)"),
        ("long_only", 0.0, "1 = 只做多"),
        ("session", 0.0, "0 全部 / 1 日盤 / 2 夜盤"),
        ("flat_at_close", 0.0, "1 = 收盤前平倉 (不留倉)"),
    ];

    pub fn new(p: &Params) -> Self {
        let ema = p.flag("ema", false);
        Self {
            fast: Ma::new(p.usize("fast", 10), ema),
            slow: Ma::new(p.usize("slow", 30), ema),
            cross: Cross::default(),
            atr: Atr::new(p.usize("atr_n", 14)),
            qty: p.get("qty", 1.0) as i64,
            atr_stop: p.get("atr_stop", 0.0),
            long_only: p.flag("long_only", false),
            session: SessionFilter::from_params(p, 0.0),
            flat_at_close: p.flag("flat_at_close", false),
        }
    }
}

impl Strategy for MaCross {
    fn on_bar(&mut self, bar: &Bar, ctx: &mut Ctx) {
        let atr = self.atr.update(bar);
        let (f, s) = (self.fast.update(bar.close), self.slow.update(bar.close));
        let (Some(f), Some(s)) = (f, s) else { return };
        let ev = self.cross.update(f, s);
        if self.flat_at_close && near_session_close(bar.ts, ctx.bar_end(bar), 5) {
            if !ctx.is_flat() || ctx.has_working_orders() {
                ctx.flatten();
            }
            return;
        }
        if !self.session.allows(bar.ts) {
            return;
        }
        let want = match ev {
            Some(CrossEvent::Above) => self.qty,
            Some(CrossEvent::Below) if self.long_only => 0,
            Some(CrossEvent::Below) => -self.qty,
            None => return,
        };
        if want == ctx.position() {
            return;
        }
        match atr {
            Some(a) if self.atr_stop > 0.0 && want != 0 => {
                ctx.flatten();
                let side = if want > 0 { Side::Buy } else { Side::Sell };
                let br = Bracket { stop_dist: Some(a * self.atr_stop), take_dist: None };
                ctx.enter(side, want.abs(), OrderKind::Market, br, "ma_cross");
            }
            _ => {
                ctx.cancel_all();
                ctx.target_position(want);
            }
        }
    }
}
