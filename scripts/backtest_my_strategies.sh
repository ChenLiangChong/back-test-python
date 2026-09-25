#!/usr/bin/env bash
# 一鍵: 編譯 → 下載免費官方資料 → 匯入 → 回測「事件盤夾子」與「破 day low ORB」→ 產生報告
# Mac / Linux 皆可. 需要: Rust (https://rustup.rs), python3, curl
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null; then
  echo "請先安裝 Rust:  curl https://sh.rustup.rs -sSf | sh   (裝完重開終端機)"; exit 1
fi
cargo build --release
TWQ=./target/release/twq

python3 tools/fetch_data.py --out data "$@"

$TWQ data taifex "data/taifex/Daily_*.csv" --product TX \
  --out data/tx_ticks.bin --bars-tf 1m --bars-out data/tx_1m.bin

echo; echo "===== 事件盤夾子 (±20, 停損 40, 大事件停利 150 / 一般 100) ====="
$TWQ backtest --data data/tx_ticks.bin --ticks -s event_clip \
  --events data/events/events.csv --out reports/event_clip

echo; echo "===== 破 day low ORB (高低差>100 且 9:30 前大盤成交>昨量0.3倍) ====="
$TWQ backtest --data data/tx_ticks.bin --ticks -s orb_daylow \
  --series taiex_vol=data/taiex_cumamt.csv --out reports/orb_daylow

echo; echo "報告: reports/event_clip/report.html  reports/orb_daylow/report.html"
if command -v open >/dev/null; then open reports/event_clip/report.html reports/orb_daylow/report.html; fi
