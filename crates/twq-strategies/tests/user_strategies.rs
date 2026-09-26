//! Scenario tests for the user's two strategies with hand-built price paths.

use std::collections::HashMap;
use std::sync::Arc;

use twq_backtest::{run_bars, run_ticks, BacktestConfig};
use twq_core::events::parse_events;
use twq_core::time::{make_ts, Ts, US_PER_MIN, US_PER_SEC};
use twq_core::{Bar, Instrument, Params, SimConfig, Tick};
use twq_strategies::{build_with, Inputs};

fn cfg() -> BacktestConfig {
    let mut c = BacktestConfig::new(Instrument::tx().with_commission(0.0));
    c.sim = SimConfig { slippage_ticks: 0.0, limit_fill_on_touch: false };
    c.bar_period = US_PER_MIN;
    c
}

fn flat_bars(from: Ts, n: i64, px: f64) -> Vec<Bar> {
    (0..n)
        .map(|i| Bar { ts: from + i * US_PER_MIN, open: px, high: px + 2.0, low: px - 2.0, close: px, volume: 10.0 })
        .collect()
}

fn events(text: &str) -> Inputs {
    Inputs { events: Arc::new(parse_events(text).unwrap()), series: HashMap::new() }
}

#[test]
fn clip_catches_upside_release_and_takes_profit_big_event() {
    // flat 20000 until 20:30, NFP (big) spikes to 20200 in the release minute
    let t0 = make_ts(2026, 9, 4, 20, 0, 0, 0);
    let mut bars = flat_bars(t0, 30, 20000.0);
    bars.push(Bar {
        ts: t0 + 30 * US_PER_MIN,
        open: 20000.0,
        high: 20200.0,
        low: 19995.0,
        close: 20180.0,
        volume: 500.0,
    });
    bars.extend(flat_bars(t0 + 31 * US_PER_MIN, 10, 20180.0));
    let inp = events("2026-09-04 20:30:00,NFP,big\n");
    let mut s = build_with("event_clip", &Params::new(), &inp).unwrap();
    let r = run_bars(&bars, &mut s, &cfg());
    assert_eq!(r.trades.len(), 1, "{:?}", r.trades);
    let t = &r.trades[0];
    assert_eq!(t.dir, 1);
    assert_eq!(t.entry_tag, "NFP");
    assert_eq!(t.entry_price, 20020.0); // 20000 + clip 20
    assert_eq!(t.exit_price, 20170.0); // + tp_big 150
    assert_eq!(t.exit_tag, "tp");
}

#[test]
fn clip_normal_event_stop_loss_and_downside() {
    // PPI (normal): drops 30 points (short entry at 19980), then rallies 50 -> SL 40 at 20020
    let t0 = make_ts(2026, 9, 10, 20, 0, 0, 0);
    let mut bars = flat_bars(t0, 30, 20000.0);
    bars.push(Bar {
        ts: t0 + 30 * US_PER_MIN,
        open: 20000.0,
        high: 20001.0,
        low: 19970.0,
        close: 19975.0,
        volume: 300.0,
    });
    bars.push(Bar {
        ts: t0 + 31 * US_PER_MIN,
        open: 19975.0,
        high: 20030.0,
        low: 19974.0,
        close: 20025.0,
        volume: 300.0,
    });
    bars.extend(flat_bars(t0 + 32 * US_PER_MIN, 5, 20025.0));
    let inp = events("2026-09-10 20:30:00,PPI,normal\n");
    let mut s = build_with("event_clip", &Params::new(), &inp).unwrap();
    let r = run_bars(&bars, &mut s, &cfg());
    assert_eq!(r.trades.len(), 1);
    let t = &r.trades[0];
    assert_eq!(t.dir, -1);
    assert_eq!(t.entry_price, 19980.0);
    assert_eq!(t.exit_price, 20020.0);
    assert_eq!(t.exit_tag, "sl");
}

#[test]
fn clip_cancelled_when_release_is_quiet() {
    let t0 = make_ts(2026, 9, 1, 21, 30, 0, 0);
    let bars = flat_bars(t0, 60, 20000.0); // never moves 20 points
    let inp = events("2026-09-01 22:00:00,JOLTS,normal\n");
    let mut s = build_with("event_clip", &Params::new(), &inp).unwrap();
    let r = run_bars(&bars, &mut s, &cfg());
    assert!(r.trades.is_empty());
    assert_eq!(r.stats.fills, 0);
}

#[test]
fn clip_tick_mode_uses_pre_release_price() {
    // quiet at 20000 until 20:29:40, first tick after the release already at 20050
    let ev = make_ts(2026, 9, 11, 20, 30, 0, 0);
    let mut ticks: Vec<Tick> =
        (0..40).map(|i| Tick::trade(ev - 60 * US_PER_SEC + i * US_PER_SEC, 20000.0, 1.0)).collect();
    for (k, px) in [20050.0, 20080.0, 20120.0, 20160.0, 20190.0, 20240.0, 20235.0].iter().enumerate() {
        ticks.push(Tick::trade(ev + k as i64 * 2 * US_PER_SEC, *px, 1.0));
    }
    let inp = events("2026-09-11 20:30:00,CPI,big\n");
    let mut s = build_with("event_clip", &Params::new(), &inp).unwrap();
    let r = run_ticks(&ticks, &mut s, &cfg());
    assert_eq!(r.trades.len(), 1, "{:?}", r.trades);
    // clip armed around 20000 (pre-release), buy stop 20020 fills at the next trade 20080
    assert_eq!(r.trades[0].entry_price, 20080.0);
    // take-profit 20080 + 150 = 20230, traded through by 20240
    assert_eq!(r.trades[0].exit_price, 20230.0);
    assert_eq!(r.trades[0].exit_tag, "tp");
}

