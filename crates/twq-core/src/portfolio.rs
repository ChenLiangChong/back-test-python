//! Position / P&L accounting shared by backtest, paper and live modes.

use crate::instrument::{AssetClass, Instrument};
use crate::time::{trading_day, Ts};
use crate::types::{Fill, OrderId, Side};

/// A completed round trip (flat → position → flat).
#[derive(Clone, Debug, PartialEq)]
pub struct Trade {
    pub entry_ts: Ts,
    pub exit_ts: Ts,
    /// +1 long, -1 short.
    pub dir: i64,
    /// Maximum absolute position held during the trade.
    pub qty: i64,
    pub entry_price: f64,
    pub exit_price: f64,
    pub gross_pnl: f64,
    pub costs: f64,
    pub net_pnl: f64,
    pub entry_tag: &'static str,
    pub exit_tag: &'static str,
}

#[derive(Clone, Debug)]
struct OpenTrade {
    entry_ts: Ts,
    dir: i64,
    max_qty: i64,
    entry_qty: i64,
    entry_value: f64,
    exit_qty: i64,
    exit_value: f64,
    gross: f64,
    costs: f64,
    entry_tag: &'static str,
}

#[derive(Clone, Debug)]
pub struct Portfolio {
    pub instrument: Instrument,
    position: i64,
    avg_price: f64,
    realized_gross: f64,
    fees: f64,
    taxes: f64,
    /// Trading day on which the current position was opened (現股當沖 tax).
    open_day: i64,
    cur: Option<OpenTrade>,
    trades: Vec<Trade>,
    record_trades: bool,
}

impl Portfolio {
    pub fn new(instrument: Instrument) -> Self {
        Self {
            instrument,
            position: 0,
            avg_price: 0.0,
            realized_gross: 0.0,
            fees: 0.0,
            taxes: 0.0,
            open_day: i64::MIN,
            cur: None,
            trades: Vec::new(),
            record_trades: true,
        }
    }

    #[inline]
    pub fn position(&self) -> i64 {
        self.position
    }
    #[inline]
    pub fn avg_price(&self) -> f64 {
        self.avg_price
    }
    #[inline]
    pub fn fees(&self) -> f64 {
        self.fees
    }
    #[inline]
    pub fn taxes(&self) -> f64 {
        self.taxes
    }
    #[inline]
    pub fn realized_gross(&self) -> f64 {
        self.realized_gross
    }
    /// Realized P&L after all fees and taxes (including entry costs of open positions).
    #[inline]
    pub fn realized_net(&self) -> f64 {
        self.realized_gross - self.fees - self.taxes
    }
    #[inline]
    pub fn unrealized(&self, mark: f64) -> f64 {
        if self.position == 0 {
            0.0
        } else {
            (mark - self.avg_price) * self.position as f64 * self.instrument.multiplier
        }
    }
    pub fn trades(&self) -> &[Trade] {
        &self.trades
    }
    pub fn take_trades(&mut self) -> Vec<Trade> {
        std::mem::take(&mut self.trades)
    }

    /// Execute `qty` at `price`, computing costs from the instrument model.
    pub fn execute(&mut self, order_id: OrderId, ts: Ts, side: Side, qty: i64, price: f64, tag: &'static str) -> Fill {
        let closes_day_trade = self.instrument.class != AssetClass::Future
            && side == Side::Sell
            && self.position > 0
            && trading_day(ts) == self.open_day;
        let (fee, tax) = self.instrument.costs(side, qty, price, closes_day_trade);
        let fill = Fill { order_id, ts, side, qty, price, fee, tax, tag };
        self.apply(&fill);
        fill
    }

    /// Apply an externally produced fill (live broker report) with its own costs.
    pub fn apply(&mut self, f: &Fill) {
        let signed = f.side.sign() * f.qty;
        let mult = self.instrument.multiplier;
        let costs = f.fee + f.tax;
        self.fees += f.fee;
        self.taxes += f.tax;

        let old = self.position;
        let new = old + signed;
        let mut gross = 0.0;
        if old == 0 || old.signum() == signed.signum() {
            // open / add
            let tot = old.abs() + f.qty;
            self.avg_price = (self.avg_price * old.abs() as f64 + f.price * f.qty as f64) / tot as f64;
            if old == 0 {
                self.open_day = trading_day(f.ts);
            }
        } else {
            // reduce / close / flip
            let closed = f.qty.min(old.abs());
            gross = (f.price - self.avg_price) * closed as f64 * old.signum() as f64 * mult;
            self.realized_gross += gross;
            if new == 0 {
                self.avg_price = 0.0;
            } else if new.signum() != old.signum() {
                self.avg_price = f.price;
                self.open_day = trading_day(f.ts);
            }
        }
        self.position = new;
        if self.record_trades {
            self.track_trade(f, old, new, gross, costs);
        }
    }

