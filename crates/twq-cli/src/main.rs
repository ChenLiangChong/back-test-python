//! `twq` — Taiwan quant engine CLI: backtest, optimize, walk-forward, data tools,
//! mock bridge, paper / live trading and benchmarks.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use twq_backtest::{
    format_stats, grid_search, report, run_bars, run_ticks, walk_forward, BacktestConfig, BacktestResult, Objective,
    ParamGrid, WalkForwardOpts,
};
use twq_core::aggregator::{bars_to_ticks, resample};
use twq_core::data::{self, SynthConfig};
use twq_core::time::{fmt_ts, parse_datetime, US_PER_DAY, US_PER_HOUR, US_PER_MIN, US_PER_SEC};
use twq_core::{Bar, Instrument, Params, SimConfig, Tick};
use twq_live::{run_live, BridgeConn, ExecMode, LiveConfig, MockConfig, RiskLimits};

#[derive(Parser)]
#[command(name = "twq", version, about = "台股/台指期 超高速回測與自動交易引擎 (Rust)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 列出所有策略與參數
    Strategies,
    /// 回測單一參數組
    Backtest(BacktestArgs),
    /// 平行網格最佳化 (+ 可選 walk-forward 樣本外驗證)
    Optimize(OptimizeArgs),
    /// 資料工具: 產生模擬資料 / 轉換期交所 tick / 重新取樣
    #[command(subcommand)]
    Data(DataCmd),
    /// 模擬券商 bridge (與群益 bridge 同協定), 用來端對端測試 live 管線
    MockBridge(MockArgs),
    /// 模擬交易 (paper) 或實單 (live), 透過 bridge 接券商
    Live(LiveArgs),
    /// 效能測試: 回測吞吐量 / 最佳化 / tick 模式 / live 管線延遲
    Bench(BenchArgs),
}

#[derive(Args, Clone)]
struct MarketArgs {
    /// 商品: TX / MTX / TMF / NQ / stock:2330 / etf:0050
    #[arg(long, default_value = "TX")]
    instrument: String,
    /// 手續費: 期貨=每口單邊 NTD, 股票=折扣倍數 (0.28 = 2.8折)
    #[arg(long)]
    commission: Option<f64>,
    /// 市價/停損單滑價 (tick 數)
    #[arg(long, default_value_t = 1.0)]
    slippage: f64,
    /// 限價單碰價即成交 (樂觀), 預設需穿價
    #[arg(long)]
    fill_on_touch: bool,
    /// 初始資金 NTD
    #[arg(long, default_value_t = 2_000_000.0)]
    capital: f64,
}

impl MarketArgs {
    fn instrument(&self) -> Result<Instrument> {
        let mut i =
            Instrument::preset(&self.instrument).ok_or_else(|| anyhow!("unknown instrument {}", self.instrument))?;
        if let Some(c) = self.commission {
            i = i.with_commission(c);
        }
        Ok(i)
    }
    fn config(&self) -> Result<BacktestConfig> {
        let mut c = BacktestConfig::new(self.instrument()?);
        c.sim = SimConfig { slippage_ticks: self.slippage, limit_fill_on_touch: self.fill_on_touch };
        c.initial_capital = self.capital;
        Ok(c)
    }
}

#[derive(Args, Clone)]
struct DataArgs {
    /// K 棒 CSV / .bin (或 --ticks 時為 tick 檔)
    #[arg(long)]
    data: PathBuf,
    /// 資料為 tick (逐筆), 以 --tf 即時合成 K 棒 (與實盤相同路徑)
    #[arg(long)]
    ticks: bool,
    /// K 棒週期, 例: 1m 5m 15m 1h (bar 資料會重新取樣)
    #[arg(long)]
    tf: Option<String>,
    /// 起始時間 (含)
    #[arg(long)]
    from: Option<String>,
    /// 結束時間 (不含)
    #[arg(long)]
    to: Option<String>,
}

#[derive(Args, Clone, Default)]
struct InputArgs {
    /// 事件行事曆 CSV, 可重複: datetime,name,tier(big/normal),tz(TW/ET)
    #[arg(long)]
    events: Vec<PathBuf>,
    /// 依規則自動加入 MSCI(2/5/8/11月最後交易日) 與富時(3/6/9/12月第三個週五) 13:25 事件, 例: 2024-2026
    #[arg(long)]
    index_events: Option<String>,
    /// 輔助資料 name=path, 可重複. 例: taiex_vol=data/taiex_cumamt.csv (大盤累積成交)
    #[arg(long)]
    series: Vec<String>,
}

