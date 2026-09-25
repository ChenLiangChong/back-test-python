//! Output helpers: trade list / equity CSV and a self-contained HTML report
//! (inline SVG, no external assets, opens offline).

use std::fmt::Write as _;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::Result;
use twq_core::time::{fmt_day, fmt_ts};
use twq_core::Trade;

use crate::metrics::Stats;

pub fn write_trades_csv(path: impl AsRef<Path>, trades: &[Trade]) -> Result<()> {
    let mut w = BufWriter::new(fs::File::create(path)?);
    writeln!(w, "entry_time,exit_time,dir,qty,entry_price,exit_price,gross_pnl,costs,net_pnl,entry_tag,exit_tag")?;
    for t in trades {
        writeln!(
            w,
            "{},{},{},{},{:.4},{:.4},{:.2},{:.2},{:.2},{},{}",
            fmt_ts(t.entry_ts),
            fmt_ts(t.exit_ts),
            if t.dir > 0 { "long" } else { "short" },
            t.qty,
            t.entry_price,
            t.exit_price,
            t.gross_pnl,
            t.costs,
            t.net_pnl,
            t.entry_tag,
            t.exit_tag
        )?;
    }
    w.flush()?;
    Ok(())
}

pub fn write_daily_equity_csv(path: impl AsRef<Path>, daily: &[(i64, f64)]) -> Result<()> {
    let mut w = BufWriter::new(fs::File::create(path)?);
    writeln!(w, "trading_day,equity")?;
    for (d, e) in daily {
        writeln!(w, "{},{:.2}", fmt_day(*d), e)?;
    }
    w.flush()?;
    Ok(())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Equity + drawdown chart as an SVG polyline (downsampled to ≤ 2000 points).
fn equity_svg(initial: f64, daily: &[(i64, f64)]) -> String {
    if daily.is_empty() {
        return "<p>no data</p>".into();
    }
    let step = (daily.len() / 2000).max(1);
    let pts: Vec<f64> = std::iter::once(initial).chain(daily.iter().step_by(step).map(|d| d.1)).collect();
    let (w, h, dh) = (1000.0, 300.0, 110.0);
    let min = pts.iter().cloned().fold(f64::MAX, f64::min);
    let max = pts.iter().cloned().fold(f64::MIN, f64::max);
    let span = (max - min).max(1.0);
    let n = (pts.len() - 1).max(1) as f64;
    let mut eq = String::new();
    let mut dd = String::new();
    let mut peak = f64::MIN;
    let mut max_dd: f64 = 1.0;
    let dds: Vec<f64> = pts
        .iter()
        .map(|&e| {
            peak = peak.max(e);
            let d = peak - e;
            max_dd = max_dd.max(d);
            d
        })
        .collect();
    for (i, e) in pts.iter().enumerate() {
        let x = i as f64 / n * w;
        let _ = write!(eq, "{:.1},{:.1} ", x, h - (e - min) / span * (h - 10.0) - 5.0);
        let _ = write!(dd, "{:.1},{:.1} ", x, dds[i] / max_dd * (dh - 10.0));
    }
    let base_y = h - (initial - min) / span * (h - 10.0) - 5.0;
    format!(
        r##"<svg viewBox="0 0 {w} {tot}" preserveAspectRatio="none" class="chart">
<line x1="0" x2="{w}" y1="{base_y:.1}" y2="{base_y:.1}" class="base"/>
<polyline points="{eq}" class="eq"/>
<g transform="translate(0,{off})"><polyline points="0,0 {dd} {w},0" class="dd"/></g>
<text x="4" y="14" class="lbl">{max:.0}</text><text x="4" y="{h2:.0}" class="lbl">{min:.0}</text>
<text x="4" y="{ddl:.0}" class="lbl">drawdown (max {max_dd:.0})</text>
</svg>"##,
        tot = h + dh + 10.0,
        off = h + 10.0,
        h2 = h - 4.0,
        ddl = h + 24.0,
    )
}

pub fn write_html_report(
    path: impl AsRef<Path>,
    title: &str,
    subtitle: &str,
    stats: &Stats,
    daily: &[(i64, f64)],
    trades: &[Trade],
) -> Result<()> {
    let rows = [
        ("淨利 Net P&L", format!("{:.0}", stats.net_pnl)),
        ("報酬 Return", format!("{:.2}%", stats.return_pct)),
        ("交易次數 Trades", stats.trades.to_string()),
        ("勝率 Win rate", format!("{:.1}%", stats.win_rate)),
        ("獲利因子 Profit factor", format!("{:.2}", stats.profit_factor)),
        ("賺賠比 Payoff", format!("{:.2}", stats.payoff_ratio)),
        ("平均每筆 Avg trade", format!("{:.0}", stats.avg_trade)),
        ("最大回撤 Max DD", format!("{:.0} ({:.1}%)", stats.max_drawdown, stats.max_drawdown_pct)),
        ("Sharpe / Sortino", format!("{:.2} / {:.2}", stats.sharpe, stats.sortino)),
        ("淨利/回撤 Ret/DD", format!("{:.2}", stats.ret_over_dd)),
        ("最大連虧 Max consec. losses", stats.max_consec_losses.to_string()),
        ("手續費 / 稅 Fees / Tax", format!("{:.0} / {:.0}", stats.fees, stats.taxes)),
        ("持倉比例 Exposure", format!("{:.1}%", stats.exposure_pct)),
        ("交易日 Days", stats.days.to_string()),
    ];
    let mut table = String::new();
    for (k, v) in rows {
        let _ = write!(table, "<tr><th>{k}</th><td>{v}</td></tr>");
    }
    let mut tl = String::new();
    for t in trades.iter().rev().take(300) {
        let _ = write!(
            tl,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{:.2}</td><td>{:.2}</td><td class=\"{}\">{:.0}</td><td>{}→{}</td></tr>",
            fmt_ts(t.entry_ts),
            fmt_ts(t.exit_ts),
            if t.dir > 0 { "多 L" } else { "空 S" },
            t.qty,
            t.entry_price,
            t.exit_price,
            if t.net_pnl >= 0.0 { "pos" } else { "neg" },
            t.net_pnl,
            esc(t.entry_tag),
            esc(t.exit_tag)
        );
    }
    let html = format!(
        r##"<!doctype html><html lang="zh-Hant"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>{t}</title>
<style>
:root{{--bg:#fff;--fg:#1d1d1f;--mut:#6e6e73;--line:#e5e5ea;--eq:#0a7cff;--dd:#ff3b30;--pos:#1a7f37;--neg:#cf222e}}
@media (prefers-color-scheme:dark){{:root{{--bg:#111;--fg:#f2f2f7;--mut:#98989d;--line:#2c2c2e;--eq:#4da3ff;--dd:#ff6961;--pos:#56d364;--neg:#ff7b72}}}}
body{{background:var(--bg);color:var(--fg);font:14px/1.5 -apple-system,"Segoe UI","Noto Sans TC",sans-serif;margin:0;padding:16px;max-width:1100px;margin:auto}}
h1{{font-size:20px;margin:8px 0 0}} .sub{{color:var(--mut);margin-bottom:16px}}
.grid{{display:grid;grid-template-columns:minmax(0,1fr);gap:16px}} @media(min-width:800px){{.grid{{grid-template-columns:320px minmax(0,1fr)}}}}
table{{border-collapse:collapse;width:100%}} th,td{{text-align:left;padding:4px 8px;border-bottom:1px solid var(--line);white-space:nowrap}}
th{{color:var(--mut);font-weight:500}} .chart{{width:100%;height:360px}} .eq{{fill:none;stroke:var(--eq);stroke-width:1.5;vector-effect:non-scaling-stroke}}
.dd{{fill:var(--dd);opacity:.35}} .base{{stroke:var(--mut);stroke-dasharray:4 4;vector-effect:non-scaling-stroke}} .lbl{{fill:var(--mut);font-size:12px}}
.pos{{color:var(--pos)}} .neg{{color:var(--neg)}} .scroll{{overflow-x:auto}}
</style></head><body>
<h1>{t}</h1><div class="sub">{sub}</div>
<div class="grid"><table>{table}</table><div>{svg}</div></div>
<h2 style="font-size:16px">最近交易 Recent trades (最多 300 筆)</h2>
<div class="scroll"><table><tr><th>進場 Entry</th><th>出場 Exit</th><th>方向</th><th>口數</th><th>進價</th><th>出價</th><th>淨損益</th><th>標籤</th></tr>{tl}</table></div>
</body></html>"##,
        t = esc(title),
        sub = esc(subtitle),
        svg = equity_svg(stats.initial_capital, daily),
    );
    fs::write(path, html)?;
    Ok(())
}
