//! Streaming technical indicators. Every `update` is O(1) (amortised) with no heap
//! allocation after construction, so the exact same objects are used in backtests
//! over millions of bars and in the live tick loop.
//!
//! Convention: `update` returns `Some(value)` once the indicator is warmed up.

use std::collections::VecDeque;

use crate::types::Bar;

/// Fixed-capacity ring buffer used by the windowed indicators.
#[derive(Clone, Debug)]
struct Ring {
    buf: Vec<f64>,
    head: usize,
    len: usize,
}

impl Ring {
    fn new(n: usize) -> Self {
        Self { buf: vec![0.0; n.max(1)], head: 0, len: 0 }
    }
    /// Push a value, returning the value that fell out of the window (if full).
    #[inline]
    fn push(&mut self, x: f64) -> Option<f64> {
        let cap = self.buf.len();
        let old = if self.len == cap { Some(self.buf[self.head]) } else { None };
        self.buf[self.head] = x;
        self.head += 1;
        if self.head == cap {
            self.head = 0;
        }
        if self.len < cap {
            self.len += 1;
        }
        old
    }
    #[inline]
    fn full(&self) -> bool {
        self.len == self.buf.len()
    }
    fn sum(&self) -> f64 {
        self.buf[..self.len].iter().sum()
    }
    fn sumsq(&self) -> f64 {
        self.buf[..self.len].iter().map(|x| x * x).sum()
    }
}

/// Simple moving average.
#[derive(Clone, Debug)]
pub struct Sma {
    ring: Ring,
    sum: f64,
    since_resync: usize,
}

impl Sma {
    pub fn new(n: usize) -> Self {
        Self { ring: Ring::new(n), sum: 0.0, since_resync: 0 }
    }
    #[inline]
    pub fn update(&mut self, x: f64) -> Option<f64> {
        let old = self.ring.push(x);
        self.sum += x - old.unwrap_or(0.0);
        // Re-sum once per window to kill floating-point drift (amortised O(1)).
        self.since_resync += 1;
        if self.since_resync >= self.ring.buf.len().max(64) {
            self.sum = self.ring.sum();
            self.since_resync = 0;
        }
        self.value()
    }
    #[inline]
    pub fn value(&self) -> Option<f64> {
        self.ring.full().then(|| self.sum / self.ring.len as f64)
    }
}

/// Exponential moving average, seeded with the SMA of the first `n` values.
#[derive(Clone, Debug)]
pub struct Ema {
    n: usize,
    alpha: f64,
    count: usize,
    seed_sum: f64,
    value: f64,
}

impl Ema {
    pub fn new(n: usize) -> Self {
        let n = n.max(1);
        Self { n, alpha: 2.0 / (n as f64 + 1.0), count: 0, seed_sum: 0.0, value: 0.0 }
    }
    #[inline]
    pub fn update(&mut self, x: f64) -> Option<f64> {
        self.count += 1;
        if self.count < self.n {
            self.seed_sum += x;
            return None;
        }
        if self.count == self.n {
            self.value = (self.seed_sum + x) / self.n as f64;
        } else {
            self.value += self.alpha * (x - self.value);
        }
        Some(self.value)
    }
    #[inline]
    pub fn value(&self) -> Option<f64> {
        (self.count >= self.n).then_some(self.value)
    }
}

/// Wilder's smoothing (RMA), used by RSI and ATR.
#[derive(Clone, Debug)]
pub struct Rma {
    n: usize,
    count: usize,
    seed_sum: f64,
    value: f64,
}

impl Rma {
    pub fn new(n: usize) -> Self {
        Self { n: n.max(1), count: 0, seed_sum: 0.0, value: 0.0 }
    }
    #[inline]
    pub fn update(&mut self, x: f64) -> Option<f64> {
        self.count += 1;
        if self.count < self.n {
            self.seed_sum += x;
            return None;
        }
        if self.count == self.n {
            self.value = (self.seed_sum + x) / self.n as f64;
        } else {
            self.value = (self.value * (self.n as f64 - 1.0) + x) / self.n as f64;
        }
        Some(self.value)
    }
    #[inline]
    pub fn value(&self) -> Option<f64> {
        (self.count >= self.n).then_some(self.value)
    }
}

