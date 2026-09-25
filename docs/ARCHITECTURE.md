# 架構與延遲設計

## 1. 模組

```
twq-core ──────────────┬──────────────┬─────────────┐
 types / time            │              │             │
 instrument (台灣規則)    │              │             │
 indicators (O(1) 串流)   ▼              ▼             ▼
 data (CSV/bin/期交所)  twq-backtest   twq-strategies  twq-live
 strategy (Strategy/Ctx)  engine         ma_cross        runner (paper/live)
 sim (SimExchange)        metrics        orb             risk
 portfolio                optimize(rayon) donchian       bridge (TCP client)
 aggregator               report(HTML)   bb_revert       protocol (JSON lines)
                                         vol_breakdown   mock (假 bridge)
                                         tail_flow       latency / journal
                                         guard(每日停損)
                               └──────────── twq-cli (`twq`) ───────────┘
```

**同一套元件，三種執行方式**

| 元件 | 回測 | 模擬交易 (paper) | 實單 (live) |
|---|---|---|---|
| `Strategy` + `Ctx` | ✓ | ✓ | ✓ |
| `BarAggregator`（tick → K 棒） | tick 模式 | ✓ | ✓ |
| `SimExchange`（撮合） | ✓ | ✓（用真實報價撮合） | ✗（改送券商；停損單在本地觸發） |
| `Portfolio`（損益、手續費、稅） | ✓ | ✓ | ✓（用券商成交回報） |
| `RiskManager` | — | ✓ | ✓ |

整合測試 `crates/twq-live/tests/pipeline.rs` 用同一批 tick 同時跑逐筆回測和模擬交易，驗證兩者的交易筆數和淨損益**完全相同**。

## 2. 策略 API（`Ctx`）

- 策略**不會直接下單**，而是把意圖（`Action::Submit / Cancel / CancelAll`）放進 `Ctx`。引擎在每次 callback 結束後統一取出處理：回測送進 `SimExchange`，實盤先經過 `RiskManager` 再送到 bridge。
- `Ctx` 會記錄在途的委託（`working_orders`、`pending_market_qty`）。所以 `target_position()` 就算每個 tick 都呼叫，也不會重複下單（實盤時市價單從送出到成交需要幾毫秒）。
- **括號單**：用 `enter(side, qty, kind, Bracket{stop_dist, take_dist}, tag)` 進場。成交後 `Ctx` 會根據**實際成交價**自動掛出一組 OCO 停損和停利；部分成交也會按成交數量掛出。
- **reduce-only 模式**：`DailyLossGuard` 或實盤風控觸發時，`Ctx` 只接受減倉方向的委託，連翻空、翻多也會被擋。

## 3. 回測撮合

**K 棒模式**：每根 K 棒依序做兩件事。
1. 用 `BarPath` 走過 K 棒內部路徑：開盤價比較靠近最低價時走 O→L→H→C，否則走 O→H→L→C。
   - 每一步都找出「最先被觸價」的委託來成交。
   - 跳空時以開盤價成交。
   - 停損和停利在同一點同時觸價時，停損優先（保守假設）。
   - 成交後策略在 `on_fill` 裡新下的單，會從目前的價格繼續沿路徑撮合。所以進場那根 K 棒如果回頭碰到停損，也會正確出場（見測試 `stop_loss_same_bar_as_entry_is_honoured`）。
2. K 棒收盤時：更新權益和統計，呼叫 `on_bar`。在這裡下的單**從下一根 K 棒才開始生效**，所以不會偷看到未來。

**逐筆模式**：每一筆 tick 先檢查是否有 K 棒收盤（有的話先呼叫 `on_bar`），接著用這筆成交價撮合在途委託，最後呼叫 `on_tick`。
- 限價單預設要「穿價」才成交；剛送出、價格已經可成交的限價單，直接以當時的價格成交。
- 市價單有買賣價時以買賣價成交，沒有的話以成交價加滑價成交。

**效能重點**
- 策略型別會被單態化（monomorphize）。
- 沒有在途委託時，完全不會進撮合器。
- 指標都是 O(1) 環形緩衝區或單調佇列。
- 每根 K 棒不做任何記憶體配置。
- 績效統計是邊跑邊算的，只保存每日權益。
- 最佳化時用 rayon 平行執行，所有執行緒共享同一份 K 棒資料（`&[Bar]`）。

## 4. 實盤執行緒模型

```
 bridge-reader thread                     engine thread (唯一持有所有狀態)
 ─────────────────────                    ─────────────────────────────────────────────
 read_line() ─▶ parse JSON ─▶ Instant ─▶  crossbeam 有界通道 ─▶ handle(event)
                                          ├ tick: BarAggregator → on_bar
                                          │       paper: SimExchange.match_tick
                                          │       live : 本地停損觸價 → IOC 範圍市價單
                                          │       on_tick → 風控 → 送單 (write syscall)
                                          ├ fill / ack / cancelled / rejected
                                          └ 計時器: K 棒逾時收盤、KILL 檔、執行時間上限
 journal thread ◀── 通道 ─────────────────  (日誌在送單之後才建立，不影響延遲)
```

