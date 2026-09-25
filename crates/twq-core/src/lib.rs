//! twq-core: types, Taiwan market rules, streaming indicators, data IO, the strategy
//! API and the simulated exchange shared by backtest, paper and live trading.

pub mod aggregator;
pub mod data;
pub mod indicators;
pub mod instrument;
pub mod portfolio;
pub mod rng;
pub mod sim;
pub mod strategy;
pub mod time;
pub mod types;

pub use aggregator::BarAggregator;
pub use instrument::{AssetClass, FeeModel, Instrument, TickRule};
pub use portfolio::{Portfolio, Trade};
pub use sim::{BarPath, Exec, SimConfig, SimExchange};
pub use strategy::{Action, Bracket, Ctx, Params, Strategy};
pub use time::Ts;
pub use types::*;