impl InputArgs {
    fn load(&self) -> Result<twq_strategies::Inputs> {
        let mut events = Vec::new();
        for p in &self.events {
            events.extend(twq_core::events::load_events(p)?);
        }
        if let Some(r) = &self.index_events {
            let (a, b) = r.split_once('-').unwrap_or((r.as_str(), r.as_str()));
            let (a, b): (i64, i64) = (a.trim().parse()?, b.trim().parse()?);
            events.extend(twq_core::events::index_review_events(a, b, 1325));
        }
        events.sort_by_key(|e| e.ts);
        events.dedup_by(|x, y| x.ts == y.ts && x.name == y.name);
        let mut series = std::collections::HashMap::new();
        for spec in &self.series {
            let (name, path) = spec.split_once('=').ok_or_else(|| anyhow!("--series expects name=path, got {spec}"))?;
            series.insert(name.trim().to_string(), std::sync::Arc::new(twq_core::events::load_series(path.trim())?));
        }
        if !events.is_empty() {
            eprintln!(
                "事件行事曆: {} 筆 ({} ~ {})",
                events.len(),
                fmt_ts(events[0].ts),
                fmt_ts(events.last().unwrap().ts)
            );
        }
        Ok(twq_strategies::Inputs { events: std::sync::Arc::new(events), series })
    }
}

