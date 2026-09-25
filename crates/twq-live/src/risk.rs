//! Pre-trade risk checks and the kill switch. Every order the strategy emits in paper
//! or live mode passes through `RiskManager::check` before it can reach the broker.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use twq_core::{OrderKind, OrderRequest};

#[derive(Clone, Debug)]
pub struct RiskLimits {
    /// Max absolute position (contracts / shares) including working orders.
    pub max_position: i64,
    /// Max quantity per order.
    pub max_order_qty: i64,
    /// Max orders per rolling second / minute (runaway-loop protection).
    pub max_orders_per_sec: usize,
    pub max_orders_per_min: usize,
    /// Kill switch: flatten and stop when the day's P&L falls below -this (NTD, 0 = off).
    pub max_daily_loss: f64,
    /// Reject limit / stop prices further than this fraction from the last trade.
    pub price_band: f64,
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self {
            max_position: 2,
            max_order_qty: 2,
            max_orders_per_sec: 5,
            max_orders_per_min: 60,
            max_daily_loss: 0.0,
            price_band: 0.05,
        }
    }
}

#[derive(Debug)]
pub struct RiskManager {
    pub limits: RiskLimits,
    sent: VecDeque<Instant>,
    halted: Option<String>,
    day: i64,
    day_start_equity: f64,
    pub rejects: usize,
}

impl RiskManager {
    pub fn new(limits: RiskLimits) -> Self {
        Self { limits, sent: VecDeque::new(), halted: None, day: i64::MIN, day_start_equity: 0.0, rejects: 0 }
    }

    pub fn halted(&self) -> Option<&str> {
        self.halted.as_deref()
    }

    pub fn halt(&mut self, reason: impl Into<String>) {
        if self.halted.is_none() {
            self.halted = Some(reason.into());
        }
    }

    /// Update the daily-loss kill switch. Returns the halt reason when it fires.
    pub fn on_equity(&mut self, trading_day: i64, equity: f64) -> Option<String> {
        if trading_day != self.day {
            self.day = trading_day;
            self.day_start_equity = equity;
        }
        let lim = self.limits.max_daily_loss;
        if lim > 0.0 && self.halted.is_none() && equity - self.day_start_equity <= -lim {
            let r = format!("daily loss {:.0} exceeded limit {:.0}", equity - self.day_start_equity, lim);
            self.halted = Some(r.clone());
            return Some(r);
        }
        None
    }

    /// `exposure` = position + signed qty of working orders on the same instrument.
    pub fn check(
        &mut self,
        o: &OrderRequest,
        position: i64,
        exposure: i64,
        last_price: f64,
        now: Instant,
    ) -> Result<(), String> {
        let signed = o.side.sign() * o.qty;
        let reduces = position != 0 && signed.signum() == -position.signum() && o.qty <= position.abs();
        let res = self.check_inner(o, signed, reduces, exposure, last_price, now);
        match res {
            Ok(()) => {
                self.sent.push_back(now);
                Ok(())
            }
            Err(e) => {
                self.rejects += 1;
                Err(e)
            }
        }
    }

    fn check_inner(
        &mut self,
        o: &OrderRequest,
        signed: i64,
        reduces: bool,
        exposure: i64,
        last: f64,
        now: Instant,
    ) -> Result<(), String> {
        if let Some(h) = &self.halted {
            if !reduces {
                return Err(format!("halted ({h}): only reducing orders allowed"));
            }
        }
        if o.qty <= 0 || o.qty > self.limits.max_order_qty {
            return Err(format!("order qty {} outside 1..={}", o.qty, self.limits.max_order_qty));
        }
        if !reduces && (exposure + signed).abs() > self.limits.max_position {
            return Err(format!("position limit: exposure {} + {} > {}", exposure, signed, self.limits.max_position));
        }
        while self.sent.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60)) {
            self.sent.pop_front();
        }
        let last_sec = self.sent.iter().rev().take_while(|t| now.duration_since(**t) <= Duration::from_secs(1)).count();
        if last_sec >= self.limits.max_orders_per_sec {
            return Err(format!("rate limit: {} orders in the last second", last_sec));
        }
        if self.sent.len() >= self.limits.max_orders_per_min {
            return Err(format!("rate limit: {} orders in the last minute", self.sent.len()));
        }
        if last.is_finite() && last > 0.0 {
            let px = match o.kind {
                OrderKind::Limit(p) | OrderKind::Stop(p) => Some(p),
                OrderKind::Market => None,
            };
            if let Some(p) = px {
                if ((p - last) / last).abs() > self.limits.price_band {
                    return Err(format!(
                        "price {p} outside ±{:.1}% band around {last}",
                        self.limits.price_band * 100.0
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use twq_core::{Side, Tif};

    fn o(side: Side, qty: i64, kind: OrderKind) -> OrderRequest {
        OrderRequest { id: 1, side, qty, kind, tif: Tif::Rod, oco: 0, tag: "" }
    }

    #[test]
    fn limits() {
        let mut r = RiskManager::new(RiskLimits { max_orders_per_sec: 2, ..Default::default() });
        let now = Instant::now();
        assert!(r.check(&o(Side::Buy, 3, OrderKind::Market), 0, 0, 100.0, now).is_err());
        assert!(r.check(&o(Side::Buy, 2, OrderKind::Market), 1, 1, 100.0, now).is_err()); // pos limit
        assert!(r.check(&o(Side::Buy, 1, OrderKind::Limit(120.0)), 0, 0, 100.0, now).is_err()); // band
        assert!(r.check(&o(Side::Buy, 1, OrderKind::Market), 0, 0, 100.0, now).is_ok());
        assert!(r.check(&o(Side::Buy, 1, OrderKind::Market), 1, 1, 100.0, now).is_ok());
        assert!(r.check(&o(Side::Sell, 1, OrderKind::Market), 2, 2, 100.0, now).is_err()); // rate
        assert_eq!(r.rejects, 4);
    }

    #[test]
    fn kill_switch_allows_only_reducing() {
        let mut r = RiskManager::new(RiskLimits { max_daily_loss: 1000.0, ..Default::default() });
        assert!(r.on_equity(1, 10_000.0).is_none());
        assert!(r.on_equity(1, 8_900.0).is_some());
        let now = Instant::now();
        assert!(r.check(&o(Side::Buy, 1, OrderKind::Market), 1, 1, 100.0, now).is_err());
        assert!(r.check(&o(Side::Sell, 1, OrderKind::Market), 1, 1, 100.0, now).is_ok());
    }
}
