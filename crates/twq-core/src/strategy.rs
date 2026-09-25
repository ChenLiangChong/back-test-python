//! The strategy API. A strategy only ever talks to [`Ctx`]; the backtester, the paper
//! trader and the live runner all drive the *same* `Strategy` object through the same
//! `Ctx`, so what you backtest is byte-for-byte what you trade.

use std::collections::BTreeMap;
use std::fmt;

use anyhow::{anyhow, Result};

use crate::instrument::{Instrument, TickRule};
use crate::time::{trading_day, Ts};
use crate::types::{Bar, Fill, OrderId, OrderKind, OrderRequest, OrderUpdate, Side, Tick, Tif};

pub trait Strategy: Send {
    /// Called when a bar of the configured timeframe closes.
    fn on_bar(&mut self, bar: &Bar, ctx: &mut Ctx);
    /// Called on every trade tick (only when [`Strategy::wants_ticks`] is true).
    fn on_tick(&mut self, _tick: &Tick, _ctx: &mut Ctx) {}
    /// Called after each fill has been applied to the position.
    fn on_fill(&mut self, _fill: &Fill, _ctx: &mut Ctx) {}
    /// Order accepted / cancelled / rejected notifications.
    fn on_order_update(&mut self, _u: &OrderUpdate, _ctx: &mut Ctx) {}
    fn wants_ticks(&self) -> bool {
        false
    }
}

impl<S: Strategy + ?Sized> Strategy for Box<S> {
    #[inline]
    fn on_bar(&mut self, bar: &Bar, ctx: &mut Ctx) {
        (**self).on_bar(bar, ctx)
    }
    #[inline]
    fn on_tick(&mut self, tick: &Tick, ctx: &mut Ctx) {
        (**self).on_tick(tick, ctx)
    }
    #[inline]
    fn on_fill(&mut self, fill: &Fill, ctx: &mut Ctx) {
        (**self).on_fill(fill, ctx)
    }
    #[inline]
    fn on_order_update(&mut self, u: &OrderUpdate, ctx: &mut Ctx) {
        (**self).on_order_update(u, ctx)
    }
    fn wants_ticks(&self) -> bool {
        (**self).wants_ticks()
    }
}

/// Instructions emitted by a strategy, consumed by the engine after each callback.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Submit(OrderRequest),
    Cancel(OrderId),
    CancelAll,
}

/// Stop-loss / take-profit distances (in price points) attached to an entry order.
/// When the entry fills, `Ctx` automatically places an OCO pair around the fill price.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bracket {
    pub stop_dist: Option<f64>,
    pub take_dist: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct Ctx {
    now: Ts,
    last_price: f64,
    position: i64,
    avg_price: f64,
    realized_net: f64,
    equity: f64,
    bar_period: i64,
    working: Vec<OrderRequest>,
    brackets: Vec<(OrderId, Bracket)>,
    actions: Vec<Action>,
    next_id: OrderId,
    next_oco: u32,
    /// Set by the risk layer: new orders must be reduce-only.
    reduce_only: bool,
    multiplier: f64,
    tick: TickRule,
}

impl Ctx {
    pub fn new(bar_period: i64) -> Self {
        Self {
            now: 0,
            last_price: f64::NAN,
            position: 0,
            avg_price: 0.0,
            realized_net: 0.0,
            equity: 0.0,
            bar_period,
            working: Vec::with_capacity(8),
            brackets: Vec::new(),
            actions: Vec::with_capacity(8),
            next_id: 1,
            next_oco: 1,
            reduce_only: false,
            multiplier: 1.0,
            tick: TickRule::Fixed(1.0),
        }
    }

    /// Context pre-configured with an instrument's multiplier and tick ladder.
    pub fn for_instrument(bar_period: i64, inst: &Instrument) -> Self {
        let mut c = Self::new(bar_period);
        c.multiplier = inst.multiplier;
        c.tick = inst.tick;
        c
    }

    // ------------------------------------------------------------ read-only state

