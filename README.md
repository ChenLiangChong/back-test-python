# twq — 台股 / 台指期 超高速回測 + 自動交易引擎 (Rust)

這個 repo 原本是用 Python + lightweight-charts 寫的手動 K 棒回放工具（`back-test-python`），現在整個換成 **Rust**。
目標是「同一份策略程式碼，回測、模擬交易、群益實單都能跑，而且快到爆炸」。

```
                 ┌────────────── twq (Rust, 任何 OS) ──────────────┐
 歷史資料 ──────▶│  回測引擎 (K棒/逐筆)  ─┐                          │
 期交所 tick CSV  │  平行最佳化 + walk-forward│  同一個 Strategy trait │
                 │  Live runner ──────────┘  + 風控 + 日誌           │
                 └──────────────▲───────────────────┬───────────────┘
                                │ TCP JSON lines (127.0.0.1:9101)
                 ┌──────────────┴───────────────────▼───────────────┐
                 │ bridge/capital_bridge.py (Windows) ◀── COM ──▶ 群益 SKCOM.dll │
                 └────────────────────────────────────────────────────┘
```

> ⚠️ **免責聲明**：這是 POC。群益 bridge 是依官方手冊與社群 SDK 寫成的參考實作，**尚未連過真實群益帳號**。
> 請務必先用 `--mode paper`、群益測試環境（`test_env = true`）和 `allow_orders = false` 驗證。
> 內建策略（包含馬克羊啟發的策略）都**不是**已證實的獲利策略，只是可以回測的假設。自動交易有虧損風險，後果請自行負責。

## 有多快？

實測環境：雲端 4 vCPU（Intel Xeon 2.8GHz）VM，`twq bench`，模擬資料為 500 萬根台指期 1 分 K（約 17 年日夜盤）

| 項目 | 結果 |
|---|---|
| 回測 `ma_cross`（單執行緒，含撮合/手續費/期交稅/統計） | **8,000 萬根 K 棒/秒**（500 萬根 62 ms） |
| 回測 `orb` / `vol_breakdown` / `donchian` | 4,100 萬 / 2,750 萬 / 2,330 萬根/秒 |
| 平行最佳化（256 組參數 × 500 萬根） | 4.2 秒，**合計 3 億根 K 棒/秒**（4 核） |
| 逐筆 tick 回測 | **1.09 億筆 tick/秒** |
| Live：引擎內部處理一筆 tick（策略 + 風控 + 路由） | **p50 0.1 µs** |
| Live：收到 tick → 處理完成（`--spin`） | p50 2.4 µs / p99 26 µs |
| Live：收到 tick → 委託寫入 socket（`--spin`） | p50 24 µs（大部分是這台 VM 的 TCP 系統呼叫） |

作為對照，最精簡的純 Python 均線迴圈（沒有手續費、撮合和統計）是每秒 150 萬根，twq 大約快 50 倍，最佳化時約快 200 倍。

實盤的延遲瓶頸是**券商往返（毫秒級）**，不是引擎。引擎本身不會是問題。

## 快速開始

```bash
# 1. 安裝 Rust (https://rustup.rs) 後編譯
cargo build --release          # 產出 target/release/twq (Windows: twq.exe)

# 2. 產生模擬台指期 1 分 K（500 個交易日，日盤 + 夜盤）
./target/release/twq data gen --days 500 --out data/tx_1m.bin

# 3. 回測，輸出 HTML 報告
./target/release/twq strategies                      # 列出策略與參數
./target/release/twq backtest --data data/tx_1m.bin -s orb -p "range_min=15,rr=2" --out reports/orb
#   → reports/orb/report.html (權益曲線/回撤/交易明細), trades.csv, equity.csv, stats.json

# 4. 平行最佳化 + walk-forward 樣本外驗證（強烈建議，避免過度最佳化）
./target/release/twq optimize --data data/tx_1m.bin -s orb \
    --grid range_min=5:60:5 --grid rr=0,1,1.5,2,3 --grid buffer=0:10:2 \
    --objective sharpe --wf 4

# 5. 端對端演練：模擬 bridge + 模擬交易（不需券商帳號、任何 OS）
#    (重播 2022-06-01 日盤，3000 倍速約 6 秒播完；-v 會印出每根 K 棒與每筆委託/成交)
./target/release/twq mock-bridge --data data/tx_1m.bin --from "2022-06-01 08:45" --to "2022-06-01 13:46" --speed 3000 &
./target/release/twq live --bridge 127.0.0.1:9101 --mode paper -s orb -p min_range=10 -v

# 6. 效能測試
./target/release/twq bench
```

