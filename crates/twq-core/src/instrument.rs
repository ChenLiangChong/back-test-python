//! Taiwan market contract specs, tick-size ladders and cost (fee + tax) models.
//!
//! Numbers are defaults as of 2025–2026 and are all overridable from the CLI / config:
//! * 期交稅 (futures transaction tax): 0.002% (十萬分之二) of contract value, per side.
//! * 證券交易稅: 0.3% on the sell side; 0.15% for 現股當沖 day trades; ETF 0.1%.
//! * 券商手續費: 0.1425% × broker discount, minimum NT$20 per order.

use crate::types::Side;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetClass {
    Future,
    Stock,
    Etf,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TickRule {
    /// Constant tick, e.g. 1 index point for TX/MTX/TMF.
    Fixed(f64),
    /// TWSE/TPEx stock price ladder (升降單位).
    TwseStock,
    /// TWSE ETF ladder.
    TwseEtf,
}

impl TickRule {
    /// Tick size (升降單位) at `price`.
    #[inline]
    pub fn size(&self, price: f64) -> f64 {
        match *self {
            TickRule::Fixed(t) => t,
            TickRule::TwseStock => match price {
                p if p < 10.0 => 0.01,
                p if p < 50.0 => 0.05,
                p if p < 100.0 => 0.1,
                p if p < 500.0 => 0.5,
                p if p < 1000.0 => 1.0,
                _ => 5.0,
            },
            TickRule::TwseEtf => {
                if price < 50.0 {
                    0.01
                } else {
                    0.05
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FeeModel {
    Futures {
        /// Broker commission per contract per side (NTD).
        commission_per_contract: f64,
        /// Futures transaction tax rate on contract value per side (0.00002).
        tax_rate: f64,
    },
    Stock {
        /// 0.001425
        commission_rate: f64,
        /// Broker rebate multiplier, e.g. 0.28 for 2.8折. 1.0 = no discount.
        discount: f64,
        /// Minimum commission per fill (NTD 20 for board lots).
        min_commission: f64,
        /// Sell-side securities transaction tax (0.003 stock, 0.001 ETF).
        sell_tax: f64,
        /// Sell-side tax when the position was opened the same day (0.0015 現股當沖).
        day_trade_sell_tax: f64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Instrument {
    pub symbol: String,
    pub class: AssetClass,
    /// NTD P&L per 1.0 price move per 1 unit of quantity.
    /// Futures: 200 (TX) / 50 (MTX) / 10 (TMF). Stocks: 1 (quantity is in shares).
    pub multiplier: f64,
    pub tick: TickRule,
    pub fees: FeeModel,
}

pub const FUTURES_TAX_RATE: f64 = 0.000_02;

impl Instrument {
    pub fn future(symbol: &str, multiplier: f64, commission_per_contract: f64) -> Self {
        Self {
            symbol: symbol.to_string(),
            class: AssetClass::Future,
            multiplier,
            tick: TickRule::Fixed(1.0),
            fees: FeeModel::Futures { commission_per_contract, tax_rate: FUTURES_TAX_RATE },
        }
    }

    /// 臺股期貨 (大台) — NT$200 / point.
    pub fn tx() -> Self {
        Self::future("TX", 200.0, 50.0)
    }
    /// 小型臺指 (小台) — NT$50 / point.
    pub fn mtx() -> Self {
        Self::future("MTX", 50.0, 25.0)
    }
    /// 微型臺指 (微台) — NT$10 / point.
    pub fn tmf() -> Self {
        Self::future("TMF", 10.0, 12.0)
    }

    /// A TWSE/TPEx common stock. Quantity is in shares (1 張 = 1000 shares).
    pub fn stock(symbol: &str) -> Self {
        Self {
            symbol: symbol.to_string(),
            class: AssetClass::Stock,
            multiplier: 1.0,
            tick: TickRule::TwseStock,
            fees: FeeModel::Stock {
                commission_rate: 0.001_425,
                discount: 1.0,
                min_commission: 20.0,
                sell_tax: 0.003,
                day_trade_sell_tax: 0.0015,
            },
        }
    }

    pub fn etf(symbol: &str) -> Self {
        Self {
            symbol: symbol.to_string(),
            class: AssetClass::Etf,
            multiplier: 1.0,
            tick: TickRule::TwseEtf,
            fees: FeeModel::Stock {
                commission_rate: 0.001_425,
                discount: 1.0,
                min_commission: 20.0,
                sell_tax: 0.001,
                day_trade_sell_tax: 0.001,
            },
        }
    }

    /// Resolve a preset by name: `TX`, `MTX`, `TMF`, `NQ` (CME demo data),
    /// `stock:2330`, `etf:0050`.
    pub fn preset(name: &str) -> Option<Self> {
        let upper = name.to_ascii_uppercase();
        match upper.as_str() {
            "TX" | "TXF" => Some(Self::tx()),
            "MTX" | "MXF" => Some(Self::mtx()),
            "TMF" => Some(Self::tmf()),
            // Nasdaq-100 E-mini, only for the bundled US sample data (USD 20 / point,
            // 0.25 tick). Costs are approximate and expressed in the quote currency.
            "NQ" => Some(Self {
                symbol: "NQ".into(),
                class: AssetClass::Future,
                multiplier: 20.0,
                tick: TickRule::Fixed(0.25),
                fees: FeeModel::Futures { commission_per_contract: 2.5, tax_rate: 0.0 },
            }),
            _ => {
                if let Some(sym) = upper.strip_prefix("STOCK:") {
                    Some(Self::stock(sym))
                } else {
                    upper.strip_prefix("ETF:").map(Self::etf)
                }
            }
        }
    }

    /// Set the broker commission: per contract for futures, discount multiplier for stocks.
    pub fn with_commission(mut self, value: f64) -> Self {
        match &mut self.fees {
            FeeModel::Futures { commission_per_contract, .. } => *commission_per_contract = value,
            FeeModel::Stock { discount, .. } => *discount = value,
        }
        self
    }

    #[inline]
    pub fn tick_size(&self, price: f64) -> f64 {
        self.tick.size(price)
    }

    /// Round a price onto the tick grid (`up` = ceil, else floor).
    #[inline]
    pub fn round_to_tick(&self, price: f64, up: bool) -> f64 {
        let t = self.tick_size(price);
        let n = price / t;
        // tolerate float noise such as 23000.000000001
        let n = if (n - n.round()).abs() < 1e-7 {
            n.round()
        } else if up {
            n.ceil()
        } else {
            n.floor()
        };
        n * t
    }

    /// Commission and tax (NTD) for one fill.
    ///
    /// `closes_day_trade` marks a sell that closes a long opened on the same trading
    /// day (現股當沖 reduced tax). Futures ignore it.
    #[inline]
    pub fn costs(&self, side: Side, qty: i64, price: f64, closes_day_trade: bool) -> (f64, f64) {
        let qty = qty.unsigned_abs() as f64;
        let notional = qty * price * self.multiplier;
        match self.fees {
            FeeModel::Futures { commission_per_contract, tax_rate } => {
                // 期交稅 is computed per contract and rounded to the nearest dollar.
                let tax_per = (price * self.multiplier * tax_rate).round();
                (commission_per_contract * qty, tax_per * qty)
            }
            FeeModel::Stock { commission_rate, discount, min_commission, sell_tax, day_trade_sell_tax } => {
                let fee = (notional * commission_rate * discount).floor().max(min_commission);
                let tax = match side {
                    Side::Buy => 0.0,
                    Side::Sell => {
                        let rate = if closes_day_trade { day_trade_sell_tax } else { sell_tax };
                        (notional * rate).floor()
                    }
                };
                (fee, tax)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_ladder() {
        let s = Instrument::stock("2330");
        assert_eq!(s.tick_size(9.99), 0.01);
        assert_eq!(s.tick_size(49.95), 0.05);
        assert_eq!(s.tick_size(50.0), 0.1);
        assert_eq!(s.tick_size(123.0), 0.5);
        assert_eq!(s.tick_size(999.0), 1.0);
        assert_eq!(s.tick_size(1085.0), 5.0);
        assert!((s.round_to_tick(123.3, true) - 123.5).abs() < 1e-9);
        assert!((s.round_to_tick(123.3, false) - 123.0).abs() < 1e-9);
        let e = Instrument::etf("0050");
        assert_eq!(e.tick_size(45.0), 0.01);
        assert_eq!(e.tick_size(180.0), 0.05);
    }

    #[test]
    fn futures_costs() {
        let tx = Instrument::tx().with_commission(60.0);
        // 23000 * 200 * 0.00002 = 92
        let (fee, tax) = tx.costs(Side::Buy, 2, 23_000.0, false);
        assert_eq!(fee, 120.0);
        assert_eq!(tax, 184.0);
        let tmf = Instrument::tmf();
        // 23000 * 10 * 0.00002 = 4.6 -> 5
        assert_eq!(tmf.costs(Side::Sell, 1, 23_000.0, false).1, 5.0);
    }

    #[test]
    fn stock_costs() {
        let s = Instrument::stock("2330");
        // buy 1000 shares @ 1000 = 1,000,000 -> fee 1425, no tax
        assert_eq!(s.costs(Side::Buy, 1000, 1000.0, false), (1425.0, 0.0));
        // sell -> tax 3000, day-trade -> 1500
        assert_eq!(s.costs(Side::Sell, 1000, 1000.0, false).1, 3000.0);
        assert_eq!(s.costs(Side::Sell, 1000, 1000.0, true).1, 1500.0);
        // minimum commission
        assert_eq!(s.costs(Side::Buy, 1, 10.0, false).0, 20.0);
    }
}
