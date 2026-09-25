"""
群益 SKCOM ↔ twq 橋接程式 (Capital Securities SKCOM bridge for the Rust engine)

    twq (Rust 引擎)  <== TCP 127.0.0.1:9101, JSON lines ==>  capital_bridge.py  <== COM ==>  SKCOM.dll

* 報價: SKQuoteLib_RequestTicks → OnNotifyTicksLONG → {"t":"tick",...}
* 下單: {"t":"order",...} → SKOrderLib.SendFutureOrderCLR / SendStockOrder
* 回報: SKReplyLib.OnNewData → {"t":"ack"|"fill"|"cancelled"|"rejected",...}

The bridge is deliberately thin: no strategy, no risk logic (the Rust engine owns those).
It owns the Windows-only parts: one STA thread with a message pump that holds every
SKCOM object, the login state machine, reconnects and keep-alive.

Latency: commands wake the COM thread through a Win32 event (MsgWaitForMultipleObjects),
so there is no polling delay. Python adds tens of microseconds per message, which is
small next to the broker round trip (milliseconds). A C# or Rust-native gateway can
replace this file later without touching the engine (same JSON protocol).

!!! Reference implementation. It follows the SKCOM 2.13.5x manual and community SDKs,
!!! but could not be run against a real Capital account while it was written. Test it
!!! step by step in the 群益 test environment (test_env = true) and with
!!! allow_orders = false before letting it send real orders.

Usage (Windows, 64-bit Python matching the registered SKCOM.dll bitness):
    pip install comtypes pywin32
    python capital_bridge.py --config config.ini

Self-test without SKCOM (any OS): a fake broker with random-walk ticks and instant fills
    python capital_bridge.py --fake
"""
from __future__ import annotations

import argparse
import calendar
import configparser
import json
import os
import queue
import random
import re
import socket
import sys
import threading
import time
from dataclasses import dataclass, field

VERSION = "0.1.0"


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


# --------------------------------------------------------------------------- TCP link


class EngineLink:
    """Single-client TCP server. Commands from the engine go into `self.commands`;
    `wake()` is called after each command so the COM thread reacts immediately."""

    def __init__(self, host: str, port: int, wake):
        self.commands: "queue.Queue[dict]" = queue.Queue()
        self._wake = wake
        self._sock: socket.socket | None = None
        self._lock = threading.Lock()
        self._srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._srv.bind((host, port))
        self._srv.listen(1)
        threading.Thread(target=self._accept_loop, daemon=True).start()
        log(f"listening on {host}:{port} — waiting for `twq live --bridge {host}:{port}`")

    @property
    def connected(self) -> bool:
        return self._sock is not None

    def _accept_loop(self):
        while True:
            conn, addr = self._srv.accept()
            conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            with self._lock:
                if self._sock is not None:
                    try:
                        self._sock.close()
                    except OSError:
                        pass
                self._sock = conn
            log(f"engine connected from {addr}")
            self.commands.put({"t": "_connected"})
            self._wake()
            threading.Thread(target=self._read_loop, args=(conn,), daemon=True).start()

    def _read_loop(self, conn: socket.socket):
        buf = b""
        while True:
            try:
                chunk = conn.recv(65536)
            except OSError:
                chunk = b""
            if not chunk:
                break
            buf += chunk
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                if line.strip():
                    try:
                        self.commands.put(json.loads(line))
                    except ValueError as e:
                        log(f"bad command from engine: {e}: {line[:200]!r}")
                    self._wake()
        with self._lock:
            if self._sock is conn:
                self._sock = None
        log("engine disconnected")
        self.commands.put({"t": "_disconnected"})
        self._wake()

    def send(self, msg: dict):
        data = (json.dumps(msg, separators=(",", ":"), ensure_ascii=False) + "\n").encode("utf-8")
        with self._lock:
            s = self._sock
            if s is None:
                return
            try:
                s.sendall(data)
            except OSError:
                self._sock = None


# --------------------------------------------------------------------------- helpers