#[derive(Args)]
struct BacktestArgs {
    #[command(flatten)]
    data: DataArgs,
    #[command(flatten)]
    inputs: InputArgs,
    #[command(flatten)]
    market: MarketArgs,
    #[arg(long, short)]
    strategy: String,
    /// 參數, 例: "fast=10,slow=30"
    #[arg(long, short, default_value = "")]
    params: String,
    /// 輸出資料夾 (trades.csv / equity.csv / report.html / stats.json)
    #[arg(long)]
    out: Option<PathBuf>,
    /// 以 JSON 輸出統計
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct OptimizeArgs {
    #[command(flatten)]
    data: DataArgs,
    #[command(flatten)]
    inputs: InputArgs,
    #[command(flatten)]
    market: MarketArgs,
    #[arg(long, short)]
    strategy: String,
    /// 固定參數
    #[arg(long, short, default_value = "")]
    params: String,
    /// 參數軸, 可重複: fast=5:50:5 (起:迄:步長) 或 k=1.5,2,2.5 (列舉)
    #[arg(long, required = true)]
    grid: Vec<String>,
    /// 目標: net | sharpe | pf | retdd
    #[arg(long, default_value = "sharpe")]
    objective: String,
    #[arg(long, default_value_t = 20)]
    top: usize,
    /// 交易次數太少的組合排最後
    #[arg(long, default_value_t = 30)]
    min_trades: usize,
    /// walk-forward 折數 (0 = 不做). 強烈建議 >= 3 以避免過度最佳化
    #[arg(long, default_value_t = 0)]
    wf: usize,
    /// 樣本外每折前的暖機 K 棒
    #[arg(long, default_value_t = 2000)]
    warmup: usize,
    /// 平行執行緒數 (預設 = CPU 核心數)
    #[arg(long)]
    threads: Option<usize>,
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Subcommand)]
enum DataCmd {
    /// 產生擬真台指期 1 分 K 模擬資料 (日盤 + 夜盤, 波動叢聚, U 型日內波動)
    Gen {
        #[arg(long, default_value_t = 250)]
        days: usize,
        #[arg(long, default_value_t = 17000.0)]
        price: f64,
        #[arg(long, default_value_t = 0.2)]
        vol: f64,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// 不含夜盤
        #[arg(long)]
        no_night: bool,
        /// 起始日 YYYY-MM-DD
        #[arg(long, default_value = "2021-01-04")]
        start: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// 由 K 棒產生模擬 tick
    GenTicks {
        #[arg(long)]
        bars: PathBuf,
        #[arg(long, default_value_t = 12)]
        per_bar: usize,
        #[arg(long, default_value_t = 7)]
        seed: u64,
        #[arg(long)]
        out: PathBuf,
    },
    /// 轉換期交所「期貨每筆成交資料」(Daily_YYYY_MM_DD.csv, 可多檔) 為 tick 檔
    Taifex {
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        #[arg(long, default_value = "TX")]
        product: String,
        /// 指定到期月份 (例 202610), 預設每日自動取成交量最大的月份 (近月)
        #[arg(long)]
        expiry: Option<String>,
        /// tick 輸出 (.bin 或 .csv)
        #[arg(long)]
        out: PathBuf,
        /// 同時輸出此週期 K 棒 (例 1m)
        #[arg(long)]
        bars_tf: Option<String>,
        #[arg(long)]
        bars_out: Option<PathBuf>,
        /// 夜盤成交的日期標法: auto (自動判斷) / trading (標交易日, 期交所慣例) / calendar (標日曆日)
        #[arg(long, default_value = "auto")]
        night_dates: String,
    },
    /// K 棒重新取樣 / 格式轉換 (csv <-> bin)
    Resample {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        tf: Option<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// 顯示資料摘要
    Info {
        path: PathBuf,
        #[arg(long)]
        ticks: bool,
    },
}

#[derive(Args)]
struct MockArgs {
    #[command(flatten)]
    data: DataArgs,
    #[arg(long, default_value = "127.0.0.1:9101")]
    listen: String,
    #[arg(long, default_value = "TX00")]
    symbol: String,
    /// 重播速度倍數 (60 = 1 分鐘資料 1 秒播完)
    #[arg(long, default_value_t = 60.0)]
    speed: f64,
    /// 鎖步模式: 每筆 tick 等引擎處理完 (可重現, 用於驗證)
    #[arg(long)]
    lockstep: bool,
    #[command(flatten)]
    market: MarketArgs,
}

#[derive(Args)]
struct LiveArgs {
    /// bridge 位址 (群益 bridge 或 twq mock-bridge)
    #[arg(long, default_value = "127.0.0.1:9101")]
    bridge: String,
    /// paper = 真實報價 + 本地模擬成交; live = 真的送單
    #[arg(long, default_value = "paper")]
    mode: String,
    #[arg(long, short)]
    strategy: String,
    #[arg(long, short, default_value = "")]
    params: String,
    #[command(flatten)]
    inputs: InputArgs,
    /// 報價代碼 (群益近月台指: TX00)
    #[arg(long, default_value = "TX00")]
    symbol: String,
    /// 下單代碼 (預設同報價代碼)
    #[arg(long)]
    order_symbol: Option<String>,
    #[arg(long, default_value = "1m")]
    tf: String,
    #[command(flatten)]
    market: MarketArgs,
    #[arg(long, default_value_t = 1)]
    max_pos: i64,
    #[arg(long, default_value_t = 1)]
    max_qty: i64,
    #[arg(long, default_value_t = 5)]
    max_orders_per_sec: usize,
    #[arg(long, default_value_t = 60)]
    max_orders_per_min: usize,
    /// 每日最大虧損 NTD, 觸發即全平並停止開新倉 (0 = 關)
    #[arg(long, default_value_t = 0.0)]
    max_daily_loss: f64,
    /// 市價單型態: P = 範圍市價 (建議), M = 市價
    #[arg(long, default_value = "P")]
    market_style: String,
    /// 當沖單 (sDayTrade=1)
    #[arg(long)]
    day_trade: bool,
    /// 交易日誌 JSONL
    #[arg(long, default_value = "journal.jsonl")]
    journal: PathBuf,
    /// 存在此檔即全平停止 (手動緊急開關)
    #[arg(long, default_value = "KILL")]
    kill_file: PathBuf,
    /// 暖機歷史 K 棒 (讓指標先就緒, 不下單)
    #[arg(long)]
    warmup: Option<PathBuf>,
    /// 最長執行秒數
    #[arg(long)]
    max_seconds: Option<u64>,
    /// 忙等輪詢 (最低延遲, 佔滿一顆 CPU)
    #[arg(long)]
    spin: bool,
    /// live 模式必須加此旗標確認風險
    #[arg(long)]
    i_understand_live_risk: bool,
    #[arg(long, short)]
    verbose: bool,
}

#[derive(Args)]
struct BenchArgs {
    /// K 棒數 (百萬)
    #[arg(long, default_value_t = 5.0)]
    million_bars: f64,
    /// 最佳化組合數
    #[arg(long, default_value_t = 256)]
    combos: usize,
    /// live 管線延遲測試的 tick 數
    #[arg(long, default_value_t = 20_000)]
    live_ticks: usize,
}

fn parse_tf(s: &str) -> Result<i64> {
    let s = s.trim().to_ascii_lowercase();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: i64 = num.parse().map_err(|_| anyhow!("bad timeframe '{s}'"))?;
    Ok(n * match unit {
        "s" => US_PER_SEC,
        "m" | "min" | "" => US_PER_MIN,
        "h" => US_PER_HOUR,
        "d" => US_PER_DAY,
        _ => bail!("bad timeframe unit in '{s}' (s/m/h/d)"),
    })
}

fn filter_range<T>(v: Vec<T>, ts: impl Fn(&T) -> i64, from: &Option<String>, to: &Option<String>) -> Result<Vec<T>> {
    let f = match from {
        Some(s) => parse_datetime(s).ok_or_else(|| anyhow!("bad --from {s}"))?,
        None => i64::MIN,
    };
    let t = match to {
        Some(s) => parse_datetime(s).ok_or_else(|| anyhow!("bad --to {s}"))?,
        None => i64::MAX,
    };
    Ok(v.into_iter().filter(|x| ts(x) >= f && ts(x) < t).collect())
}

enum Loaded {
    Bars(Vec<Bar>, i64),
    Ticks(Vec<Tick>, i64),
}

fn load(d: &DataArgs) -> Result<Loaded> {
    let t0 = Instant::now();
    let tf = d.tf.as_deref().map(parse_tf).transpose()?;
    if d.ticks {
        let ticks = filter_range(data::load_ticks(&d.data)?, |t| t.ts, &d.from, &d.to)?;
        eprintln!("載入 {} 筆 tick ({:.0} ms)", ticks.len(), t0.elapsed().as_secs_f64() * 1e3);
        if ticks.is_empty() {
            bail!("no ticks in range");
        }
        Ok(Loaded::Ticks(ticks, tf.unwrap_or(US_PER_MIN)))
    } else {
        let mut bars = filter_range(data::load_bars(&d.data)?, |b| b.ts, &d.from, &d.to)?;
        if bars.is_empty() {
            bail!("no bars in range");
        }
        let src = twq_backtest::detect_period(&bars);
        let period = match tf {
            Some(p) if p != src => {
                if p % src != 0 {
                    bail!("--tf must be a multiple of the data period");
                }
                bars = resample(&bars, p);
                p
            }
            _ => src,
        };
        eprintln!(
            "載入 {} 根 K 棒 ({} ~ {}, 週期 {}s, {:.0} ms)",
            bars.len(),
            fmt_ts(bars[0].ts),
            fmt_ts(bars.last().unwrap().ts),
            period / US_PER_SEC,
            t0.elapsed().as_secs_f64() * 1e3
        );
        Ok(Loaded::Bars(bars, period))
    }
}

fn print_result(name: &str, p: &Params, r: &BacktestResult) {
    let per_sec = r.events as f64 / r.elapsed.as_secs_f64().max(1e-9);
    println!("== {name} [{p}] ==");
    println!("{}", format_stats(&r.stats));
    println!("  耗時 {:.3} ms, {:.1} M events/s", r.elapsed.as_secs_f64() * 1e3, per_sec / 1e6);
}

fn write_outputs(out: &Path, title: &str, sub: &str, r: &BacktestResult) -> Result<()> {
    std::fs::create_dir_all(out)?;
    report::write_trades_csv(out.join("trades.csv"), &r.trades)?;
    report::write_daily_equity_csv(out.join("equity.csv"), &r.daily_equity)?;
    report::write_html_report(out.join("report.html"), title, sub, &r.stats, &r.daily_equity, &r.trades)?;
    std::fs::write(out.join("stats.json"), serde_json::to_string_pretty(&r.stats)?)?;
    eprintln!("輸出: {}/{{trades.csv, equity.csv, report.html, stats.json}}", out.display());
    Ok(())
}

/// Per-tag breakdown (event name / entry signal) of a trade list.
fn print_tag_breakdown(trades: &[twq_core::Trade]) {
    let mut tags: Vec<&str> = trades.iter().map(|t| t.entry_tag).collect();
    tags.sort_unstable();
    tags.dedup();
    if tags.len() < 2 && trades.len() < 2 {
        return;
    }
    println!(
        "  {:<14} {:>6} {:>7} {:>12} {:>10}   出場 (停利/停損/其他)",
        "進場標籤", "筆數", "勝率", "淨損益", "平均"
    );
    for tag in tags {
        let ts: Vec<_> = trades.iter().filter(|t| t.entry_tag == tag).collect();
        let net: f64 = ts.iter().map(|t| t.net_pnl).sum();
        let wins = ts.iter().filter(|t| t.net_pnl > 0.0).count();
        let tp = ts.iter().filter(|t| t.exit_tag == "tp").count();
        let sl = ts.iter().filter(|t| t.exit_tag == "sl").count();
        println!(
            "  {:<14} {:>6} {:>6.1}% {:>12.0} {:>10.0}   {}/{}/{}",
            tag,
            ts.len(),
            wins as f64 / ts.len() as f64 * 100.0,
            net,
            net / ts.len() as f64,
            tp,
            sl,
            ts.len() - tp - sl
        );
    }
}

fn cmd_backtest(a: BacktestArgs) -> Result<()> {
    let params = Params::parse(&a.params)?;
    let inputs = a.inputs.load()?;
    let mut strat = twq_strategies::build_with(&a.strategy, &params, &inputs)?;
    let mut cfg = a.market.config()?;
    let r = match load(&a.data)? {
        Loaded::Bars(bars, period) => {
            cfg.bar_period = period;
            run_bars(&bars, &mut strat, &cfg)
        }
        Loaded::Ticks(ticks, period) => {
            cfg.bar_period = period;
            run_ticks(&ticks, &mut strat, &cfg)
        }
    };
    if a.json {
        println!("{}", serde_json::to_string_pretty(&r.stats)?);
    } else {
        print_result(&a.strategy, &params, &r);
        print_tag_breakdown(&r.trades);
    }
    if let Some(out) = &a.out {
        let sub =
            format!("{} · {} · params [{}] · {}", a.strategy, cfg.instrument.symbol, params, a.data.data.display());
        write_outputs(out, &format!("回測報告 {}", a.strategy), &sub, &r)?;
    }
    Ok(())
}

fn cmd_optimize(a: OptimizeArgs) -> Result<()> {
    if let Some(n) = a.threads {
        rayon::ThreadPoolBuilder::new().num_threads(n).build_global().ok();
    }
    let base = Params::parse(&a.params)?;
    let inputs = a.inputs.load()?;
    twq_strategies::build_with(&a.strategy, &base, &inputs)?; // validate names early
    let grid = ParamGrid::parse(&a.grid)?;
    let combos = grid.combos(&base);
    for c in combos.iter().take(1) {
        twq_strategies::build_with(&a.strategy, c, &inputs)?;
    }
    let objective = Objective::parse(&a.objective)?;
    let mut cfg = a.market.config()?;
    let Loaded::Bars(bars, period) = load(&a.data)? else {
        bail!("optimize works on bar data (convert ticks with `twq data resample`)")
    };
    cfg.bar_period = period;
    let name = a.strategy.clone();
    let inp = inputs.clone();
    let build = move |p: &Params| twq_strategies::build_with(&name, p, &inp);
    let t0 = Instant::now();
    let rows = grid_search(&bars, &combos, &cfg, &build, objective, a.min_trades);
    let el = t0.elapsed().as_secs_f64();
    println!(
        "{} 組參數 × {} 根 K 棒 = {:.1} 億根 K 棒模擬, 耗時 {:.2}s ({:.0} 組/秒, {:.1} M bars/s, {} 執行緒)",
        combos.len(),
        bars.len(),
        combos.len() as f64 * bars.len() as f64 / 1e8,
        el,
        combos.len() as f64 / el,
        combos.len() as f64 * bars.len() as f64 / el / 1e6,
        rayon::current_num_threads()
    );
    println!(
        "{:<4} {:>12} {:>12} {:>7} {:>7} {:>6} {:>11} {:>7}  params",
        "#", "score", "net", "trades", "win%", "PF", "maxDD", "sharpe"
    );
    for (i, r) in rows.iter().take(a.top).enumerate() {
        let s = &r.stats;
        println!(
            "{:<4} {:>12.3} {:>12.0} {:>7} {:>7.1} {:>6.2} {:>11.0} {:>7.2}  {}",
            i + 1,
            if r.score == f64::MIN { f64::NAN } else { r.score },
            s.net_pnl,
            s.trades,
            s.win_rate,
            s.profit_factor.min(99.0),
            s.max_drawdown,
            s.sharpe,
            r.params
        );
    }
    if let Some(out) = &a.out {
        std::fs::create_dir_all(out)?;
        std::fs::write(out.join("optimize.json"), serde_json::to_string_pretty(&rows)?)?;
    }
    if a.wf > 0 {
        let name = a.strategy.clone();
        let build = move |p: &Params| twq_strategies::build_with(&name, p, &inputs);
        let opts = WalkForwardOpts { objective, folds: a.wf, min_trades: a.min_trades, warmup_bars: a.warmup };
        let wf = walk_forward(&bars, &combos, &cfg, &build, opts)?;
        println!("\n== Walk-forward ({} 折, 錨定式; 每折用之前全部資料最佳化, 下一段樣本外交易) ==", a.wf);
        println!(
            "{:<5} {:<20} {:<20} {:>11} {:>7} {:>7}  best params",
            "fold", "test from", "test to", "OOS net", "trades", "sharpe"
        );
        for f in &wf.folds {
            println!(
                "{:<5} {:<20} {:<20} {:>11.0} {:>7} {:>7.2}  {}",
                f.fold, f.test_from, f.test_to, f.test.net_pnl, f.test.trades, f.test.sharpe, f.best_params
            );
        }
        println!(
            "樣本外總淨利 {:.0}, {} 筆交易, {}/{} 折獲利 — 樣本外結果才是可信的預期績效",
            wf.oos_net_pnl,
            wf.oos_trades,
            wf.oos_positive_folds,
            wf.folds.len()
        );
        if let Some(out) = &a.out {
            std::fs::write(out.join("walk_forward.json"), serde_json::to_string_pretty(&wf)?)?;
        }
    }
    Ok(())
}

fn cmd_data(c: DataCmd) -> Result<()> {
    match c {
        DataCmd::Gen { days, price, vol, seed, no_night, start, out } => {
            let d = parse_datetime(&start).ok_or_else(|| anyhow!("bad --start"))? / US_PER_DAY;
            let t0 = Instant::now();
            let bars = data::synth_futures_bars(&SynthConfig {
                start_day: d,
                days,
                start_price: price,
                annual_vol: vol,
                include_night: !no_night,
                seed,
            });
            data::save_bars(&out, &bars)?;
            println!(
                "產生 {} 根 1 分 K ({} 交易日) -> {} ({:.0} ms)",
                bars.len(),
                days,
                out.display(),
                t0.elapsed().as_secs_f64() * 1e3
            );
        }
        DataCmd::GenTicks { bars, per_bar, seed, out } => {
            let b = data::load_bars(&bars)?;
            let t = data::synth_ticks_from_bars(&b, per_bar, seed);
            data::save_ticks(&out, &t)?;
            println!("產生 {} 筆 tick -> {}", t.len(), out.display());
        }
        DataCmd::Taifex { inputs, product, expiry, out, bars_tf, bars_out, night_dates } => {
            let mut all = Vec::new();
            for p in &expand_globs(&inputs)? {
                let bytes = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
                let t = data::parse_taifex_ticks(&bytes, &product, expiry.as_deref())?;
                eprintln!("{}: {} ticks", p.display(), t.len());
                all.extend(t);
            }
            all.sort_by_key(|t| t.ts);
            let mode = match night_dates.as_str() {
                "auto" => data::NightDates::Auto,
                "trading" => data::NightDates::TradingDate,
                "calendar" => data::NightDates::Calendar,
                m => bail!("--night-dates must be auto|trading|calendar, got {m}"),
            };
            let applied = data::fix_taifex_night_dates(&mut all, mode);
            eprintln!(
                "夜盤日期: {}",
                match applied {
                    data::NightDates::TradingDate => "檔案標的是交易日 → 已把夜盤成交移回實際日期",
                    _ => "檔案標的已是日曆日 → 不調整",
                }
            );
            data::save_ticks(&out, &all)?;
            println!("{} {} 筆 tick -> {}", product, all.len(), out.display());
            if let Some(bo) = bars_out {
                let period = parse_tf(bars_tf.as_deref().unwrap_or("1m"))?;
                let mut agg = twq_core::BarAggregator::new(period);
                let mut bars: Vec<Bar> = all.iter().filter_map(|t| agg.update(t)).collect();
                bars.extend(agg.flush());
                data::save_bars(&bo, &bars)?;
                println!("{} 根 K 棒 -> {}", bars.len(), bo.display());
            }
        }
        DataCmd::Resample { input, tf, out } => {
            let mut b = data::load_bars(&input)?;
            if let Some(tf) = tf {
                b = resample(&b, parse_tf(&tf)?);
            }
            data::save_bars(&out, &b)?;
            println!("{} 根 K 棒 -> {}", b.len(), out.display());
        }
        DataCmd::Info { path, ticks } => {
            if ticks {
                let t = data::load_ticks(&path)?;
                if let (Some(a), Some(b)) = (t.first(), t.last()) {
                    println!(
                        "{} ticks  {} ~ {}  first={} last={}",
                        t.len(),
                        fmt_ts(a.ts),
                        fmt_ts(b.ts),
                        a.price,
                        b.price
                    );
                }
            } else {
                let b = data::load_bars(&path)?;
                if let (Some(x), Some(y)) = (b.first(), b.last()) {
                    let p = twq_backtest::detect_period(&b);
                    println!(
                        "{} bars  {} ~ {}  period={}s  first close={} last close={}",
                        b.len(),
                        fmt_ts(x.ts),
                        fmt_ts(y.ts),
                        p / US_PER_SEC,
                        x.close,
                        y.close
                    );
                }
            }
        }
    }
    Ok(())
}

/// Expand `*` / `?` wildcards ourselves: Windows shells pass them through literally.
fn expand_globs(inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    fn matches(pat: &[u8], s: &[u8]) -> bool {
        match (pat.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) => matches(&pat[1..], s) || (!s.is_empty() && matches(pat, &s[1..])),
            (Some(b'?'), Some(_)) => matches(&pat[1..], &s[1..]),
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => matches(&pat[1..], &s[1..]),
            _ => false,
        }
    }
    let mut out = Vec::new();
    for p in inputs {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if !name.contains(['*', '?']) {
            out.push(p.clone());
            continue;
        }
        let dir = p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
            .with_context(|| format!("listing {}", dir.display()))?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_str().is_some_and(|f| matches(name.as_bytes(), f.as_bytes())))
            .map(|e| e.path())
            .collect();
        if found.is_empty() {
            bail!("no files match {}", p.display());
        }
        found.sort();
        out.extend(found);
    }
    Ok(out)
}

