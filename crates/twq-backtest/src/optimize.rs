//! Parallel grid search and anchored walk-forward analysis (rayon, all cores).

use anyhow::{anyhow, bail, Result};
use rayon::prelude::*;
use serde::Serialize;
use twq_core::{Bar, Params, Strategy};

use crate::engine::{run_bars, BacktestConfig};
use crate::metrics::Stats;

/// Cartesian product of parameter axes.
#[derive(Clone, Debug, Default)]
pub struct ParamGrid {
    axes: Vec<(String, Vec<f64>)>,
}

impl ParamGrid {
    /// Each spec is `name=start:end:step` (inclusive) or `name=v1,v2,v3` (`|` also works).
    pub fn parse(specs: &[String]) -> Result<Self> {
        let mut axes = Vec::new();
        for spec in specs {
            let (name, rhs) = spec.split_once('=').ok_or_else(|| anyhow!("bad grid spec '{spec}'"))?;
            let vals: Vec<f64> = if rhs.contains(':') {
                let p: Vec<f64> = rhs.split(':').map(|x| x.trim().parse::<f64>()).collect::<Result<_, _>>()?;
                if p.len() != 3 || p[2] <= 0.0 || p[1] < p[0] {
                    bail!("range must be start:end:step with step > 0 in '{spec}'");
                }
                let n = ((p[1] - p[0]) / p[2] + 1e-9).floor() as usize + 1;
                (0..n).map(|i| p[0] + p[2] * i as f64).collect()
            } else {
                rhs.split(['|', ',']).map(|x| x.trim().parse::<f64>()).collect::<Result<_, _>>()?
            };
            if vals.is_empty() {
                bail!("no values in '{spec}'");
            }
            axes.push((name.trim().to_string(), vals));
        }
        Ok(Self { axes })
    }

    pub fn len(&self) -> usize {
        self.axes.iter().map(|a| a.1.len()).product::<usize>().max(1)
    }

    pub fn is_empty(&self) -> bool {
        self.axes.is_empty()
    }

