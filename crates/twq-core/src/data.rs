//! Data IO: flexible OHLCV CSV reader, TAIFEX 期貨每筆成交資料 tick parser,
//! and a flat little-endian binary cache that loads at memory bandwidth.

use std::collections::HashMap;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

use crate::rng::Rng;
use crate::time::{hhmm_to_min, make_ts, parse_datetime, weekday, Ts, US_PER_DAY, US_PER_MIN, US_PER_SEC};
use crate::types::{Bar, Tick};

const BAR_MAGIC: &[u8; 8] = b"TWQBAR01";
const TICK_MAGIC: &[u8; 8] = b"TWQTCK01";

fn is_bin(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()), Some("bin") | Some("twq"))
}

// ---------------------------------------------------------------- bars

/// Load bars from `.csv`/`.txt` (auto-detected columns) or `.bin` (binary cache).
pub fn load_bars(path: impl AsRef<Path>) -> Result<Vec<Bar>> {
    let path = path.as_ref();
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut bars = if is_bin(path) { decode_bars(&bytes)? } else { parse_bars_csv(&bytes)? };
    if bars.windows(2).any(|w| w[1].ts < w[0].ts) {
        bars.sort_by_key(|b| b.ts);
    }
    Ok(bars)
}

pub fn save_bars(path: impl AsRef<Path>, bars: &[Bar]) -> Result<()> {
    let path = path.as_ref();
    let mut w = BufWriter::with_capacity(1 << 20, fs::File::create(path)?);
    if is_bin(path) {
        w.write_all(BAR_MAGIC)?;
        w.write_all(&(bars.len() as u64).to_le_bytes())?;
        for b in bars {
            w.write_all(&b.ts.to_le_bytes())?;
            for v in [b.open, b.high, b.low, b.close, b.volume] {
                w.write_all(&v.to_le_bytes())?;
            }
        }
    } else {
        writeln!(w, "timestamp,open,high,low,close,volume")?;
        for b in bars {
            writeln!(w, "{},{},{},{},{},{}", crate::time::fmt_ts(b.ts), b.open, b.high, b.low, b.close, b.volume)?;
        }
    }
    w.flush()?;
    Ok(())
}

fn decode_bars(bytes: &[u8]) -> Result<Vec<Bar>> {
    if bytes.len() < 16 || &bytes[..8] != BAR_MAGIC {
        bail!("not a twq bar file");
    }
    let n = u64::from_le_bytes(bytes[8..16].try_into()?) as usize;
    let body = &bytes[16..];
    if body.len() != n * 48 {
        bail!("truncated bar file: expected {} records", n);
    }
    let f = |c: &[u8], i: usize| f64::from_le_bytes(c[i..i + 8].try_into().unwrap());
    Ok(body
        .chunks_exact(48)
        .map(|c| Bar {
            ts: i64::from_le_bytes(c[0..8].try_into().unwrap()),
            open: f(c, 8),
            high: f(c, 16),
            low: f(c, 24),
            close: f(c, 32),
            volume: f(c, 40),
        })
        .collect())
}

#[derive(Default, Debug)]
struct BarCols {
    ts: Option<usize>,
    date: Option<usize>,
    time: Option<usize>,
    open: usize,
    high: usize,
    low: usize,
    close: usize,
    volume: Option<usize>,
}

fn detect_columns(header: &[&str]) -> Option<BarCols> {
    let mut map: HashMap<&'static str, usize> = HashMap::new();
    for (i, raw) in header.iter().enumerate() {
        let h = raw.trim().trim_matches('"').to_ascii_lowercase();
        let key = match h.as_str() {
            "timestamp" | "datetime" | "date_time" | "time_stamp" | "ts" => "ts",
            "date" | "日期" | "交易日期" => "date",
            "time" | "時間" => "time",
            "open" | "o" | "開盤價" | "開盤" => "open",
            "high" | "h" | "最高價" | "最高" => "high",
            "low" | "l" | "最低價" | "最低" => "low",
            "close" | "c" | "收盤價" | "收盤" | "last" => "close",
            "volume" | "vol" | "v" | "totalvolume" | "成交量" => "volume",
            _ => continue,
        };
        map.entry(key).or_insert(i);
    }
    Some(BarCols {
        ts: map.get("ts").copied(),
        date: map.get("date").copied(),
        time: map.get("time").copied(),
        open: *map.get("open")?,
        high: *map.get("high")?,
        low: *map.get("low")?,
        close: *map.get("close")?,
        volume: map.get("volume").copied(),
    })
}