fn ticks_for(d: &DataArgs) -> Result<Vec<Tick>> {
    Ok(match load(d)? {
        Loaded::Ticks(t, _) => t,
        Loaded::Bars(b, p) => bars_to_ticks(&b, p),
    })
}

fn cmd_mock(a: MockArgs) -> Result<()> {
    let ticks = ticks_for(&a.data)?;
    let listener = TcpListener::bind(&a.listen)?;
    println!(
        "mock bridge 監聽 {} ({} 筆 tick, 速度 {}x{}) — 等待 twq live 連線…",
        a.listen,
        ticks.len(),
        a.speed,
        if a.lockstep { ", lockstep" } else { "" }
    );
    let cfg = MockConfig {
        ticks,
        symbol: a.symbol,
        speed: a.speed,
        lockstep: a.lockstep,
        instrument: a.market.instrument()?,
        sim: SimConfig { slippage_ticks: a.market.slippage, limit_fill_on_touch: a.market.fill_on_touch },
    };
    let st = twq_live::serve_one(listener, cfg)?;
    println!(
        "mock bridge 結束: 送出 {} tick, 收到 {} 筆委託, {} 筆成交, 部位 {}",
        st.ticks_sent, st.orders, st.fills, st.position
    );
    Ok(())
}

fn cmd_live(a: LiveArgs) -> Result<()> {
    let mode = match a.mode.as_str() {
        "paper" => ExecMode::Paper,
        "live" => {
            if !a.i_understand_live_risk {
                bail!("live 模式會真的下單. 請先用 paper 模式驗證, 確認後加上 --i-understand-live-risk");
            }
            ExecMode::Live
        }
        m => bail!("unknown --mode {m} (paper|live)"),
    };
    let params = Params::parse(&a.params)?;
    let inputs = a.inputs.load()?;
    let mut strat = twq_strategies::build_with(&a.strategy, &params, &inputs)?;
    let mut cfg = LiveConfig::new(a.market.instrument()?, &a.symbol, mode);
    cfg.order_symbol = a.order_symbol.clone().unwrap_or_else(|| a.symbol.clone());
    cfg.bar_period = parse_tf(&a.tf)?;
    cfg.initial_equity = a.market.capital;
    cfg.sim = SimConfig { slippage_ticks: a.market.slippage, limit_fill_on_touch: a.market.fill_on_touch };
    cfg.risk = RiskLimits {
        max_position: a.max_pos,
        max_order_qty: a.max_qty,
        max_orders_per_sec: a.max_orders_per_sec,
        max_orders_per_min: a.max_orders_per_min,
        max_daily_loss: a.max_daily_loss,
        ..RiskLimits::default()
    };
    cfg.market_style = a.market_style.clone();
    cfg.day_trade = a.day_trade;
    cfg.journal = Some(a.journal.clone());
    cfg.kill_file = Some(a.kill_file.clone());
    cfg.max_runtime = a.max_seconds.map(Duration::from_secs);
    cfg.verbose = a.verbose;
    cfg.spin = a.spin;
    if let Some(w) = &a.warmup {
        let mut b = data::load_bars(w)?;
        let src = twq_backtest::detect_period(&b);
        if src != cfg.bar_period && cfg.bar_period % src == 0 {
            b = resample(&b, cfg.bar_period);
        }
        cfg.warmup = b;
    }
    let mut conn = BridgeConn::connect(&a.bridge, Duration::from_secs(5))?;
    eprintln!("已連線 bridge {} — 模式 {:?}, 策略 {} [{}], 風控 {:?}", a.bridge, mode, a.strategy, params, cfg.risk);
    let s = run_live(&mut strat, &mut conn, &cfg)?;
    println!("== live 結束 ({:?}) ==", mode);
    println!(
        "  tick {} / K棒 {} / 委託 {} / 成交 {} / 取消 {} / 拒絕 {}",
        s.ticks, s.bars, s.orders_sent, s.fills, s.cancels, s.rejects
    );
    println!("  部位 {} / 已實現淨損益 {:.0} / 權益 {:.0}", s.position, s.realized_net, s.equity);
    if let Some(h) = &s.halted {
        println!("  已停機: {h}");
    }
    println!("  tick→決策完成 延遲: {}", s.tick_latency);
    println!("  tick→委託送出 延遲: {}", s.order_latency);
    println!("  引擎內部處理時間: {}", s.engine_latency);
    println!("  日誌: {}", a.journal.display());
    Ok(())
}

