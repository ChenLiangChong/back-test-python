//! twq-live: paper / live trading runner, pre-trade risk, broker bridge client and a
//! mock bridge for end-to-end testing without a broker.

pub mod bridge;
pub mod journal;
pub mod latency;
pub mod mock;
pub mod protocol;
pub mod risk;
pub mod runner;

pub use bridge::BridgeConn;
pub use mock::{serve_one, MockConfig, MockStats};
pub use risk::{RiskLimits, RiskManager};
pub use runner::{run_live, ExecMode, LiveConfig, LiveSummary};
