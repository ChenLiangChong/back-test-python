//! Simulated matching engine used by the backtester (bar and tick mode) and by paper
//! trading on live quotes.
//!
//! **Bar mode** walks an intrabar path `O → L → H → C` when the open is nearer the
//! low and `O → H → L → C` otherwise (NautilusTrader's adaptive rule). Orders trigger in the order the path reaches them,
//! so a stop-loss and a take-profit touched in the same bar resolve deterministically,
//! and orders submitted from `on_fill` (e.g. bracket stops) keep matching against the
//! *rest* of the same bar. Gaps fill at the open.
//!
//! **Tick mode** matches every order against each trade print. Limits need the price
//! to trade *through* them unless `limit_fill_on_touch` is set (queue position is
//! unknown, so touch fills are optimistic).

use crate::instrument::Instrument;
use crate::time::Ts;
use crate::types::{Bar, OrderId, OrderKind, OrderRequest, Side, Tick, Tif};

#[derive(Clone, Copy, Debug)]
pub struct SimConfig {
    /// Adverse slippage for market and stop orders, in ticks.
    pub slippage_ticks: f64,
    /// Fill resting limits when price merely touches them (optimistic).
    pub limit_fill_on_touch: bool,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self { slippage_ticks: 1.0, limit_fill_on_touch: false }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Exec {
    pub order: OrderRequest,
    pub price: f64,
    pub ts: Ts,
}

#[derive(Clone, Copy, Debug)]
struct Working {
    req: OrderRequest,
    /// Not yet checked against any price (IOC / marketable-limit handling).
    fresh: bool,
}

/// Position along the intrabar path of one bar.
#[derive(Clone, Copy, Debug)]
pub struct BarPath {
    pts: [f64; 4],
    seg: usize,
    cur: f64,
    ts: Ts,
}

impl BarPath {
    pub fn new(bar: &Bar) -> Self {
        let (p1, p2) = if bar.low_first() { (bar.low, bar.high) } else { (bar.high, bar.low) };
        Self { pts: [bar.open, p1, p2, bar.close], seg: 0, cur: bar.open, ts: bar.ts }
    }
    #[inline]
    pub fn current(&self) -> f64 {
        self.cur
    }
}

#[derive(Clone, Debug)]
pub struct SimExchange {
    cfg: SimConfig,
    inst: Instrument,
    orders: Vec<Working>,
    cancelled: Vec<OrderId>,
}

impl SimExchange {
    pub fn new(cfg: SimConfig, inst: Instrument) -> Self {
        Self { cfg, inst, orders: Vec::with_capacity(16), cancelled: Vec::new() }
    }

    pub fn submit(&mut self, req: OrderRequest) {
        self.orders.push(Working { req, fresh: true });
    }

    pub fn cancel(&mut self, id: OrderId) -> bool {
        let n = self.orders.len();
        self.orders.retain(|w| w.req.id != id);
        n != self.orders.len()
    }

    pub fn cancel_all(&mut self) -> Vec<OrderId> {
        let mut v = Vec::with_capacity(self.orders.len());
        self.cancel_all_into(&mut v);
        v
    }

    /// Allocation-free variant of [`SimExchange::cancel_all`]: appends ids to `out`.
    pub fn cancel_all_into(&mut self, out: &mut Vec<OrderId>) {
        out.extend(self.orders.drain(..).map(|w| w.req.id));
    }

    pub fn working(&self) -> impl Iterator<Item = &OrderRequest> {
        self.orders.iter().map(|w| &w.req)
    }

    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }

    /// Orders cancelled by the exchange itself (OCO siblings, unfilled IOC) since the
    /// last call; the engine forwards these to `Ctx`.
    pub fn take_cancelled(&mut self, out: &mut Vec<OrderId>) {
        out.clear();
        out.append(&mut self.cancelled);
    }

    #[inline]
    fn slip(&self, side: Side, px: f64) -> f64 {
        if self.cfg.slippage_ticks == 0.0 {
            return px;
        }
        px + side.sign() as f64 * self.cfg.slippage_ticks * self.inst.tick_size(px)
    }

