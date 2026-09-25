//! Latency sampling with exact percentiles at shutdown.

use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct Latency {
    samples: Vec<u64>,
    cap: usize,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct LatencySummary {
    pub count: usize,
    pub p50_us: f64,
    pub p90_us: f64,
    pub p99_us: f64,
    pub p999_us: f64,
    pub max_us: f64,
    pub mean_us: f64,
}

impl Latency {
    pub fn new(cap: usize) -> Self {
        Self { samples: Vec::with_capacity(cap.min(1 << 20)), cap }
    }

    #[inline]
    pub fn record(&mut self, d: Duration) {
        if self.samples.len() < self.cap {
            self.samples.push(d.as_nanos() as u64);
        }
    }

    pub fn summary(&self) -> LatencySummary {
        if self.samples.is_empty() {
            return LatencySummary::default();
        }
        let mut s = self.samples.clone();
        s.sort_unstable();
        let q = |p: f64| s[((s.len() as f64 - 1.0) * p).round() as usize] as f64 / 1_000.0;
        LatencySummary {
            count: s.len(),
            p50_us: q(0.50),
            p90_us: q(0.90),
            p99_us: q(0.99),
            p999_us: q(0.999),
            max_us: *s.last().unwrap() as f64 / 1_000.0,
            mean_us: s.iter().sum::<u64>() as f64 / s.len() as f64 / 1_000.0,
        }
    }
}

impl std::fmt::Display for LatencySummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "n={} p50={:.1}µs p90={:.1}µs p99={:.1}µs p99.9={:.1}µs max={:.1}µs",
            self.count, self.p50_us, self.p90_us, self.p99_us, self.p999_us, self.max_us
        )
    }
}