/// Parse OHLCV CSV. Accepts a header (English or Chinese names, `timestamp` or
/// separate `date`+`time` columns) or no header (`ts,open,high,low,close[,volume]`,
/// the FirstRateData / generic export layout).
pub fn parse_bars_csv(bytes: &[u8]) -> Result<Vec<Bar>> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().ok_or_else(|| anyhow!("empty csv"))?;
    let first_fields: Vec<&str> = first.split(',').collect();
    let (cols, pending_first) = if parse_datetime(first_fields[0].trim_matches('"')).is_some() {
        let c = BarCols { ts: Some(0), open: 1, high: 2, low: 3, close: 4, volume: Some(5), ..Default::default() };
        (c, Some(first))
    } else {
        let c = detect_columns(&first_fields)
            .ok_or_else(|| anyhow!("cannot find open/high/low/close columns in header: {first}"))?;
        (c, None)
    };
    let mut out = Vec::with_capacity(bytes.len() / 48);
    let mut buf = String::new();
    for (lineno, line) in pending_first.into_iter().chain(lines).enumerate() {
        let f: Vec<&str> = line.split(',').map(|s| s.trim().trim_matches('"')).collect();
        let ts = match (cols.ts, cols.date, cols.time) {
            (Some(i), _, _) => f.get(i).and_then(|s| parse_datetime(s)),
            (None, Some(d), Some(t)) => {
                buf.clear();
                buf.push_str(f.get(d).copied().unwrap_or(""));
                buf.push(' ');
                buf.push_str(f.get(t).copied().unwrap_or(""));
                parse_datetime(&buf)
            }
            (None, Some(d), None) => f.get(d).and_then(|s| parse_datetime(s)),
            _ => None,
        };
        let num = |i: usize| f.get(i).and_then(|s| s.parse::<f64>().ok());
        let (Some(ts), Some(open), Some(high), Some(low), Some(close)) =
            (ts, num(cols.open), num(cols.high), num(cols.low), num(cols.close))
        else {
            bail!("bad csv row {}: {line}", lineno + 1);
        };
        let volume = cols.volume.and_then(num).unwrap_or(0.0);
        out.push(Bar { ts, open, high, low, close, volume });
    }
    Ok(out)
}

// ---------------------------------------------------------------- ticks

pub fn load_ticks(path: impl AsRef<Path>) -> Result<Vec<Tick>> {
    let path = path.as_ref();
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut ticks = if is_bin(path) { decode_ticks(&bytes)? } else { parse_ticks_csv(&bytes)? };
    if ticks.windows(2).any(|w| w[1].ts < w[0].ts) {
        ticks.sort_by_key(|t| t.ts);
    }
    Ok(ticks)
}

pub fn save_ticks(path: impl AsRef<Path>, ticks: &[Tick]) -> Result<()> {
    let path = path.as_ref();
    let mut w = BufWriter::with_capacity(1 << 20, fs::File::create(path)?);
    if is_bin(path) {
        w.write_all(TICK_MAGIC)?;
        w.write_all(&(ticks.len() as u64).to_le_bytes())?;
        for t in ticks {
            w.write_all(&t.ts.to_le_bytes())?;
            for v in [t.price, t.qty, t.bid, t.ask] {
                w.write_all(&v.to_le_bytes())?;
            }
        }
    } else {
        writeln!(w, "timestamp,price,qty,bid,ask")?;
        for t in ticks {
            writeln!(w, "{},{},{},{},{}", crate::time::fmt_ts(t.ts), t.price, t.qty, t.bid, t.ask)?;
        }
    }
    w.flush()?;
    Ok(())
}

fn decode_ticks(bytes: &[u8]) -> Result<Vec<Tick>> {
    if bytes.len() < 16 || &bytes[..8] != TICK_MAGIC {
        bail!("not a twq tick file");
    }
    let n = u64::from_le_bytes(bytes[8..16].try_into()?) as usize;
    let body = &bytes[16..];
    if body.len() != n * 40 {
        bail!("truncated tick file");
    }
    let f = |c: &[u8], i: usize| f64::from_le_bytes(c[i..i + 8].try_into().unwrap());
    Ok(body
        .chunks_exact(40)
        .map(|c| Tick {
            ts: i64::from_le_bytes(c[0..8].try_into().unwrap()),
            price: f(c, 8),
            qty: f(c, 16),
            bid: f(c, 24),
            ask: f(c, 32),
        })
        .collect())
}