def to_engine_ts(date_yyyymmdd: int, hms: int, micros: int) -> int:
    """SKCOM tick time → twq timestamp (µs since 1970, exchange-local wall clock)."""
    y, mo, d = date_yyyymmdd // 10000, (date_yyyymmdd // 100) % 100, date_yyyymmdd % 100
    hh, mm, ss = hms // 10000, (hms // 100) % 100, hms % 100
    return calendar.timegm((y, mo, d, hh, mm, ss, 0, 0, 0)) * 1_000_000 + int(micros)


def fmt_price(px: float) -> str:
    s = f"{px:.4f}".rstrip("0").rstrip(".")
    return s or "0"


SEQ_RE = re.compile(r"\d{13}")


@dataclass
class OrderState:
    cid: int
    sym: str
    side: str
    qty: int
    seq_no: str = ""
    filled: int = 0
    done: bool = False


@dataclass
class Config:
    user_id: str = ""
    password: str = ""
    dll_path: str = r"C:\SKCOM\x64\SKCOM.dll"
    log_path: str = r"C:\SKCOM\logs"
    futures_account: str = ""
    stock_account: str = ""
    test_env: bool = False
    allow_orders: bool = False
    async_orders: bool = True
    forward_trial_ticks: bool = False
    listen_host: str = "127.0.0.1"
    listen_port: int = 9101
    max_orders_per_sec: int = 5
    extra: dict = field(default_factory=dict)

    @staticmethod
    def load(path: str | None) -> "Config":
        c = Config()
        if path:
            cp = configparser.ConfigParser()
            if not cp.read(path, encoding="utf-8"):
                raise SystemExit(f"cannot read config {path}")
            s = cp["capital"] if cp.has_section("capital") else {}
            b = cp["bridge"] if cp.has_section("bridge") else {}
            c.user_id = s.get("user_id", "").strip().upper()
            c.password = s.get("password", "").strip()
            c.dll_path = s.get("dll_path", c.dll_path)
            c.log_path = s.get("log_path", c.log_path)
            c.futures_account = s.get("futures_account", "").strip()
            c.stock_account = s.get("stock_account", "").strip()
            c.test_env = s.get("test_env", "false").lower() == "true"
            c.allow_orders = b.get("allow_orders", "false").lower() == "true"
            c.async_orders = b.get("async_orders", "true").lower() == "true"
            c.forward_trial_ticks = b.get("forward_trial_ticks", "false").lower() == "true"
            c.listen_host = b.get("listen_host", c.listen_host)
            c.listen_port = int(b.get("listen_port", c.listen_port))
            c.max_orders_per_sec = int(b.get("max_orders_per_sec", c.max_orders_per_sec))
        # never keep the password in the file if you can avoid it
        c.password = os.environ.get("CAPITAL_PASSWORD", c.password)
        c.user_id = os.environ.get("CAPITAL_USER_ID", c.user_id).upper()
        return c


# --------------------------------------------------------------------------- brokers


