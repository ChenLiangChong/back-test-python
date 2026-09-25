//! Performance statistics, computed incrementally during the run (no per-bar storage
//! needed, which keeps the optimizer cache-friendly).

use serde::Serialize;
use twq_core::time::{trading_day, Ts};
use twq_core::Trade;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub initial_capital: f64,
    pub final_equity: f64,
    pub net_pnl: f64,
    pub gross_pnl: f64,
    pub fees: f64,
    pub taxes: f64,
    pub return_pct: f64,
    pub trades: usize,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub avg_trade: f64,
    pub avg_win: f64,
    pub avg_loss: f64,
    pub payoff_ratio: f64,
    pub max_consec_losses: usize,
    pub max_drawdown: f64,
    pub max_drawdown_pct: f64,
    /// Net P&L / max drawdown.
    pub ret_over_dd: f64,
    /// Annualised Sharpe of daily P&L relative to initial capital (√252).
    pub sharpe: f64,
    pub sortino: f64,
    /// Share of bars with an open position.
    pub exposure_pct: f64,
    pub days: usize,
    pub bars: usize,
    pub fills: usize,
    /// Equity hit zero: the account was liquidated and trading stopped.
    pub ruined: bool,
}

#[derive(Clone, Debug)]
pub struct StatsBuilder {
    initial: f64,
    peak: f64,
    max_dd: f64,
    max_dd_pct: f64,
    cur_day: i64,
    last_equity: f64,
    daily: Vec<(i64, f64)>,
    bars: usize,
    bars_in_market: usize,
    started: bool,
}

impl StatsBuilder {
    pub fn new(initial: f64) -> Self {
        Self {
            initial,
            peak: initial,
            max_dd: 0.0,
            max_dd_pct: 0.0,
            cur_day: i64::MIN,
            last_equity: initial,
            daily: Vec::with_capacity(512),
            bars: 0,
            bars_in_market: 0,
            started: false,
        }
    }

    /// Reset the baseline (used when stats start after a warm-up period).
    pub fn rebase(&mut self, equity: f64) {
        *self = Self::new(equity);
    }

    #[inline]
    pub fn on_mark(&mut self, ts: Ts, equity: f64, in_market: bool) {
        self.bars += 1;
        self.bars_in_market += in_market as usize;
        let d = trading_day(ts);
        if d != self.cur_day {
            if self.started {
                self.daily.push((self.cur_day, self.last_equity));
            }
            self.cur_day = d;
            self.started = true;
        }
        self.last_equity = equity;
        if equity > self.peak {
            self.peak = equity;
        } else {
            let dd = self.peak - equity;
            if dd > self.max_dd {
                self.max_dd = dd;
            }
            if self.peak > 0.0 {
                let p = dd / self.peak;
                if p > self.max_dd_pct {
                    self.max_dd_pct = p;
                }
            }
        }
    }

    /// Daily closing equity by trading day (available after `finish`).
    pub fn daily(&self) -> &[(i64, f64)] {
        &self.daily
    }

