# CLAUDE.md

twq — Rust backtest + auto-trading engine for Taiwan futures (台指期 TX/MTX/TMF) and stocks,
with a 群益 (Capital) SKCOM bridge for live trading. The user writes in Traditional Chinese;
answer in Traditional Chinese. **Current status, results so far and the next steps are in
[`docs/HANDOFF.md`](docs/HANDOFF.md) — read it first.**

## Commands

```bash
cargo build --release                                   # binary: target/release/twq
cargo test --release                                    # 56 tests, all must pass
cargo fmt --all && cargo clippy --release --all-targets -- -D warnings   # CI runs both
./target/release/twq strategies                         # list strategies + params
./target/release/twq backtest --data <bars.bin|csv> -s <strategy> -p "k=v,..." --out reports/x
./target/release/twq backtest --data <ticks.bin> --ticks -s ...          # tick mode
./target/release/twq optimize --data ... -s ... --grid k=a:b:step --grid k2=v1,v2 --wf 4
./target/release/twq bench
```

Before every commit: fmt, clippy with `-D warnings`, and all tests green. CI is
`.github/workflows/ci.yml` (Ubuntu + Windows, plus a Python-bridge ↔ engine e2e run).

## Layout

- `crates/twq-core` — types, time, TW market rules (`instrument.rs`), O(1) indicators,
  data IO + TAIFEX parser (`data.rs`), event calendars / aux series (`events.rs`),
  `Strategy`/`Ctx` API (`strategy.rs`), matching engine `SimExchange` (`sim.rs`), `Portfolio`.
- `crates/twq-backtest` — bar & tick engines, stats, rayon grid search, walk-forward, HTML report.
- `crates/twq-strategies` — strategies + `REGISTRY` in `lib.rs`. The user's own strategies are
  `event_clip.rs` (事件盤夾子) and `orb_daylow.rs` (破 day low ORB). Scenario tests in `tests/`.
- `crates/twq-live` — live/paper runner, risk manager, JSON-lines bridge protocol, mock bridge.
- `crates/twq-cli` — the `twq` command (`src/main.rs`).
- `bridge/capital_bridge.py` — 群益 SKCOM bridge (Windows only, **never run against a real account yet**).
- `tools/` — data downloaders (Python stdlib + curl). `scripts/backtest_my_strategies.sh` — one-shot pipeline.
- `data/events/` — event calendars (committed). Downloaded market data under `data/` is git-ignored.
- `docs/RESEARCH.md` (群益 API, TW rules, 馬克羊 research), `docs/ARCHITECTURE.md`, `docs/HANDOFF.md`.

## Conventions

- **Time**: `Ts` = i64 µs since 1970 in **Taipei wall-clock time** (no tz conversion anywhere).
  Bars are stamped with their **open** time; `ctx.bar_end(bar)` gives the close time.
  `trading_day()` maps the night session (≥14:50) to the next business day; holidays are not modelled.
- **Strategies** never touch the engine: they call `Ctx` (`submit`, `enter` + `Bracket`, `target_position`,
  `flatten`, `cancel_all`, …). Orders from `on_bar` become active on the next bar. Add a strategy by
  implementing `Strategy`, a `PARAMS` table and `new(&Params)`, then registering it in `REGISTRY`.
  Unknown params are rejected. Extra inputs (event calendar, aux series) come via `Inputs`
  (`--events`, `--series name=path`, `--index-events 2017-2023`).
- Bar-mode matching walks an adaptive intrabar path (open nearer low → O-L-H-C); limits need a
  trade-through; stops/markets get `--slippage` ticks (default 1).
- Hot paths must not allocate per bar/tick. Keep user-facing CLI text in Traditional Chinese.
- Commit messages: imperative subject, explain *why* in the body. Never commit downloaded data.

## Data gotchas (learned the hard way)

- TAIFEX tick files (`Daily_YYYY_MM_DD.csv`, Big5 header): time may lack the leading zero (`84500`)
  or carry 8 digits (`HHMMSS00`); volume is B+S (halved); spread legs are excluded. **Night-session
  rows are calendar-dated in the real files** (the official page says "trading date"); `twq data taifex`
  auto-detects this. The front month rolls on the 3rd-Wednesday settlement day.
- Community 1-min CSVs (CrazyIndicator) stamp bars with their **end** time → merge with `--shift-sec -60`.
- TWSE (`MI_5MINS`) has a firewall that bans bursts ("FOR SECURITY REASONS"): use `--twse-delay 5` or more.
- bls.gov blocks scripts (403) → release dates come from FRED (`tools/fetch_release_dates.py`).
- Fixed point thresholds mean different things at index 8,000 and 46,000 → use `range_pct` / `sl_pct`
  for multi-year tests.
- 1-minute bars cannot resolve the first seconds after a release: event-trading results on 1-min bars
  are estimates; tick data is authoritative.
