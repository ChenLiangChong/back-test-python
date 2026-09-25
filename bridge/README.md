# 群益 SKCOM Bridge（Windows）

`capital_bridge.py` 是 twq 引擎和群益 SKCOM API 之間的橋樑：

```
twq.exe (Rust)  ⇄  TCP 127.0.0.1:9101 (JSON lines)  ⇄  capital_bridge.py  ⇄  SKCOM.dll (COM)
```

> ⚠️ 這是依照 SKCOM 2.13.5x 官方手冊和社群 SDK（[capital_api](https://github.com/AlexChiang0208/capital_api)）寫成的**參考實作，還沒有連過真實帳號測試**。
> 請照下面第 6 到第 8 步，從測試環境開始一步一步驗證。

## 1. 券商端準備（一次性）
1. 開立群益證券或群益期貨帳戶。
2. 到**群益金融網線上簽署**「證券 / 期貨 API 下單聲明書」。API 本身沒辦法簽，沒簽會出現 2018 或 2019 錯誤。
3. 安裝 AP 憑證：群益金融網 → 憑證專區 → AP 軟體憑證，下載 RAWinApp，依序登入、同意、完成 email 驗證。
4. 到 API 下載區下載 **SKCOM 最新版**（寫這份文件時是 2.13.59）。
   - 用系統管理員身分執行 `install.bat` 註冊 **x64** DLL。
   - 安裝 VC++ 2010 SP1 MFC 可轉散發套件。
5. 執行 `SKCOMVerifyDJ.exe` 完成**主管機關要求的連線測試**：雙因子登入並送出測試單，確認紀錄顯示「驗證-成功」。沒做會出現錯誤 321。

## 2. 安裝 bridge
```powershell
# 64 位元 Python 3.10+（位元數必須和註冊的 SKCOM.dll 一致）
pip install -r bridge\requirements.txt
copy bridge\config.example.ini bridge\config.ini
notepad bridge\config.ini          # 填 user_id 和 dll_path；密碼建議用環境變數
$env:CAPITAL_PASSWORD = "你的密碼"
```

## 3. 編譯 twq（Windows）
```powershell
# 安裝 Rust: https://rustup.rs
cargo build --release              # 產出 target\release\twq.exe
```
也可以在 Linux 上交叉編譯：`cargo build --release --target x86_64-pc-windows-gnu`（需要先安裝 mingw-w64）。

## 4. 不接券商的自我測試（任何 OS）
```bash
python bridge/capital_bridge.py --fake --fake-seconds 30      # 假券商：隨機漫步報價，委託立即成交
twq live --bridge 127.0.0.1:9101 --mode live --i-understand-live-risk -s ma_cross -p fast=3,slow=8
```

## 5. 只收報價（allow_orders = false）
```powershell
python bridge\capital_bridge.py --config bridge\config.ini
```
Log 應該依序出現：
1. `SKCenterLib_Login: 0`
2. `SKReplyLib_ConnectByID: 0`
3. `order reports: backfill complete`
4. `SKOrderLib_Initialize: 0`
5. `ReadCertByID: 0`
6. `accounts: futures=...`
7. `quote OnConnection kind=3003`

## 6. 模擬交易：真實報價 + 本地撮合，不會送單
```powershell
twq live --bridge 127.0.0.1:9101 --mode paper -s orb --symbol TX00 --warmup data\tx_1m.bin -v
```
確認 tick 時間、價格、K 棒都正確：TX00 的價格應該是指數點數，例如 23000，不是 2300000。

## 7. 群益測試環境送單
在 `config.ini` 設定 `test_env = true`、`allow_orders = true`，然後執行：
```powershell
twq live --bridge 127.0.0.1:9101 --mode live --i-understand-live-risk -s orb --symbol TX00 --order-symbol TX00 --max-pos 1 --max-qty 1
```
要逐項確認以下流程：委託 → `ack`（13 碼序號）→ `fill` → 刪單 → `cancelled` → 拒單原因。
**下單代碼可能和報價代碼不同**，群益的期貨代碼在報價、下單、部位三種用途下格式不一樣。請在測試環境確認正確的 `--order-symbol`。

## 8. 正式環境
`test_env = false`，先用 1 口微台（`--instrument TMF`）開始，並加上 `--max-daily-loss`。
緊急停止的方法：在 `twq live` 的執行目錄建立一個名為 `KILL` 的檔案，程式會全部平倉後結束。

## 常見錯誤碼

| 碼 | 意義 / 處理 |
|---|---|
| 2017 | 沒有在登入前註冊 `OnReplyMessage` 並回傳 -1（bridge 已處理） |
| 2018 / 2019 | 還沒簽 API 下單聲明書 |
| 321 | 還沒完成 SKCOMVerifyDJ 連線測試 |
| 1011 | 沒有 `ReadCertByID`，或憑證有問題 |
| 600 / 602 / 603 / 1097 / 1045 | 憑證問題 |
| 3030 | 報價連線數超過上限（每個帳號約 2 條） |
| 1040 | 超過 `SetMaxCount` 或 `SetMaxQty` 的下單上限，市場被鎖住，要呼叫 `UnlockOrder` |
| 9996 | API 版本太舊，被強制要求升級 |
| 9997 | 連續登入失敗 5 次，要重新啟動程式 |

## 已知未驗證事項
- 非同步下單時，`SendFutureOrderCLR` 回傳的 `bstrMessage` 是不是 thread id（bridge 會同時嘗試解析 thread id 和 13 碼序號）。如果有問題，可以設 `async_orders = false`。
- `OnNewData` 的成交價格和時間欄位格式（第 11 欄和第 24 欄）。
- 夜盤跨日時 `nDate` 是日曆日還是交易日。
- 啟動時的部位對帳：目前只回報這次 bridge 啟動後自己成交的部位，**啟動前請先到策略王確認沒有殘留部位**。
