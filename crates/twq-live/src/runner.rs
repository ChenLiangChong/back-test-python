//! The live / paper trading loop.
//!
//! One engine thread owns the strategy, `Ctx`, portfolio, risk manager and order
//! book; a reader thread feeds it bridge events over a bounded channel. There is no
//! locking and no allocation on the per-tick path, so tick → decision → order-on-the-
//! wire stays in the low microseconds; the broker round trip (milliseconds) dominates.
//!
//! * **Paper** (`ExecMode::Paper`): real quotes from the bridge, fills simulated
//!   locally by the same `SimExchange` the backtester uses. Nothing is sent to the broker.
//! * **Live** (`ExecMode::Live`): market / limit orders go to the broker through the
//!   bridge. Stop orders are *synthetic*: held in the engine and fired as market (or
//!   範圍市價) IOC orders on the first trade through the stop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{RecvTimeoutError, TryRecvError};
use serde_json::json;
use twq_core::aggregator::BarAggregator;
use twq_core::time::{fmt_ts, trading_day, Ts, US_PER_MIN};
use twq_core::{
    Action, Bar, Ctx, Exec, Fill, Instrument, OrderId, OrderKind, OrderRequest, OrderStatus, OrderUpdate, Portfolio,
    Side, SimConfig, SimExchange, Strategy, Tick, Tif, Trade,
};

use crate::bridge::{BridgeConn, Inbound};
use crate::journal::Journal;
use crate::latency::{Latency, LatencySummary};
use crate::protocol::{BridgeEvent, EngineCmd};
use crate::risk::{RiskLimits, RiskManager};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecMode {
    Paper,
    Live,
}

#[derive(Clone, Debug)]
pub struct LiveConfig {
    pub instrument: Instrument,
    /// Symbol to subscribe quotes for (群益: e.g. `TX00` near-month).
    pub quote_symbol: String,
    /// Symbol used in orders (群益 order codes can differ from quote codes).
    pub order_symbol: String,
    pub bar_period: i64,
    pub mode: ExecMode,
    pub risk: RiskLimits,
    pub initial_equity: f64,
    pub sim: SimConfig,
    /// Market order flavour sent to the broker: "P" 範圍市價 (default) or "M" 市價.
    pub market_style: String,
    pub day_trade: bool,
    pub journal: Option<PathBuf>,
    /// Touch this file to flatten everything and stop (manual kill switch).
    pub kill_file: Option<PathBuf>,
    pub max_runtime: Option<Duration>,
    pub flatten_on_exit: bool,
    /// Close a bar this long after its end when no new tick arrives.
    pub bar_flush_grace: Duration,
    /// History fed through the strategy before trading (orders suppressed).
    pub warmup: Vec<Bar>,
    /// Busy-poll the event queue instead of sleeping: removes the thread wake-up
    /// latency (tens of µs on VMs) at the cost of one fully used CPU core.
    pub spin: bool,
    pub verbose: bool,
}