/// Relative Strength Index (Wilder).
#[derive(Clone, Debug)]
pub struct Rsi {
    prev: Option<f64>,
    gain: Rma,
    loss: Rma,
}

impl Rsi {
    pub fn new(n: usize) -> Self {
        Self { prev: None, gain: Rma::new(n), loss: Rma::new(n) }
    }
    #[inline]
    pub fn update(&mut self, close: f64) -> Option<f64> {
        let prev = self.prev.replace(close)?;
        let ch = close - prev;
        let g = self.gain.update(ch.max(0.0));
        let l = self.loss.update((-ch).max(0.0));
        match (g, l) {
            (Some(g), Some(l)) => Some(if l == 0.0 {
                if g == 0.0 {
                    50.0
                } else {
                    100.0
                }
            } else {
                100.0 - 100.0 / (1.0 + g / l)
            }),
            _ => None,
        }
    }
}

/// Average True Range (Wilder).
#[derive(Clone, Debug)]
pub struct Atr {
    prev_close: Option<f64>,
    rma: Rma,
}

impl Atr {
    pub fn new(n: usize) -> Self {
        Self { prev_close: None, rma: Rma::new(n) }
    }
    #[inline]
    pub fn update(&mut self, bar: &Bar) -> Option<f64> {
        let tr = match self.prev_close {
            Some(pc) => (bar.high - bar.low).max((bar.high - pc).abs()).max((bar.low - pc).abs()),
            None => bar.high - bar.low,
        };
        self.prev_close = Some(bar.close);
        self.rma.update(tr)
    }
    #[inline]
    pub fn value(&self) -> Option<f64> {
        self.rma.value()
    }
}

/// Rolling mean and population standard deviation.
#[derive(Clone, Debug)]
pub struct RollingStd {
    ring: Ring,
    sum: f64,
    sumsq: f64,
    since_resync: usize,
}