/// Generic tick CSV: `timestamp,price,qty[,bid,ask]` (header optional).
pub fn parse_ticks_csv(bytes: &[u8]) -> Result<Vec<Tick>> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = Vec::with_capacity(bytes.len() / 32);
    for line in text.lines() {
        let f: Vec<&str> = line.split(',').map(str::trim).collect();
        let Some(ts) = f.first().and_then(|s| parse_datetime(s)) else { continue };
        let num = |i: usize| f.get(i).and_then(|s| s.parse::<f64>().ok());
        let Some(price) = num(1) else { continue };
        out.push(Tick {
            ts,
            price,
            qty: num(2).unwrap_or(1.0),
            bid: num(3).unwrap_or(f64::NAN),
            ask: num(4).unwrap_or(f64::NAN),
        });
    }
    Ok(out)
}

/// Parse the TAIFEX daily tick file (期貨每筆成交資料, `Daily_YYYY_MM_DD.csv`).
///
/// Columns: 成交日期, 商品代號, 到期月份(週別), 成交時間, 成交價格, 成交數量(B+S), 近月價格,
/// 遠月價格, 開盤集合競價. The header is Big5 and is skipped; data rows are ASCII.
///
/// * `product`: e.g. `TX`, `MTX`, `TMF` (compared after trimming padding).
/// * `expiry`: `Some("202610")` to pin a contract month, `None` = per trading date pick the
///   monthly contract with the largest volume (front month). Spreads (`202610/202611`)
///   and weekly contracts (`202610W1`) are excluded in auto mode.
/// * 成交數量(B+S) counts both sides, so quantity is halved.
pub fn parse_taifex_ticks(bytes: &[u8], product: &str, expiry: Option<&str>) -> Result<Vec<Tick>> {
    struct Raw {
        ts: Ts,
        exp: u32,
        price: f64,
        qty: f64,
    }
    let mut raws: Vec<Raw> = Vec::new();
    for line in bytes.split(|&c| c == b'\n') {
        let Ok(line) = std::str::from_utf8(line) else { continue };
        let mut it = line.split(',').map(str::trim);
        let (Some(date), Some(prod), Some(exp), Some(time), Some(px), Some(q)) =
            (it.next(), it.next(), it.next(), it.next(), it.next(), it.next())
        else {
            continue;
        };
        if prod != product || date.len() != 8 || exp.contains('/') {
            continue;
        }
        // 近月價格 / 遠月價格 are filled only on spread-leg prints: skip those.
        let (near, far) = (it.next().unwrap_or("-"), it.next().unwrap_or("-"));
        if !(near.is_empty() || near == "-") || !(far.is_empty() || far == "-") {
            continue;
        }
        match expiry {
            Some(want) if exp != want => continue,
            None if exp.len() != 6 => continue,
            _ => {}
        }
        let (Ok(d), Ok(price), Ok(qty)) = (date.parse::<u32>(), px.parse::<f64>(), q.parse::<f64>()) else {
            continue;
        };
        // 成交時間 is HHMMSS with the leading zero dropped (84500 = 08:45:00), sometimes
        // followed by milliseconds / microseconds: left-pad to at least 6 digits.
        let padded;
        let time = if time.len() < 6 {
            padded = format!("{time:0>6}");
            padded.as_str()
        } else {
            time
        };
        let tb = time.as_bytes();
        if !tb.iter().all(u8::is_ascii_digit) {
            continue;
        }
        let tnum = |a: usize, b: usize| time[a..b].parse::<u32>().unwrap_or(0);
        let mut us = 0u32;
        if tb.len() > 6 {
            let frac = &time[6..tb.len().min(12)];
            us = frac.parse::<u32>().unwrap_or(0) * 10u32.pow(6 - frac.len() as u32);
        }
        let ts = make_ts((d / 10_000) as i64, (d / 100) % 100, d % 100, tnum(0, 2), tnum(2, 4), tnum(4, 6), us);
        raws.push(Raw { ts, exp: exp.parse().unwrap_or(0), price, qty: qty / 2.0 });
    }
    if raws.is_empty() {
        bail!("no {product} ticks found (check product code / expiry)");
    }
    // front month per (trading day, session-agnostic) = largest volume
    let keep: Box<dyn Fn(&Raw) -> bool> = if expiry.is_some() {
        Box::new(|_| true)
    } else {
        let mut vol: HashMap<(i64, u32), f64> = HashMap::new();
        for r in &raws {
            *vol.entry((crate::time::trading_day(r.ts), r.exp)).or_default() += r.qty;
        }
        let mut best: HashMap<i64, (u32, f64)> = HashMap::new();
        for ((day, exp), v) in vol {
            let e = best.entry(day).or_insert((exp, v));
            if v > e.1 {
                *e = (exp, v);
            }
        }
        Box::new(move |r: &Raw| best.get(&crate::time::trading_day(r.ts)).map(|b| b.0) == Some(r.exp))
    };
    let mut ticks: Vec<Tick> = raws.iter().filter(|r| keep(r)).map(|r| Tick::trade(r.ts, r.price, r.qty)).collect();
    ticks.sort_by_key(|t| t.ts); // stable: keeps exchange order within the same timestamp
    Ok(ticks)
}