    fn remove_filled(&mut self, idx: usize) -> OrderRequest {
        let req = self.orders.remove(idx).req;
        if req.oco != 0 {
            let cancelled = &mut self.cancelled;
            self.orders.retain(|w| {
                let sib = w.req.oco == req.oco;
                if sib {
                    cancelled.push(w.req.id);
                }
                !sib
            });
        }
        req
    }

    /// Price at which `w` executes if it is already marketable at price `a`.
    /// Resting limits still need a trade-through (unless touch mode); a limit that is
    /// marketable on arrival fills immediately.
    #[inline]
    fn fill_at_point(&self, w: &Working, a: f64) -> Option<f64> {
        let o = &w.req;
        let eq_ok = w.fresh || self.cfg.limit_fill_on_touch;
        match (o.kind, o.side) {
            (OrderKind::Market, s) => Some(self.slip(s, a)),
            (OrderKind::Limit(l), Side::Buy) if a < l || (eq_ok && a == l) => Some(a),
            (OrderKind::Limit(l), Side::Sell) if a > l || (eq_ok && a == l) => Some(a),
            (OrderKind::Stop(s), Side::Buy) if a >= s => Some(self.slip(Side::Buy, a)),
            (OrderKind::Stop(s), Side::Sell) if a <= s => Some(self.slip(Side::Sell, a)),
            _ => None,
        }
    }

    /// If `o` triggers while price moves from `a` to `b`, returns (trigger price, fill price).
    #[inline]
    fn fill_on_move(&self, o: &OrderRequest, a: f64, b: f64) -> Option<(f64, f64)> {
        let touch = self.cfg.limit_fill_on_touch;
        match (o.kind, o.side) {
            (OrderKind::Limit(l), Side::Buy) if b < l || (touch && b <= l) => Some((l, l)),
            (OrderKind::Limit(l), Side::Sell) if b > l || (touch && b >= l) => Some((l, l)),
            (OrderKind::Stop(s), Side::Buy) if a < s && b >= s => Some((s, self.slip(Side::Buy, s))),
            (OrderKind::Stop(s), Side::Sell) if a > s && b <= s => Some((s, self.slip(Side::Sell, s))),
            _ => None,
        }
    }

    /// Find the next order to execute along the bar path, advance the path to that point,
    /// and return the execution. Returns `None` once the bar is exhausted.
    pub fn next_exec_on_path(&mut self, path: &mut BarPath) -> Option<Exec> {
        if self.orders.is_empty() {
            return None;
        }
        loop {
            let a = path.cur;
            // 1) anything already marketable at the current point (gaps, market orders,
            //    freshly placed orders). Stops win ties (pessimistic).
            let mut best: Option<(usize, f64, bool)> = None;
            for (i, w) in self.orders.iter().enumerate() {
                if let Some(px) = self.fill_at_point(w, a) {
                    let is_stop = !matches!(w.req.kind, OrderKind::Limit(_));
                    if best.is_none() || (is_stop && !best.unwrap().2) {
                        best = Some((i, px, is_stop));
                    }
                }
            }
            if let Some((i, px, _)) = best {
                let req = self.remove_filled(i);
                return Some(Exec { order: req, price: px, ts: path.ts });
            }
            // 2) IOC / FOK orders that are not marketable right now are cancelled.
            if self.orders.iter().any(|w| w.req.tif != Tif::Rod) {
                let cancelled = &mut self.cancelled;
                self.orders.retain(|w| {
                    let kill = w.req.tif != Tif::Rod;
                    if kill {
                        cancelled.push(w.req.id);
                    }
                    !kill
                });
            }
            for w in &mut self.orders {
                w.fresh = false;
            }
            if path.seg >= 3 {
                return None;
            }
            // 3) earliest trigger along the current segment
            let b = path.pts[path.seg + 1];
            let mut best: Option<(usize, f64, f64, f64)> = None; // idx, dist, trigger, fill
            for (i, w) in self.orders.iter().enumerate() {
                if let Some((trig, px)) = self.fill_on_move(&w.req, a, b) {
                    let d = (trig - a).abs();
                    if best.is_none_or(|x| d < x.1) {
                        best = Some((i, d, trig, px));
                    }
                }
            }
            match best {
                Some((i, _, trig, px)) => {
                    path.cur = trig;
                    let req = self.remove_filled(i);
                    return Some(Exec { order: req, price: px, ts: path.ts });
                }
                None => {
                    path.seg += 1;
                    path.cur = b;
                    if self.orders.is_empty() {
                        return None;
                    }
                }
            }
        }
    }