class Bridge:
    """Protocol logic shared by the real and the fake broker."""

    def __init__(self, cfg: Config, link_factory):
        self.cfg = cfg
        self.link: EngineLink = link_factory(self.wake)
        self.orders: dict[int, OrderState] = {}
        self.by_seq: dict[str, int] = {}
        self.position: dict[str, int] = {}
        self.sent_times: list[float] = []

    # overridden
    def wake(self):
        pass

    def subscribe(self, sym: str):
        raise NotImplementedError

    def place(self, o: OrderState, cmd: dict):
        raise NotImplementedError

    def cancel(self, o: OrderState):
        raise NotImplementedError

    # ------------------------------------------------------------ engine commands

    def handle_command(self, cmd: dict):
        t = cmd.get("t")
        if t == "_connected":
            self.link.send({"t": "hello", "bridge": self.name(), "version": VERSION, "simulated": self.simulated()})
        elif t == "_disconnected":
            pass
        elif t == "subscribe":
            self.subscribe(cmd["sym"])
        elif t == "order":
            self.on_order(cmd)
        elif t == "cancel":
            o = self.orders.get(int(cmd["cid"]))
            if o is None or o.done:
                return
            self.cancel(o)
        elif t == "query_position":
            sym = cmd.get("sym", "")
            # NOTE: reports positions opened through this bridge session only. Check
            # 策略王 / GetOpenInterestGW for positions opened elsewhere before starting.
            self.link.send({"t": "position", "sym": sym, "qty": self.position.get(sym, 0), "avg": 0.0})
        elif t == "ping":
            self.link.send({"t": "pong", "id": cmd.get("id", 0)})
        elif t == "sync_ack":
            pass
        else:
            self.link.send({"t": "error", "msg": f"unknown command {t}"})

    def on_order(self, cmd: dict):
        cid = int(cmd["cid"])
        o = OrderState(cid=cid, sym=cmd["sym"], side=cmd["side"], qty=int(cmd["qty"]))
        if not self.cfg.allow_orders:
            self.link.send({"t": "rejected", "cid": cid, "reason": "bridge allow_orders=false (dry run) — order NOT sent"})
            log(f"[dry-run] would send {cmd}")
            return
        now = time.monotonic()
        self.sent_times = [t for t in self.sent_times if now - t < 1.0]
        if len(self.sent_times) >= self.cfg.max_orders_per_sec:
            self.link.send({"t": "rejected", "cid": cid, "reason": "bridge rate limit"})
            return
        self.sent_times.append(now)
        self.orders[cid] = o
        try:
            self.place(o, cmd)
        except Exception as e:  # noqa: BLE001 — report every failure back to the engine
            o.done = True
            self.link.send({"t": "rejected", "cid": cid, "reason": f"send failed: {e}"})

    # ------------------------------------------------------------ broker callbacks

    def on_ack(self, o: OrderState, seq_no: str):
        if seq_no:
            o.seq_no = seq_no
            self.by_seq[seq_no] = o.cid
        self.link.send({"t": "ack", "cid": o.cid, "oid": seq_no})

    def on_fill(self, o: OrderState, px: float, qty: int, ts: int):
        o.filled += qty
        if o.filled >= o.qty:
            o.done = True
        self.position[o.sym] = self.position.get(o.sym, 0) + (qty if o.side == "B" else -qty)
        self.link.send({"t": "fill", "cid": o.cid, "px": px, "qty": qty, "ts": ts})

    def on_cancelled(self, o: OrderState):
        if not o.done:
            o.done = True
            self.link.send({"t": "cancelled", "cid": o.cid})

    def on_rejected(self, o: OrderState, reason: str):
        if not o.done:
            o.done = True
            self.link.send({"t": "rejected", "cid": o.cid, "reason": reason})

    def name(self) -> str:
        return "capital-skcom"

    def simulated(self) -> bool:
        return self.cfg.test_env

    def run(self):
        raise NotImplementedError


