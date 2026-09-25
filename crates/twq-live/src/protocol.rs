//! Wire protocol between the Rust engine and a broker bridge (e.g. the 群益 SKCOM
//! bridge on Windows): newline-delimited JSON over TCP, one message per line.
//!
//! JSON costs ~1 µs per message here, which is noise next to a broker round trip
//! (milliseconds), and it keeps the bridge trivially debuggable with `nc`/`telnet`.
//! Timestamps are exchange-local microseconds since 1970 (see `twq_core::time`).

use serde::{Deserialize, Serialize};

/// Bridge → engine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum BridgeEvent {
    Hello {
        bridge: String,
        #[serde(default)]
        version: String,
        /// true when the bridge is a simulator (mock) or the broker test environment.
        #[serde(default)]
        simulated: bool,
    },
    Tick {
        sym: String,
        ts: i64,
        px: f64,
        qty: f64,
        #[serde(default)]
        bid: Option<f64>,
        #[serde(default)]
        ask: Option<f64>,
        /// 試撮 (trial-match) tick — informational, not a real trade.
        #[serde(default)]
        sim: bool,
    },
    /// Broker accepted the order (委託成功). `oid` = broker sequence / order number.
    Ack {
        cid: u64,
        #[serde(default)]
        oid: String,
    },
    Fill {
        cid: u64,
        px: f64,
        qty: i64,
        ts: i64,
        /// Actual commission / tax if the broker reports them; otherwise the engine's
        /// instrument model is used.
        #[serde(default)]
        fee: Option<f64>,
        #[serde(default)]
        tax: Option<f64>,
    },
    Cancelled {
        cid: u64,
    },
    Rejected {
        cid: u64,
        reason: String,
    },
    Position {
        sym: String,
        qty: i64,
        #[serde(default)]
        avg: f64,
    },
    Info {
        msg: String,
    },
    Error {
        msg: String,
    },
    Pong {
        id: u64,
    },
    /// Lock-step barrier used by the mock bridge for deterministic replays: the engine
    /// answers with `EngineCmd::SyncAck` after it has fully processed every earlier event.
    Sync {
        id: u64,
    },
}

/// Engine → bridge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum EngineCmd {
    Subscribe {
        sym: String,
    },
    Order {
        cid: u64,
        sym: String,
        /// "B" or "S"
        side: String,
        qty: i64,
        /// `None` = market order; see `mkt`.
        px: Option<f64>,
        /// "ROD" | "IOC" | "FOK"
        tif: String,
        /// Market flavour when `px` is None: "M" 市價 or "P" 範圍市價 (TAIFEX, IOC/FOK only).
        #[serde(default)]
        mkt: String,
        /// 當沖 flag (sDayTrade).
        #[serde(default)]
        day_trade: bool,
        /// "auto" | "new" | "close" (sNewClose).
        #[serde(default)]
        oc: String,
    },
    Cancel {
        cid: u64,
    },
    QueryPosition {
        sym: String,
    },
    Ping {
        id: u64,
    },
    SyncAck {
        id: u64,
    },
}

impl BridgeEvent {
    pub fn parse(line: &str) -> serde_json::Result<Self> {
        serde_json::from_str(line)
    }
}

pub fn encode<T: Serialize>(msg: &T, buf: &mut Vec<u8>) {
    buf.clear();
    serde_json::to_writer(&mut *buf, msg).expect("serialize");
    buf.push(b'\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let e = BridgeEvent::parse(r#"{"t":"tick","sym":"TX00","ts":1,"px":23000,"qty":2}"#).unwrap();
        assert_eq!(
            e,
            BridgeEvent::Tick { sym: "TX00".into(), ts: 1, px: 23000.0, qty: 2.0, bid: None, ask: None, sim: false }
        );
        let mut buf = Vec::new();
        encode(
            &EngineCmd::Order {
                cid: 7,
                sym: "TX00".into(),
                side: "B".into(),
                qty: 1,
                px: None,
                tif: "IOC".into(),
                mkt: "P".into(),
                day_trade: true,
                oc: "auto".into(),
            },
            &mut buf,
        );
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with(r#"{"t":"order","cid":7"#));
        assert!(s.ends_with('\n'));
        let back: EngineCmd = serde_json::from_str(s.trim()).unwrap();
        assert!(matches!(back, EngineCmd::Order { cid: 7, .. }));
    }
}
