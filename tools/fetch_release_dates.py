#!/usr/bin/env python3
"""
從 ALFRED (聖路易聯準銀行 FRED 的歷史版) 的公布行事曆抓美國經濟數據歷年公布日期, 產生 event_clip 用的行事曆:

    非農+失業率 (Employment Situation, rid 50)  08:30 ET  big
    CPI         (Consumer Price Index, rid 10)  08:30 ET  big
    PPI         (Producer Price Index, rid 46)  08:30 ET  normal
    JOLTS       (Job Openings and Labor Turnover Survey, rid 192)  10:00 ET  normal

用法:
    python3 tools/fetch_release_dates.py --from 2017 --to 2026 --out data/events/us_macro.csv

時間是美東時間 (tz=ET), twq 會自動換算台灣時間並處理夏令時間.
ALFRED 頁面: https://alfred.stlouisfed.org/releases/calendar?rid=50&y=2019
(FRED 的行事曆只有 2025 年以後; ALFRED 有歷史. 標 "Updated N/A" 的是資料更正或季節因子修訂, 不是正式公布, 會略過.)
"""
from __future__ import annotations

import argparse
import datetime as dt
import re
import shutil
import subprocess
import time
import urllib.request

RELEASES = [
    # rid, name, tier, ET time
    (50, "NFP", "big", "08:30"),
    (10, "CPI", "big", "08:30"),
    (46, "PPI", "normal", "08:30"),
    (192, "JOLTS", "normal", "10:00"),
]
MONTHS = "January|February|March|April|May|June|July|August|September|October|November|December"
# "Friday May 08, 2020 Updated 7:30 am" (已公布) / "Tuesday September 29, 2026 9:00 am" (排定) / "... Updated N/A" (更正, 略過)
DATE_RE = re.compile(
    rf"(?:Monday|Tuesday|Wednesday|Thursday|Friday|Saturday|Sunday),?\s+({MONTHS})\s+(\d{{1,2}}),\s+(\d{{4}})\s+(?:Updated\s+)?(N/A|\d{{1,2}}:\d\d\s*[ap]m)"
)


def get(url: str) -> str:
    if shutil.which("curl"):
        # 不要偽裝瀏覽器 UA: FRED 會擋「curl 的 TLS 指紋 + Chrome UA」的組合 (連線被重置或逾時)
        r = subprocess.run(["curl", "-fsSL", "--max-time", "60", url], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        if r.returncode != 0:
            raise RuntimeError(r.stderr.decode(errors="replace").strip())
        return r.stdout.decode("utf-8", "replace")
    with urllib.request.urlopen(url, timeout=60) as resp:
        return resp.read().decode("utf-8", "replace")


def release_dates(rid: int, year: int) -> set[dt.date]:
    page = get(f"https://alfred.stlouisfed.org/releases/calendar?rid={rid}&y={year}")
    text = re.sub(r"<[^>]+>", " ", page)
    out = set()
    for m, d, y, t in DATE_RE.findall(text):
        if t != "N/A":
            out.add(dt.datetime.strptime(f"{m} {d} {y}", "%B %d %Y").date())
    if not out:
        raise RuntimeError(f"rid={rid} {year} 解析不到任何日期, ALFRED 頁面格式可能改了, 請修 DATE_RE")
    return {d for d in out if d.year == year}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--from", dest="start", type=int, default=2017)
    ap.add_argument("--to", dest="end", type=int, default=dt.date.today().year)
    ap.add_argument("--out", default="data/events/us_macro.csv")
    a = ap.parse_args()
    rows = []
    for rid, name, tier, et in RELEASES:
        for y in range(a.start, a.end + 1):
            try:
                ds = release_dates(rid, y)
            except RuntimeError as e:
                raise SystemExit(f"抓取失敗: {e}\n請確認網路可連 alfred.stlouisfed.org")
            print(f"{name} {y}: {len(ds)} 次")
            rows += [(d, name, tier, et) for d in ds]
            time.sleep(1.0)
    # CPI/PPI 每年 2 月會多一筆「季節因子修訂」, 比正式公布早 2~5 天 (ALFRED 同樣標 7:30 am, 分不出來).
    # 同一項目 7 天內出現兩筆 → 只留後面那筆. 政府關門後的補發間隔都 ≥ 2 週, 不受影響.
    rows = sorted(set(rows), key=lambda r: (r[1], r[0]))
    rows = [r for r, nxt in zip(rows, rows[1:] + [None]) if not (nxt and nxt[1] == r[1] and (nxt[0] - r[0]).days <= 7)]
    rows.sort()
    with open(a.out, "w") as f:
        f.write("# 美國經濟數據公布日期, 來源 ALFRED release calendar (alfred.stlouisfed.org). 時間為美東時間.\n")
        f.write("datetime,name,tier,tz\n")
        for d, name, tier, et in rows:
            f.write(f"{d.isoformat()} {et},{name},{tier},ET\n")
    print(f"{len(rows)} 筆 → {a.out}")


if __name__ == "__main__":
    main()
