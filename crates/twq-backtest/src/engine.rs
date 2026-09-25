//! Event-driven backtest engine (bar mode and tick mode).
//!
//! Both modes use the same `SimExchange`, `Portfolio` and `Ctx` as paper / live
//! trading. The hot loop is monomorphised over the strategy type, performs no heap
//! allocation per bar, and skips the matcher entirely when no orders are working.

use std::time::{Duration, Instant};

use twq_core::aggregator::BarAggregator;
use twq_core::time::Ts;
use twq_core::{
    Action, Bar, BarPath, Ctx, Exec, Instrument, OrderId, OrderStatus, OrderUpdate, Portfolio, SimConfig, SimExchange,
    Strategy, Tick, Trade,
};

use crate::metrics::{Stats, StatsBuilder};

#[derive(Clone, Debug)]
pub struct BacktestConfig {
    pub instrument: Instrument,
    pub sim: SimConfig,
    pub initial_capital: f64,
    /// Bar timeframe in microseconds; 0 = auto-detect (bar mode) / 1 minute (tick mode).
    pub bar_period: i64,
    /// Orders are suppressed before this timestamp (indicator warm-up for walk-forward).
    pub trade_from: Ts,
    /// Store equity at every bar close (for charts). Daily equity is always kept.
    pub record_equity: bool,
}

