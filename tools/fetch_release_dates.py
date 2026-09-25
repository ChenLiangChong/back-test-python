#!/usr/bin/env python3
"""
從 FRED (聖路易聯準銀行) 的公布行事曆抓美國經濟數據歷年公布日期, 產生 event_clip 用的行事曆:

    非農+失業率 (Employment Situation, rid 50)  08:30 ET  big
    CPI         (Consumer Price Index, rid 10)  08:30 ET  big
    PPI         (Producer Price Index, rid 46)  08:30 ET  normal
    JOLTS       (Job Openings and Labor Turnover Survey, rid 192)  10:00 ET  normal

用法:
    python3 tools/fetch_release_dates.py --from 2017 --to 2026 --out data/events/us_macro.csv

時間是美東時間 (tz=ET), twq 會自動換算台灣時間並處理夏令時間.
FRED 頁面: https://fred.stlouisfed.org/releases/calendar?rid=50&y=2019
"""
from __future__ import annotations

import argparse
import datetime as dt
import re
import shutil
import subprocess
import time
import urllib.request

UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0 Safari/537.36"
RELEASES = [
    # rid, name, tier, ET time
    (50, "NFP", "big", "08:30"),
    (10, "CPI", "big", "08:30"),
    (46, "PPI", "normal", "08:30"),
    (192, "JOLTS", "normal", "10:00"),
]
MONTHS = "January|February|March|April|May|June|July|August|September|October|November|December"
DATE_RE = re.compile(rf"(?:Monday|Tuesday|Wednesday|Thursday|Friday|Saturday|Sunday),?\s+({MONTHS})\s+(\d{{1,2}}),\s+(\d{{4}})")
ISO_RE = re.compile(r"\b(20\d\d)-(\d\d)-(\d\d)\b")


def get(url: str) -> str:
    if shutil.which("curl"):
        r = subprocess.run(["curl", "-fsSL", "--max-time", "60", "-A", UA, url], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        if r.returncode != 0:
            raise RuntimeError(r.stderr.decode(errors="replace").strip())
        return r.stdout.decode("utf-8", "replace")
    with urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": UA}), timeout=60) as resp:
        return resp.read().decode("utf-8", "replace")


def release_dates(rid: int, year: int) -> set[dt.date]:
    page = get(f"https://fred.stlouisfed.org/releases/calendar?rid={rid}&y={year}")
    text = re.sub(r"<[^>]+>", " ", page)
    out = set()
    for m, d, y in DATE_RE.findall(text):
        out.add(dt.datetime.strptime(f"{m} {d} {y}", "%B %d %Y").date())
    if not out:  # fall back to ISO dates embedded in links / JSON
        for y, m, d in ISO_RE.findall(page):
            out.add(dt.date(int(y), int(m), int(d)))
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
                raise SystemExit(f"無法連線 FRED ({e}). 請確認網路可連 fred.stlouisfed.org")
            print(f"{name} {y}: {len(ds)} 次")
            rows += [(d, name, tier, et) for d in ds]
            time.sleep(1.0)
    rows.sort()
    with open(a.out, "w") as f:
        f.write("# 美國經濟數據公布日期, 來源 FRED release calendar (fred.stlouisfed.org). 時間為美東時間.\n")
        f.write("datetime,name,tier,tz\n")
        for d, name, tier, et in rows:
            f.write(f"{d.isoformat()} {et},{name},{tier},ET\n")
    print(f"{len(rows)} 筆 → {a.out}")


if __name__ == "__main__":
    main()
