#!/usr/bin/env python3
"""
下載網友整理的免費台指期 1 分 K 歷史資料 (CrazyIndicator, Google Drive 公開連結),
解壓縮並合併成 twq 用的二進位檔:

    data/txf_2011_2023_1m.bin   2011-01-03 ~ 2023-12-29 (日盤 + 2017 起夜盤), 約 231 萬根
    data/txf_1998_2023_1m.bin   1998-07-22 ~ 2023-12-29, 約 318 萬根

來源: https://crazyindicator.pixnet.net/blog/posts/12222323760
格式: Date,Time,Open,High,Low,Close,Volume; Time 是 K 棒「結束」時間 (08:46 = 08:45~08:46),
      所以合併時用 --shift-sec -60 轉成 twq 的「開始」時間.
未修正換月價差 (近月連續). 另有修正換月價差版 (_Fix_Gap, 2001 ~ 2021/07) 一併下載備用.

需要: curl, 以及 7z 解壓工具之一: `pip3 install py7zr` / macOS 內建 tar (bsdtar) / 7z
用法: python3 tools/fetch_community_data.py   (先 cargo build --release 才會自動合併)
"""
from __future__ import annotations

import glob
import os
import shutil
import subprocess
import sys

FILES = {
    "1xB1bvwBDtaoUEAobmcRUTHNcyOZodTz0": "TXF 1998-07-22 ~ 2000-12-31",
    "1762OrBEo7q5B6YgM2DqXKY0J2ykIxBsw": "TXF 2001 ~ 2010",
    "1lsam29dX2n8oPOP25SfyZIgeMJAGKpeK": "TXF 2011 ~ 2020",
    "1VOqnu11Tarn1IVZrO6Y6sdX7_YwG1ZEP": "TXF 2021 ~ 2023",
    "1WYPShnOqwb8lmE5FLKiW8rUKMDuhwBw0": "TXF 2001 ~ 2021/07 修正換月價差版",
}
URL = "https://drive.usercontent.google.com/download?id={id}&export=download&confirm=t"
UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0 Safari/537.36"
SEVENZ_MAGIC = b"7z\xbc\xaf'\x1c"


def extract(archive: str, out_dir: str) -> None:
    try:
        import py7zr  # type: ignore

        with py7zr.SevenZipFile(archive, "r") as z:
            z.extractall(path=out_dir)
        return
    except ImportError:
        pass
    for cmd in (["tar", "-xf", archive, "-C", out_dir], ["7z", "x", "-y", f"-o{out_dir}", archive]):
        if shutil.which(cmd[0]) and subprocess.run(cmd, capture_output=True).returncode == 0:
            return
    raise SystemExit(f"無法解壓 {archive}: 請先 `pip3 install py7zr` 或安裝 7z")


def main() -> None:
    out = os.path.join("data", "community")
    os.makedirs(out, exist_ok=True)
    for fid, label in FILES.items():
        arc = os.path.join(out, f"{fid}.7z")
        if not (os.path.exists(arc) and open(arc, "rb").read(6) == SEVENZ_MAGIC):
            print(f"下載 {label} …")
            r = subprocess.run(["curl", "-fsSL", "--max-time", "900", "-A", UA, "-o", arc, URL.format(id=fid)])
            if r.returncode != 0 or open(arc, "rb").read(6) != SEVENZ_MAGIC:
                print(f"  !! {label} 下載失敗 (Google Drive 可能要求登入, 請用瀏覽器手動下載)", file=sys.stderr)
                continue
        extract(arc, out)
        print(f"  {label}: OK")
    twq = os.path.join("target", "release", "twq")
    if not os.path.exists(twq):
        print("\n尚未編譯 twq, 請先 cargo build --release 再執行以下合併指令:")
    merges = [
        ("data/txf_2011_2023_1m.bin", ["TXF2011*.csv", "TXF2021*.csv"]),
        ("data/txf_1998_2023_1m.bin", ["TXF1998*.csv", "TXF2001*~*.csv", "TXF2011*.csv", "TXF2021*.csv"]),
    ]
    for dst, pats in merges:
        args = [os.path.join(out, p) for p in pats]
        cmd = [twq, "data", "merge", *args, "--shift-sec", "-60", "--out", dst]
        if os.path.exists(twq):
            subprocess.run(cmd, check=True)
        else:
            print("  " + " ".join(f'"{a}"' if "*" in a else a for a in cmd))
    missing = [p for p in ("TXF2011*.csv", "TXF2021*.csv") if not glob.glob(os.path.join(out, p))]
    if missing:
        print(f"!! 缺少 {missing}", file=sys.stderr)


if __name__ == "__main__":
    main()