// ---------------------------------------------------------------- synthetic data

/// Parameters for the synthetic TAIFEX-like 1-minute generator.
#[derive(Clone, Debug)]
pub struct SynthConfig {
    pub start_day: i64,
    pub days: usize,
    pub start_price: f64,
    /// Annualised volatility (e.g. 0.20).
    pub annual_vol: f64,
    pub include_night: bool,
    pub seed: u64,
}

impl Default for SynthConfig {
    fn default() -> Self {
        Self {
            start_day: crate::time::days_from_civil(2021, 1, 4),
            days: 250,
            start_price: 17_000.0,
            annual_vol: 0.20,
            include_night: true,
            seed: 42,
        }
    }
}

/// Generate realistic-looking TX 1-minute bars: day session 08:45–13:45 and optional
/// night session 15:00–05:00, weekdays only, GARCH-like volatility clustering, a
/// U-shaped intraday volatility profile, overnight gaps, and prices on the 1-point grid.
pub fn synth_futures_bars(cfg: &SynthConfig) -> Vec<Bar> {
    let mut rng = Rng::new(cfg.seed);
    let per_min_vol = cfg.annual_vol / (252.0_f64 * 1140.0).sqrt();
    let mut price = cfg.start_price;
    let mut var_mult = 1.0_f64;
    let mut out = Vec::with_capacity(cfg.days * 1140);
    let mut day = cfg.start_day;
    let mut generated = 0;
    // Sessions expressed as (start minute, length in minutes), relative to calendar day.
    let day_sess = (hhmm_to_min(845) as i64, 300_i64);
    let night_sess = (hhmm_to_min(1500) as i64, 840_i64);
    while generated < cfg.days {
        let wd = weekday(day * US_PER_DAY);
        if wd >= 5 {
            day += 1;
            continue;
        }
        let mut sessions = vec![day_sess];
        if cfg.include_night {
            sessions.push(night_sess);
        }
        for (start, len) in sessions {
            // overnight / inter-session gap
            price *= 1.0 + rng.normal() * per_min_vol * 6.0;
            let mut vwap_anchor = price;
            for m in 0..len {
                let ts = day * US_PER_DAY + (start + m) * US_PER_MIN;
                // U-shape: more volatile near session open / close
                let x = m as f64 / len as f64;
                let u = 0.6 + 1.6 * ((x - 0.5) * 2.0).powi(2);
                var_mult = 0.97 * var_mult + 0.03 * (1.0 + 2.0 * rng.normal().powi(2)) / 3.0;
                let sigma = per_min_vol * u * var_mult.sqrt() * price;
                // weak mean reversion toward the session anchor + occasional trend bursts
                let burst = rng.uniform() < 0.002;
                let drift = -0.002 * (price - vwap_anchor) + if burst { rng.normal() * sigma * 8.0 } else { 0.0 };
                let open = price;
                let mut hi = open;
                let mut lo = open;
                let mut p = open;
                for _ in 0..4 {
                    p += drift / 4.0 + rng.normal() * sigma / 2.0;
                    hi = hi.max(p);
                    lo = lo.min(p);
                }
                let close = p.round();
                // volume rises with the size of the move; bursts print 3-7x normal volume
                let move_z = ((close - open) / sigma.max(1e-9)).abs().min(10.0);
                let vol_mult = (1.0 + 0.35 * move_z) * if burst { 3.0 + 4.0 * rng.uniform() } else { 1.0 };
                let bar = Bar {
                    ts,
                    open: open.round(),
                    high: hi.round().max(open.round()).max(close),
                    low: lo.round().min(open.round()).min(close),
                    close,
                    volume: ((50.0 + 400.0 * u * var_mult) * (0.5 + rng.uniform()) * vol_mult).round(),
                };
                price = close;
                vwap_anchor += 0.01 * (price - vwap_anchor);
                out.push(bar);
            }
        }
        generated += 1;
        day += 1;
    }
    // Night-session minutes past midnight overflow naturally onto the next calendar day.
    out.sort_by_key(|b| b.ts);
    out
}

