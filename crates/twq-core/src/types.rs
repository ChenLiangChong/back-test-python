use crate::time::Ts;
use serde::{Deserialize, Serialize};

/// OHLCV bar. `ts` is the bar's **open** time (a 1-minute bar stamped 09:00 covers
/// 09:00:00–09:00:59.999). `repr(C)` keeps it a flat 48-byte record for the binary cache.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bar {
    pub ts: Ts,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

impl Bar {
    /// Intrabar path heuristic (same rule as NautilusTrader's adaptive bar execution):
    /// if the open is nearer the low, assume O → L → H → C, otherwise O → H → L → C.
    #[inline]
    pub fn low_first(&self) -> bool {
        self.open - self.low <= self.high - self.open
    }
}

/// Last-trade tick with optional best bid/ask (NaN when unknown).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tick {
    pub ts: Ts,
    pub price: f64,
    pub qty: f64,
    pub bid: f64,
    pub ask: f64,
}

impl Tick {
    #[inline]
    pub fn trade(ts: Ts, price: f64, qty: f64) -> Self {
        Self { ts, price, qty, bid: f64::NAN, ask: f64::NAN }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    #[inline]
    pub fn sign(self) -> i64 {
        match self {
            Side::Buy => 1,
            Side::Sell => -1,
        }
    }
    #[inline]
    pub fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum OrderKind {
    Market,
    Limit(f64),
    /// Stop-market: becomes a market order once the trade price touches the stop.
    /// In live trading this is held *locally* by the engine (synthetic stop) so it
    /// reacts in microseconds and does not depend on broker stop-order support.
    Stop(f64),
}

/// Time in force. ROD = 當日有效 (rest of day), IOC = 立即成交否則取消, FOK = 全部成交否則取消.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tif {
    Rod,
    Ioc,
    Fok,
}

pub type OrderId = u64;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrderRequest {
    pub id: OrderId,
    pub side: Side,
    pub qty: i64,
    pub kind: OrderKind,
    pub tif: Tif,
    /// One-cancels-other group; 0 = none. When an order in a group fills, the
    /// remaining orders of the same group are cancelled (bracket SL / TP).
    pub oco: u32,
    /// Free-form tag for journals / trade lists (e.g. "entry", "sl", "tp").
    pub tag: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fill {
    pub order_id: OrderId,
    pub ts: Ts,
    pub side: Side,
    pub qty: i64,
    pub price: f64,
    /// Broker commission (手續費) in NTD.
    pub fee: f64,
    /// Transaction tax (交易稅 / 期交稅) in NTD.
    pub tax: f64,
    pub tag: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OrderStatus {
    Accepted,
    Filled,
    Cancelled,
    Rejected(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrderUpdate {
    pub order_id: OrderId,
    pub ts: Ts,
    pub status: OrderStatus,
}