    /// Match all working orders against one trade print. Executions are appended to `out`.
    pub fn match_tick(&mut self, tick: &Tick, out: &mut Vec<Exec>) {
        if self.orders.is_empty() {
            return;
        }
        let p = tick.price;
        let touch = self.cfg.limit_fill_on_touch;
        let mut i = 0;
        while i < self.orders.len() {
            let w = self.orders[i];
            let o = &w.req;
            let px = match (o.kind, o.side) {
                (OrderKind::Market, Side::Buy) => {
                    Some(if tick.ask.is_finite() { tick.ask.max(p) } else { self.slip(Side::Buy, p) })
                }
                (OrderKind::Market, Side::Sell) => {
                    Some(if tick.bid.is_finite() { tick.bid.min(p) } else { self.slip(Side::Sell, p) })
                }
                (OrderKind::Limit(l), Side::Buy) => {
                    if w.fresh && p <= l {
                        Some(p) // marketable on arrival
                    } else if p < l || (touch && p <= l) {
                        Some(l)
                    } else {
                        None
                    }
                }
                (OrderKind::Limit(l), Side::Sell) => {
                    if w.fresh && p >= l {
                        Some(p)
                    } else if p > l || (touch && p >= l) {
                        Some(l)
                    } else {
                        None
                    }
                }
                (OrderKind::Stop(s), Side::Buy) if p >= s => Some(self.slip(Side::Buy, p)),
                (OrderKind::Stop(s), Side::Sell) if p <= s => Some(self.slip(Side::Sell, p)),
                _ => None,
            };
            match px {
                Some(price) => {
                    let before = self.orders.len();
                    let req = self.remove_filled(i);
                    out.push(Exec { order: req, price, ts: tick.ts });
                    // OCO siblings before `i` were removed too; restart the scan safely.
                    let removed = before - self.orders.len();
                    if removed > 1 {
                        i = 0;
                    }
                }
                None if o.tif != Tif::Rod => {
                    self.cancelled.push(o.id);
                    self.orders.remove(i);
                }
                None => {
                    self.orders[i].fresh = false;
                    i += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(id: OrderId, side: Side, kind: OrderKind, oco: u32) -> OrderRequest {
        OrderRequest { id, side, qty: 1, kind, tif: Tif::Rod, oco, tag: "" }
    }

    fn bar(o: f64, h: f64, l: f64, c: f64) -> Bar {
        Bar { ts: 0, open: o, high: h, low: l, close: c, volume: 1.0 }
    }

    fn ex(slip: f64) -> SimExchange {
        SimExchange::new(SimConfig { slippage_ticks: slip, limit_fill_on_touch: false }, Instrument::tx())
    }

    #[test]
    fn market_fills_at_open_with_slippage() {
        let mut e = ex(1.0);
        e.submit(req(1, Side::Buy, OrderKind::Market, 0));
        let mut p = BarPath::new(&bar(100.0, 110.0, 90.0, 105.0));
        let x = e.next_exec_on_path(&mut p).unwrap();
        assert_eq!(x.price, 101.0);
        assert!(e.next_exec_on_path(&mut p).is_none());
    }

    #[test]
    fn stop_gap_fills_at_open() {
        let mut e = ex(0.0);
        e.submit(req(1, Side::Sell, OrderKind::Stop(95.0), 0));
        let mut p = BarPath::new(&bar(90.0, 92.0, 85.0, 88.0));
        assert_eq!(e.next_exec_on_path(&mut p).unwrap().price, 90.0);
    }

    #[test]
    fn open_near_low_hits_low_first_so_stop_wins() {
        // long bracket: SL 95, TP 108; O100 L94 H110 C109, open nearer low -> O->L->H: SL first
        let mut e = ex(0.0);
        e.submit(req(1, Side::Sell, OrderKind::Stop(95.0), 7));
        e.submit(req(2, Side::Sell, OrderKind::Limit(108.0), 7));
        let mut p = BarPath::new(&bar(100.0, 110.0, 94.0, 109.0));
        let x = e.next_exec_on_path(&mut p).unwrap();
        assert_eq!(x.order.id, 1);
        assert_eq!(x.price, 95.0);
        assert!(e.next_exec_on_path(&mut p).is_none());
        let mut c = vec![];
        e.take_cancelled(&mut c);
        assert_eq!(c, vec![2]);
    }

    #[test]
    fn open_near_high_hits_high_first_so_tp_wins() {
        let mut e = ex(0.0);
        e.submit(req(1, Side::Sell, OrderKind::Stop(95.0), 7));
        e.submit(req(2, Side::Sell, OrderKind::Limit(103.0), 7));
        let mut p = BarPath::new(&bar(100.0, 104.0, 90.0, 92.0));
        let x = e.next_exec_on_path(&mut p).unwrap();
        assert_eq!(x.order.id, 2);
        assert_eq!(x.price, 103.0);
    }

    #[test]
    fn order_added_mid_path_continues_from_fill_point() {
        // entry buy-stop at 105; O100 L98 H112 C101 -> path O,L,H,C
        let mut e = ex(0.0);
        e.submit(req(1, Side::Buy, OrderKind::Stop(105.0), 0));
        let mut p = BarPath::new(&bar(100.0, 112.0, 98.0, 101.0));
        let x = e.next_exec_on_path(&mut p).unwrap();
        assert_eq!(x.price, 105.0);
        // bracket SL at 102 placed after entry: path continues 105 -> 112 -> 101 => SL hits
        e.submit(req(2, Side::Sell, OrderKind::Stop(102.0), 0));
        let y = e.next_exec_on_path(&mut p).unwrap();
        assert_eq!(y.order.id, 2);
        assert_eq!(y.price, 102.0);
    }

    #[test]
    fn limit_requires_trade_through() {
        let mut e = ex(0.0);
        e.submit(req(1, Side::Buy, OrderKind::Limit(95.0), 0));
        let mut p = BarPath::new(&bar(100.0, 101.0, 95.0, 100.0));
        assert!(e.next_exec_on_path(&mut p).is_none());
        let mut p = BarPath::new(&bar(100.0, 101.0, 94.0, 100.0));
        assert_eq!(e.next_exec_on_path(&mut p).unwrap().price, 95.0);
    }

    #[test]
    fn ioc_cancels_if_not_marketable() {
        let mut e = ex(0.0);
        let mut r = req(1, Side::Buy, OrderKind::Limit(90.0), 0);
        r.tif = Tif::Ioc;
        e.submit(r);
        let mut p = BarPath::new(&bar(100.0, 101.0, 80.0, 100.0));
        assert!(e.next_exec_on_path(&mut p).is_none());
        let mut c = vec![];
        e.take_cancelled(&mut c);
        assert_eq!(c, vec![1]);
    }

    #[test]
    fn tick_matching() {
        let mut e = ex(1.0);
        e.submit(req(1, Side::Buy, OrderKind::Stop(105.0), 0));
        e.submit(req(2, Side::Sell, OrderKind::Limit(110.0), 3));
        e.submit(req(3, Side::Sell, OrderKind::Stop(90.0), 3));
        let mut out = vec![];
        e.match_tick(&Tick::trade(0, 104.0, 1.0), &mut out);
        assert!(out.is_empty());
        e.match_tick(&Tick::trade(1, 106.0, 1.0), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].price, 107.0);
        out.clear();
        e.match_tick(&Tick::trade(2, 110.0, 1.0), &mut out);
        assert!(out.is_empty()); // needs trade-through
        e.match_tick(&Tick::trade(3, 111.0, 1.0), &mut out);
        assert_eq!(out[0].order.id, 2);
        assert_eq!(out[0].price, 110.0);
        assert!(e.is_empty()); // OCO sibling gone
    }
}