class SkcomBroker(Bridge):
    """Real 群益 SKCOM implementation (Windows only)."""

    def __init__(self, cfg: Config):
        import pythoncom  # noqa: F401  (pywin32)
        import win32event

        self._ev = win32event.CreateEvent(None, 0, 0, None)
        super().__init__(cfg, lambda wake: EngineLink(cfg.listen_host, cfg.listen_port, wake))
        self.subs: dict[str, int] = {}  # sym -> page
        self.pending_subs: list[str] = []
        self.idx: dict[tuple[int, int], tuple[str, int]] = {}  # (market, stockidx) -> (sym, decimals)
        self.quote_ready = False
        self.reply_ready = False
        self.accounts: list[tuple[str, str]] = []  # (type, full account)
        self.async_threads: dict[int, int] = {}  # nThreadID -> cid
        self.next_page = 0
        self.last_keepalive = 0.0
        self.reconnect_at = 0.0

    def wake(self):
        import win32event

        win32event.SetEvent(self._ev)

    # ------------------------------------------------------------ COM setup

    def load_com(self):
        import comtypes.client

        comtypes.client.GetModule(self.cfg.dll_path)
        import comtypes.gen.SKCOMLib as sk  # type: ignore

        self.sk = sk
        self.center = comtypes.client.CreateObject(sk.SKCenterLib, interface=sk.ISKCenterLib)
        self.reply = comtypes.client.CreateObject(sk.SKReplyLib, interface=sk.ISKReplyLib)
        self.order = comtypes.client.CreateObject(sk.SKOrderLib, interface=sk.ISKOrderLib)
        self.quote = comtypes.client.CreateObject(sk.SKQuoteLib, interface=sk.ISKQuoteLib)
        # Event sinks must exist BEFORE login (OnReplyMessage has to answer -1, else 2017).
        self._sinks = [
            comtypes.client.GetEvents(self.reply, ReplySink(self)),
            comtypes.client.GetEvents(self.order, OrderSink(self)),
            comtypes.client.GetEvents(self.quote, QuoteSink(self)),
        ]

    def msg(self, code: int) -> str:
        try:
            return str(self.center.SKCenterLib_GetReturnCodeMessage(int(code)))
        except Exception:  # noqa: BLE001
            return ""

    def check(self, what: str, code, ok=(0,)):
        code = int(code[-1] if isinstance(code, tuple) else code)
        if code not in ok:
            raise RuntimeError(f"{what} failed: {code} {self.msg(code)}")
        log(f"{what}: {code} {self.msg(code)}")
        return code

    def pump(self, seconds: float):
        import pythoncom

        end = time.time() + seconds
        while time.time() < end:
            pythoncom.PumpWaitingMessages()
            time.sleep(0.01)

    def login(self):
        c = self.cfg
        if not c.user_id or not c.password:
            raise SystemExit("user_id / password missing (config.ini or CAPITAL_USER_ID / CAPITAL_PASSWORD)")
        os.makedirs(c.log_path, exist_ok=True)
        self.check("SKCenterLib_SetLogPath", self.center.SKCenterLib_SetLogPath(c.log_path))
        if c.test_env:
            # bit 1 = test environment (see manual, SKCenterLib_SetAuthority)
            self.check("SKCenterLib_SetAuthority(test)", self.center.SKCenterLib_SetAuthority(2))
        # 2003 = already logged in
        self.check("SKCenterLib_Login", self.center.SKCenterLib_Login(c.user_id, c.password), ok=(0, 2003))
        self.check("SKReplyLib_ConnectByID", self.reply.SKReplyLib_ConnectByID(c.user_id))
        deadline = time.time() + 20
        while not self.reply_ready and time.time() < deadline:
            self.pump(0.1)
        if not self.reply_ready:
            log("WARNING: order-report channel not complete yet (OnComplete not received)")
        self.check("SKOrderLib_Initialize", self.order.SKOrderLib_Initialize())
        self.order.GetUserAccount()
        self.pump(1.0)
        self.check("ReadCertByID", self.order.ReadCertByID(c.user_id))
        if not c.futures_account:
            c.futures_account = next((a for t, a in self.accounts if t.upper() == "TF"), "")
        if not c.stock_account:
            c.stock_account = next((a for t, a in self.accounts if t.upper() == "TS"), "")
        log(f"accounts: futures={c.futures_account or '-'} stock={c.stock_account or '-'}")
        self.enter_monitor()

    def enter_monitor(self):
        self.quote_ready = False
        self.check("SKQuoteLib_EnterMonitorLONG", self.quote.SKQuoteLib_EnterMonitorLONG())

    # ------------------------------------------------------------ quotes

    def subscribe(self, sym: str):
        if sym not in self.pending_subs:
            self.pending_subs.append(sym)
        if self.quote_ready:
            self._do_subscribe(sym)

    def _do_subscribe(self, sym: str):
        page = self.subs.get(sym)
        if page is None:
            page = self.next_page
            self.next_page += 1
            self.subs[sym] = page
        ret = self.quote.SKQuoteLib_RequestTicks(page, sym)
        code = int(ret[-1] if isinstance(ret, tuple) else ret)
        log(f"RequestTicks({page}, {sym}) -> {code} {self.msg(code)}")
        # learn market / index / decimals so ticks can be mapped back to the symbol
        st = self.sk.SKSTOCKLONG()
        r = self.quote.SKQuoteLib_GetStockByNoLONG(sym, st)
        if isinstance(r, tuple):
            st = next((x for x in r if hasattr(x, "nStockIdx")), st)
        dec = getattr(st, "sDecimal", None)
        dec = 2 if dec is None else int(dec)
        market = int(getattr(st, "bstrMarketNo", 2) or 2)
        self.idx[(market, int(st.nStockIdx))] = (sym, dec)
        log(f"{sym}: market={market} index={st.nStockIdx} decimals={dec}")

    def on_connection(self, kind: int, code: int):
        log(f"quote OnConnection kind={kind} code={code}")
        if kind == 3003:  # stock data ready — subscribe only after this
            self.quote_ready = True
            for s in self.pending_subs:
                self._do_subscribe(s)
            self.link.send({"t": "info", "msg": "quote ready"})
        elif kind in (3002, 3021, 3022, 3033):
            self.quote_ready = False
            self.reconnect_at = time.time() + 5  # never reconnect inside the event
            self.link.send({"t": "error", "msg": f"quote disconnected ({kind}); reconnecting in 5s"})

    def on_tick(self, market, sidx, ptr, date, hms, micros, bid, ask, close, qty, simulate, history=False):
        if history:
            return  # today's backfill — never feed old ticks to a live engine as if new
        if simulate and not self.cfg.forward_trial_ticks:
            return
        key = (int(market), int(sidx))
        sym, dec = self.idx.get(key, (None, 2))
        if sym is None:
            return
        div = 10.0 ** dec
        self.link.send({
            "t": "tick", "sym": sym, "ts": to_engine_ts(int(date), int(hms), int(micros)),
            "px": close / div, "qty": int(qty), "bid": bid / div, "ask": ask / div, "sim": bool(simulate),
        })

    # ------------------------------------------------------------ orders

    def place(self, o: OrderState, cmd: dict):
        side = 0 if o.side == "B" else 1
        tif = {"ROD": 0, "IOC": 1, "FOK": 2}.get(cmd.get("tif", "ROD"), 0)
        px = cmd.get("px")
        if o.sym.isdigit():
            ret = self._place_stock(o, side, tif, px)
        else:
            f = self.sk.FUTUREORDER()
            f.bstrFullAccount = self.cfg.futures_account
            f.bstrStockNo = o.sym
            f.sBuySell = side
            f.sTradeType = tif
            f.sDayTrade = 1 if cmd.get("day_trade") else 0
            f.sNewClose = {"new": 0, "close": 1}.get(cmd.get("oc", "auto"), 2)
            # "M" 市價 / "P" 範圍市價 are only valid with IOC/FOK
            f.bstrPrice = fmt_price(px) if px is not None else (cmd.get("mkt") or "P")
            if px is None and tif == 0:
                f.sTradeType = 1
            f.nQty = o.qty
            f.sReserved = 0
            ret = self.order.SendFutureOrderCLR(self.cfg.user_id, self.cfg.async_orders, f)
        self._handle_send_result(o, ret)

    def _place_stock(self, o: OrderState, side: int, tif: int, px):
        s = self.sk.STOCKORDER()
        s.bstrFullAccount = self.cfg.stock_account
        s.bstrStockNo = o.sym
        s.sPrime = 0
        s.sPeriod = 0  # regular session, board lots
        s.sFlag = 0  # cash
        s.sBuySell = side
        s.nTradeType = tif
        if px is None:
            s.bstrPrice = "0"
            s.nSpecialTradeType = 1  # market
        else:
            s.bstrPrice = fmt_price(px)
            s.nSpecialTradeType = 2  # limit
        if o.qty % 1000:
            raise ValueError("stock orders must be whole board lots (multiples of 1000 shares)")
        s.nQty = o.qty // 1000
        return self.order.SendStockOrder(self.cfg.user_id, self.cfg.async_orders, s)

    def _handle_send_result(self, o: OrderState, ret):
        msg, code = "", 0
        if isinstance(ret, tuple):
            if len(ret) >= 2 and isinstance(ret[1], int):
                msg, code = str(ret[0]), int(ret[1])
            elif len(ret) >= 2 and isinstance(ret[0], int):
                code, msg = int(ret[0]), str(ret[1])
        elif isinstance(ret, int):
            code = ret
        if code != 0:
            self.on_rejected(o, f"{code} {self.msg(code)} {msg}".strip())
            return
        if self.cfg.async_orders:
            # async: bstrMessage carries the thread id; the result arrives in OnAsyncOrder
            try:
                self.async_threads[int(msg.strip())] = o.cid
            except ValueError:
                m = SEQ_RE.search(msg)
                if m:
                    self.on_ack(o, m.group(0))
        else:
            m = SEQ_RE.search(msg)
            self.on_ack(o, m.group(0) if m else msg)

    def on_async_order(self, thread_id: int, code: int, message: str):
        cid = self.async_threads.pop(int(thread_id), None)
        o = self.orders.get(cid) if cid is not None else None
        if o is None:
            return
        if code != 0:
            self.on_rejected(o, f"{code} {self.msg(code)} {message}")
            return
        m = SEQ_RE.search(message or "")
        self.on_ack(o, m.group(0) if m else "")

    def cancel(self, o: OrderState):
        if not o.seq_no:
            self.link.send({"t": "error", "msg": f"cancel {o.cid}: no broker sequence number yet"})
            return
        acct = self.cfg.stock_account if o.sym.isdigit() else self.cfg.futures_account
        ret = self.order.CancelOrderBySeqNo(self.cfg.user_id, self.cfg.async_orders, acct, o.seq_no)
        log(f"CancelOrderBySeqNo({o.seq_no}) -> {ret}")

    def on_new_data(self, raw: str):
        """OnNewData (manual 4-3-g, 0-based): 1 MarketType, 2 Type (N order / C cancel /
        U reduce / P reprice / D deal), 3 OrderErr, 11 Price, 20 Qty, 23 Date, 24 Time,
        44 ErrorMsg, 47 SeqNo (13 digits)."""
        f = raw.split(",")
        g = lambda i: f[i].strip() if i < len(f) else ""  # noqa: E731
        seq = g(47)
        cid = self.by_seq.get(seq)
        o = self.orders.get(cid) if cid is not None else None
        if o is None:
            return  # an order not placed through this bridge (e.g. manual)
        typ, err = g(2), g(3)
        if err and err not in ("N", "0", ""):
            self.on_rejected(o, g(44) or f"OrderErr={err}")
            return
        if typ == "D":
            try:
                px = float(g(11))
                qty = int(float(g(20)))
                d = int(g(23) or time.strftime("%Y%m%d"))
                t = g(24).replace(":", "")
                hms = int(t[:6] or 0)
                us = int((t[6:12] + "000000")[:6]) if len(t) > 6 else 0
                if o.sym.isdigit():
                    qty *= 1000
                self.on_fill(o, px, qty, to_engine_ts(d, hms, us))
            except ValueError as e:
                self.link.send({"t": "error", "msg": f"cannot parse fill: {e}: {raw}"})
        elif typ == "C":
            self.on_cancelled(o)
        elif typ == "N" and not o.seq_no:
            self.on_ack(o, seq)

    # ------------------------------------------------------------ main loop

    def run(self):
        import pythoncom
        import win32event

        pythoncom.CoInitialize()  # STA: every SKCOM object lives on this thread
        self.load_com()
        self.login()
        log("ready")
        while True:
            # wake on COM messages OR engine commands, whichever comes first
            win32event.MsgWaitForMultipleObjects([self._ev], False, 200, win32event.QS_ALLINPUT)
            pythoncom.PumpWaitingMessages()
            while True:
                try:
                    cmd = self.link.commands.get_nowait()
                except queue.Empty:
                    break
                self.handle_command(cmd)
            now = time.time()
            if now - self.last_keepalive > 15:  # keep firewalls from dropping the link
                self.last_keepalive = now
                try:
                    self.quote.SKQuoteLib_RequestServerTime()
                except Exception as e:  # noqa: BLE001
                    log(f"keepalive failed: {e}")
            if self.reconnect_at and now >= self.reconnect_at:
                self.reconnect_at = 0.0
                try:
                    self.enter_monitor()
                except Exception as e:  # noqa: BLE001
                    log(f"reconnect failed: {e}; retry in 5s")
                    self.reconnect_at = time.time() + 5