#[test]
fn clip_cancelled_after_10s_but_tsmc_waits_20s() {
    // quiet at 20000, first move through the clip 15 s after the release
    let ev = make_ts(2026, 9, 10, 13, 30, 0, 0);
    let mut ticks: Vec<Tick> =
        (0..75).map(|i| Tick::trade(ev - 60 * US_PER_SEC + i * US_PER_SEC, 20000.0, 1.0)).collect();
    ticks.extend((0..20).map(|i| Tick::trade(ev + 15 * US_PER_SEC + i * US_PER_SEC, 20030.0 + i as f64, 1.0)));
    for (text, fills) in [("2026-09-10 13:30:00,PPI,normal\n", 0), ("2026-09-10 13:30:00,TSMC_REV,normal\n", 1)] {
        let mut s = build_with("event_clip", &Params::new(), &events(text)).unwrap();
        let r = run_ticks(&ticks, &mut s, &cfg());
        assert_eq!(r.stats.fills, fills, "{text}"); // entry only, the trade is still open at the end
    }
}

#[test]
fn orb_daylow_enters_on_break_of_day_low() {
    let d = make_ts(2026, 9, 2, 8, 45, 0, 0);
    // the previous trading day's session (so "昨量" really is yesterday's), quiet
    let mut bars = flat_bars(make_ts(2026, 9, 1, 8, 45, 0, 0), 300, 20050.0);
    // 08:45-09:29: range 20000..20120 (>100), then from 09:30 drift down through the low
    for i in 0..45 {
        let px = if i == 10 { 20120.0 } else { 20050.0 };
        bars.push(Bar {
            ts: d + i * US_PER_MIN,
            open: px,
            high: px + 1.0,
            low: if i == 20 { 20000.0 } else { px - 1.0 },
            close: px,
            volume: 10.0,
        });
    }
    for i in 45..80 {
        let px = 20050.0 - (i - 44) as f64 * 3.0; // 20047 .. 19945
        bars.push(Bar {
            ts: d + i * US_PER_MIN,
            open: px + 1.0,
            high: px + 2.0,
            low: px - 1.0,
            close: px,
            volume: 10.0,
        });
    }
    // quiet until the close so the 13:40 exit closes the trade
    bars.extend(flat_bars(d + 80 * US_PER_MIN, 220, 19945.0));
    // TAIEX cumulative turnover: yesterday total 3000, today 1500 by 09:20 (> 0.45 x 3000)
    let y = make_ts(2026, 9, 1, 13, 30, 0, 0);
    let series =
        vec![(y, 3000.0), (make_ts(2026, 9, 2, 9, 0, 0, 0), 100.0), (make_ts(2026, 9, 2, 9, 20, 0, 0), 1500.0)];
    let mut map = HashMap::new();
    map.insert("taiex_vol".to_string(), Arc::new(series));
    let inp = Inputs { events: Arc::new(vec![]), series: map };
    let mut s = build_with("orb_daylow", &Params::new(), &inp).unwrap();
    let r = run_bars(&bars, &mut s, &cfg());
    assert_eq!(r.trades.len(), 1, "{:?}", r.trades);
    assert_eq!(r.trades[0].dir, -1);
    assert_eq!(r.trades[0].entry_price, 19999.0); // day low 20000 - 1
    assert_eq!(r.trades[0].exit_tag, "flatten"); // 13:40 exit
    assert_eq!(r.trades[0].exit_price, 19945.0);

    // turnover data missing for the previous trading day -> the day is skipped
    let mut map = HashMap::new();
    map.insert(
        "taiex_vol".to_string(),
        Arc::new(vec![(make_ts(2026, 8, 31, 13, 30, 0, 0), 3000.0), (make_ts(2026, 9, 2, 9, 20, 0, 0), 1500.0)]),
    );
    let inp = Inputs { events: Arc::new(vec![]), series: map };
    let mut s = build_with("orb_daylow", &Params::new(), &inp).unwrap();
    assert!(run_bars(&bars, &mut s, &cfg()).trades.is_empty());

    // volume condition not met (1300 < 0.45 x 3000) -> no trade
    let mut map = HashMap::new();
    map.insert("taiex_vol".to_string(), Arc::new(vec![(y, 3000.0), (make_ts(2026, 9, 2, 9, 20, 0, 0), 1300.0)]));
    let inp = Inputs { events: Arc::new(vec![]), series: map };
    let mut s = build_with("orb_daylow", &Params::new(), &inp).unwrap();
    assert!(run_bars(&bars, &mut s, &cfg()).trades.is_empty());

    // range condition not met -> no trade
    let mut s =
        build_with("orb_daylow", &Params::new().with("use_vol", 0.0).with("range_pts", 200.0), &Inputs::default())
            .unwrap();
    assert!(run_bars(&bars, &mut s, &cfg()).trades.is_empty());

    // missing series is a clear error
    assert!(build_with("orb_daylow", &Params::new(), &Inputs::default()).is_err());
}