    #[inline]
    pub fn now(&self) -> Ts {
        self.now
    }
    #[inline]
    pub fn last_price(&self) -> f64 {
        self.last_price
    }
    /// Signed position (contracts / shares). Positive = long.
    #[inline]
    pub fn position(&self) -> i64 {
        self.position
    }
    #[inline]
    pub fn avg_price(&self) -> f64 {
        self.avg_price
    }
    #[inline]
    pub fn is_flat(&self) -> bool {
        self.position == 0
    }
    /// Realized P&L after fees and taxes (NTD).
    #[inline]
    pub fn realized_pnl(&self) -> f64 {
        self.realized_net
    }
    #[inline]
    pub fn equity(&self) -> f64 {
        self.equity
    }
    /// Bar timeframe in microseconds.
    #[inline]
    pub fn bar_period(&self) -> i64 {
        self.bar_period
    }
    /// Close time of a bar delivered to `on_bar`.
    #[inline]
    pub fn bar_end(&self, bar: &Bar) -> Ts {
        bar.ts + self.bar_period
    }
    #[inline]
    pub fn trading_day(&self) -> i64 {
        trading_day(self.now)
    }
    pub fn working_orders(&self) -> &[OrderRequest] {
        &self.working
    }
    pub fn has_working_orders(&self) -> bool {
        !self.working.is_empty()
    }
    /// Net signed quantity of working *market* orders (sent but not yet filled).
    pub fn pending_market_qty(&self) -> i64 {
        self.working.iter().filter(|o| matches!(o.kind, OrderKind::Market)).map(|o| o.side.sign() * o.qty).sum()
    }
    pub fn is_reduce_only(&self) -> bool {
        self.reduce_only
    }
    /// NTD per point per contract (200 for TX, 50 for MTX, 10 for TMF, 1 for stocks).
    #[inline]
    pub fn multiplier(&self) -> f64 {
        self.multiplier
    }
    /// Tick size at `price`.
    #[inline]
    pub fn tick_size(&self, price: f64) -> f64 {
        self.tick.size(price)
    }
    /// Contracts such that a stop `stop_points` away risks at most `risk_ntd`.
    #[inline]
    pub fn qty_for_risk(&self, risk_ntd: f64, stop_points: f64) -> i64 {
        if stop_points <= 0.0 {
            return 0;
        }
        (risk_ntd / (stop_points * self.multiplier)).floor().max(0.0) as i64
    }

    // ------------------------------------------------------------ order entry

    /// Allocate a fresh OCO group id.
    pub fn new_oco(&mut self) -> u32 {
        let g = self.next_oco;
        self.next_oco += 1;
        g
    }

