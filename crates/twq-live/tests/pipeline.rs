//! End-to-end: mock bridge (TCP) → live runner, in paper and live mode, compared with
//! the tick-level backtest of the same strategy on the same ticks.

use std::net::TcpListener;
use std::time::Duration;

use twq_backtest::{run_ticks, BacktestConfig};
use twq_core::data::{synth_futures_bars, synth_ticks_from_bars, SynthConfig};
use twq_core::time::US_PER_MIN;
use twq_core::{Instrument, Params, SimConfig, Tick};
use twq_live::{run_live, serve_one, BridgeConn, ExecMode, LiveConfig, LiveSummary, MockConfig, RiskLimits};

fn ticks() -> Vec<Tick> {
    let bars = synth_futures_bars(&SynthConfig { days: 3, include_night: false, seed: 11, ..Default::default() });
    synth_ticks_from_bars(&bars, 8, 5)
}

fn run(
    mode: ExecMode,
    strategy: &str,
    params: &str,
    ticks: Vec<Tick>,
    risk: RiskLimits,
) -> (LiveSummary, twq_live::MockStats) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let mcfg = MockConfig {
        ticks,
        symbol: "TX00".into(),
        speed: 0.0,
        lockstep: true,
        instrument: Instrument::tx(),
        sim: SimConfig { slippage_ticks: 1.0, limit_fill_on_touch: false },
    };
    let server = std::thread::spawn(move || serve_one(listener, mcfg).unwrap());
    let mut conn = BridgeConn::connect(addr, Duration::from_secs(5)).unwrap();
    let mut cfg = LiveConfig::new(Instrument::tx(), "TX00", mode);
    cfg.bar_period = US_PER_MIN;
    cfg.risk = risk;
    let mut s = twq_strategies::build(strategy, &Params::parse(params).unwrap()).unwrap();
    let sum = run_live(&mut s, &mut conn, &cfg).unwrap();
    let mock = server.join().unwrap();
    (sum, mock)
}

fn loose() -> RiskLimits {
    RiskLimits {
        max_position: 10,
        max_order_qty: 10,
        max_orders_per_sec: 10_000,
        max_orders_per_min: 1_000_000,
        ..Default::default()
    }
}

#[test]
fn paper_mode_matches_tick_backtest() {
    let t = ticks();
    let mut cfg = BacktestConfig::new(Instrument::tx());
    cfg.bar_period = US_PER_MIN;
    let mut s = twq_strategies::build("ma_cross", &Params::parse("fast=3,slow=8").unwrap()).unwrap();
    let bt = run_ticks(&t, &mut s, &cfg);
    let (paper, mock) = run(ExecMode::Paper, "ma_cross", "fast=3,slow=8", t, loose());
    assert!(bt.stats.trades > 20, "expected activity, got {}", bt.stats.trades);
    assert_eq!(mock.orders, 0, "paper mode must never send orders to the broker");
    assert_eq!(paper.rejects, 0);
    // identical engine + matcher => identical results
    assert_eq!(paper.trades.len(), bt.trades.len());
    let bt_net: f64 = bt.trades.iter().map(|t| t.net_pnl).sum();
    let pp_net: f64 = paper.trades.iter().map(|t| t.net_pnl).sum();
    assert!((bt_net - pp_net).abs() < 1e-6, "backtest {bt_net} vs paper {pp_net}");
}

#[test]
fn live_mode_routes_orders_through_bridge() {
    let t = ticks();
    let (live, mock) = run(ExecMode::Live, "ma_cross", "fast=3,slow=8", t, loose());
    assert!(live.orders_sent > 20);
    assert_eq!(mock.orders as u64, live.orders_sent);
    assert_eq!(live.rejects, 0);
    assert_eq!(live.fills, mock.fills as u64);
    // engine and broker agree on the final position (flattened on exit)
    assert_eq!(live.position, mock.position);
    assert_eq!(live.position, 0);
}

#[test]
fn live_mode_synthetic_stops_fire() {
    // ORB uses stop entries + bracket stops, all held locally in live mode
    let bars = synth_futures_bars(&SynthConfig { days: 6, include_night: false, seed: 3, ..Default::default() });
    let t = synth_ticks_from_bars(&bars, 8, 9);
    let (live, mock) = run(ExecMode::Live, "orb", "min_range=5", t, loose());
    assert!(live.orders_sent > 0, "stop orders should have triggered market orders");
    assert_eq!(live.position, mock.position);
    assert_eq!(live.position, 0);
}

#[test]
fn risk_limits_block_orders() {
    let t = ticks();
    let tight = RiskLimits { max_position: 1, max_order_qty: 1, ..loose() };
    // ma_cross qty=2 violates max_order_qty=1 on every signal
    let (live, mock) = run(ExecMode::Live, "ma_cross", "fast=3,slow=8,qty=2", t, tight);
    assert_eq!(mock.orders, 0);
    assert!(live.rejects > 0);
    assert_eq!(live.position, 0);
}