- 整條熱路徑上**沒有鎖**，也沒有記憶體配置，引擎處理一筆 tick 的中位數是 **0.1 µs**。
- `--spin` 忙等模式會一直輪詢通道，省掉執行緒被喚醒的時間（在 VM 上大約 20 µs），代價是佔滿一顆 CPU 核心。
- 延遲量測分成三段：
  1. 引擎內部處理
  2. 收到 tick → 處理完成
  3. 收到 tick → 委託寫入 socket

  程式結束時會印出 p50、p90、p99、p99.9，並寫進日誌。
- 如果收盤前一段時間都沒有新 tick，會用「最後一筆 tick 的時間加上經過的實際時間」推算交易所時間，時間到了就把 K 棒收掉（預設寬限 500 ms）。

**實盤延遲預算（估計）**

| 路段 | 時間 |
|---|---|
| 交易所 → 群益 → SKCOM 事件 | 毫秒級（網路，無法控制） |
| SKCOM 事件 → Python bridge → TCP | 數十 µs |
| twq 收到 → 決策 → 寫出委託 | 約 3–25 µs |
| bridge → `SendFutureOrderCLR` → 群益 → 交易所 | 毫秒級 |

→ 真正的瓶頸在券商網路。想再更快，下一步是：
1. 把 bridge 換成 C# 或 Rust 原生 COM（省下數十 µs）。
2. 主機放在離券商近的機房。
3. 換成支援 FIX 或 DMA 的券商。

## 5. Bridge 協定（JSON lines over TCP）

每行一個 JSON 物件，用欄位 `t` 表示訊息類型。時間 `ts` 是交易所當地時間，從 1970 年起算的**微秒**數，不做時區轉換，例如 08:45:00 → `…*86400e6 + 8*3600e6 + 45*60e6`。

**Bridge → 引擎**

| `t` | 欄位 | 說明 |
|---|---|---|
| `hello` | `bridge, version, simulated` | 連線後送出的第一則訊息 |
| `tick` | `sym, ts, px, qty, bid?, ask?, sim?` | 成交；`sim=true` 表示試撮，引擎會忽略 |
| `ack` | `cid, oid` | 委託成功，`oid` 是券商序號 |
| `fill` | `cid, px, qty, ts, fee?, tax?` | 成交（可以分批）；沒帶手續費和稅時，由引擎自行計算 |
| `cancelled` | `cid` | 已刪單，或 IOC 剩餘數量被取消 |
| `rejected` | `cid, reason` | 委託失敗 |
| `position` | `sym, qty, avg` | 回應 `query_position`，引擎會據此對帳 |
| `info` / `error` | `msg` | `info` 的 `msg` 是 `"eof"` 時，引擎會結束 |
| `pong` | `id` | 回應 `ping` |
| `sync` | `id` | 模擬 bridge 的鎖步屏障，引擎回 `sync_ack` |

**引擎 → Bridge**

| `t` | 欄位 | 說明 |
|---|---|---|
| `subscribe` | `sym` | 訂閱 tick |
| `order` | `cid, sym, side(B/S), qty, px(null = 市價), tif(ROD/IOC/FOK), mkt(M/P), day_trade, oc(auto/new/close)` | 下單 |
| `cancel` | `cid` | 刪單 |
| `query_position` | `sym` | 查詢部位 |
| `ping` / `sync_ack` | `id` | |

**`cid`** 是引擎產生的委託編號，bridge 負責在 `cid` 和券商的 13 碼序號之間做對應。
想接其他券商（永豐、富邦、IB…），只要實作這個協定，引擎完全不用改。

## 6. 安全設計

- `twq live` 預設是 `--mode paper`；`--mode live` 必須加上 `--i-understand-live-risk`。
- bridge 預設 `allow_orders=false`：只轉發報價，引擎送來的委託一律回覆 `rejected`（dry run）。
- 風控有三層：
  1. 策略層：`DailyLossGuard`
  2. 引擎層：`RiskManager` 檢查部位、單筆口數、下單頻率、價格偏離、每日虧損
  3. bridge 層：每秒下單數上限
- 緊急停止：建立 `KILL` 檔 → 全部平倉 → 程式結束。
- 程式結束時（包括 bridge 送出 eof）會自動平倉，最多等 3 秒確認成交。
- 所有委託、成交、拒單、停機事件都寫進 `journal.jsonl`，寫完每筆就 flush。
