# 交接文件（Handoff）

整理時間：2026-09-26，雲端 session 轉交給本機 Mac 上的 Claude Code 繼續。
分支：`claude/determined-cray-g27xm5`（已推上 GitHub；`main` 還是舊的 Python 手動回放工具，尚未合併）。

## 1. 使用者要什麼

- 用**自己的策略**在台指期做回測，之後透過**群益 API**全自動下單。要快，Rust 實作。
- 使用者用 **Mac**：回測在 Mac 上跑；群益 SKCOM 只能在 Windows 上跑（實盤時需要 Parallels 或 Windows VPS）。
- 使用者比較想要「別人做好的成熟開源專案」。調查後發現，成熟又有台灣券商介面的框架（vnpy + vnpy_sinopac、AutoTradingPlatform）都是接**永豐**，接群益的只有很小的個人專案。使用者表示「用群益可以寫 adapter 沒關係」，所以目前沿用本 repo 的 twq 引擎。
- 早期有研究馬克羊（楊震）的交易方法，見 `docs/RESEARCH.md` 第 5 節。後來使用者給了自己的策略，重點轉到使用者的策略。

## 2. 使用者的策略（原話整理）

**事件盤夾子**：`crates/twq-strategies/src/event_clip.rs`，策略名 `event_clip`

- 公布前在最新價上下 ±20 點掛觸價單（OCO），停損 40 點。
- **大事件**停利 150：非農+失業率、FOMC（fed fund rate）、CPI。
- **一般事件**停利 100：JOLTS、PPI、MSCI 調整日 13:25:00、富時調整日 13:25:00、台積電營收公布 13:30:00。

**破 day low ORB**：`crates/twq-strategies/src/orb_daylow.rs`，策略名 `orb_daylow`

1. 9:30 前，日盤台指期最高減最低 > 100 點。
2. 9:30 前，加權指數成交量 > 昨量 × 0.3。
3. 兩個條件都達標後，掛「日盤最低點 − 1 點」的觸價空單（破 day low 進場）。

### 使用者還沒回答的問題（不要自己拍板，要問）

- ORB 是「9:30 前任何時候達標就掛單」（`at_deadline=0`），還是「9:30 那一刻才檢查」（`at_deadline=1`）？
- ORB 的停損和停利。目前預設停損 40、不設停利、13:40 平倉，這些都是我先填的假設。
- 事件盤的夾子公布後多久沒觸發就取消（`cancel_min=5`，假設），以及最長持倉時間（`max_hold_min=0` 表示只看停損停利和收盤前平倉）。
- 「加權指數成交量」指的是成交金額還是成交股數？目前預設用成交金額（`taiex_cumamt.csv`）。
- 用大台、小台還是微台？目前預設 1 口大台（TX）。

## 3. 已完成

- twq 引擎：
  - 回測：K 棒模式和逐筆模式。
  - 平行最佳化、walk-forward（錨定式、樣本外）。
  - HTML 報告。
  - 模擬交易和實盤 runner：風控、KILL 檔、JSONL 日誌。
  - 群益 bridge：參考實作，有 `--fake` 自測模式。
  - 55 個測試。
- 資料工具：
  - `tools/fetch_data.py`：期交所近 30 個交易日逐筆成交，加上證交所 MI_5MINS 大盤累積成交。證交所被擋時會退避重試。
  - `tools/fetch_community_data.py`：CrazyIndicator 的 1998～2023 年 1 分 K，會自動下載、解壓、合併（雲端上實測成功）。
  - `tools/fetch_release_dates.py`：從 FRED 抓非農、CPI、PPI、JOLTS 的歷年公布日期。**尚未實際跑過**，因為雲端上 FRED 被擋。
  - `twq data taifex`：自動偵測夜盤日期標法，結算日換月。
  - `twq data merge`：合併多個 K 棒檔並平移時間。
- 行事曆：
  - `data/events/fomc.csv`：2017～2027 共 87 場，從 federalreserve.gov 解析。
  - `data/events/events.csv`：2026 年 8～9 月、以網路搜尋核對過的事件。
  - `--index-events 2017-2023`：依規則產生 MSCI 和富時調整日（未處理假日）。

## 4. 目前的回測結果（1 口大台，含手續費、期交稅和 1 tick 滑價）

**事件盤夾子，期交所逐筆資料 2026/8/14～9/24**：8 筆，−16,787 元。

| 事件 | 結果 |
|---|---|
| 非農 | 停利 +29,525 |
| 台積電營收 | 收盤前平倉 +6,525 |
| 其他 6 個事件 | 全部停損，每筆約 −8,700 |

**事件盤夾子，社群 1 分 K 2017～2023**（只有 FOMC、MSCI、富時，缺非農、CPI、PPI、JOLTS）：56 筆，+64,845 元，獲利因子 1.34。

