//! Risk overlays that wrap any strategy.
//!
//! `DailyLossGuard` implements 馬克羊's 「計程車司機法則」(taxi-driver rule: go home early on
//! bad days): once the day's P&L drops below `-max_daily_loss`, flatten and switch the
//! context to reduce-only until the next trading day. Winning days are never capped.

use twq_core::{Bar, Ctx, Fill, OrderUpdate, Strategy, Tick};

pub struct DailyLossGuard<S> {
    inner: S,
    limit: f64,
    day: i64,
    day_start_equity: f64,
    halted: bool,
    pub halts: u32,
}

impl<S: Strategy> DailyLossGuard<S> {
    pub fn new(inner: S, max_daily_loss: f64) -> Self {
        Self { inner, limit: max_daily_loss.abs(), day: i64::MIN, day_start_equity: 0.0, halted: false, halts: 0 }
    }

    #[inline]
    fn check(&mut self, ctx: &mut Ctx) {
        let d = ctx.trading_day();
        if d != self.day {
            self.day = d;
            self.day_start_equity = ctx.equity();
            if self.halted {
                self.halted = false;
                ctx.engine_set_reduce_only(false);
            }
        }
        if !self.halted && ctx.equity() - self.day_start_equity <= -self.limit {
            self.halted = true;
            self.halts += 1;
            ctx.flatten();
            ctx.engine_set_reduce_only(true);
        }
    }
}

impl<S: Strategy> Strategy for DailyLossGuard<S> {
    fn on_bar(&mut self, bar: &Bar, ctx: &mut Ctx) {
        self.check(ctx);
        if !self.halted {
            self.inner.on_bar(bar, ctx);
        }
    }
    fn on_tick(&mut self, tick: &Tick, ctx: &mut Ctx) {
        self.check(ctx);
        if !self.halted {
            self.inner.on_tick(tick, ctx);
        }
    }
    fn on_fill(&mut self, fill: &Fill, ctx: &mut Ctx) {
        self.inner.on_fill(fill, ctx);
        self.check(ctx);
    }
    fn on_order_update(&mut self, u: &OrderUpdate, ctx: &mut Ctx) {
        self.inner.on_order_update(u, ctx);
    }
    fn wants_ticks(&self) -> bool {
        self.inner.wants_ticks()
    }
}