impl BacktestConfig {
    pub fn new(instrument: Instrument) -> Self {
        Self {
            instrument,
            sim: SimConfig::default(),
            initial_capital: 2_000_000.0,
            bar_period: 0,
            trade_from: Ts::MIN,
            record_equity: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BacktestResult {
    pub stats: Stats,
    pub trades: Vec<Trade>,
    /// (trading day, closing equity)
    pub daily_equity: Vec<(i64, f64)>,
    /// (bar close time, equity) when `record_equity` is set.
    pub equity_curve: Vec<(Ts, f64)>,
    pub elapsed: Duration,
    pub events: usize,
}

/// Most common spacing between consecutive bars (robust to session gaps).
pub fn detect_period(bars: &[Bar]) -> i64 {
    let mut counts: Vec<(i64, usize)> = Vec::new();
    for w in bars.windows(2).take(5_000) {
        let d = w[1].ts - w[0].ts;
        if d <= 0 {
            continue;
        }
        match counts.iter_mut().find(|c| c.0 == d) {
            Some(c) => c.1 += 1,
            None => counts.push((d, 1)),
        }
    }
    counts.into_iter().max_by_key(|c| c.1).map(|c| c.0).unwrap_or(twq_core::time::US_PER_MIN)
}

struct Engine<'a, S: Strategy + ?Sized> {
    strat: &'a mut S,
    ctx: Ctx,
    ex: SimExchange,
    pf: Portfolio,
    stats: StatsBuilder,
    actions: Vec<Action>,
    cancelled: Vec<OrderId>,
    initial: f64,
    trade_from: Ts,
    fills: usize,
    stats_started: bool,
    equity_curve: Vec<(Ts, f64)>,
    record_equity: bool,
    ruined: bool,
}

impl<'a, S: Strategy + ?Sized> Engine<'a, S> {
    fn new(strat: &'a mut S, cfg: &BacktestConfig, period: i64) -> Self {
        Self {
            strat,
            ctx: Ctx::for_instrument(period, &cfg.instrument),
            ex: SimExchange::new(cfg.sim, cfg.instrument.clone()),
            pf: Portfolio::new(cfg.instrument.clone()),
            stats: StatsBuilder::new(cfg.initial_capital),
            actions: Vec::with_capacity(16),
            cancelled: Vec::with_capacity(8),
            initial: cfg.initial_capital,
            trade_from: cfg.trade_from,
            fills: 0,
            stats_started: cfg.trade_from == Ts::MIN,
            equity_curve: Vec::new(),
            record_equity: cfg.record_equity,
            ruined: false,
        }
    }

    #[inline]
    fn equity(&self, mark: f64) -> f64 {
        self.initial + self.pf.realized_net() + self.pf.unrealized(mark)
    }

    #[inline]
    fn sync_account(&mut self, mark: f64) {
        let eq = self.equity(mark);
        self.ctx.engine_sync_account(self.pf.position(), self.pf.avg_price(), self.pf.realized_net(), eq);
    }

    #[inline]
    fn forward_cancels(&mut self) {
        self.ex.take_cancelled(&mut self.cancelled);
        for i in 0..self.cancelled.len() {
            let id = self.cancelled[i];
            self.ctx.engine_order_closed(id);
            let u = OrderUpdate { order_id: id, ts: self.ctx.now(), status: OrderStatus::Cancelled };
            self.strat.on_order_update(&u, &mut self.ctx);
        }
    }

    #[inline]
    fn process_actions(&mut self) {
        while self.ctx.engine_has_actions() {
            let mut acts = std::mem::take(&mut self.actions);
            self.ctx.engine_drain(&mut acts);
            let warmup = self.ctx.now() < self.trade_from;
            for a in acts.iter() {
                match *a {
                    Action::Submit(o) => {
                        if warmup {
                            self.ctx.engine_order_closed(o.id);
                        } else {
                            self.ex.submit(o);
                        }
                    }
                    Action::Cancel(id) => {
                        if self.ex.cancel(id) {
                            self.ctx.engine_order_closed(id);
                        }
                    }
                    Action::CancelAll => {
                        let mut ids = std::mem::take(&mut self.cancelled);
                        ids.clear();
                        self.ex.cancel_all_into(&mut ids);
                        for &id in &ids {
                            self.ctx.engine_order_closed(id);
                        }
                        self.cancelled = ids;
                    }
                }
            }
            self.actions = acts;
        }
    }

    #[inline]
    fn on_exec(&mut self, e: Exec) {
        let o = e.order;
        let fill = self.pf.execute(o.id, e.ts, o.side, o.qty, e.price, o.tag);
        self.fills += 1;
        self.ctx.engine_on_fill(&fill);
        self.sync_account(e.price);
        self.forward_cancels();
        self.strat.on_fill(&fill, &mut self.ctx);
        self.process_actions();
    }

    #[inline]
    fn mark(&mut self, ts: Ts, price: f64) {
        if !self.stats_started {
            if ts < self.trade_from {
                return;
            }
            self.stats_started = true;
            let eq = self.equity(price);
            self.stats.rebase(eq);
        }
        let eq = self.equity(price);
        self.stats.on_mark(ts, eq, self.pf.position() != 0);
        if self.record_equity {
            self.equity_curve.push((ts, eq));
        }
    }

    #[inline]
    fn close_bar(&mut self, bar: &Bar) {
        let end = bar.ts + self.ctx.bar_period();
        self.ctx.engine_set_clock(end, bar.close);
        self.sync_account(bar.close);
        self.mark(end, bar.close);
        if self.ruined {
            return;
        }
        // Account blown (equity <= 0): liquidate like a margin call and stop trading.
        if self.ctx.equity() <= 0.0 {
            self.ruined = true;
            self.ctx.engine_set_reduce_only(true);
            self.ctx.flatten();
            self.process_actions();
            return;
        }
        self.strat.on_bar(bar, &mut self.ctx);
        self.process_actions();
    }

    fn finish(mut self, start: Instant, events: usize) -> BacktestResult {
        let trades = self.pf.take_trades();
        let trades: Vec<Trade> = if self.trade_from == Ts::MIN {
            trades
        } else {
            trades.into_iter().filter(|t| t.entry_ts >= self.trade_from).collect()
        };
        let mut stats = self.stats.finish(&trades, self.pf.fees(), self.pf.taxes(), self.fills);
        stats.ruined = self.ruined;
        BacktestResult {
            stats,
            daily_equity: self.stats.daily().to_vec(),
            trades,
            equity_curve: self.equity_curve,
            elapsed: start.elapsed(),
            events,
        }
    }
}

/// Run a strategy over bars. Orders placed in `on_bar` become active on the next bar.
pub fn run_bars<S: Strategy + ?Sized>(bars: &[Bar], strat: &mut S, cfg: &BacktestConfig) -> BacktestResult {
    let start = Instant::now();
    let period = if cfg.bar_period > 0 { cfg.bar_period } else { detect_period(bars) };
    let mut eng = Engine::new(strat, cfg, period);
    for bar in bars {
        if !eng.ex.is_empty() {
            eng.ctx.engine_set_clock(bar.ts, bar.open);
            let mut path = BarPath::new(bar);
            while let Some(e) = eng.ex.next_exec_on_path(&mut path) {
                eng.ctx.engine_set_clock(bar.ts, e.price);
                eng.on_exec(e);
            }
            eng.forward_cancels();
        }
        eng.close_bar(bar);
    }
    eng.finish(start, bars.len())
}

/// Run a strategy over trade ticks; bars of `cfg.bar_period` (default 1 min) are built
/// on the fly exactly like the live runner does.
pub fn run_ticks<S: Strategy + ?Sized>(ticks: &[Tick], strat: &mut S, cfg: &BacktestConfig) -> BacktestResult {
    let start = Instant::now();
    let period = if cfg.bar_period > 0 { cfg.bar_period } else { twq_core::time::US_PER_MIN };
    let wants_ticks = strat.wants_ticks();
    let mut eng = Engine::new(strat, cfg, period);
    let mut agg = BarAggregator::new(period);
    let mut execs: Vec<Exec> = Vec::with_capacity(8);
    for t in ticks {
        if let Some(done) = agg.update(t) {
            eng.close_bar(&done);
        }
        eng.ctx.engine_set_clock(t.ts, t.price);
        if !eng.ex.is_empty() {
            execs.clear();
            eng.ex.match_tick(t, &mut execs);
            for &e in &execs {
                eng.on_exec(e);
            }
            eng.forward_cancels();
        }
        if wants_ticks && !eng.ruined {
            eng.sync_account(t.price);
            eng.strat.on_tick(t, &mut eng.ctx);
            eng.process_actions();
        }
    }
    if let Some(done) = agg.flush() {
        eng.close_bar(&done);
    }
    eng.finish(start, ticks.len())
}