    /// Queue an order. Returns its id, or 0 when the order was dropped (zero quantity,
    /// or it would increase exposure while the risk layer has set reduce-only mode).
    pub fn submit(&mut self, side: Side, qty: i64, kind: OrderKind, tif: Tif, oco: u32, tag: &'static str) -> OrderId {
        if qty == 0 {
            return 0;
        }
        if self.reduce_only {
            let exposure = self.position + self.pending_market_qty();
            let after = exposure + side.sign() * qty.abs();
            if after.abs() > exposure.abs() || after.signum() == -exposure.signum() && after != 0 {
                return 0;
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        let req = OrderRequest { id, side, qty: qty.abs(), kind, tif, oco, tag };
        self.working.push(req);
        self.actions.push(Action::Submit(req));
        id
    }

    pub fn buy(&mut self, qty: i64) -> OrderId {
        self.submit(Side::Buy, qty, OrderKind::Market, Tif::Rod, 0, "buy")
    }
    pub fn sell(&mut self, qty: i64) -> OrderId {
        self.submit(Side::Sell, qty, OrderKind::Market, Tif::Rod, 0, "sell")
    }
    pub fn buy_limit(&mut self, qty: i64, price: f64) -> OrderId {
        self.submit(Side::Buy, qty, OrderKind::Limit(price), Tif::Rod, 0, "buy_limit")
    }
    pub fn sell_limit(&mut self, qty: i64, price: f64) -> OrderId {
        self.submit(Side::Sell, qty, OrderKind::Limit(price), Tif::Rod, 0, "sell_limit")
    }
    pub fn buy_stop(&mut self, qty: i64, price: f64) -> OrderId {
        self.submit(Side::Buy, qty, OrderKind::Stop(price), Tif::Rod, 0, "buy_stop")
    }
    pub fn sell_stop(&mut self, qty: i64, price: f64) -> OrderId {
        self.submit(Side::Sell, qty, OrderKind::Stop(price), Tif::Rod, 0, "sell_stop")
    }

    /// Entry order with an automatic OCO stop-loss / take-profit around the fill price.
    pub fn enter(&mut self, side: Side, qty: i64, kind: OrderKind, bracket: Bracket, tag: &'static str) -> OrderId {
        let id = self.submit(side, qty, kind, Tif::Rod, 0, tag);
        self.attach_bracket(id, bracket);
        id
    }

    /// Attach an OCO stop-loss / take-profit pair to an already submitted entry order.
    pub fn attach_bracket(&mut self, entry_id: OrderId, bracket: Bracket) {
        if entry_id != 0 && (bracket.stop_dist.is_some() || bracket.take_dist.is_some()) {
            self.brackets.push((entry_id, bracket));
        }
    }

    /// Send a market order for the difference between `target` and the current position
    /// (including market orders already in flight, so calling this on every tick is safe).
    pub fn target_position(&mut self, target: i64) -> Option<OrderId> {
        let diff = target - self.position - self.pending_market_qty();
        match diff.signum() {
            1 => Some(self.submit(Side::Buy, diff, OrderKind::Market, Tif::Rod, 0, "target")),
            -1 => Some(self.submit(Side::Sell, -diff, OrderKind::Market, Tif::Rod, 0, "target")),
            _ => None,
        }
    }

    pub fn cancel(&mut self, id: OrderId) {
        self.actions.push(Action::Cancel(id));
    }

    pub fn cancel_all(&mut self) {
        if !self.working.is_empty() {
            self.actions.push(Action::CancelAll);
        }
    }

    /// Cancel everything and close the position at market.
    pub fn flatten(&mut self) {
        self.cancel_all();
        // Pending market orders are about to be cancelled, so target against the raw position.
        let pos = self.position;
        if pos > 0 {
            self.submit(Side::Sell, pos, OrderKind::Market, Tif::Rod, 0, "flatten");
        } else if pos < 0 {
            self.submit(Side::Buy, -pos, OrderKind::Market, Tif::Rod, 0, "flatten");
        }
    }

    // ------------------------------------------------------------ engine hooks
    // These are called by the backtest / live engines, not by strategies.

    #[doc(hidden)]
    #[inline]
    pub fn engine_set_clock(&mut self, now: Ts, last_price: f64) {
        self.now = now;
        self.last_price = last_price;
    }

    #[doc(hidden)]
    #[inline]
    pub fn engine_sync_account(&mut self, position: i64, avg_price: f64, realized_net: f64, equity: f64) {
        self.position = position;
        self.avg_price = avg_price;
        self.realized_net = realized_net;
        self.equity = equity;
    }

    #[doc(hidden)]
    pub fn engine_set_reduce_only(&mut self, v: bool) {
        self.reduce_only = v;
    }

    /// Drain pending actions into `out` (reusing its allocation).
    #[doc(hidden)]
    #[inline]
    pub fn engine_drain(&mut self, out: &mut Vec<Action>) {
        out.clear();
        out.append(&mut self.actions);
    }

    #[doc(hidden)]
    #[inline]
    pub fn engine_has_actions(&self) -> bool {
        !self.actions.is_empty()
    }

    /// An order left the book (cancelled / rejected / expired).
    #[doc(hidden)]
    pub fn engine_order_closed(&mut self, id: OrderId) {
        self.working.retain(|o| o.id != id);
        self.brackets.retain(|b| b.0 != id);
    }

    /// Apply a fill: removes / reduces the working order and places bracket children.
    #[doc(hidden)]
    pub fn engine_on_fill(&mut self, fill: &Fill) {
        let mut done = false;
        if let Some(o) = self.working.iter_mut().find(|o| o.id == fill.order_id) {
            o.qty -= fill.qty;
            done = o.qty <= 0;
        }
        if done {
            self.working.retain(|o| o.id != fill.order_id);
        }
        if let Some(pos) = self.brackets.iter().position(|b| b.0 == fill.order_id) {
            let (_, br) = self.brackets[pos];
            if done {
                self.brackets.swap_remove(pos);
            }
            let exit = fill.side.opposite();
            let dir = fill.side.sign() as f64;
            let oco = self.new_oco();
            if let Some(d) = br.stop_dist {
                self.submit(exit, fill.qty, OrderKind::Stop(fill.price - dir * d), Tif::Rod, oco, "sl");
            }
            if let Some(d) = br.take_dist {
                self.submit(exit, fill.qty, OrderKind::Limit(fill.price + dir * d), Tif::Rod, oco, "tp");
            }
        }
    }
}

/// Named numeric strategy parameters, e.g. `fast=10,slow=30`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Params {
    map: BTreeMap<String, f64>,
}

impl Params {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse `k=v,k2=v2` (also accepts spaces / semicolons as separators).
    pub fn parse(s: &str) -> Result<Self> {
        let mut p = Self::new();
        for part in s.split([',', ';', ' ']).filter(|x| !x.trim().is_empty()) {
            let (k, v) = part.split_once('=').ok_or_else(|| anyhow!("bad param '{part}', expected key=value"))?;
            let v: f64 = v.trim().parse().map_err(|_| anyhow!("bad number in '{part}'"))?;
            p.map.insert(k.trim().to_string(), v);
        }
        Ok(p)
    }