/// Generate a synthetic tick stream (random trade arrivals inside each bar).
pub fn synth_ticks_from_bars(bars: &[Bar], ticks_per_bar: usize, seed: u64) -> Vec<Tick> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(bars.len() * ticks_per_bar);
    let period = if bars.len() > 1 { (bars[1].ts - bars[0].ts).clamp(US_PER_SEC, US_PER_MIN) } else { US_PER_MIN };
    for b in bars {
        let n = ticks_per_bar.max(4);
        let (p1, p2) = if b.low_first() { (b.low, b.high) } else { (b.high, b.low) };
        let i1 = 1 + (rng.uniform() * (n as f64 - 3.0)) as usize;
        let i2 = i1 + 1 + (rng.uniform() * (n - i1 - 2) as f64) as usize;
        let anchors = [(0, b.open), (i1, p1), (i2, p2), (n - 1, b.close)];
        let mut k = 0;
        for i in 0..n {
            while k + 1 < anchors.len() && anchors[k + 1].0 < i {
                k += 1;
            }
            let (ia, pa) = anchors[k];
            let (ib, pb) = anchors[(k + 1).min(3)];
            let px = if ib == ia || i == ia {
                pa
            } else if i >= ib {
                pb
            } else {
                let f = (i - ia) as f64 / (ib - ia) as f64;
                (pa + (pb - pa) * f + rng.normal() * 0.5).clamp(b.low, b.high).round()
            };
            let ts = b.ts + (period * i as i64) / n as i64;
            out.push(Tick::trade(ts, px, 1.0 + (rng.uniform() * 5.0).floor()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_formats() {
        let a = b"2023-11-19 18:00:00,15895.00,15903.75,15888.00,15893.25,571\n2023-11-19 18:01:00,1,2,0.5,1.5,3\n";
        let bars = parse_bars_csv(a).unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].high, 15903.75);
        let b = b"Date,Time,Open,High,Low,Close,TotalVolume\n2024/01/02,08:46:00,17900,17910,17890,17905,1234\n";
        let bars = parse_bars_csv(b).unwrap();
        assert_eq!(bars[0].ts, make_ts(2024, 1, 2, 8, 46, 0, 0));
        assert_eq!(bars[0].volume, 1234.0);
        let c = "日期,開盤價,最高價,最低價,收盤價,成交量\n2024-01-02,1,2,0.5,1.5,10\n".as_bytes();
        assert_eq!(parse_bars_csv(c).unwrap()[0].close, 1.5);
    }

    #[test]
    fn taifex_parse_front_month() {
        let s = "header-in-big5\n\
20240102,TX     ,202401     ,84500,17950.0,24,-,-,*\n\
20240102,TX     ,202401     ,084501,17951,2,-,-,\n\
20240102,TX     ,202402     ,084501,18000,2,-,-,\n\
20240102,TX     ,202401/202402,084502,50,2,17950,18000,\n\
20240102,TX     ,202401     ,084502,17950,2,17950,18000,\n\
20240102,MTX    ,202401     ,084502,17952,2,-,-,\n";
        let t = parse_taifex_ticks(s.as_bytes(), "TX", None).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].ts, make_ts(2024, 1, 2, 8, 45, 0, 0));
        assert_eq!(t[0].price, 17950.0);
        assert_eq!(t[0].qty, 12.0);
        let t2 = parse_taifex_ticks(s.as_bytes(), "TX", Some("202402")).unwrap();
        assert_eq!(t2.len(), 1);
    }

    #[test]
    fn binary_roundtrip() {
        let dir = std::env::temp_dir().join(format!("twq-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bars = synth_futures_bars(&SynthConfig { days: 3, ..Default::default() });
        assert!(bars.len() > 3000);
        let p = dir.join("b.bin");
        save_bars(&p, &bars).unwrap();
        assert_eq!(load_bars(&p).unwrap(), bars);
        let ticks = synth_ticks_from_bars(&bars[..100], 10, 1);
        let p2 = dir.join("t.bin");
        save_ticks(&p2, &ticks).unwrap();
        let back = load_ticks(&p2).unwrap();
        assert_eq!(back.len(), ticks.len());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn synth_bars_are_valid() {
        let bars = synth_futures_bars(&SynthConfig { days: 5, ..Default::default() });
        for b in &bars {
            assert!(b.low <= b.open && b.low <= b.close && b.high >= b.open && b.high >= b.close);
            assert_eq!(b.close, b.close.round());
        }
        assert!(bars.windows(2).all(|w| w[1].ts > w[0].ts));
    }
}
