//! A mock broker bridge that speaks the same protocol as the Windows 群益 bridge.
//! It replays ticks (at N× real time, or lock-step for deterministic tests) and fills
//! orders with the shared `SimExchange`, so the whole live pipeline can be exercised
//! on any OS without a broker account.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError};
use twq_core::{Instrument, OrderKind, OrderRequest, Side, SimConfig, SimExchange, Tick, Tif};

use crate::protocol::{encode, BridgeEvent, EngineCmd};

#[derive(Clone, Debug)]
pub struct MockConfig {
    pub ticks: Vec<Tick>,
    pub symbol: String,
    /// Replay speed multiplier (1.0 = real time). Ignored in lock-step mode.
    pub speed: f64,
    /// Wait for the engine to finish each tick before sending the next (deterministic).
    pub lockstep: bool,
    pub instrument: Instrument,
    pub sim: SimConfig,
}

#[derive(Clone, Debug, Default)]
pub struct MockStats {
    pub ticks_sent: usize,
    pub orders: usize,
    pub fills: usize,
    pub position: i64,
}

fn tif_of(s: &str) -> Tif {
    match s {
        "IOC" => Tif::Ioc,
        "FOK" => Tif::Fok,
        _ => Tif::Rod,
    }
}

/// Serve exactly one engine connection, then return.
pub fn serve_one(listener: TcpListener, cfg: MockConfig) -> Result<MockStats> {
    let (stream, _) = listener.accept()?;
    stream.set_nodelay(true)?;
    let read_half = stream.try_clone()?;
    let (tx, rx): (_, Receiver<EngineCmd>) = unbounded();
    std::thread::spawn(move || {
        let mut r = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            match r.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if let Ok(cmd) = serde_json::from_str::<EngineCmd>(line.trim()) {
                        if tx.send(cmd).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });
    let mut w = std::io::BufWriter::new(stream);
    let mut buf = Vec::with_capacity(256);
    let mut send = |w: &mut std::io::BufWriter<_>, ev: &BridgeEvent| -> Result<()> {
        encode(ev, &mut buf);
        w.write_all(&buf)?;
        Ok(())
    };
    let mut ex = SimExchange::new(cfg.sim, cfg.instrument.clone());
    let mut st = MockStats::default();
    let mut sync_id = 0u64;

    send(
        &mut w,
        &BridgeEvent::Hello { bridge: "twq-mock".into(), version: env!("CARGO_PKG_VERSION").into(), simulated: true },
    )?;
    w.flush()?;

    let handle = |cmd: EngineCmd,
                  ex: &mut SimExchange,
                  w: &mut std::io::BufWriter<std::net::TcpStream>,
                  st: &mut MockStats|
     -> Result<Option<u64>> {
        let mut out = Vec::new();
        let mut ack = None;
        match cmd {
            EngineCmd::Order { cid, side, qty, px, tif, .. } => {
                st.orders += 1;
                let side = if side == "B" { Side::Buy } else { Side::Sell };
                let kind = px.map(OrderKind::Limit).unwrap_or(OrderKind::Market);
                ex.submit(OrderRequest { id: cid, side, qty, kind, tif: tif_of(&tif), oco: 0, tag: "bridge" });
                out.push(BridgeEvent::Ack { cid, oid: format!("M{cid:012}") });
            }
            EngineCmd::Cancel { cid } => {
                if ex.cancel(cid) {
                    out.push(BridgeEvent::Cancelled { cid });
                }
            }
            EngineCmd::QueryPosition { sym } => out.push(BridgeEvent::Position { sym, qty: st.position, avg: 0.0 }),
            EngineCmd::Ping { id } => out.push(BridgeEvent::Pong { id }),
            EngineCmd::SyncAck { id } => ack = Some(id),
            EngineCmd::Subscribe { .. } => {}
        }
        let mut b = Vec::new();
        for ev in out {
            encode(&ev, &mut b);
            w.write_all(&b)?;
        }
        Ok(ack)
    };

    // wait for the subscription
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(EngineCmd::Subscribe { .. }) => break,
            Ok(cmd) => {
                handle(cmd, &mut ex, &mut w, &mut st)?;
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(st),
            Err(RecvTimeoutError::Timeout) if Instant::now() > deadline => break,
            Err(_) => {}
        }
    }
    // answer the initial position query etc.
    while let Ok(cmd) = rx.try_recv() {
        handle(cmd, &mut ex, &mut w, &mut st)?;
    }
    w.flush()?;

    let t0 = Instant::now();
    let first_ts = cfg.ticks.first().map(|t| t.ts).unwrap_or(0);
    let mut execs = Vec::new();
    let mut cancelled = Vec::new();
    for tick in &cfg.ticks {
        if !cfg.lockstep && cfg.speed > 0.0 {
            let due = Duration::from_micros(((tick.ts - first_ts) as f64 / cfg.speed).max(0.0) as u64);
            let now = t0.elapsed();
            if due > now {
                // drain commands while waiting
                let wait = due - now;
                let end = Instant::now() + wait;
                while Instant::now() < end {
                    match rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
                        Ok(cmd) => {
                            handle(cmd, &mut ex, &mut w, &mut st)?;
                        }
                        Err(RecvTimeoutError::Disconnected) => return Ok(st),
                        Err(RecvTimeoutError::Timeout) => break,
                    }
                }
            }
        }
        while let Ok(cmd) = rx.try_recv() {
            handle(cmd, &mut ex, &mut w, &mut st)?;
        }
        // match resting orders against this trade first, then publish it
        execs.clear();
        ex.match_tick(tick, &mut execs);
        send(
            &mut w,
            &BridgeEvent::Tick {
                sym: cfg.symbol.clone(),
                ts: tick.ts,
                px: tick.price,
                qty: tick.qty,
                bid: tick.bid.is_finite().then_some(tick.bid),
                ask: tick.ask.is_finite().then_some(tick.ask),
                sim: false,
            },
        )?;
        for e in &execs {
            st.fills += 1;
            st.position += e.order.side.sign() * e.order.qty;
            send(
                &mut w,
                &BridgeEvent::Fill { cid: e.order.id, px: e.price, qty: e.order.qty, ts: e.ts, fee: None, tax: None },
            )?;
        }
        ex.take_cancelled(&mut cancelled);
        for &cid in &cancelled {
            send(&mut w, &BridgeEvent::Cancelled { cid })?;
        }
        st.ticks_sent += 1;
        if cfg.lockstep {
            sync_id += 1;
            send(&mut w, &BridgeEvent::Sync { id: sync_id })?;
            w.flush()?;
            // process engine commands until it acknowledges this tick
            loop {
                match rx.recv_timeout(Duration::from_secs(10)) {
                    Ok(cmd) => {
                        if handle(cmd, &mut ex, &mut w, &mut st)? == Some(sync_id) {
                            break;
                        }
                    }
                    Err(_) => return Ok(st),
                }
            }
        }
        w.flush()?;
    }
    // give the engine a moment to send final orders (e.g. flatten), fill them at the last price
    let end = Instant::now() + Duration::from_millis(300);
    let last = cfg.ticks.last().copied();
    send(&mut w, &BridgeEvent::Info { msg: "eof".into() })?;
    w.flush()?;
    while Instant::now() < end {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(cmd) => {
                handle(cmd, &mut ex, &mut w, &mut st)?;
                if let Some(mut t) = last {
                    t.ts += 1;
                    execs.clear();
                    ex.match_tick(&t, &mut execs);
                    for e in &execs {
                        st.fills += 1;
                        st.position += e.order.side.sign() * e.order.qty;
                        send(
                            &mut w,
                            &BridgeEvent::Fill {
                                cid: e.order.id,
                                px: e.price,
                                qty: e.order.qty,
                                ts: t.ts,
                                fee: None,
                                tax: None,
                            },
                        )?;
                    }
                }
                w.flush()?;
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
    Ok(st)
}