    pub fn with(mut self, k: &str, v: f64) -> Self {
        self.map.insert(k.to_string(), v);
        self
    }

    pub fn set(&mut self, k: &str, v: f64) {
        self.map.insert(k.to_string(), v);
    }

    #[inline]
    pub fn get(&self, k: &str, default: f64) -> f64 {
        self.map.get(k).copied().unwrap_or(default)
    }

    #[inline]
    pub fn usize(&self, k: &str, default: usize) -> usize {
        self.map.get(k).map(|v| v.max(0.0).round() as usize).unwrap_or(default)
    }

    #[inline]
    pub fn flag(&self, k: &str, default: bool) -> bool {
        self.map.get(k).map(|v| *v != 0.0).unwrap_or(default)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &f64)> {
        self.map.iter()
    }

    /// Merge `other` on top of `self`.
    pub fn merged(&self, other: &Params) -> Params {
        let mut p = self.clone();
        for (k, v) in &other.map {
            p.map.insert(k.clone(), *v);
        }
        p
    }
}

impl fmt::Display for Params {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (k, v) in &self.map {
            if !first {
                write!(f, ",")?;
            }
            first = false;
            write!(f, "{k}={v}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_parse() {
        let p = Params::parse("fast=10, slow=30;k=2.5").unwrap();
        assert_eq!(p.usize("fast", 0), 10);
        assert_eq!(p.get("k", 0.0), 2.5);
        assert_eq!(p.get("missing", 7.0), 7.0);
        assert_eq!(p.to_string(), "fast=10,k=2.5,slow=30");
        assert!(Params::parse("oops").is_err());
    }

    #[test]
    fn target_accounts_for_pending() {
        let mut c = Ctx::new(60);
        c.target_position(2);
        assert_eq!(c.pending_market_qty(), 2);
        assert!(c.target_position(2).is_none());
        c.target_position(-1);
        assert_eq!(c.pending_market_qty(), -1);
    }

    #[test]
    fn reduce_only_blocks_new_exposure() {
        let mut c = Ctx::new(60);
        c.engine_sync_account(2, 100.0, 0.0, 0.0);
        c.engine_set_reduce_only(true);
        assert_eq!(c.buy(1), 0);
        assert_eq!(c.sell(3), 0); // would flip short
        assert_ne!(c.sell(2), 0);
        let mut d = Ctx::new(60);
        d.engine_set_reduce_only(true);
        assert_eq!(d.sell(1), 0);
    }

    #[test]
    fn bracket_children_on_fill() {
        let mut c = Ctx::new(60);
        let id =
            c.enter(Side::Buy, 1, OrderKind::Market, Bracket { stop_dist: Some(30.0), take_dist: Some(60.0) }, "e");
        let fill = Fill { order_id: id, ts: 0, side: Side::Buy, qty: 1, price: 100.0, fee: 0.0, tax: 0.0, tag: "e" };
        c.engine_on_fill(&fill);
        let w = c.working_orders();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].kind, OrderKind::Stop(70.0));
        assert_eq!(w[1].kind, OrderKind::Limit(160.0));
        assert_eq!(w[0].oco, w[1].oco);
        assert_eq!(w[0].side, Side::Sell);
    }
}
