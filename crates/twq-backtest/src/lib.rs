//! twq-backtest: event-driven backtester (bar & tick mode), statistics, and the
//! parallel optimizer / walk-forward analysis.

pub mod engine;
pub mod metrics;
pub mod optimize;
pub mod report;

pub use engine::{detect_period, run_bars, run_ticks, BacktestConfig, BacktestResult};
pub use metrics::{format_stats, Stats};
pub use optimize::{grid_search, walk_forward, Objective, OptRow, ParamGrid, WalkForward, WalkForwardOpts};