| 事件 | 筆數 | 淨損益 |
|---|---|---|
| FOMC | 31 | +70,856 |
| 富時 | 13 | +8,612 |
| MSCI | 12 | −14,623 |

**ORB，只用條件一**（大盤量資料被證交所擋，沒抓到）

| 資料 | 設定 | 結果 |
|---|---|---|
| 2026/8～9 逐筆 | 停損 40 點 | 25 筆全部停損（指數 46,000 點時 40 點太窄） |
| 2026/8～9 逐筆 | `at_deadline=1`，停損 100～200 | 16 筆，獲利因子約 1.4，+9～13 萬 |
| 2011～2023 1 分 K | 固定點數 | 12 種組合全部正報酬，獲利因子 1.13～1.26，最佳約 +53 萬（13 年） |
| 2011～2023 1 分 K | 百分比版：`at_deadline=1, range_pct=0.7, sl_pct=0.8` | 466 筆，獲利因子 1.20，Sharpe 0.38 |
| 2011～2023 walk-forward | 同上 | 4 折裡 3 折賺錢，樣本外 +36 萬 |

2011～2023 的固定點數版獲利集中在 2020～2022 高波動年。百分比版 walk-forward 的樣本外獲利大部分來自 2019～2020 那一折。

⚠️ 樣本都偏少，而且缺條件二，**不能當結論**。

## 5. 接下來要做的（依優先順序）

```bash
cargo build --release
python3 tools/fetch_community_data.py                         # 1998~2023 1 分 K (需 pip3 install py7zr 或 macOS tar)
python3 tools/fetch_data.py --skip-twse                       # 期交所近 30 個交易日逐筆
./target/release/twq data taifex "data/taifex/Daily_*.csv" --product TX --out data/tx_ticks.bin --bars-tf 1m --bars-out data/tx_1m.bin
python3 tools/fetch_release_dates.py --from 2017 --to 2026    # → data/events/us_macro.csv
python3 tools/fetch_data.py --skip-taifex --twse-from 2017-01-01 --twse-delay 5   # 約 2~3 小時, 可中斷續抓
```

1. **驗證 `fetch_release_dates.py`**：每年每項應該約 12 次。抽查這幾個日期：
   - 2026-09-04 NFP、2026-09-11 CPI、2026-09-10 PPI、2026-09-01 JOLTS
   - 2025 年 10～11 月美國政府關門，那段期間的公布日期有延後或取消，要確認 FRED 抓到的是實際日期

   如果 FRED 頁面格式和解析器不符，就修正 regex。
2. **事件盤完整回測**：
   ```bash
   ./target/release/twq backtest --data data/txf_2011_2023_1m.bin --from 2017-05-15 -s event_clip \
     --events data/events/fomc.csv --events data/events/us_macro.csv --index-events 2017-2023 \
     --events <台積電營收日期檔> --out reports/event_full
   ```
   - 台積電營收日期還沒有來源。規則大約是每月 10 日左右，但需要查證。
   - 另外用 2026 年的逐筆資料（`data/tx_ticks.bin --ticks`）再跑一次比對。
   - 結果要依事件、依年份拆開呈現。
3. **ORB 加入條件二**：
   ```bash
   ./target/release/twq backtest --data data/txf_2011_2023_1m.bin --from 2017-01-01 -s orb_daylow \
     --series taiex_vol=data/taiex_cumamt.csv -p "at_deadline=1,range_pct=0.7,sl_pct=0.8"
   ```
   - 用 walk-forward 驗證。
   - 策略會自動跳過缺前一交易日大盤量的日子，看 `missing_vol_days`。
4. **補 2024/1～2026/7 的資料缺口**：
   - QuantPass 的 2006～2024/5 1 分 K 要登入 Google、用瀏覽器手動下載：https://quantpass.org/txf/
   - 最好的方法是期交所「期貨成交簡檔」：每半年 NT$1,000，逐筆、含夜盤，格式和免費 30 天檔一樣，可以直接用 `twq data taifex` 匯入。要在 E-Data Shop 購買，**需要使用者付款，先問**。
5. **把結果整理給使用者**：先用 HTML 報告和簡表，再問第 2 節那些待決問題。
6. **之後（實盤）**：
   - 在 Windows 上用群益**測試環境**逐步驗證 `bridge/capital_bridge.py`。步驟見 `bridge/README.md`：先 `allow_orders=false`，再 `test_env=true`。
   - 群益下單代碼可能和報價代碼不同（`--order-symbol`）。

## 6. 注意事項

- 使用者不熟程式。要給**直接能貼的指令**和簡短結論，不要一次丟一大堆選項。
- 不要把使用者的帳密放進任何檔案或環境變數設定檔。群益密碼用環境變數 `CAPITAL_PASSWORD`。
- 不確定的事（日期、費率、API 行為）要標「未驗證」，不要自己編。
- 回測結果要附上樣本數和限制：1 分 K 和逐筆的差異、缺少的條件、樣本外表現。