impl LiveConfig {
    pub fn new(instrument: Instrument, symbol: &str, mode: ExecMode) -> Self {
        Self {
            instrument,
            quote_symbol: symbol.to_string(),
            order_symbol: symbol.to_string(),
            bar_period: US_PER_MIN,
            mode,
            risk: RiskLimits::default(),
            initial_equity: 2_000_000.0,
            sim: SimConfig::default(),
            market_style: "P".into(),
            day_trade: false,
            journal: None,
            kill_file: None,
            max_runtime: None,
            flatten_on_exit: true,
            bar_flush_grace: Duration::from_millis(500),
            warmup: Vec::new(),
            spin: false,
            verbose: false,
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct LiveSummary {
    pub bridge: String,
    pub ticks: u64,
    pub bars: u64,
    pub orders_sent: u64,
    pub fills: u64,
    pub cancels: u64,
    pub rejects: u64,
    pub position: i64,
    pub realized_net: f64,
    pub equity: f64,
    pub halted: Option<String>,
    /// Bridge message received → strategy + risk + order routing finished.
    pub tick_latency: LatencySummary,
    /// Bridge message received → order written to the bridge socket.
    pub order_latency: LatencySummary,
    /// Pure engine time per tick (dequeue → strategy, risk and routing done), i.e.
    /// excluding the reader-thread → engine-thread hand-off.
    pub engine_latency: LatencySummary,
    #[serde(skip)]
    pub trades: Vec<Trade>,
}

#[derive(Clone, Copy, Debug)]
struct LiveOrder {
    req: OrderRequest,
    filled: i64,
    /// false = synthetic stop held in the engine.
    at_broker: bool,
    cancel_sent: bool,
}

struct Runner<'a, S: Strategy + ?Sized> {
    strat: &'a mut S,
    cfg: &'a LiveConfig,
    conn: &'a mut BridgeConn,
    ctx: Ctx,
    pf: Portfolio,
    risk: RiskManager,
    sim: SimExchange,
    orders: HashMap<OrderId, LiveOrder>,
    agg: BarAggregator,
    actions: Vec<Action>,
    cancelled: Vec<OrderId>,
    execs: Vec<Exec>,
    journal: Journal,
    last_px: f64,
    last_tick_ts: Ts,
    last_tick_at: Option<Instant>,
    cur_recv: Option<Instant>,
    tick_lat: Latency,
    order_lat: Latency,
    engine_lat: Latency,
    sum: LiveSummary,
    warming: bool,
    stop: bool,
    /// Set by the kill file: stop the loop once the book is flat.
    exit_when_flat: bool,
}

impl<'a, S: Strategy + ?Sized> Runner<'a, S> {
    /// Journal an event. The JSON is only built when someone will read it, and callers
    /// log *after* the latency-critical work (e.g. after the order hit the socket).
    #[inline]
    fn log(&self, f: impl FnOnce() -> serde_json::Value) {
        if self.cfg.verbose || self.journal.enabled() {
            let v = f();
            if self.cfg.verbose {
                eprintln!("{v}");
            }
            self.journal.log(v);
        }
    }

    #[inline]
    fn equity(&self) -> f64 {
        let mark = if self.last_px.is_finite() { self.last_px } else { self.pf.avg_price() };
        self.cfg.initial_equity + self.pf.realized_net() + self.pf.unrealized(mark)
    }

    #[inline]
    fn sync_account(&mut self) {
        let eq = self.equity();
        self.ctx.engine_sync_account(self.pf.position(), self.pf.avg_price(), self.pf.realized_net(), eq);
    }

    fn notify(&mut self, id: OrderId, status: OrderStatus) {
        let u = OrderUpdate { order_id: id, ts: self.ctx.now(), status };
        self.strat.on_order_update(&u, &mut self.ctx);
    }

    // ------------------------------------------------------------------ orders

    fn process_actions(&mut self) {
        while self.ctx.engine_has_actions() {
            let mut acts = std::mem::take(&mut self.actions);
            self.ctx.engine_drain(&mut acts);
            for a in acts.iter().copied() {
                match a {
                    Action::Submit(o) => self.submit(o),
                    Action::Cancel(id) => self.cancel(id),
                    Action::CancelAll => {
                        let ids: Vec<OrderId> = match self.cfg.mode {
                            ExecMode::Paper => self.sim.working().map(|o| o.id).collect(),
                            ExecMode::Live => self.orders.keys().copied().collect(),
                        };
                        for id in ids {
                            self.cancel(id);
                        }
                    }
                }
            }
            self.actions = acts;
        }
    }

    fn submit(&mut self, o: OrderRequest) {
        if self.warming {
            self.ctx.engine_order_closed(o.id);
            return;
        }
        let pos = self.pf.position();
        let exposure = pos + self.ctx.pending_market_qty()
            - if matches!(o.kind, OrderKind::Market) { o.side.sign() * o.qty } else { 0 };
        if let Err(reason) = self.risk.check(&o, pos, exposure, self.last_px, Instant::now()) {
            self.sum.rejects += 1;
            self.log(|| json!({"ev":"risk_reject","id":o.id,"tag":o.tag,"reason":reason,"ts":fmt_ts(self.ctx.now())}));
            self.ctx.engine_order_closed(o.id);
            self.notify(o.id, OrderStatus::Rejected(reason));
            return;
        }
        match self.cfg.mode {
            ExecMode::Paper => {
                self.sim.submit(o);
                self.sum.orders_sent += 1;
                self.record_order_latency();
                self.log(|| json!({"ev":"paper_order","id":o.id,"side":format!("{:?}",o.side),"qty":o.qty,"kind":format!("{:?}",o.kind),"tag":o.tag,"ts":fmt_ts(self.ctx.now())}));
            }
            ExecMode::Live => match o.kind {
                OrderKind::Stop(_) => {
                    self.orders.insert(o.id, LiveOrder { req: o, filled: 0, at_broker: false, cancel_sent: false });
                    self.log(|| json!({"ev":"local_stop","id":o.id,"side":format!("{:?}",o.side),"qty":o.qty,"kind":format!("{:?}",o.kind),"tag":o.tag}));
                }
                _ => self.send_to_broker(o),
            },
        }
    }

    fn send_to_broker(&mut self, o: OrderRequest) {
        let (px, tif, mkt) = match o.kind {
            // 群益: "M"/"P" market orders are only valid as IOC / FOK.
            OrderKind::Market | OrderKind::Stop(_) => (None, "IOC", self.cfg.market_style.clone()),
            OrderKind::Limit(p) => {
                let p = self.cfg.instrument.round_to_tick(p, o.side == Side::Sell);
                let tif = match o.tif {
                    Tif::Rod => "ROD",
                    Tif::Ioc => "IOC",
                    Tif::Fok => "FOK",
                };
                (Some(p), tif, String::new())
            }
        };
        let cmd = EngineCmd::Order {
            cid: o.id,
            sym: self.cfg.order_symbol.clone(),
            side: if o.side == Side::Buy { "B".into() } else { "S".into() },
            qty: o.qty,
            px,
            tif: tif.into(),
            mkt,
            day_trade: self.cfg.day_trade,
            oc: "auto".into(),
        };
        match self.conn.send(&cmd) {
            Ok(()) => {
                self.record_order_latency();
                self.sum.orders_sent += 1;
                let entry = self.orders.entry(o.id).or_insert(LiveOrder {
                    req: o,
                    filled: 0,
                    at_broker: true,
                    cancel_sent: false,
                });
                entry.at_broker = true;
                entry.req.kind = if matches!(o.kind, OrderKind::Stop(_)) { OrderKind::Market } else { o.kind };
                self.log(|| json!({"ev":"order","cmd":cmd,"tag":o.tag,"ts":fmt_ts(self.ctx.now())}));
            }
            Err(e) => {
                self.log(|| json!({"ev":"send_error","id":o.id,"err":e.to_string()}));
                self.orders.remove(&o.id);
                self.ctx.engine_order_closed(o.id);
                self.notify(o.id, OrderStatus::Rejected(format!("bridge send failed: {e}")));
                self.risk.halt("bridge connection lost");
            }
        }
    }

    #[inline]
    fn record_order_latency(&mut self) {
        if let Some(r) = self.cur_recv {
            self.order_lat.record(r.elapsed());
        }
    }

    fn cancel(&mut self, id: OrderId) {
        match self.cfg.mode {
            ExecMode::Paper => {
                if self.sim.cancel(id) {
                    self.sum.cancels += 1;
                    self.ctx.engine_order_closed(id);
                    self.notify(id, OrderStatus::Cancelled);
                }
            }
            ExecMode::Live => match self.orders.get(&id).copied() {
                Some(o) if !o.at_broker => {
                    self.orders.remove(&id);
                    self.sum.cancels += 1;
                    self.ctx.engine_order_closed(id);
                    self.notify(id, OrderStatus::Cancelled);
                }
                Some(o) if !o.cancel_sent => {
                    if let Some(x) = self.orders.get_mut(&id) {
                        x.cancel_sent = true;
                    }
                    let _ = self.conn.send(&EngineCmd::Cancel { cid: id });
                    self.log(|| json!({"ev":"cancel_sent","id":id}));
                }
                _ => {}
            },
        }
    }

    /// Fire synthetic stops touched by this trade.
    fn check_local_stops(&mut self, px: f64) {
        let mut hit: Vec<OrderRequest> = Vec::new();
        for o in self.orders.values() {
            if o.at_broker {
                continue;
            }
            if let OrderKind::Stop(s) = o.req.kind {
                if (o.req.side == Side::Buy && px >= s) || (o.req.side == Side::Sell && px <= s) {
                    hit.push(o.req);
                }
            }
        }
        hit.sort_by_key(|o| o.id);
        for o in hit {
            self.log(|| json!({"ev":"stop_triggered","id":o.id,"px":px,"tag":o.tag}));
            self.send_to_broker(o);
        }
    }

    // ------------------------------------------------------------------ fills

    #[allow(clippy::too_many_arguments)]
    fn on_fill(
        &mut self,
        id: OrderId,
        ts: Ts,
        side: Side,
        qty: i64,
        px: f64,
        fee: Option<f64>,
        tax: Option<f64>,
        tag: &'static str,
    ) {
        let fill = match (fee, tax) {
            (Some(fee), Some(tax)) => {
                let f = Fill { order_id: id, ts, side, qty, price: px, fee, tax, tag };
                self.pf.apply(&f);
                f
            }
            _ => self.pf.execute(id, ts, side, qty, px, tag),
        };
        self.sum.fills += 1;
        self.ctx.engine_on_fill(&fill);
        self.sync_account();
        self.log(|| json!({"ev":"fill","id":id,"side":format!("{side:?}"),"qty":qty,"px":px,"fee":fill.fee,"tax":fill.tax,"tag":tag,
            "pos":self.pf.position(),"realized":self.pf.realized_net(),"ts":fmt_ts(ts)}));
        if self.cfg.mode == ExecMode::Live {
            let mut done_oco = 0;
            if let Some(o) = self.orders.get_mut(&id) {
                o.filled += qty;
                if o.filled >= o.req.qty {
                    done_oco = o.req.oco;
                    self.orders.remove(&id);
                }
            }
            if done_oco != 0 {
                let sibs: Vec<OrderId> =
                    self.orders.values().filter(|o| o.req.oco == done_oco).map(|o| o.req.id).collect();
                for s in sibs {
                    self.cancel(s);
                }
            }
        }
        self.strat.on_fill(&fill, &mut self.ctx);
        self.process_actions();
    }

    fn forward_sim_cancels(&mut self) {
        self.sim.take_cancelled(&mut self.cancelled);
        let ids = std::mem::take(&mut self.cancelled);
        for &id in &ids {
            self.sum.cancels += 1;
            self.ctx.engine_order_closed(id);
            self.notify(id, OrderStatus::Cancelled);
        }
        self.cancelled = ids;
    }

    // ------------------------------------------------------------------ market data

    fn on_bar(&mut self, bar: Bar) {
        let end = bar.ts + self.agg.period();
        self.ctx.engine_set_clock(end, bar.close);
        self.sync_account();
        self.sum.bars += 1;
        self.strat.on_bar(&bar, &mut self.ctx);
        self.process_actions();
        if !self.warming && self.cfg.verbose {
            eprintln!(
                "[bar] {} O{} H{} L{} C{} V{} pos={} eq={:.0}",
                fmt_ts(bar.ts),
                bar.open,
                bar.high,
                bar.low,
                bar.close,
                bar.volume,
                self.pf.position(),
                self.equity()
            );
        }
    }

    fn on_tick(&mut self, t: Tick, recv_at: Instant) {
        self.sum.ticks += 1;
        self.last_px = t.price;
        self.last_tick_ts = t.ts;
        self.last_tick_at = Some(recv_at);
        if let Some(done) = self.agg.update(&t) {
            self.on_bar(done);
        }
        self.ctx.engine_set_clock(t.ts, t.price);
        match self.cfg.mode {
            ExecMode::Paper => {
                if !self.sim.is_empty() {
                    self.execs.clear();
                    self.sim.match_tick(&t, &mut self.execs);
                    let execs = std::mem::take(&mut self.execs);
                    for e in &execs {
                        let o = e.order;
                        self.on_fill(o.id, e.ts, o.side, o.qty, e.price, None, None, o.tag);
                    }
                    self.execs = execs;
                    self.forward_sim_cancels();
                }
            }
            ExecMode::Live => self.check_local_stops(t.price),
        }
        if self.strat.wants_ticks() {
            self.sync_account();
            self.strat.on_tick(&t, &mut self.ctx);
            self.process_actions();
        }
        self.check_risk();
    }

    fn check_risk(&mut self) {
        let eq = self.equity();
        if let Some(reason) = self.risk.on_equity(trading_day(self.ctx.now()), eq) {
            self.halt(&reason);
        }
    }

    fn halt(&mut self, reason: &str) {
        self.risk.halt(reason.to_string());
        self.log(|| json!({"ev":"halt","reason":reason,"ts":fmt_ts(self.ctx.now())}));
        eprintln!("!! HALT: {reason} — flattening, only reducing orders allowed");
        self.ctx.engine_set_reduce_only(true);
        self.ctx.flatten();
        self.process_actions();
    }

    // ------------------------------------------------------------------ bridge events

    fn handle(&mut self, inb: Inbound) {
        let recv_at = inb.recv_at;
        self.cur_recv = Some(recv_at);
        match inb.ev {
            BridgeEvent::Tick { ts, px, qty, bid, ask, sim, .. } => {
                if !sim {
                    let t0 = Instant::now();
                    let t = Tick { ts, price: px, qty, bid: bid.unwrap_or(f64::NAN), ask: ask.unwrap_or(f64::NAN) };
                    self.on_tick(t, recv_at);
                    self.engine_lat.record(t0.elapsed());
                    self.tick_lat.record(recv_at.elapsed());
                }
            }
            BridgeEvent::Fill { cid, px, qty, ts, fee, tax } => match self.orders.get(&cid).copied() {
                Some(o) => self.on_fill(cid, ts, o.req.side, qty, px, fee, tax, o.req.tag),
                None => self.log(|| json!({"ev":"unknown_fill","cid":cid,"px":px,"qty":qty})),
            },
            BridgeEvent::Ack { cid, oid } => {
                self.log(|| json!({"ev":"ack","cid":cid,"oid":oid}));
                self.notify(cid, OrderStatus::Accepted);
            }
            BridgeEvent::Cancelled { cid } => {
                if self.orders.remove(&cid).is_some() {
                    self.sum.cancels += 1;
                    self.ctx.engine_order_closed(cid);
                    self.notify(cid, OrderStatus::Cancelled);
                }
            }
            BridgeEvent::Rejected { cid, reason } => {
                self.sum.rejects += 1;
                self.log(|| json!({"ev":"broker_reject","cid":cid,"reason":reason}));
                eprintln!("broker rejected order {cid}: {reason}");
                self.orders.remove(&cid);
                self.ctx.engine_order_closed(cid);
                self.notify(cid, OrderStatus::Rejected(reason));
            }
            BridgeEvent::Position { qty, avg, sym } => {
                self.log(|| json!({"ev":"position","sym":sym,"qty":qty,"avg":avg}));
                let cur = self.pf.position();
                if qty != cur {
                    // adopt the broker's position (reconciliation), zero-cost synthetic fill
                    let diff = qty - cur;
                    let side = if diff > 0 { Side::Buy } else { Side::Sell };
                    let px = if avg > 0.0 { avg } else { self.last_px };
                    if px.is_finite() {
                        let f = Fill {
                            order_id: 0,
                            ts: self.ctx.now(),
                            side,
                            qty: diff.abs(),
                            price: px,
                            fee: 0.0,
                            tax: 0.0,
                            tag: "reconcile",
                        };
                        self.pf.apply(&f);
                        self.sync_account();
                        eprintln!("reconciled position to broker: {qty} @ {px}");
                    }
                }
            }
            BridgeEvent::Hello { bridge, version, simulated } => {
                eprintln!("bridge: {bridge} {version} simulated={simulated}");
                self.sum.bridge = format!("{bridge} {version}");
            }
            BridgeEvent::Info { msg } => {
                self.log(|| json!({"ev":"info","msg":msg}));
                if msg == "eof" {
                    self.stop = true;
                }
            }
            BridgeEvent::Error { msg } => {
                eprintln!("bridge error: {msg}");
                self.log(|| json!({"ev":"bridge_error","msg":msg}));
            }
            BridgeEvent::Pong { .. } => {}
            BridgeEvent::Sync { id } => {
                let _ = self.conn.send(&EngineCmd::SyncAck { id });
            }
        }
        self.cur_recv = None;
    }

    fn on_timer(&mut self, started: Instant, last_kill_check: &mut Instant) {
        // close a bar when the market goes quiet
        if let Some(at) = self.last_tick_at {
            let est_now =
                self.last_tick_ts + at.elapsed().as_micros() as i64 - self.cfg.bar_flush_grace.as_micros() as i64;
            if let Some(done) = self.agg.flush_if_due(est_now) {
                self.on_bar(done);
            }
        }
        if last_kill_check.elapsed() >= Duration::from_secs(1) {
            *last_kill_check = Instant::now();
            if let Some(k) = &self.cfg.kill_file {
                if k.exists() && !self.exit_when_flat {
                    self.exit_when_flat = true;
                    self.halt(&format!("kill file {} present", k.display()));
                }
            }
        }
        if self.exit_when_flat && self.pf.position() == 0 && !self.orders.values().any(|o| o.at_broker) {
            self.stop = true;
        }
        if let Some(max) = self.cfg.max_runtime {
            if started.elapsed() >= max {
                self.stop = true;
            }
        }
    }
}

/// Run a strategy against a connected bridge until the bridge disconnects, sends
/// `eof`, the runtime limit passes, or the kill switch fires and the book is flat.
pub fn run_live<S: Strategy + ?Sized>(strat: &mut S, conn: &mut BridgeConn, cfg: &LiveConfig) -> Result<LiveSummary> {
    let journal = match &cfg.journal {
        Some(p) => Journal::open(p)?,
        None => Journal::disabled(),
    };
    let mut r = Runner {
        ctx: Ctx::for_instrument(cfg.bar_period, &cfg.instrument),
        pf: Portfolio::new(cfg.instrument.clone()),
        risk: RiskManager::new(cfg.risk.clone()),
        sim: SimExchange::new(cfg.sim, cfg.instrument.clone()),
        orders: HashMap::new(),
        agg: BarAggregator::new(cfg.bar_period),
        actions: Vec::with_capacity(16),
        cancelled: Vec::new(),
        execs: Vec::with_capacity(8),
        journal,
        last_px: f64::NAN,
        last_tick_ts: 0,
        last_tick_at: None,
        cur_recv: None,
        tick_lat: Latency::new(5_000_000),
        order_lat: Latency::new(1_000_000),
        engine_lat: Latency::new(5_000_000),
        sum: LiveSummary::default(),
        warming: true,
        stop: false,
        exit_when_flat: false,
        strat,
        cfg,
        conn,
    };

    // warm-up: indicators see history, orders are dropped
    for b in &cfg.warmup {
        r.last_px = b.close;
        r.on_bar(*b);
    }
    r.sum.bars = 0;
    r.warming = false;
    r.log(|| json!({"ev":"start","mode":format!("{:?}",cfg.mode),"symbol":cfg.quote_symbol,"warmup_bars":cfg.warmup.len()}));

    r.conn.send(&EngineCmd::Subscribe { sym: cfg.quote_symbol.clone() })?;
    if cfg.mode == ExecMode::Live {
        r.conn.send(&EngineCmd::QueryPosition { sym: cfg.order_symbol.clone() })?;
    }

    let started = Instant::now();
    let mut last_kill_check = Instant::now();
    let mut last_timer = Instant::now();
    let rx = r.conn.rx.clone();
    while !r.stop {
        let next = if cfg.spin {
            match rx.try_recv() {
                Ok(inb) => Ok(Some(inb)),
                Err(TryRecvError::Empty) => {
                    std::hint::spin_loop();
                    Ok(None)
                }
                Err(TryRecvError::Disconnected) => Err(()),
            }
        } else {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(inb) => Ok(Some(inb)),
                Err(RecvTimeoutError::Timeout) => Ok(None),
                Err(RecvTimeoutError::Disconnected) => Err(()),
            }
        };
        match next {
            Ok(Some(inb)) => {
                r.handle(inb);
                // drain whatever else is already queued before running timers
                while let Ok(inb) = rx.try_recv() {
                    r.handle(inb);
                    if r.stop {
                        break;
                    }
                }
            }
            Ok(None) => {}
            Err(()) => {
                eprintln!("bridge disconnected");
                break;
            }
        }
        if last_timer.elapsed() >= Duration::from_millis(10) {
            last_timer = Instant::now();
            r.on_timer(started, &mut last_kill_check);
        }
    }

    if let Some(done) = r.agg.flush() {
        r.on_bar(done);
    }
    if cfg.flatten_on_exit && cfg.mode == ExecMode::Live && (r.pf.position() != 0 || !r.orders.is_empty()) {
        eprintln!("flattening on exit…");
        r.ctx.flatten();
        r.process_actions();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && (r.pf.position() != 0 || r.orders.values().any(|o| o.at_broker)) {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(inb) => r.handle(inb),
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    r.sum.position = r.pf.position();
    r.sum.realized_net = r.pf.realized_net();
    r.sum.equity = r.equity();
    r.sum.halted = r.risk.halted().map(str::to_string);
    r.sum.tick_latency = r.tick_lat.summary();
    r.sum.order_latency = r.order_lat.summary();
    r.sum.engine_latency = r.engine_lat.summary();
    r.sum.trades = r.pf.take_trades();
    r.log(|| json!({"ev":"stop","summary":serde_json::to_value(&r.sum).unwrap_or_default()}));
    Ok(r.sum)
}