class ReplySink:
    def __init__(self, b: SkcomBroker):
        self.b = b

    def OnReplyMessage(self, bstrUserID, bstrMessages):
        log(f"announcement: {bstrMessages}")
        return -1  # sConfirmCode = -1 is mandatory (else error 2017)

    def OnComplete(self, bstrUserID):
        self.b.reply_ready = True
        log("order reports: backfill complete")

    def OnSolaceReplyConnection(self, bstrUserID, nErrorCode):
        log(f"order reports connected: {nErrorCode}")

    def OnSolaceReplyDisconnect(self, bstrUserID, nErrorCode):
        log(f"order reports disconnected: {nErrorCode}")
        self.b.link.send({"t": "error", "msg": f"order report channel disconnected ({nErrorCode})"})

    def OnNewData(self, bstrUserID, bstrData):
        self.b.on_new_data(str(bstrData))


class OrderSink:
    def __init__(self, b: SkcomBroker):
        self.b = b

    def OnAccount(self, bstrLogInID, bstrAccountData):
        f = str(bstrAccountData).split(",")
        if len(f) > 3:
            self.b.accounts.append((f[0].strip(), f[1].strip() + f[3].strip()))

    def OnAsyncOrder(self, nThreadID, nCode, bstrMessage):
        self.b.on_async_order(int(nThreadID), int(nCode), str(bstrMessage))