真實資料：
- **期交所逐筆成交**（最近 30 個交易日免費下載，更早的要付費申請）：
  `twq data taifex Daily_2026_09_*.csv --product TX --out data/tx_ticks.bin --bars-tf 1m --bars-out data/tx_1m.bin`
  程式會自動處理 Big5 表頭、時間欄位省略開頭 0（`84500`）、成交量 B+S 除以 2、排除價差單，並自動挑選近月合約。
  萬用字元由程式自己展開，Windows PowerShell 也能用。期交所下載的是 zip 檔，請先解壓縮。
  如果發現夜盤成交的日期被標成「交易日」（晚上的成交跑到隔天），加上 `--night-prev-day` 修正。
- 一般 K 棒 CSV：`timestamp,open,high,low,close,volume`，也接受 `Date,Time,...` 或中文欄名（`日期,開盤價,...`）。
- `data/sample/NQ_1min_sample.csv` 是原 repo 附的 NQ 範例資料：`twq backtest --data data/sample/NQ_1min_sample.csv --instrument NQ -s orb -p "open_hhmm=930,session_end=1600,exit_hhmm=1555"`

## 接群益實單

完整步驟在 [`bridge/README.md`](bridge/README.md)，摘要如下：

1. 到群益金融網簽署「API 下單聲明書」，並安裝 AP 憑證。
2. 下載 SKCOM 2.13.x 並註冊 DLL（x64），然後執行 `SKCOMVerifyDJ.exe` 完成主管機關要求的連線測試。
3. `pip install -r bridge/requirements.txt`，複製 `bridge/config.example.ini` 成 `config.ini`。
4. `python bridge/capital_bridge.py --config bridge/config.ini`（預設 `allow_orders=false`：只收報價，委託一律拒絕）
5. `twq live --bridge 127.0.0.1:9101 --mode paper -s orb --warmup data/tx_1m.bin`：真實報價 + 本地模擬成交
6. 確認沒問題後，把 bridge 設成 `allow_orders=true`，執行：
   `twq live --mode live --i-understand-live-risk --max-pos 1 --max-qty 1 --max-daily-loss 20000 --day-trade ...`

## 策略

| 名稱 | 說明 | 來源 |
|---|---|---|
| `orb` | 台指日盤開盤區間突破。OCO 雙向停損單，搭配括號停損停利，13:40 強制平倉 | 經典 |
| `ma_cross` | 均線交叉，可選 EMA 或 ATR 停損 | 經典 |
| `donchian` | 唐奇安通道突破（海龜），ATR 加通道移動停損 | 經典 |
| `bb_revert` | 布林通道均值回歸 + KD 濾網，中軌停利 | 經典 |
| `vol_breakdown` | **爆量紅K低點 −1 tick 跌破放空，停損紅K**（預設只做夜盤） | 馬克羊〈夜盤當沖交易手法公開〉說明欄 |
| `tail_flow` | 13:30 當沖強制平倉的順勢單（盤中趨勢 ≥150 點時跟單，13:44 出場） | 馬克羊 2026-02 群益講座（第三方筆記） |
| 任何策略 + `max_daily_loss=N` | 「計程車司機法則」：當日虧損達 N 元就全部平倉，當天停止交易 | 馬克羊講座 |