impl RollingStd {
    pub fn new(n: usize) -> Self {
        Self { ring: Ring::new(n), sum: 0.0, sumsq: 0.0, since_resync: 0 }
    }
    /// Returns `(mean, std)`.
    #[inline]
    pub fn update(&mut self, x: f64) -> Option<(f64, f64)> {
        let old = self.ring.push(x).unwrap_or(0.0);
        self.sum += x - old;
        self.sumsq += x * x - old * old;
        self.since_resync += 1;
        if self.since_resync >= self.ring.buf.len().max(64) {
            self.sum = self.ring.sum();
            self.sumsq = self.ring.sumsq();
            self.since_resync = 0;
        }
        if !self.ring.full() {
            return None;
        }
        let n = self.ring.len as f64;
        let mean = self.sum / n;
        let var = (self.sumsq / n - mean * mean).max(0.0);
        Some((mean, var.sqrt()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bands {
    pub mid: f64,
    pub upper: f64,
    pub lower: f64,
}

/// Bollinger Bands (布林通道): SMA(n) ± k·σ.
#[derive(Clone, Debug)]
pub struct Bollinger {
    std: RollingStd,
    k: f64,
}

impl Bollinger {
    pub fn new(n: usize, k: f64) -> Self {
        Self { std: RollingStd::new(n), k }
    }
    #[inline]
    pub fn update(&mut self, close: f64) -> Option<Bands> {
        let (m, s) = self.std.update(close)?;
        Some(Bands { mid: m, upper: m + self.k * s, lower: m - self.k * s })
    }
}

/// Rolling maximum over the last `n` values (monotonic deque, amortised O(1)).
#[derive(Clone, Debug)]
pub struct Highest {
    n: u64,
    i: u64,
    q: VecDeque<(u64, f64)>,
}

impl Highest {
    pub fn new(n: usize) -> Self {
        Self { n: n.max(1) as u64, i: 0, q: VecDeque::with_capacity(n.max(1) + 1) }
    }
    #[inline]
    pub fn update(&mut self, x: f64) -> Option<f64> {
        while matches!(self.q.back(), Some(&(_, v)) if v <= x) {
            self.q.pop_back();
        }
        self.q.push_back((self.i, x));
        while matches!(self.q.front(), Some(&(j, _)) if j + self.n <= self.i) {
            self.q.pop_front();
        }
        self.i += 1;
        (self.i >= self.n).then(|| self.q.front().unwrap().1)
    }
}

/// Rolling minimum over the last `n` values.
#[derive(Clone, Debug)]
pub struct Lowest {
    inner: Highest,
}

impl Lowest {
    pub fn new(n: usize) -> Self {
        Self { inner: Highest::new(n) }
    }
    #[inline]
    pub fn update(&mut self, x: f64) -> Option<f64> {
        self.inner.update(-x).map(|v| -v)
    }
}

/// Taiwan-style Stochastic KD (default 9,3,3): RSV → K = ⅔K + ⅓RSV → D = ⅔D + ⅓K,
/// K and D initialised at 50 as in most Taiwanese charting software.
#[derive(Clone, Debug)]
pub struct Kd {
    hh: Highest,
    ll: Lowest,
    k_w: f64,
    d_w: f64,
    k: f64,
    d: f64,
}

impl Kd {
    pub fn new(n: usize, k_smooth: usize, d_smooth: usize) -> Self {
        Self {
            hh: Highest::new(n),
            ll: Lowest::new(n),
            k_w: 1.0 / k_smooth.max(1) as f64,
            d_w: 1.0 / d_smooth.max(1) as f64,
            k: 50.0,
            d: 50.0,
        }
    }
    /// Returns `(K, D)`.
    #[inline]
    pub fn update(&mut self, bar: &Bar) -> Option<(f64, f64)> {
        let hh = self.hh.update(bar.high);
        let ll = self.ll.update(bar.low);
        let (hh, ll) = (hh?, ll?);
        let rsv = if hh > ll { (bar.close - ll) / (hh - ll) * 100.0 } else { 50.0 };
        self.k = self.k * (1.0 - self.k_w) + rsv * self.k_w;
        self.d = self.d * (1.0 - self.d_w) + self.k * self.d_w;
        Some((self.k, self.d))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MacdValue {
    pub macd: f64,
    pub signal: f64,
    pub hist: f64,
}

/// MACD (12, 26, 9).
#[derive(Clone, Debug)]
pub struct Macd {
    fast: Ema,
    slow: Ema,
    signal: Ema,
}

impl Macd {
    pub fn new(fast: usize, slow: usize, signal: usize) -> Self {
        Self { fast: Ema::new(fast), slow: Ema::new(slow), signal: Ema::new(signal) }
    }
    #[inline]
    pub fn update(&mut self, close: f64) -> Option<MacdValue> {
        let f = self.fast.update(close);
        let s = self.slow.update(close);
        let m = f? - s?;
        let sig = self.signal.update(m)?;
        Some(MacdValue { macd: m, signal: sig, hist: m - sig })
    }
}

/// Session VWAP that resets whenever `session_key` changes (e.g. the trading day).
#[derive(Clone, Debug, Default)]
pub struct Vwap {
    key: i64,
    pv: f64,
    v: f64,
    started: bool,
}

impl Vwap {
    pub fn new() -> Self {
        Self::default()
    }
    #[inline]
    pub fn update(&mut self, bar: &Bar, session_key: i64) -> Option<f64> {
        if !self.started || session_key != self.key {
            self.key = session_key;
            self.pv = 0.0;
            self.v = 0.0;
            self.started = true;
        }
        let tp = (bar.high + bar.low + bar.close) / 3.0;
        let vol = if bar.volume > 0.0 { bar.volume } else { 1.0 };
        self.pv += tp * vol;
        self.v += vol;
        Some(self.pv / self.v)
    }
}

/// Detects sign changes of `a - b` (golden / death cross).
#[derive(Clone, Copy, Debug, Default)]
pub struct Cross {
    prev: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrossEvent {
    Above,
    Below,
}

impl Cross {
    #[inline]
    pub fn update(&mut self, a: f64, b: f64) -> Option<CrossEvent> {
        let d = a - b;
        let prev = self.prev.replace(d)?;
        if prev <= 0.0 && d > 0.0 {
            Some(CrossEvent::Above)
        } else if prev >= 0.0 && d < 0.0 {
            Some(CrossEvent::Below)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(h: f64, l: f64, c: f64) -> Bar {
        Bar { ts: 0, open: c, high: h, low: l, close: c, volume: 1.0 }
    }

    fn series(n: usize) -> Vec<f64> {
        let mut rng = crate::rng::Rng::new(7);
        let mut x = 100.0;
        (0..n)
            .map(|_| {
                x += rng.normal();
                x
            })
            .collect()
    }

    #[test]
    fn sma_matches_naive() {
        let xs = series(5_000);
        let n = 20;
        let mut s = Sma::new(n);
        for (i, &x) in xs.iter().enumerate() {
            let v = s.update(x);
            if i + 1 >= n {
                let naive: f64 = xs[i + 1 - n..=i].iter().sum::<f64>() / n as f64;
                assert!((v.unwrap() - naive).abs() < 1e-9);
            } else {
                assert!(v.is_none());
            }
        }
    }

    #[test]
    fn highest_lowest_match_naive() {
        let xs = series(3_000);
        let n = 14;
        let (mut h, mut l) = (Highest::new(n), Lowest::new(n));
        for (i, &x) in xs.iter().enumerate() {
            let (hv, lv) = (h.update(x), l.update(x));
            if i + 1 >= n {
                let w = &xs[i + 1 - n..=i];
                assert_eq!(hv.unwrap(), w.iter().cloned().fold(f64::MIN, f64::max));
                assert_eq!(lv.unwrap(), w.iter().cloned().fold(f64::MAX, f64::min));
            }
        }
    }

    #[test]
    fn bollinger_matches_naive() {
        let xs = series(2_000);
        let n = 20;
        let mut b = Bollinger::new(n, 2.0);
        for (i, &x) in xs.iter().enumerate() {
            let v = b.update(x);
            if i + 1 >= n {
                let w = &xs[i + 1 - n..=i];
                let m = w.iter().sum::<f64>() / n as f64;
                let sd = (w.iter().map(|y| (y - m).powi(2)).sum::<f64>() / n as f64).sqrt();
                let v = v.unwrap();
                assert!((v.mid - m).abs() < 1e-8);
                assert!((v.upper - (m + 2.0 * sd)).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn ema_rsi_atr_kd_macd_sane() {
        let xs = series(500);
        let mut e = Ema::new(10);
        let mut r = Rsi::new(14);
        let mut a = Atr::new(14);
        let mut kd = Kd::new(9, 3, 3);
        let mut m = Macd::new(12, 26, 9);
        for &x in &xs {
            e.update(x);
            if let Some(v) = r.update(x) {
                assert!((0.0..=100.0).contains(&v));
            }
            let b = bar(x + 0.5, x - 0.5, x);
            if let Some(v) = a.update(&b) {
                assert!(v > 0.0);
            }
            if let Some((k, d)) = kd.update(&b) {
                assert!((0.0..=100.0).contains(&k) && (0.0..=100.0).contains(&d));
            }
            m.update(x);
        }
        assert!(e.value().is_some());
        // constant series -> EMA equals constant
        let mut e2 = Ema::new(5);
        for _ in 0..20 {
            e2.update(3.0);
        }
        assert!((e2.value().unwrap() - 3.0).abs() < 1e-12);
    }

    #[test]
    fn cross_detects() {
        let mut c = Cross::default();
        assert_eq!(c.update(1.0, 2.0), None);
        assert_eq!(c.update(3.0, 2.0), Some(CrossEvent::Above));
        assert_eq!(c.update(3.0, 2.0), None);
        assert_eq!(c.update(1.0, 2.0), Some(CrossEvent::Below));
    }
}
