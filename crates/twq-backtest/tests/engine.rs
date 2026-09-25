//! Deterministic engine checks with hand-computable P&L.

use twq_backtest::{run_bars, run_ticks, BacktestConfig};
use twq_core::time::{make_ts, US_PER_MIN};
use twq_core::{Bar, Bracket, Ctx, Instrument, OrderKind, Side, SimConfig, Strategy, Tick};

fn bar(i: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    Bar { ts: make_ts(2026, 9, 1, 9, 0, 0, 0) + i * US_PER_MIN, open: o, high: h, low: l, close: c, volume: 10.0 }
}

fn cfg(slip: f64) -> BacktestConfig {
    let mut c = BacktestConfig::new(Instrument::tx().with_commission(0.0));
    c.sim = SimConfig { slippage_ticks: slip, limit_fill_on_touch: false };
    c.bar_period = US_PER_MIN;
    c
}

/// Buys at the first bar close with a bracket, never trades again.
struct OneShot {
    done: bool,
    br: Bracket,
}

impl Strategy for OneShot {
    fn on_bar(&mut self, _bar: &Bar, ctx: &mut Ctx) {
        if !self.done {
            self.done = true;
            ctx.enter(Side::Buy, 1, OrderKind::Market, self.br, "entry");
        }
    }
}

#[test]
fn market_entry_fills_next_open_and_take_profit_hits() {
    let bars = vec![
        bar(0, 100.0, 101.0, 99.0, 100.0),
        bar(1, 102.0, 103.0, 101.0, 103.0), // entry @ open 102
        bar(2, 103.0, 125.0, 102.0, 124.0), // TP 102+20=122 hit (open nearer low -> L then H)
        bar(3, 124.0, 124.0, 90.0, 95.0),
    ];
    let mut s = OneShot { done: false, br: Bracket { stop_dist: Some(10.0), take_dist: Some(20.0) } };
    let r = run_bars(&bars, &mut s, &cfg(0.0));
    assert_eq!(r.trades.len(), 1);
    let t = &r.trades[0];
    assert_eq!(t.entry_price, 102.0);
    assert_eq!(t.exit_price, 122.0);
    assert_eq!(t.gross_pnl, 20.0 * 200.0);
    // futures tax both sides: round(102*200*2e-5)=0 and round(122*200*2e-5)=0 at these toy prices
    assert_eq!(r.stats.fills, 2);
}

#[test]
fn stop_loss_same_bar_as_entry_is_honoured() {
    // entry fills at the open of bar 1; the same bar then trades down through the stop
    let bars = vec![
        bar(0, 100.0, 101.0, 99.0, 100.0),
        bar(1, 100.0, 100.5, 80.0, 81.0), // open nearer high: O -> H -> L; SL 90 hit in-bar
    ];
    let mut s = OneShot { done: false, br: Bracket { stop_dist: Some(10.0), take_dist: None } };
    let r = run_bars(&bars, &mut s, &cfg(1.0));
    assert_eq!(r.trades.len(), 1);
    let t = &r.trades[0];
    assert_eq!(t.entry_price, 101.0); // 1 tick slippage
    assert_eq!(t.exit_price, 90.0); // stop at 101-10=91, minus 1 tick slippage
    assert_eq!(t.exit_tag, "sl");
}

#[test]
fn tick_mode_agrees_with_bar_mode_on_simple_path() {
    let bars = vec![
        bar(0, 100.0, 101.0, 99.0, 100.0),
        bar(1, 102.0, 103.0, 101.0, 103.0),
        bar(2, 103.0, 125.0, 102.0, 124.0),
        bar(3, 124.0, 124.0, 90.0, 95.0),
    ];
    let ticks: Vec<Tick> = twq_core::aggregator::bars_to_ticks(&bars, US_PER_MIN);
    let mut s1 = OneShot { done: false, br: Bracket { stop_dist: Some(10.0), take_dist: Some(20.0) } };
    let mut s2 = OneShot { done: false, br: Bracket { stop_dist: Some(10.0), take_dist: Some(20.0) } };
    let a = run_bars(&bars, &mut s1, &cfg(0.0));
    let b = run_ticks(&ticks, &mut s2, &cfg(0.0));
    assert_eq!(a.trades.len(), b.trades.len());
    assert_eq!(a.trades[0].entry_price, b.trades[0].entry_price);
    assert_eq!(a.trades[0].exit_price, b.trades[0].exit_price);
}

#[test]
fn no_lookahead_on_random_walk() {
    // With zero costs the expected P&L of any causal strategy on a driftless random walk is
    // ~0; a look-ahead bug would show up as a large systematic profit.
    let bars = twq_core::data::synth_futures_bars(&twq_core::data::SynthConfig {
        days: 120,
        include_night: false,
        seed: 99,
        ..Default::default()
    });
    let mut c = cfg(0.0);
    c.instrument = Instrument::future("X", 1.0, 0.0);
    if let twq_core::FeeModel::Futures { tax_rate, .. } = &mut c.instrument.fees {
        *tax_rate = 0.0;
    }
    let mut total = 0.0;
    let mut gross_abs = 0.0;
    for (f, sl) in [(3, 10), (5, 20), (10, 40), (20, 80)] {
        let p = twq_core::Params::new().with("fast", f as f64).with("slow", sl as f64);
        let mut s = twq_strategies_stub::ma(p);
        let r = run_bars(&bars, &mut s, &c);
        total += r.stats.net_pnl;
        gross_abs += r.trades.iter().map(|t| t.gross_pnl.abs()).sum::<f64>();
    }
    assert!(total.abs() < 0.15 * gross_abs, "suspicious edge on random data: {total} of {gross_abs}");
}

/// Minimal MA-cross used above (kept local so this crate's tests don't depend on the
/// strategy crate).
mod twq_strategies_stub {
    use twq_core::indicators::{Cross, CrossEvent, Sma};
    use twq_core::{Bar, Ctx, Params, Strategy};

    pub struct Ma {
        f: Sma,
        s: Sma,
        c: Cross,
    }

    pub fn ma(p: Params) -> Ma {
        Ma { f: Sma::new(p.usize("fast", 5)), s: Sma::new(p.usize("slow", 20)), c: Cross::default() }
    }

    impl Strategy for Ma {
        fn on_bar(&mut self, bar: &Bar, ctx: &mut Ctx) {
            let (Some(a), Some(b)) = (self.f.update(bar.close), self.s.update(bar.close)) else { return };
            match self.c.update(a, b) {
                Some(CrossEvent::Above) => {
                    ctx.target_position(1);
                }
                Some(CrossEvent::Below) => {
                    ctx.target_position(-1);
                }
                None => {}
            }
        }
    }
}