class QuoteSink:
    def __init__(self, b: SkcomBroker):
        self.b = b

    def OnConnection(self, nKind, nCode):
        self.b.on_connection(int(nKind), int(nCode))

    def OnNotifyTicksLONG(self, sMarketNo, nStockidx, nPtr, lDate, lTimehms, lTimemillismicros, nBid, nAsk, nClose, nQty, nSimulate):
        self.b.on_tick(sMarketNo, nStockidx, nPtr, lDate, lTimehms, lTimemillismicros, nBid, nAsk, nClose, nQty, nSimulate)

    def OnNotifyHistoryTicksLONG(self, sMarketNo, nStockidx, nPtr, lDate, lTimehms, lTimemillismicros, nBid, nAsk, nClose, nQty, nSimulate):
        self.b.on_tick(sMarketNo, nStockidx, nPtr, lDate, lTimehms, lTimemillismicros, nBid, nAsk, nClose, nQty, nSimulate, history=True)

    def OnNotifyServerTime(self, sHour, sMinute, sSecond, nTotal):
        pass


# --------------------------------------------------------------------------- fake broker


class FakeBroker(Bridge):
    """Random-walk ticks + immediate fills. Exercises the whole protocol on any OS."""

    def __init__(self, cfg: Config, ticks_per_sec: float = 50.0, seconds: float = 0.0):
        self._cv = threading.Event()
        super().__init__(cfg, lambda wake: EngineLink(cfg.listen_host, cfg.listen_port, wake))
        self.cfg.allow_orders = True
        self.syms: list[str] = []
        self.px = 23000.0
        self.dt = 1.0 / ticks_per_sec
        self.seconds = seconds
        self.clock = calendar.timegm((2026, 9, 25, 9, 0, 0, 0, 0, 0)) * 1_000_000
        self.resting: list[tuple[OrderState, float | None]] = []
        self.seq = 0

    def name(self) -> str:
        return "capital-bridge-fake"

    def simulated(self) -> bool:
        return True

    def wake(self):
        self._cv.set()

    def subscribe(self, sym: str):
        if sym not in self.syms:
            self.syms.append(sym)
            log(f"[fake] subscribed {sym}")

    def place(self, o: OrderState, cmd: dict):
        self.seq += 1
        self.on_ack(o, f"{self.seq:013d}")
        px = cmd.get("px")
        self.resting.append((o, px))
        if cmd.get("tif") in ("IOC", "FOK"):
            self._match(ioc=True)

    def cancel(self, o: OrderState):
        self.resting = [(r, p) for r, p in self.resting if r.cid != o.cid]
        self.on_cancelled(o)

    def _match(self, ioc=False):
        keep = []
        for o, px in self.resting:
            marketable = px is None or (o.side == "B" and px >= self.px) or (o.side == "S" and px <= self.px)
            if marketable:
                fill_px = self.px + (1 if o.side == "B" else -1) if px is None else px
                self.on_fill(o, fill_px, o.qty - o.filled, self.clock)
            elif ioc:
                self.on_cancelled(o)
            else:
                keep.append((o, px))
        self.resting = keep

    def run(self):
        log("[fake] ready (no SKCOM) — random-walk ticks")
        start = time.time()
        next_tick = time.time()
        while True:
            timeout = max(0.0, next_tick - time.time())
            self._cv.wait(timeout)
            self._cv.clear()
            while True:
                try:
                    cmd = self.link.commands.get_nowait()
                except queue.Empty:
                    break
                self.handle_command(cmd)
            if time.time() >= next_tick:
                next_tick += self.dt
                if self.syms and self.link.connected:
                    self.px = round(self.px + random.gauss(0, 2.0))
                    self.clock += int(self.dt * 60 * 1_000_000)  # 60x accelerated exchange clock
                    self._match()
                    for s in self.syms:
                        self.link.send({"t": "tick", "sym": s, "ts": self.clock, "px": self.px, "qty": random.randint(1, 5),
                                        "bid": self.px - 1, "ask": self.px + 1})
            if self.seconds and time.time() - start > self.seconds:
                self.link.send({"t": "info", "msg": "eof"})
                # keep serving for a moment so the engine can flatten on exit
                end = time.time() + 3.0
                while time.time() < end:
                    try:
                        cmd = self.link.commands.get(timeout=0.05)
                    except queue.Empty:
                        continue
                    self.handle_command(cmd)
                    self._match()
                return


# --------------------------------------------------------------------------- main


def main():
    ap = argparse.ArgumentParser(description="群益 SKCOM ↔ twq bridge")
    ap.add_argument("--config", help="config.ini (see config.example.ini)")
    ap.add_argument("--fake", action="store_true", help="run a fake broker (no SKCOM needed, any OS)")
    ap.add_argument("--fake-seconds", type=float, default=0.0, help="fake mode: send eof after N seconds")
    ap.add_argument("--fake-tps", type=float, default=50.0, help="fake mode: ticks per second")
    ap.add_argument("--port", type=int, help="override listen port")
    args = ap.parse_args()
    cfg = Config.load(args.config)
    if args.port:
        cfg.listen_port = args.port
    if args.fake:
        FakeBroker(cfg, ticks_per_sec=args.fake_tps, seconds=args.fake_seconds).run()
        return
    if sys.platform != "win32":
        raise SystemExit("SKCOM is Windows-only. Use --fake to test the bridge protocol on this OS.")
    if not cfg.allow_orders:
        log("allow_orders=false: orders from the engine will be REJECTED (dry run). Quotes still flow.")
    SkcomBroker(cfg).run()


if __name__ == "__main__":
    main()