fn cmd_bench(a: BenchArgs) -> Result<()> {
    let n_bars = (a.million_bars * 1e6) as usize;
    let days = n_bars / 1140 + 1;
    println!("== twq bench ({} 執行緒) ==", rayon::current_num_threads());
    let t0 = Instant::now();
    let mut bars = data::synth_futures_bars(&SynthConfig { days, ..Default::default() });
    bars.truncate(n_bars);
    println!(
        "產生 {:.2} M 根 1 分 K (~{} 年台指期日夜盤): {:.2}s",
        bars.len() as f64 / 1e6,
        days / 250,
        t0.elapsed().as_secs_f64()
    );

    let mut cfg = BacktestConfig::new(Instrument::tx());
    cfg.bar_period = US_PER_MIN;
    for (name, p) in [
        ("ma_cross", "fast=20,slow=60"),
        ("donchian", "entry_n=60,exit_n=30"),
        ("orb", ""),
        ("vol_breakdown", "session=0,vol_mult=2.5"),
    ] {
        let params = Params::parse(p)?;
        let mut s = twq_strategies::build(name, &params)?;
        let r = run_bars(&bars, &mut s, &cfg);
        println!(
            "  回測 {:<14} {:>8.1} ms  {:>7.1} M bars/s  ({} 筆交易)",
            name,
            r.elapsed.as_secs_f64() * 1e3,
            bars.len() as f64 / r.elapsed.as_secs_f64() / 1e6,
            r.stats.trades
        );
    }

    let grid = ParamGrid::parse(&["fast=5:80:5".into(), "slow=20:320:20".into()])?;
    let combos: Vec<Params> = grid.combos(&Params::new()).into_iter().take(a.combos).collect();
    let build = |p: &Params| twq_strategies::build("ma_cross", p);
    let t0 = Instant::now();
    let rows = grid_search(&bars, &combos, &cfg, &build, Objective::Sharpe, 0);
    let el = t0.elapsed().as_secs_f64();
    println!(
        "  最佳化 {} 組 ma_cross × {:.2}M bars: {:.2}s → {:.0} 組/秒, 合計 {:.0} M bars/s",
        rows.len(),
        bars.len() as f64 / 1e6,
        el,
        rows.len() as f64 / el,
        rows.len() as f64 * bars.len() as f64 / el / 1e6
    );

    let tick_bars = &bars[..bars.len().min(200_000)];
    let ticks = data::synth_ticks_from_bars(tick_bars, 20, 1);
    let mut s = twq_strategies::build("orb", &Params::new())?;
    let r = run_ticks(&ticks, &mut s, &cfg);
    println!(
        "  tick 模式 orb: {:.2} M ticks {:.1} ms → {:.1} M ticks/s",
        ticks.len() as f64 / 1e6,
        r.elapsed.as_secs_f64() * 1e3,
        ticks.len() as f64 / r.elapsed.as_secs_f64() / 1e6
    );

    // live pipeline latency through a real TCP socket (mock bridge, lock-step)
    let live_ticks: Vec<Tick> = data::synth_ticks_from_bars(&bars[..(a.live_ticks / 10).max(10)], 10, 3);
    for spin in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let mcfg = MockConfig {
            ticks: live_ticks.clone(),
            symbol: "TX00".into(),
            speed: 0.0,
            lockstep: true,
            instrument: Instrument::tx(),
            sim: SimConfig::default(),
        };
        let server = std::thread::spawn(move || twq_live::serve_one(listener, mcfg));
        let mut conn = BridgeConn::connect(addr, Duration::from_secs(5))?;
        let mut lcfg = LiveConfig::new(Instrument::tx(), "TX00", ExecMode::Live);
        lcfg.risk = RiskLimits {
            max_position: 5,
            max_order_qty: 5,
            max_orders_per_sec: 1000,
            max_orders_per_min: 100_000,
            ..Default::default()
        };
        lcfg.flatten_on_exit = true;
        lcfg.spin = spin;
        let mut s = twq_strategies::build("ma_cross", &Params::parse("fast=3,slow=8")?)?;
        let sum = run_live(&mut s, &mut conn, &lcfg)?;
        let _ = server.join();
        println!(
            "  live 管線 {} (TCP bridge, {} ticks, {} 筆委託):",
            if spin { "--spin 忙等模式" } else { "一般模式" },
            sum.ticks,
            sum.orders_sent
        );
        println!("    引擎內部處理 (每 tick):     {}", sum.engine_latency);
        println!("    收到 tick → 處理完成:       {}", sum.tick_latency);
        println!("    收到 tick → 委託寫入 socket: {}", sum.order_latency);
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Strategies => {
            for s in twq_strategies::REGISTRY {
                println!("{:<14} {}", s.name, s.description);
                for (k, v, d) in s.params.iter().chain(twq_strategies::COMMON_PARAMS) {
                    println!("    {:<14} = {:<8} {}", k, v, d);
                }
            }
            Ok(())
        }
        Cmd::Backtest(a) => cmd_backtest(a),
        Cmd::Optimize(a) => cmd_optimize(a),
        Cmd::Data(c) => cmd_data(c),
        Cmd::MockBridge(a) => cmd_mock(a),
        Cmd::Live(a) => cmd_live(a),
        Cmd::Bench(a) => cmd_bench(a),
    }
}