    pub fn combos(&self, base: &Params) -> Vec<Params> {
        let mut out = vec![base.clone()];
        for (name, vals) in &self.axes {
            out = out.into_iter().flat_map(|p| vals.iter().map(move |v| p.clone().with(name, *v))).collect();
        }
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Objective {
    NetPnl,
    Sharpe,
    ProfitFactor,
    RetOverDd,
}

impl Objective {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "net" | "pnl" | "net_pnl" => Self::NetPnl,
            "sharpe" => Self::Sharpe,
            "pf" | "profit_factor" => Self::ProfitFactor,
            "retdd" | "ret_over_dd" | "calmar" => Self::RetOverDd,
            _ => bail!("unknown objective '{s}' (net|sharpe|pf|retdd)"),
        })
    }

    pub fn score(self, s: &Stats) -> f64 {
        let v = match self {
            Self::NetPnl => s.net_pnl,
            Self::Sharpe => s.sharpe,
            Self::ProfitFactor => s.profit_factor.min(100.0),
            Self::RetOverDd => s.ret_over_dd,
        };
        if v.is_finite() {
            v
        } else {
            f64::MIN
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct OptRow {
    pub params: String,
    #[serde(skip)]
    pub p: Params,
    pub score: f64,
    pub stats: Stats,
}

/// Evaluate every combination in parallel; returns rows sorted best-first.
/// Combos producing fewer than `min_trades` trades are ranked last.
pub fn grid_search<F>(
    bars: &[Bar],
    combos: &[Params],
    cfg: &BacktestConfig,
    build: &F,
    objective: Objective,
    min_trades: usize,
) -> Vec<OptRow>
where
    F: Fn(&Params) -> Result<Box<dyn Strategy>> + Sync,
{
    let mut rows: Vec<OptRow> = combos
        .par_iter()
        .filter_map(|p| {
            let mut s = build(p).ok()?;
            let r = run_bars(bars, &mut s, cfg);
            let score = if r.stats.trades >= min_trades { objective.score(&r.stats) } else { f64::MIN };
            Some(OptRow { params: p.to_string(), p: p.clone(), score, stats: r.stats })
        })
        .collect();
    rows.sort_by(|a, b| b.score.total_cmp(&a.score));
    rows
}

#[derive(Clone, Debug, Serialize)]
pub struct WfFold {
    pub fold: usize,
    pub train_from: String,
    pub test_from: String,
    pub test_to: String,
    pub best_params: String,
    pub train_score: f64,
    pub test: Stats,
}

#[derive(Clone, Debug, Serialize)]
pub struct WalkForward {
    pub folds: Vec<WfFold>,
    pub oos_net_pnl: f64,
    pub oos_trades: usize,
    pub oos_positive_folds: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct WalkForwardOpts {
    pub objective: Objective,
    pub folds: usize,
    /// Parameter sets with fewer in-sample trades are ranked last.
    pub min_trades: usize,
    /// Bars of history replayed (orders suppressed) before each test window.
    pub warmup_bars: usize,
}

/// Anchored walk-forward: data is split into `folds + 1` chunks; fold *i* optimises on
/// chunks `0..=i` and trades chunk `i+1` out-of-sample with the winning parameters
/// (with `warmup_bars` of history before the test window for indicator warm-up).
pub fn walk_forward<F>(
    bars: &[Bar],
    combos: &[Params],
    cfg: &BacktestConfig,
    build: &F,
    opts: WalkForwardOpts,
) -> Result<WalkForward>
where
    F: Fn(&Params) -> Result<Box<dyn Strategy>> + Sync,
{
    let WalkForwardOpts { objective, folds, min_trades, warmup_bars } = opts;
    if folds == 0 || bars.len() < (folds + 1) * 100 {
        bail!("not enough data for {folds} walk-forward folds");
    }
    let chunk = bars.len() / (folds + 1);
    let mut out = Vec::with_capacity(folds);
    for i in 0..folds {
        let train = &bars[..chunk * (i + 1)];
        let test_start = chunk * (i + 1);
        let test_end = if i + 1 == folds { bars.len() } else { chunk * (i + 2) };
        let rows = grid_search(train, combos, cfg, build, objective, min_trades);
        let best = rows.first().ok_or_else(|| anyhow!("no results"))?;
        let warm_start = test_start.saturating_sub(warmup_bars);
        let mut test_cfg = cfg.clone();
        test_cfg.trade_from = bars[test_start].ts;
        let mut s = build(&best.p)?;
        let r = run_bars(&bars[warm_start..test_end], &mut s, &test_cfg);
        out.push(WfFold {
            fold: i + 1,
            train_from: twq_core::time::fmt_ts(train[0].ts),
            test_from: twq_core::time::fmt_ts(bars[test_start].ts),
            test_to: twq_core::time::fmt_ts(bars[test_end - 1].ts),
            best_params: best.params.clone(),
            train_score: best.score,
            test: r.stats,
        });
    }
    Ok(WalkForward {
        oos_net_pnl: out.iter().map(|f| f.test.net_pnl).sum(),
        oos_trades: out.iter().map(|f| f.test.trades).sum(),
        oos_positive_folds: out.iter().filter(|f| f.test.net_pnl > 0.0).count(),
        folds: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_parse_and_expand() {
        let g = ParamGrid::parse(&["fast=5:15:5".into(), "slow=20|40".into()]).unwrap();
        assert_eq!(g.len(), 6);
        let c = g.combos(&Params::new().with("k", 1.0));
        assert_eq!(c.len(), 6);
        assert!(c.iter().all(|p| p.get("k", 0.0) == 1.0));
        assert_eq!(c[0].get("fast", 0.0), 5.0);
        assert_eq!(c[5].get("slow", 0.0), 40.0);
        assert!(ParamGrid::parse(&["x=5:1:1".into()]).is_err());
    }
}