    pub fn finish(&mut self, trades: &[Trade], fees: f64, taxes: f64, fills: usize) -> Stats {
        if self.started {
            self.daily.push((self.cur_day, self.last_equity));
            self.started = false;
        }
        let final_equity = self.last_equity;
        let net = final_equity - self.initial;

        // daily P&L series
        let mut prev = self.initial;
        let rets: Vec<f64> = self
            .daily
            .iter()
            .map(|&(_, e)| {
                let r = (e - prev) / self.initial.max(1.0);
                prev = e;
                r
            })
            .collect();
        let n = rets.len() as f64;
        let (sharpe, sortino) = if rets.len() > 1 {
            let mean = rets.iter().sum::<f64>() / n;
            let var = rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
            let down = (rets.iter().map(|r| r.min(0.0).powi(2)).sum::<f64>() / n).sqrt();
            let ann = 252f64.sqrt();
            (if var > 0.0 { mean / var.sqrt() * ann } else { 0.0 }, if down > 0.0 { mean / down * ann } else { 0.0 })
        } else {
            (0.0, 0.0)
        };

        let mut wins = 0usize;
        let mut gp = 0.0;
        let mut gl = 0.0;
        let mut streak = 0usize;
        let mut max_streak = 0usize;
        let mut gross = 0.0;
        for t in trades {
            gross += t.gross_pnl;
            if t.net_pnl > 0.0 {
                wins += 1;
                gp += t.net_pnl;
                streak = 0;
            } else {
                gl += -t.net_pnl;
                streak += 1;
                max_streak = max_streak.max(streak);
            }
        }
        let nt = trades.len();
        let losses = nt - wins;
        let avg_win = if wins > 0 { gp / wins as f64 } else { 0.0 };
        let avg_loss = if losses > 0 { -gl / losses as f64 } else { 0.0 };
        Stats {
            initial_capital: self.initial,
            final_equity,
            net_pnl: net,
            gross_pnl: gross,
            fees,
            taxes,
            return_pct: net / self.initial.max(1.0) * 100.0,
            trades: nt,
            win_rate: if nt > 0 { wins as f64 / nt as f64 * 100.0 } else { 0.0 },
            profit_factor: if gl > 0.0 {
                gp / gl
            } else if gp > 0.0 {
                f64::INFINITY
            } else {
                0.0
            },
            avg_trade: if nt > 0 { trades.iter().map(|t| t.net_pnl).sum::<f64>() / nt as f64 } else { 0.0 },
            avg_win,
            avg_loss,
            payoff_ratio: if avg_loss < 0.0 { avg_win / -avg_loss } else { 0.0 },
            max_consec_losses: max_streak,
            max_drawdown: self.max_dd,
            max_drawdown_pct: self.max_dd_pct * 100.0,
            ret_over_dd: if self.max_dd > 0.0 { net / self.max_dd } else { 0.0 },
            sharpe,
            sortino,
            exposure_pct: if self.bars > 0 { self.bars_in_market as f64 / self.bars as f64 * 100.0 } else { 0.0 },
            days: self.daily.len(),
            bars: self.bars,
            fills,
            ruined: false,
        }
    }
}

/// Human-readable multi-line summary.
pub fn format_stats(s: &Stats) -> String {
    format!(
        "\
  淨利 Net P&L        {:>14.0}   報酬 Return      {:>8.2}%
  毛利 Gross P&L      {:>14.0}   手續費/稅 Costs  {:>8.0} / {:.0}
  交易次數 Trades     {:>14}   勝率 Win rate    {:>8.2}%
  獲利因子 PF         {:>14.2}   賺賠比 Payoff    {:>8.2}
  平均每筆 Avg trade  {:>14.0}   平均賺/賠       {:>8.0} / {:.0}
  最大回撤 Max DD     {:>14.0}   回撤% Max DD%    {:>8.2}%
  淨利/回撤 Ret/DD    {:>14.2}   Sharpe / Sortino {:>8.2} / {:.2}
  最大連虧 Max losses {:>14}   持倉比例 Exposure {:>7.1}%
  交易日 Days         {:>14}   K棒 Bars / Fills {:>8} / {}{}",
        s.net_pnl,
        s.return_pct,
        s.gross_pnl,
        s.fees,
        s.taxes,
        s.trades,
        s.win_rate,
        s.profit_factor,
        s.payoff_ratio,
        s.avg_trade,
        s.avg_win,
        s.avg_loss,
        s.max_drawdown,
        s.max_drawdown_pct,
        s.ret_over_dd,
        s.sharpe,
        s.sortino,
        s.max_consec_losses,
        s.exposure_pct,
        s.days,
        s.bars,
        s.fills,
        if s.ruined { "\n  !! 權益歸零: 已強制平倉並停止交易 (爆倉) !!" } else { "" },
    )
}
