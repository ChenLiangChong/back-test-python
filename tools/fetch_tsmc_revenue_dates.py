#!/usr/bin/env python3
"""
從公開資訊觀測站 (MOPS) 重大訊息抓台積電 (2330) 每月營收報告的公布日期, 產生 event_clip 用的行事曆.

用法:
    python3 tools/fetch_tsmc_revenue_dates.py --from 2017 --to 2026 --out data/events/tsmc_revenue.csv

事件時間固定用 13:30 (使用者的規則; 2026-09-10 逐筆資料在 13:30~13:32 有明顯放量).
MOPS 的申報時間通常晚 10~40 分鐘 (例: 13:51), 寫在 source 欄供核對.
2017~2018 年的 MOPS 申報時間多在 14:30 以後, 當年官網是否也是 13:30 公布「未驗證」.
"""
from __future__ import annotations

import argparse
import html
import re
import subprocess
import time

URL = "https://mopsov.twse.com.tw/mops/web/ajax_t05st01"
FORM = "encodeURIComponent=1&step=1&firstin=1&off=1&TYPEK=all&co_id=2330&year={roc}"
TITLE_RE = re.compile(r"(\d{4})年(\d{1,2})月營收報告")


def announcements(year: int) -> list[tuple[str, str, str]]:
    r = subprocess.run(["curl", "-fsSL", "--max-time", "60", URL, "--data", FORM.format(roc=year - 1911)], capture_output=True)
    if r.returncode != 0:
        raise SystemExit(f"無法連線 MOPS: {r.stderr.decode(errors='replace').strip()}")
    out = []
    for row in re.findall(r"<tr[^>]*>(.*?)</tr>", r.stdout.decode("utf-8", "replace"), re.S):
        c = [html.unescape(re.sub(r"<[^>]+>", "", x)).strip() for x in re.findall(r"<td[^>]*>(.*?)</td>", row, re.S)]
        if len(c) >= 5 and TITLE_RE.search(c[4]):
            y, m, d = c[2].split("/")  # 民國 108/01/10
            out.append((f"{int(y) + 1911}-{m}-{d}", c[3][:5], c[4]))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--from", dest="start", type=int, default=2017)
    ap.add_argument("--to", dest="end", type=int, default=time.localtime().tm_year)
    ap.add_argument("--out", default="data/events/tsmc_revenue.csv")
    a = ap.parse_args()
    rows = []
    for y in range(a.start, a.end + 1):
        got = announcements(y)
        print(f"{y}: {len(got)} 次")
        rows += got
        time.sleep(3.0)
    rows = sorted(set(rows))
    with open(a.out, "w") as f:
        f.write("# 台積電每月營收公布日, 來源: 公開資訊觀測站重大訊息 (co_id=2330). 時間用 13:30 (使用者規則), source 欄是 MOPS 申報時間.\n")
        f.write("datetime,name,tier,tz,source\n")
        for day, t, title in rows:
            f.write(f"{day} 13:30,TSMC_REV,normal,TW,{title} MOPS {t}\n")
    print(f"{len(rows)} 筆 → {a.out}")


if __name__ == "__main__":
    main()