    fn track_trade(&mut self, f: &Fill, old: i64, new: i64, gross: f64, costs: f64) {
        let opening_qty = if old == 0 || old.signum() == new.signum() && new.abs() > old.abs() {
            (new.abs() - old.abs()).max(0)
        } else if new != 0 && new.signum() != old.signum() {
            new.abs()
        } else {
            0
        };
        let closing_qty = f.qty - opening_qty;
        // closing part
        if closing_qty > 0 {
            if let Some(t) = self.cur.as_mut() {
                t.exit_qty += closing_qty;
                t.exit_value += f.price * closing_qty as f64;
                t.gross += gross;
                t.costs += costs * closing_qty as f64 / f.qty as f64;
            }
            if new == 0 || new.signum() != old.signum() {
                if let Some(t) = self.cur.take() {
                    self.trades.push(Trade {
                        entry_ts: t.entry_ts,
                        exit_ts: f.ts,
                        dir: t.dir,
                        qty: t.max_qty,
                        entry_price: t.entry_value / t.entry_qty as f64,
                        exit_price: t.exit_value / t.exit_qty.max(1) as f64,
                        gross_pnl: t.gross,
                        costs: t.costs,
                        net_pnl: t.gross - t.costs,
                        entry_tag: t.entry_tag,
                        exit_tag: f.tag,
                    });
                }
            }
        }
        // opening part
        if opening_qty > 0 {
            let part_cost = costs * opening_qty as f64 / f.qty as f64;
            match self.cur.as_mut() {
                Some(t) => {
                    t.entry_qty += opening_qty;
                    t.entry_value += f.price * opening_qty as f64;
                    t.max_qty = t.max_qty.max(new.abs());
                    t.costs += part_cost;
                }
                None => {
                    self.cur = Some(OpenTrade {
                        entry_ts: f.ts,
                        dir: new.signum(),
                        max_qty: new.abs(),
                        entry_qty: opening_qty,
                        entry_value: f.price * opening_qty as f64,
                        exit_qty: 0,
                        exit_value: 0.0,
                        gross: 0.0,
                        costs: part_cost,
                        entry_tag: f.tag,
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_round_trip() {
        let mut p = Portfolio::new(Instrument::tx().with_commission(50.0));
        p.execute(1, 0, Side::Buy, 2, 20_000.0, "in");
        assert_eq!(p.position(), 2);
        p.execute(2, 1, Side::Sell, 2, 20_010.0, "out");
        assert_eq!(p.position(), 0);
        // 10 pts * 2 * 200 = 4000 gross
        assert_eq!(p.realized_gross(), 4000.0);
        let t = &p.trades()[0];
        assert_eq!(t.dir, 1);
        assert_eq!(t.qty, 2);
        assert_eq!(t.gross_pnl, 4000.0);
        // fees 4*50=200, tax 2*80 + 2*80(20010*200*2e-5=80.04->80) = 320
        assert!((t.costs - (200.0 + 320.0)).abs() < 1e-9);
        assert!((p.realized_net() - (4000.0 - 520.0)).abs() < 1e-9);
    }

    #[test]
    fn flip_short_to_long() {
        let mut p = Portfolio::new(Instrument::mtx().with_commission(0.0));
        p.execute(1, 0, Side::Sell, 1, 100.0, "s");
        p.execute(2, 1, Side::Buy, 3, 90.0, "flip");
        assert_eq!(p.position(), 2);
        assert_eq!(p.avg_price(), 90.0);
        assert_eq!(p.trades().len(), 1);
        assert_eq!(p.trades()[0].gross_pnl, 10.0 * 50.0);
        p.execute(3, 2, Side::Sell, 2, 95.0, "x");
        assert_eq!(p.trades().len(), 2);
        assert_eq!(p.trades()[1].gross_pnl, 5.0 * 2.0 * 50.0);
        assert_eq!(p.trades()[1].dir, 1);
    }

    #[test]
    fn scale_in_average() {
        let mut p = Portfolio::new(Instrument::tmf().with_commission(0.0));
        p.execute(1, 0, Side::Buy, 1, 100.0, "a");
        p.execute(2, 0, Side::Buy, 1, 110.0, "b");
        assert_eq!(p.avg_price(), 105.0);
        p.execute(3, 0, Side::Sell, 1, 120.0, "c");
        assert_eq!(p.position(), 1);
        assert_eq!(p.realized_gross(), 150.0);
        assert!(p.trades().is_empty());
    }
}
