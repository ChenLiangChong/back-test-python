# 研究報告：台股自動交易（群益 API、開源專案、台灣市場規則、馬克羊策略）

整理日期：2026-09-25

這份報告是 4 個研究 agent 平行研究後彙整而成。有兩點要先說明：
- 研究環境的網路政策擋掉了 capital.com.tw、taifex.com.tw、twse.com.tw、youtube.com、threads.com、facebook.com 等網站，所以很多官方資訊只能透過搜尋引擎摘要，或 GitHub 上的官方手冊鏡像取得。
- 沒辦法直接從官方來源確認的內容都標為 **〔未驗證〕**。正式上線前請逐項核對。

目錄
1. [群益 API (SKCOM)](#1-群益-api-skcom)
2. [開源專案與可借鏡的設計](#2-開源專案與可借鏡的設計)
3. [台灣市場規則（回測必要參數）](#3-台灣市場規則回測必要參數)
4. [歷史資料來源](#4-歷史資料來源)
5. [馬克羊（楊震）交易方法研究](#5-馬克羊-楊震-交易方法研究)
6. [結論：這個 POC 採用了什麼](#6-結論這個-poc-採用了什麼)

---

## 1. 群益 API (SKCOM)

### 1.1 目前提供的產品（2025–2026）
- **仍然只有 SKCOM**（策略王 COM 元件，`SKCOM.dll`），只支援 Windows COM。沒有找到 REST、WebSocket、gRPC、Linux 版，也沒有官方的 Python 原生套件。〔capital.com.tw 無法直接連線，未驗證〕
- **最新版本 2.13.59（2026-08-03）**，前一版 2.13.58 是 2026-03-09。來源：[官方手冊與範例的 GitHub 鏡像](https://github.com/Fin-Agentian/SKCOM_C_codebase_memory)、[2.13.57→2.13.59 變更紀錄](https://github.com/Fin-Agentian/SKCOM_C_codebase_memory/blob/main/api_spec/changelog_2.13.57_to_2.13.59.md)。這個鏡像是非官方、經 AI 整理過的，請視為很強的二手證據。
- 2.13.58/59 的重點變更：
  - 新增 `SKQuoteLib_EnterMonitorLONGByMarket`、盤中即時分K（`OnNotifyLiveKLineData`）、`OnOpenInterestJson`。
  - 「查無資料」的字串格式改了。
  - **會強制升級**：版本過舊時登入回傳 9996（`SK_ERROR_UPDATE_API_REQUIRED`），所以要持續追蹤新版。
- 群益期貨和群益證券用的是同一個 SKCOM 元件、同一本手冊，沒有另外的期貨 API。
- 另外還有 Proxy Server 下單元件（`SKOrderLib_InitialProxyByID` 等），以及 C# 簡化版 `SKDLLCSharp.dll`。

### 1.2 環境與申請流程

**執行環境**
- 只能在 Windows 上跑，x86 和 x64 的 DLL 分開。呼叫端的位元數必須和註冊的 DLL 一致。
- 用系統管理員身分執行 `install.bat` 或 `regsvr32` 註冊 DLL。
- 需要安裝 VC++ 2010 SP1 MFC（`mfc100u.dll`）。
- 必須在**有桌面的使用者工作階段**中執行，不支援以 LocalSystem 身分跑的 Windows 服務（見 [tai-robot](https://github.com/ericwu13/tai-robot)）。

**憑證**
- 路徑：群益金融網 → 憑證專區 → AP 軟體憑證（RAWinApp），依序登入、同意、完成 email 驗證、安裝。
- 相關錯誤碼：600、602、603、1097 是憑證問題；1045 表示 IE 憑證庫裡找不到憑證。

**同意書**
- 手冊原文：「登入前，需先簽署證券或期貨API下單聲明書」，而且「API不支援簽署同意書」，必須到金融網線上簽署。
- 沒簽的話會出現 2018 / 2019 錯誤；沒簽期貨同意書時連期貨報價都查不到（3031）。

**連線測試（強制）**
- 手冊原文：「依主管機關規定，使用正式API前，須先進行連線測試」。
- 做法是執行 `SKCOMVerifyDJ.exe`，完成雙因子登入並送出測試單，紀錄顯示「驗證-成功」才算完成。沒做會出現錯誤 321。
- 來源：[3.登入](https://github.com/Fin-Agentian/SKCOM_C_codebase_memory/blob/main/api_spec/_raw/v2.13.59/3.登入.md)、[A-login flow](https://github.com/Fin-Agentian/SKCOM_C_codebase_memory/blob/main/api_spec/flows/A-login.md)

**測試環境**
- `SKCenterLib_SetAuthority(2)` 切換到測試環境，0 是正式環境。

**費用**
- 社群普遍認為 API 免費，但沒找到官方說明〔未驗證〕。

### 1.3 物件模型與關鍵函式
共 6 個 COM 類別：`SKCenterLib`、`SKOrderLib`、`SKQuoteLib`、`SKReplyLib`、`SKOSQuoteLib`（海外期貨報價）、`SKOOQuoteLib`（海外選擇權報價）。
所有函式都回傳 int 代碼，用 `SKCenterLib_GetReturnCodeMessage(code)` 轉成文字說明。

**登入順序**（本 repo 的 `bridge/capital_bridge.py` 就是照這個順序實作）：

1. `SKCenterLib_SetLogPath`
2. **登入前先註冊 `SKReplyLib.OnReplyMessage`，並回傳 `-1`**。沒做的話會出現錯誤 2017（2.13.17 之後必要）。
3. `SKCenterLib_Login(ID 大寫, 密碼)`。回傳 2003 表示已經登入，也算成功。
   - 登入失敗後要等 5 秒才能再試（1129）。
   - 連續失敗 5 次會出現 9997，必須重新啟動 API。
4. `SKReplyLib_ConnectByID(ID)`，然後等 `OnSolaceReplyConnection(0)` 和 `OnComplete`。
5. `SKOrderLib_Initialize()`
6. `GetUserAccount()`，帳號從 `OnAccount` 回傳；完整帳號 = 欄位[1] + 欄位[3]，期貨帳號的類型是 `TF`。
7. `ReadCertByID(ID)`。沒做的話下單會出現 1011。
8. `SKQuoteLib_EnterMonitorLONG()`，**等 `OnConnection` 回傳 3003 之後才能訂閱**。

**報價**
- `SKQuoteLib_RequestTicks(ref page, 代碼)`：一頁一檔，第一次訂閱會先用 `OnNotifyHistoryTicksLONG` 回補當天的 tick，之後才是 `OnNotifyTicksLONG`。
  - **bridge 刻意不把回補的歷史 tick 轉給引擎**，避免策略把舊 tick 當成即時行情來交易。
- `OnNotifyTicksLONG(sMarketNo, nIndex, nPtr, nDate, nTimehms, nTimemillismicros, nBid, nAsk, nClose, nQty, nSimulate)`：
  - `nSimulate=1` 表示試撮，不是真實成交。
  - `nTimemillismicros` 是這一秒內的毫秒加微秒。
  - 價格是整數，要除以 10^`sDecimal`（從 `GetStockByNoLONG` 取得的 `SKSTOCKLONG` 結構裡拿）。
- `sMarketNo`：0 上市、1 上櫃、2 期貨、3 選擇權、4 興櫃、5/6 盤中零股。
- 期貨代碼在不同用途下格式不一樣：報價用 `TM2609`，下單和回報用 `TMFI6`，部位用 `TM09`（見 [capital_api mapping](https://github.com/AlexChiang0208/capital_api/blob/main/docs/official_mapping.md)）。**上線前一定要在測試環境確認下單代碼**，所以 `twq live` 有獨立的 `--order-symbol` 參數。

**下單**
- 期貨建議用 `SendFutureOrderCLR(ID, bAsync, FUTUREORDER)`。舊的 `SendFutureOrder` 會忽略 `sNewClose` 和 `sReserved`。
- `FUTUREORDER` 欄位：

  | 欄位 | 內容 |
  |---|---|
  | `bstrFullAccount` | 帳號 |
  | `bstrStockNo` | 商品代碼 |
  | `sBuySell` | 0 買、1 賣 |
  | `sTradeType` | 0 ROD、1 IOC、2 FOK |
  | `sDayTrade` | 當沖旗標 |
  | `sNewClose` | 0 新倉、1 平倉、2 自動 |
  | `bstrPrice` | 價格字串；市價填 `"M"`、範圍市價填 `"P"`，**這兩種只能配 IOC 或 FOK** |
  | `nQty` | 口數 |
  | `sReserved` | 盤別 |

- 股票用 `SendStockOrder` 和 `STOCKORDER`：
  - `sPeriod`：0 盤中、1 盤後、2 零股、4 盤中零股
  - `sFlag`：0 現股、1 融資、2 融券、3 無券賣出
  - `nTradeType` 和 `nSpecialTradeType`（1 市價、2 限價）
- 刪單：`CancelOrderBySeqNo(ID, async, 帳號, 13碼序號)`。
- 同步下單會阻塞，回傳的 `bstrMessage` 就是 13 碼委託序號。非同步下單的結果從 `OnAsyncOrder(nThreadID, nCode, msg)` 回來。
- 2.13.58 之後，下單函式回傳 0 只代表「已送出」，最終狀態要看 `OnNewData`。

**回報 `OnNewData`**（逗號分隔，約 48 欄以上，索引從 0 開始）

| 欄位 | 內容 |
|---|---|
| 1 | MarketType（TF 期貨、TS 證券…） |
| 2 | Type：N 委託、C 刪單、U 減量、P 改價、D 成交 |
| 3 | OrderErr |
| 10 | 委託書號 |
| 11 | 價格 |
| 20 | 數量 |
| 23 | 日期 |
| 24 | 時間 |
| 38 | 成交序號 |
| 44 | 錯誤訊息 |
| 47 | 13 碼序號 |

- 2.13.59 在 TF/TO 回報多加了一個未公開的欄位，所以**解析時要能容錯**。
- 來源：[SKReplyLib spec](https://github.com/Fin-Agentian/SKCOM_C_codebase_memory/blob/main/api_spec/modules/SKReplyLib.md)、[SKOrderLib spec](https://github.com/Fin-Agentian/SKCOM_C_codebase_memory/blob/main/api_spec/modules/SKOrderLib.md)、[SKQuoteLib spec](https://github.com/Fin-Agentian/SKCOM_C_codebase_memory/blob/main/api_spec/modules/SKQuoteLib.md)、[capital_api parsers](https://github.com/AlexChiang0208/capital_api)

### 1.4 限制與陷阱

**連線與訂閱**
- **報價連線額度**：每個帳號 2 條（另一種說法是國內 1 條 + 海外 1 條，說法有衝突〔未驗證〕），超過會出現 3030。只下單的程式可以用 `LoginSetQuote(..., "N")`，不佔報價額度。
- **訂閱上限**：
  - `RequestStocks` 一次最多 100 檔，超過的會被默默丟掉。
  - tick 訂閱的上限，官方說 10 檔，社群 SDK 說頁碼 0–49〔有衝突，請自行測試〕。
  - 斷線重連後要重新訂閱。
- **心跳**：每 15 秒呼叫一次 `SKQuoteLib_RequestServerTime()`，避免防火牆因為閒置而切斷連線。
- **重連**：不要在事件處理函式裡直接重連，要用計時器等至少 5 秒。會觸發重連的狀態碼：3002、3021、3022、3033。

**下單與查詢**
- **下單節流**：券商端的上限沒有公開〔未知，建議直接問群益〕。
  - API 端可以用 `SetMaxCount` / `SetMaxQty` 自行設上限，超過時該市場會被鎖住（1040），要呼叫 `UnlockOrder` 解鎖。
  - twq 的 `RiskManager` 和 bridge 都各自做了節流。
- **查詢**：帳務類查詢每次要間隔 5 秒（1019 / M999），而且是阻塞式的。

**執行緒**
- STA 單一執行緒加上訊息迴圈（官方 C++ 範例：`CoInitialize` + `GetMessage/DispatchMessage`；Python 用 `pythoncom.PumpWaitingMessages`）。
- 不要在 `GetTickLONG` 或 `GetBest5LONG` 自己的事件裡呼叫它們，可能會死鎖。

### 1.5 社群專案

| Repo | 內容 |
|---|---|
| [AlexChiang0208/capital_api](https://github.com/AlexChiang0208/capital_api) | Python comtypes，對應 2.13.58，2026-09 仍在更新。**目前最完整的參考**，本 bridge 的 COM 呼叫方式就是照它核對的 |
| [Fin-Agentian/SKCOM_C_codebase_memory](https://github.com/Fin-Agentian/SKCOM_C_codebase_memory) | 手冊 markdown、226 個錯誤碼、官方 C#/C++ 範例（2.13.57 / 2.13.59） |
| [ericwu13/tai-robot](https://github.com/ericwu13/tai-robot) | Python 台指期機器人，對應 2.13.57 |
| [kaihg/stock-trader](https://github.com/kaihg/stock-trader) | 「COM 主執行緒 + REST 背景執行緒」的 bridge 模式 |
| [GNAySolution/GNAy](https://github.com/GNAySolution/GNAy) | C# WPF 全自動期權交易 |
| [eermagic/csharp-console-capital-socket-server](https://github.com/eermagic/csharp-console-capital-socket-server) | C# 透過 TCP socket 轉發報價 |
| [tacosync/skcom](https://github.com/tacosync/skcom) | 安裝和註冊輔助工具（2022 年後停止更新） |

**Rust / Go 完全沒有前例**。GitHub 上搜尋 SKCOM 或群益相關的 Rust repo，結果是 0 個。

### 1.6 Rust 能不能直接呼叫 SKCOM？
**技術上可行，但目前沒有人做過。**
- 事件介面是 dispinterface，官方 C++ 範例在 `Invoke` 裡用 DISPID 分派事件，例如 21 = `OnNotifyTicksLONG`。參數在 `rgvarg` 裡是**反序**排列。
- Rust 的做法：
  - 用 `#[implement(IDispatch, Agile = false)]` 實作事件接收端（`windows` crate 在 2025-09 合併的 [PR #3770](https://github.com/microsoft/windows-rs/pull/3770) 才支援 `Agile = false`）。
  - 用 `IConnectionPointContainer::FindConnectionPoint` + `Advise` 掛上事件。
- 呼叫方法時建議用 vtable 早期綁定，因為 `FUTUREORDER` 這種結構用 IDispatch 傳很麻煩。vtable 的順序可以從 type library 產生，**每次升級 SKCOM 都要重新產生**。

**本 POC 的選擇**：先用薄薄一層 Python bridge 負責 COM，Rust 引擎透過 TCP 連線。
- 這也是研究結論推薦的「gateway 和引擎分成兩個程序」架構：9996、9997 這類錯誤、廠商 DLL 當掉、強制升級，都可以只重啟 bridge，引擎不受影響。
- Python 每則訊息多出幾十微秒，和券商往返的毫秒級延遲相比可以忽略。
- 之後要換成 C# 或 Rust 原生 gateway 時，**協定不需要改**。

### 1.7 其他券商比較（2025–2026）

| 券商 / API | 語言 | 作業系統 | 速率限制 |
|---|---|---|---|
| **永豐 Shioaji** v1.7.6 | Python 原生；`shioaji server start` 提供 REST + SSE（:8080），**任何語言都能透過 HTTP 使用** | Linux / macOS / Windows | 下單每 10 秒 250 次、訂閱 200 檔（[limits](https://github.com/Sinotrade/Sinotrade.github.io/blob/master/tutor/limit/index.html)） |
| **富邦 Neo** v2.2.8 | Python、Node、C#、**C++20**、**Go**；有實驗性的 [Rust FFI](https://github.com/SDpower/r-fubon-neo) | Windows / macOS / Linux | 每條 WebSocket 200 檔；REST 超量回 429 |
| **凱基 SuperPy** | Python | Windows / Linux | 未公開 |
| **元大** | 期貨：YuantaOneAPI（.NET） | Windows | 〔未驗證〕 |
| **群益 SKCOM** | COM：C#、C++、Python、VB | **只有 Windows** | 見上文 |

→ 如果之後想讓整套系統都在 Linux 上跑，永豐（HTTP/SSE）和富邦（C++/Go SDK）是最容易接的。twq 的 bridge 協定和券商無關，換券商只需要再寫一個 bridge。

---

## 2. 開源專案與可借鏡的設計

### 2.1 台灣相關

| 專案 | 語言 | 重點 |
|---|---|---|
| [FinLab](https://pypi.org/project/finlab/) | Python（只提供 wheel，GPL） | 向量化台股回測。預設手續費 0.001425、證交稅 0.003 |
| [twstock](https://github.com/mlouielu/twstock) | Python | 爬 TWSE / TPEx 資料。證交所限速約每 5 秒 3 次 |
| [FinMind](https://github.com/FinMind/FinMind) | Python | 50 多種資料集的 API |
| [Sinotrade/Shioaji](https://github.com/Sinotrade/Shioaji)、[rshioaji](https://github.com/Sinotrade/rshioaji) | Python / Rust（alpha） | 永豐官方；rshioaji 提供 REST 和 SSE |
| [AutoTradingPlatform](https://github.com/chrisli-kw/AutoTradingPlatform) | Python | Shioaji 自動交易，附**期交所 tick 檔前處理**程式 |
| [taifex-resolver](https://github.com/lawrence910426/taifex-resolver) | C++ | 解碼期交所 UDP multicast（I024 / I081 / I083），主機共置時會用到 |
| 其他 | Python / Java | [ultra-trader](https://github.com/ppcvote/ultra-trader)（時段風控）、[Shioaji_job](https://github.com/wenli/Shioaji_job)、[FuturesBot_Backtesting](https://github.com/elanifegnirf/FuturesBot_Backtesting) |

**結論：找不到成熟的 Rust 或 Go 台股回測框架**，台灣的生態系幾乎都是 Python。可以沿用的是知識，不是程式碼：期交所檔案的格式陷阱、各種費率常數、券商 API 的使用細節。

### 2.2 高效能引擎（重點是 Rust）

| 專案 | 授權 | 值得學的設計 |
|---|---|---|
| [NautilusTrader](https://github.com/nautechsystems/nautilus_trader) | LGPL-3.0 | 單執行緒、可重現的核心，I/O 放在邊緣；回測、sandbox、實盤共用同一個 kernel；**K 棒內部採自適應 O→H→L→C 路徑**；風控引擎在執行層前面 |
| [barter-rs](https://github.com/barter-rs/barter-rs) | MIT | 用整數索引存 O(1) 狀態；策略是「狀態 → 下單意圖」的純函式；`run_backtests` 在共享資料上平行跑多組參數 |
| [hftbacktest](https://github.com/nkaz001/hftbacktest) | MIT | 每個事件帶兩個時間戳（交易所 / 本地），可以模擬延遲；`LatencyModel` 和 `QueueModel`（排隊位置）；回測和實盤共用 `Bot` trait |
| wingfoil、lfest-rs（AGPL）、rust_ti、yata、ta | — | 串流 DAG、槓桿期貨模擬器、指標庫 |

**本 POC 採用的設計**
- 單執行緒、可重現的事件迴圈（Nautilus）。
- 同一個 `Strategy` trait 通吃回測、模擬交易、實盤。
- 自適應的 K 棒內部路徑，而且 `on_bar` 裡下的單不會在同一根 K 棒成交（Nautilus）。
- 策略只輸出意圖，由風控層把關（barter / Nautilus）。
- 用 rayon 在共享資料上平行最佳化（barter）。
- 限價單預設要穿價才成交（hftbacktest 的 risk-averse 排隊模型）。

**刻意沒有照抄程式碼**：NautilusTrader 是 LGPL、lfest-rs 是 AGPL，只參考它們的想法。

---

## 3. 台灣市場規則（回測必要參數）

### 3.1 證交所股票
- **手續費**：上限是買賣各 0.1425%，整股最低 20 元。電子下單折扣大約 1～6.5 折，也有券商是事後退佣〔四捨五入或無條件捨去的方式依券商而定〕。
- **證交稅（只有賣出方）**：股票 0.3%、ETF 0.1%。債券 ETF 免稅到 2026-12-31，延長法案還沒通過〔狀態不明〕。
- **現股當沖**：0.15%，已延長到 **2027-12-31**（[RTI](https://www.rti.org.tw/news?uid=3&pid=120734)、[MOF](https://www.mof.gov.tw/singlehtml/384fb3077bb349ea973e7fc6f13b6974?cntId=163515032583410f91d2fab867e3b113)）。
- **升降單位**（[TWSE PDF](https://www.twse.com.tw/downloads/zh/trading/introduce/introduce004.pdf)）：

  | 股價 | 未滿 10 | 10–50 | 50–100 | 100–500 | 500–1000 | 1000 以上 |
  |---|---|---|---|---|---|---|
  | 跳動點 | 0.01 | 0.05 | 0.1 | 0.5 | 1 | 5 |

  - ETF：未滿 50 元跳 0.01，50 元以上跳 0.05。
  - **1000 元以上的跳動點預計在 2027 年 7 月從 5 元改成 1 元**，還要等金管會核准（[工商](https://www.ctee.com.tw/news/20260825701782-430201)）。
- **漲跌幅**：±10%。
- **交易時段**：
  - 09:00 開盤集合競價，09:00–13:25 逐筆交易，13:25–13:30 收盤集合競價。
  - 14:00–14:30 盤後定價交易。
  - 瞬間價格穩定措施：±3.5% 會延緩撮合 2 分鐘。
- **盤中零股**：目前每 5 秒撮合一次。2026-12-07 起改成 08:30 開始收單、09:00 第一次撮合。約 2027 年 7 月改成每 1 秒撮合（[CNA](https://www.cna.com.tw/news/afe/202607080100.aspx)）。

### 3.2 期交所（TX / MTX / TMF）
- **每點價值**：TX 200 元、MTX 50 元、TMF 10 元（2024-07-29 上市），跳動點都是 1 點。
- **期交稅**：契約價值的十萬分之二，買賣各課一次（[eTax](https://www.etax.nat.gov.tw/etwmain/tax-info/understanding/tax-q-and-a/national/future-transaction-tax/filing-payment-and-collection-reward/jDxbO8a)）〔四捨五入方式未驗證〕。
- **手續費**：可以議價。社群程式大多用 TX 40–100 元、MTX 15–50 元、TMF 8–20 元〔未驗證〕。twq 的預設值是 TX 50、MTX 25、TMF 12，可以用 `--commission` 調整。
- **交易時段**：一般時段 08:45–13:45；盤後時段 15:00 到隔天 05:00，屬於下一個交易日。
- **結算**：每月第三個星期三，結算價是現貨最後 30 分鐘（13:00–13:30）的平均（[TAIFEX](https://www.taifex.com.tw/cht/5/formulaIndex)）。
- **漲跌幅**：±10%〔動態價格穩定措施未驗證〕。
- **保證金**：變動頻繁，應該從資料檔讀取，不要寫死。2026-08-12 起：
  - TX 原始 70.1 萬、維持 53.8 萬
  - MTX 17.525 萬、TMF 3.505 萬
  - 〔來源為轉述，未驗證〕

---

## 4. 歷史資料來源

- **期交所逐筆成交**（[下載頁](https://www.taifex.com.tw/cht/3/dlFutPrevious30DaysSalesData)）
  - 免費的只有最近 30 個交易日，更早的要另外申請購買。
  - 欄位：`成交日期,商品代號,到期月份(週別),成交時間,成交價格,成交數量(B+S),近月價格,遠月價格,開盤集合競價`。
  - 格式陷阱（`twq data taifex` 都已處理）：
    - 檔案是 **Big5** 編碼。
    - 時間欄位**省略開頭的 0**（`84500`），要補零成 6 位。
    - **成交量是買賣雙邊加總**，要除以 2。
    - 近月價格或遠月價格欄位有值的，是**價差單**成交，要排除。
    - `*` 標記的是開盤集合競價。
  - 夜盤成交在每日檔案中的日期歸屬〔未驗證，請實際檢查〕。
- **FinMind**：
  - 未登入每小時 300 次，有 token 每小時 600 次。
  - `TaiwanFuturesTick` 從 2011 年開始（需付費方案）。
  - `TaiwanStockKBar` 1 分 K 從 2019 年開始。
- **Shioaji**：`api.ticks()` / `api.kbars()` 從 2020-03-02 開始。
- **群益 API**：`RequestTicks` 可以回補當天的 tick；`RequestKLineAMByDate` 可以抓歷史 K 線。
- **證交所 OpenAPI**：只有最新一天的資料；歷史資料要用 `STOCK_DAY`（限速約每 5 秒 3 次）。付費資料可以到 [TWSE e-Shop](https://eshop.twse.com.tw/zh/category/main/7) 購買。

---

## 5. 馬克羊 (楊震) 交易方法研究

### 5.1 身分

| 項目 | 內容 |
|---|---|
| 本名 / 稱號 | **楊震醫師**，又稱馬克羊、Dr. Mark Yang、電競醫生 Dr.馬克羊、電玩醫生 馬克羊 |
| 背景 | 前醫師、科學奧林匹亞金牌、爐石職業選手、楊震數學創辦人、拉斯維加斯算牌、群益期貨講座講師，現為投顧老師 |
| Threads | **[@dr.markyang](https://www.threads.com/@dr.markyang)**。能被索引到的自己的貼文只有健身文，沒有抓到任何交易貼文 |
| YouTube | [電玩醫生 馬克羊](https://www.youtube.com/channel/UC2CeWmIKcVfqBbWPS3Hf3CQ)，播放清單「[投資交易 & 行為經濟學](https://www.youtube.com/playlist?list=PLDk-2-4tqDyDdOWe6FkWsyk-Yxi9SAQmP)」 |
| Facebook | [電競醫生 Dr.馬克羊](https://www.facebook.com/dryangmark)，是交易內容最多的平台 |
| IG | [dr.markyang](https://www.instagram.com/dr.markyang/) |
| 交易商品 | 台指期（大台、小台、微台，日盤和夜盤）、台指選擇權（週選和月選）、股票期貨、現股當沖 |

**風格演變**
- 2021–2022 年以當沖和隔日沖為主。
- 2025–2026 年自述：「**長線持有，靠事件交易與遊戲漏洞做短線，並且盡量做到少輸**」。

**⚠️ 資料取得限制**
- YouTube、Threads、Facebook 的內容頁全部被網路政策擋住，**影片和貼文全文都沒讀到**。
- 以下內容來自三種管道：
  1. 搜尋引擎摘要。
  2. FB 貼文網址裡的開頭原文。FB 會把貼文前幾句放進網址，所以這部分是他本人的原話。
  3. 第三方的講座逐字稿筆記。
- 如果想補齊，最直接的方法是把他近期的 Threads 貼文或影片逐字稿貼給我。

### 5.2 已知的影片、貼文與講座（節錄）
- 2022-05-08〈[【實戰覆盤】夜盤當沖交易手法公開](https://www.youtube.com/watch?v=vIk4yxI_ZTw)〉。說明欄原文：「**爆大量紅K低點 -1tick跌破 → 空單進場 停損紅K**」。進階的進出場調整只在 LINE 群提供。
- 2022-04-12〈[一天來回賺150萬](https://www.youtube.com/watch?v=jBz53I9hSEE)〉：股票期貨，169 元賣出 30 口，142.5 元買回 28 口。
- 2022-05-17〈[台指期鬼之操作 90分鐘內 在高低振幅75點內 刷出100多點](https://www.youtube.com/watch?v=KT2hLJvVG34)〉：規則沒有公開。
- 2026-02-07〈[少輸，才是交易中真正的競爭優勢](https://www.youtube.com/watch?v=ifs_foXrkmU)〉（群益期貨講座），筆記見 [HackMD](https://hackmd.io/@JbhlYkBtRJukuvsXMtL6lg/HylGhIHfzx)、[vocus](https://vocus.cc/article/69995cb7fd897800011fb515)。
- 2026-06〈[手割分析與帕雷托優勢](https://www.youtube.com/watch?v=vEVnotRNBaY)〉（群益期貨講座）。
- FB 貼文：
  - [大小台開盤價差套利](https://www.facebook.com/dryangmark/posts/1207026074114141/)：「開盤前2分鐘不能刪單…大小台開盤價會有巨大價差就可以一多一空套利…夜盤開盤這樣夾就可以賺70幾點」
  - [颱風假套利](https://www.facebook.com/dryangmark/posts/1077764157040334/)：「選擇權價格近似正比於剩餘時間的1/2次方倍」
  - [存股用期貨](https://www.facebook.com/dryangmark/posts/973655370784547/)
  - [月結算日結算價形成](https://www.facebook.com/dryangmark/posts/1149525983197484/)
  - [每週五結算選擇權](https://www.facebook.com/dryangmark/posts/1264461735037241/)

### 5.3 可程式化的策略（〔C〕= 他本人說的，〔I〕= 我們補的參數）

| 策略 | 規則 | 自動化難度 | twq 狀態 |
|---|---|---|---|
| **B1 爆量紅K破低放空** | 〔C〕爆大量紅K，跌破低點 −1 tick 放空，停損設紅K高點<br>〔I〕量 ≥ 20 根均量的 3 倍、5 根 K 棒內有效、停利 2R 或移動停損、收盤前平倉、預設夜盤；`mirror=1` 加上對稱的多單 | 容易 | ✅ `vol_breakdown` |
| **A5 尾盤當沖強平順勢** | 〔第三方筆記〕盤中趨勢 ≥150 點時，13:30 順勢進場，13:44 出場 | 容易 | ✅ `tail_flow`（第三方實測：不穩健） |
| **計程車司機法則** | 〔C〕手氣好就繼續、手氣差就早點收工；短停損、長停利 | 容易 | ✅ 任何策略加上 `max_daily_loss=`，實盤另有 `--max-daily-loss` |
| **A1 大小台開盤價差** | 〔C〕開盤前 2 分鐘不能刪單，大小台試撮價差大時一多一空套利 | 中（需要試撮報價、多商品同時訂閱） | ⏳ Roadmap |
| **A2 微台/小台/大台價差 + 部位互抵** | 〔第三方〕價差大時鎖住，用部位互抵平倉 | 中（觸發次數極少） | ⏳ |
| **A3 颱風假時間價值** | 〔C〕結算日放颱風假時，時間價值約為 √(T新/T舊) 倍，公布後立刻買進 | 需要新聞來源 + TXO 報價 | ⏳ |
| **A4 必歸零選擇權** | 〔第三方〕結算前一日，漲跌停外的履約價必定歸零，可以賣出 | 需要 TXO 報價 | ⏳ |
| **C1/C2 用期貨取代存股、自組正二** | 〔C〕存股改用期貨；0050 + 小台/微台自組 2 倍槓桿 | 容易（資產配置） | ⏳ |
| **C3 反處分效應再平衡** | 〔C〕定期砍掉虧損部位、加碼賺錢部位 | 容易（多商品） | ⏳ |
| B2 強勢股隔日沖、B4 台指鬼之操作、夜盤策略A、處置股公式 | 規則不公開或需要主觀判斷 | 無法自動化 | ✗ |

**他的通用原則**（已轉成 twq 的風控預設）
- 只跑正期望值的系統，每個動作都要有明確規則。
- **絕不攤平或凹單**：內建策略都不攤平；實盤風控的「最大部位（含在途委託）」會擋下超出上限的加碼。
- 評估系統要同時看勝率和賺賠比。
- 用凱利公式決定部位大小。自動化時建議用四分之一凱利〔I〕；`risk_pct` 參數可以依權益百分比計算口數。

**⚠️ 重要提醒**
- 第三方 repo（[maplab-ai-handbook](https://github.com/page1010/maplab-ai-handbook)）整理了 **31 個「馬克羊」策略說法，扣掉成本後沒有一個是正期望值**：
  - 19 個只是影片裡的宣稱
  - 6 個規則不完整
  - 另一個第三方（[financial-daily-digest](https://github.com/marketdaily/financial-daily-digest)）測試了尾盤順勢和指數調整日，結果也都不顯著
- 在模擬的隨機漫步資料上，twq 的所有策略都是虧損的。這是正確的：如果在隨機資料上賺錢，才代表回測引擎有問題。
- **請一定要用真實的期交所 tick 資料加上 walk-forward 驗證，確認有正期望值之後，再小口數上線。**

---

## 6. 結論：這個 POC 採用了什麼

1. **Rust 核心 + 可以替換的券商 bridge**
   - 群益只支援 Windows COM，所以把 COM 隔離在獨立程序裡，引擎可以跑在任何 OS 上。
   - 協定和券商無關，之後可以接永豐或富邦。
2. **回測和實盤用同一套程式**
   - 模擬撮合器 `SimExchange`、部位計算 `Portfolio`、策略介面 `Ctx`、K 棒合成器 `BarAggregator` 在回測、模擬交易、實盤中都是同一份程式碼。
   - 整合測試證明模擬交易和逐筆回測的損益完全一致。
3. **台灣規則寫成參數**：期交稅、證交稅、當沖稅、跳動點級距、日夜盤、交易日歸屬。
4. **防止過度最佳化**：平行網格搜尋 + 錨定式 walk-forward + 最少交易數門檻。
5. **實盤安全**：
   - 預設是 paper 模式；live 模式必須加上 `--i-understand-live-risk`。
   - bridge 預設 `allow_orders=false`。
   - 多層風控、每日虧損斷路器、KILL 檔、JSONL 日誌。
