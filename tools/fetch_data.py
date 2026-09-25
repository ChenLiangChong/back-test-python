#!/usr/bin/env python3
"""
下載回測需要的免費官方資料 (只用 Python 標準庫 + 系統內建的 curl, Mac / Linux / Windows 皆可):

1. 期交所「前30個交易日期貨每筆成交資料」 → data/taifex/Daily_YYYY_MM_DD.csv
   https://www.taifex.com.tw/cht/3/dlFutPrevious30DaysSalesData
2. 證交所「每5秒委託成交統計」(大盤累積成交量/金額) → data/taiex_cumamt.csv, data/taiex_cumvol.csv
   https://www.twse.com.tw/zh/trading/historical/mi-5mins.html

用法:
    python3 tools/fetch_data.py                 # 最近 45 天 (期交所只保留 30 個交易日)
    python3 tools/fetch_data.py --twse-from 2024-01-01   # 大盤成交資料可以抓很多年

已下載的檔案會跳過, 中斷後重跑即可續抓. 證交所有流量限制, 每次請求間隔約 3.5 秒.
"""
from __future__ import annotations

import argparse
import datetime as dt
import io
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.request
import zipfile

UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36"
TAIFEX_URL = "https://www.taifex.com.tw/file/taifex/Dailydownload/DailydownloadCSV/Daily_{y:04d}_{m:02d}_{d:02d}.zip"
TWSE_URLS = [
    "https://www.twse.com.tw/rwd/zh/afterTrading/MI_5MINS?date={ymd}&response=json",
    "https://www.twse.com.tw/exchangeReport/MI_5MINS?response=json&date={ymd}",
]