馬克羊（楊震醫師）的研究整理、每條規則的出處，以及哪些部分是我自行補上的參數，都寫在 [`docs/RESEARCH.md`](docs/RESEARCH.md#5-馬克羊-楊震-交易方法研究)。
**提醒**：有第三方把他 31 個策略說法拿去做成本後測試，結果沒有一個是正期望值。請把這些策略當成待驗證的假設，一定要用自己的資料跑 walk-forward。

### 寫自己的策略

```rust
use twq_core::{Bar, Bracket, Ctx, OrderKind, Params, Side, Strategy};
use twq_core::indicators::Sma;

pub struct MyStrat { sma: Sma, qty: i64 }

impl MyStrat {
    pub const PARAMS: &'static [(&'static str, f64, &'static str)] = &[("n", 20.0, "均線"), ("qty", 1.0, "口數")];
    pub fn new(p: &Params) -> Self { Self { sma: Sma::new(p.usize("n", 20)), qty: p.get("qty", 1.0) as i64 } }
}

impl Strategy for MyStrat {
    fn on_bar(&mut self, bar: &Bar, ctx: &mut Ctx) {
        let Some(ma) = self.sma.update(bar.close) else { return };
        if ctx.is_flat() && bar.close > ma {
            // 市價進場，成交後自動掛 30 點停損、60 點停利 (OCO)
            ctx.enter(Side::Buy, self.qty, OrderKind::Market, Bracket { stop_dist: Some(30.0), take_dist: Some(60.0) }, "long");
        }
    }
}
```

在 `crates/twq-strategies/src/lib.rs` 的 `REGISTRY` 加一筆，回測、最佳化、實盤就都能用。
`Ctx` 提供的功能：`buy/sell/buy_limit/sell_stop/...`、`enter`（附帶括號單）、`target_position`、`flatten`、`cancel_all`、`position()`、`equity()`、`qty_for_risk()`、`tick_size()`。

## 撮合與成本模型（台灣規則）

- **期貨**：TX 每點 200 元、MTX 50 元、TMF 10 元。期交稅是契約價值的十萬分之二（每口四捨五入），手續費每口單邊金額可自訂（`--commission`）。
- **股票**：手續費 0.1425% × 折扣（最低 20 元），證交稅賣出 0.3%，現股當沖 0.15%（目前延長到 2027-12-31），ETF 0.1%。股價跳動點依台股級距表（10/50/100/500/1000 元分界）。
- **K 棒內部路徑**：開盤價比較靠近最低價時，假設走 O→L→H→C，否則走 O→H→L→C（同 NautilusTrader 的規則）。停損停利依觸價順序決定。在 `on_fill` 裡新下的單（例如括號停損）會繼續用同一根 K 棒剩下的路徑撮合。跳空時以開盤價成交。
- 市價單和停損單預設滑價 1 tick（`--slippage`）。限價單預設要「穿價」才算成交（`--fill-on-touch` 改成碰價即成交，比較樂觀）。
- 權益歸零會強制平倉並停止交易（爆倉）。
- Walk-forward：錨定式，每一折都用之前的全部資料做最佳化，再拿下一段做樣本外交易，交易前會先用歷史資料暖機（這段期間不下單）。

## 風控（live / paper）

每張委託送出前都會經過 `RiskManager` 檢查：
- 最大部位（含在途委託）
- 單筆最大口數
- 每秒 / 每分鐘委託數上限（防止程式失控連續下單）
- 限價偏離最新成交價的上限
- **每日虧損斷路器**：觸發後全部平倉，之後只允許減倉
- **緊急停止**：建立 `KILL` 檔 → 全部平倉 → 程式結束
- 所有委託、成交、拒單都寫進 `journal.jsonl`

停損單在實盤是**引擎本地的合成停損**：價格一觸發，就在微秒級內送出範圍市價（`P`）IOC 單，不依賴券商的停損單功能。

## 專案結構

```
crates/
  twq-core/        型別、時間、台灣市場規則、O(1) 串流指標、資料 IO、Strategy/Ctx、模擬撮合、部位計算
  twq-backtest/    回測引擎 (K棒/逐筆)、績效統計、平行最佳化、walk-forward、HTML 報告
  twq-strategies/  策略庫 + 風控包裝 (DailyLossGuard)
  twq-live/        live runner、風控、bridge 協定/客戶端、模擬 bridge、延遲量測、日誌
  twq-cli/         `twq` 指令
bridge/            群益 SKCOM Windows bridge (Python)，含 --fake 自我測試模式
docs/RESEARCH.md   研究報告：群益 API、開源專案、台灣市場規則、資料來源、馬克羊策略
docs/ARCHITECTURE.md 架構與延遲設計、bridge 協定規格
data/sample/       原 repo 的 NQ 範例資料
```

測試：`cargo test --release`，共 44 個測試，涵蓋以下項目：
- 指標和樸素實作的比對
- 撮合路徑
- 手續費和稅
- 期交所格式解析
- **模擬交易和逐筆回測的損益完全一致**
- 實單模式透過 TCP bridge 的委託和成交對帳
- 風控攔截
- 隨機資料上沒有「未來函數」

## 下一步 (Roadmap)

- [ ] 在 Windows + 群益**測試環境**逐步驗證 bridge：登入、報價、下單、回報、刪單
- [ ] 啟動時用 `GetOpenInterestGW` 對帳，取得在其他地方建立的部位
- [ ] 多商品和價差策略，例如馬克羊的大小台開盤價差套利。需要試撮報價，以及 TX、MTX、TMF 同時訂閱
- [ ] 選擇權策略（颱風假時間價值、必歸零選擇權），需要 TXO 報價
- [ ] 期交所保證金、漲跌停、交易所休市日曆
- [ ] 如果需要更低延遲，用 Rust `windows` crate 直接呼叫 SKCOM COM，或改用 C# gateway（協定不變）
- [ ] 評估其他券商：永豐 Shioaji 和富邦 Neo 都支援 Linux / HTTP / C++，可以寫成新的 bridge

## 授權

MIT。`data/sample/NQ_1min_sample.csv` 來自 FirstRateData 的範例資料，授權以其網站為準。
