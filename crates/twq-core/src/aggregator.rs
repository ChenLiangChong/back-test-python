//! Tick → bar aggregation. Used identically by the tick-level backtest and by the
//! live runner, so a strategy sees exactly the same bars in both worlds.

use crate::time::Ts;
use crate::types::{Bar, Tick};

#[derive(Clone, Debug)]
pub struct BarAggregator {
    period: i64,
    cur: Option<Bar>,
}

impl BarAggregator {
    /// `period` in microseconds (e.g. `60 * US_PER_SEC` for 1-minute bars).
    pub fn new(period: i64) -> Self {
        assert!(period > 0);
        Self { period, cur: None }
    }

    pub fn period(&self) -> i64 {
        self.period
    }

    #[inline]
    pub fn bucket(&self, ts: Ts) -> Ts {
        ts - ts.rem_euclid(self.period)
    }

    /// Feed one trade. Returns the *completed* previous bar when this tick opens a new one.
    #[inline]
    pub fn update(&mut self, tick: &Tick) -> Option<Bar> {
        let b = self.bucket(tick.ts);
        match &mut self.cur {
            Some(cur) if cur.ts == b => {
                cur.high = cur.high.max(tick.price);
                cur.low = cur.low.min(tick.price);
                cur.close = tick.price;
                cur.volume += tick.qty;
                None
            }
            slot => {
                let done = slot.take();
                *slot = Some(Bar {
                    ts: b,
                    open: tick.price,
                    high: tick.price,
                    low: tick.price,
                    close: tick.price,
                    volume: tick.qty,
                });
                done
            }
        }
    }

    /// Close the in-progress bar if wall-clock time has passed its end (live timer path).
    pub fn flush_if_due(&mut self, now: Ts) -> Option<Bar> {
        match self.cur {
            Some(cur) if now >= cur.ts + self.period => self.cur.take(),
            _ => None,
        }
    }

    /// Force-close the in-progress bar (end of data).
    pub fn flush(&mut self) -> Option<Bar> {
        self.cur.take()
    }

    pub fn current(&self) -> Option<&Bar> {
        self.cur.as_ref()
    }
}

/// Resample bars into a coarser timeframe (period must be a multiple of the source).
pub fn resample(bars: &[Bar], period: i64) -> Vec<Bar> {
    let mut out: Vec<Bar> = Vec::with_capacity(bars.len() / 2 + 1);
    for b in bars {
        let bucket = b.ts - b.ts.rem_euclid(period);
        match out.last_mut() {
            Some(last) if last.ts == bucket => {
                last.high = last.high.max(b.high);
                last.low = last.low.min(b.low);
                last.close = b.close;
                last.volume += b.volume;
            }
            _ => out.push(Bar { ts: bucket, ..*b }),
        }
    }
    out
}

/// Expand bars into a synthetic tick path O → (L,H or H,L) → C, used by the mock
/// bridge and tick-mode demos when only bar data is available (same intrabar path
/// rule as the bar-mode matcher, see [`Bar::low_first`]).
pub fn bars_to_ticks(bars: &[Bar], period: i64) -> Vec<Tick> {
    let mut out = Vec::with_capacity(bars.len() * 4);
    let q = period / 4;
    for b in bars {
        let (p1, p2) = if b.low_first() { (b.low, b.high) } else { (b.high, b.low) };
        let v = (b.volume / 4.0).max(1.0);
        out.push(Tick::trade(b.ts, b.open, v));
        out.push(Tick::trade(b.ts + q, p1, v));
        out.push(Tick::trade(b.ts + 2 * q, p2, v));
        out.push(Tick::trade(b.ts + 3 * q, b.close, v));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::{make_ts, US_PER_MIN, US_PER_SEC};

    #[test]
    fn aggregates_minutes() {
        let t0 = make_ts(2026, 9, 25, 9, 0, 0, 0);
        let mut agg = BarAggregator::new(US_PER_MIN);
        assert!(agg.update(&Tick::trade(t0 + US_PER_SEC, 100.0, 1.0)).is_none());
        assert!(agg.update(&Tick::trade(t0 + 20 * US_PER_SEC, 105.0, 2.0)).is_none());
        assert!(agg.update(&Tick::trade(t0 + 40 * US_PER_SEC, 95.0, 1.0)).is_none());
        let done = agg.update(&Tick::trade(t0 + US_PER_MIN, 101.0, 1.0)).unwrap();
        assert_eq!(done, Bar { ts: t0, open: 100.0, high: 105.0, low: 95.0, close: 95.0, volume: 4.0 });
        assert!(agg.flush_if_due(t0 + US_PER_MIN + 30 * US_PER_SEC).is_none());
        assert_eq!(agg.flush_if_due(t0 + 2 * US_PER_MIN).unwrap().open, 101.0);
    }

    #[test]
    fn resample_5m() {
        let t0 = make_ts(2026, 9, 25, 9, 0, 0, 0);
        let bars: Vec<Bar> = (0..10)
            .map(|i| Bar {
                ts: t0 + i * US_PER_MIN,
                open: i as f64,
                high: i as f64 + 1.0,
                low: i as f64 - 1.0,
                close: i as f64 + 0.5,
                volume: 1.0,
            })
            .collect();
        let r = resample(&bars, 5 * US_PER_MIN);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].open, 0.0);
        assert_eq!(r[0].high, 5.0);
        assert_eq!(r[0].low, -1.0);
        assert_eq!(r[0].close, 4.5);
        assert_eq!(r[0].volume, 5.0);
    }
}