def http_get(url: str) -> bytes | None:
    """GET with curl when available (uses the OS certificate store), else urllib."""
    if shutil.which("curl"):
        r = subprocess.run(
            ["curl", "-fsSL", "--max-time", "60", "-A", UA, url],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if r.returncode == 0:
            return r.stdout
        if r.returncode == 22:  # HTTP error (404 on non-trading days)
            return None
        raise RuntimeError(f"curl failed ({r.returncode}): {r.stderr.decode(errors='replace').strip()}")
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.read()
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise


def weekdays(start: dt.date, end: dt.date):
    d = start
    while d <= end:
        if d.weekday() < 5:
            yield d
        d += dt.timedelta(days=1)


def fetch_taifex(out_dir: str, days: int) -> int:
    os.makedirs(out_dir, exist_ok=True)
    today = dt.date.today()
    got = 0
    for d in weekdays(today - dt.timedelta(days=days), today):
        name = f"Daily_{d.year:04d}_{d.month:02d}_{d.day:02d}.csv"
        path = os.path.join(out_dir, name)
        if os.path.exists(path) and os.path.getsize(path) > 0:
            got += 1
            continue
        data = http_get(TAIFEX_URL.format(y=d.year, m=d.month, d=d.day))
        if not data or data[:2] != b"PK":
            continue  # holiday, weekend, or older than 30 trading days
        with zipfile.ZipFile(io.BytesIO(data)) as z:
            csvs = [n for n in z.namelist() if n.lower().endswith(".csv")]
            if not csvs:
                continue
            with open(path, "wb") as f:
                f.write(z.read(csvs[0]))
        got += 1
        print(f"  期交所 {d}: {os.path.getsize(path) / 1e6:.1f} MB")
        time.sleep(0.5)
    return got


def parse_mi5mins(raw: bytes, day: dt.date):
    j = json.loads(raw.decode("utf-8"))
    if j.get("stat") not in ("OK", "ok") or not j.get("data"):
        return None
    fields = j.get("fields") or []

    def idx(key):
        for i, f in enumerate(fields):
            if key in f:
                return i
        raise KeyError(key)

    it, iv, ia = idx("時間"), idx("累積成交數量"), idx("累積成交金額")
    rows = []
    for r in j["data"]:
        t = str(r[it]).strip()
        if len(t) < 8 or t[2] != ":":
            continue
        num = lambda s: float(str(s).replace(",", "").strip() or 0)  # noqa: E731
        rows.append((f"{day.isoformat()} {t}", num(r[iv]), num(r[ia])))
    return rows


BLOCK_MARKERS = ("FOR SECURITY REASONS", "安全性考量")


def fetch_twse(out_dir: str, start: dt.date, delay: float = 3.5, max_block_wait: float = 1800) -> int:
    cache = os.path.join(out_dir, "twse_mi5mins")
    os.makedirs(cache, exist_ok=True)
    today = dt.date.today()
    n = 0
    for d in weekdays(start, today):
        ymd = d.strftime("%Y%m%d")
        cpath = os.path.join(cache, f"{ymd}.json")
        if os.path.exists(cpath):
            n += 1
            continue
        raw = None
        waited = 0.0
        while True:
            blocked = False
            for u in TWSE_URLS:
                try:
                    raw = http_get(u.format(ymd=ymd))
                except RuntimeError as e:
                    print(f"  證交所 {d}: {e}")
                    raw, blocked = None, True
                    continue
                if raw and any(m in raw.decode("utf-8", "replace") for m in BLOCK_MARKERS):
                    raw, blocked = None, True
                    continue
                if raw:
                    try:
                        if parse_mi5mins(raw, d) is not None:
                            break
                    except (ValueError, KeyError):
                        pass
                    raw = None
            if raw is not None or not blocked:
                break
            # TWSE's firewall throttles bursts: back off and retry the same day
            if waited >= max_block_wait:
                print(f"  證交所暫時封鎖, 已等 {waited:.0f}s, 先停止 (稍後重跑會從中斷處繼續)")
                break
            pause = min(300.0, 60.0 + waited)  # 60s, 120s, 240s, 300s…
            print(f"  證交所 {d}: 被暫時封鎖, {pause:.0f} 秒後重試…")
            time.sleep(pause)
            waited += pause
        time.sleep(delay)  # TWSE allows roughly 3 requests per 5 seconds
        if raw is None and waited >= max_block_wait:
            break  # still blocked: stop, keep what we have
        if raw is None:
            continue  # holiday
        with open(cpath, "wb") as f:
            f.write(raw)
        n += 1
        print(f"  證交所 {d}: OK")
    # rebuild the two CSV series from the cache
    amt = ["timestamp,value"]
    vol = ["timestamp,value"]
    for fn in sorted(os.listdir(cache)):
        if not fn.endswith(".json"):
            continue
        d = dt.datetime.strptime(fn[:8], "%Y%m%d").date()
        with open(os.path.join(cache, fn), "rb") as f:
            rows = parse_mi5mins(f.read(), d) or []
        for ts, v, a in rows:
            vol.append(f"{ts},{v:.0f}")
            amt.append(f"{ts},{a:.0f}")
    with open(os.path.join(out_dir, "taiex_cumamt.csv"), "w") as f:
        f.write("\n".join(amt) + "\n")
    with open(os.path.join(out_dir, "taiex_cumvol.csv"), "w") as f:
        f.write("\n".join(vol) + "\n")
    return n


def main():
    ap = argparse.ArgumentParser(description="下載期交所逐筆成交 + 證交所每5秒成交統計")
    ap.add_argument("--out", default="data")
    ap.add_argument("--days", type=int, default=45, help="期交所: 往回抓幾個日曆天 (官方只保留 30 個交易日)")
    ap.add_argument("--twse-from", help="證交所: 起始日 YYYY-MM-DD (預設與期交所相同)")
    ap.add_argument("--skip-taifex", action="store_true")
    ap.add_argument("--skip-twse", action="store_true")
    ap.add_argument("--twse-delay", type=float, default=3.5, help="證交所每次請求間隔秒數")
    a = ap.parse_args()
    if not a.skip_taifex:
        print("下載期交所逐筆成交 (前 30 個交易日)…")
        n = fetch_taifex(os.path.join(a.out, "taifex"), a.days)
        print(f"期交所: {n} 天")
        if n == 0:
            print("!! 沒有抓到任何期交所檔案, 請確認網路可以連 www.taifex.com.tw", file=sys.stderr)
    if not a.skip_twse:
        start = dt.date.fromisoformat(a.twse_from) if a.twse_from else dt.date.today() - dt.timedelta(days=a.days + 5)
        print(f"下載證交所每5秒成交統計 (從 {start})…")
        n = fetch_twse(a.out, start, delay=a.twse_delay)
        print(f"證交所: {n} 天 → {a.out}/taiex_cumamt.csv (成交金額), {a.out}/taiex_cumvol.csv (成交量)")


if __name__ == "__main__":
    main()
