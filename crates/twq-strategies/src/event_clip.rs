//! 事件盤「夾子」: just before a scheduled release (非農 / CPI / FOMC / PPI / JOLTS /
//! 指數調整日 / 台積電營收 …) place a buy-stop `clip` points above and a sell-stop
//! `clip` points below the last price (OCO). Whichever side the release pushes price
//! through becomes the trade; it carries a stop-loss `sl` and a take-profit that depends
//! on the event tier (大事件 `tp_big`, 一般事件 `tp_normal`).
//!
//! Defaults follow the user's rules: 夾子 ±20, 停損 40, 大事件停利 150, 一般事件停利 100.
//! Assumptions (parameters): untriggered clips are cancelled `cancel_min` minutes after
//! the release; an open trade exits on SL / TP, after `max_hold_min` (0 = no limit), or
//! just before the session closes.
//!
//! Runs on 1-minute bars (clips placed at the close of the bar ending at the release
//! time) or, more precisely, on ticks (placed `lead_sec` seconds before the release).

use std::sync::Arc;

use twq_core::events::{Event, Tier};
use twq_core::time::{Ts, US_PER_MIN, US_PER_SEC};
use twq_core::{Bar, Bracket, Ctx, OrderKind, Params, Side, Strategy, Tick, Tif};

use crate::common::near_session_close;

#[derive(Clone, Copy, Debug)]
struct Active {
    ev_ts: Ts,
    entered_at: Option<Ts>,
}

pub struct EventClip {
    events: Arc<Vec<Event>>,
    next: usize,
    clip: f64,
    sl: f64,
    tp_big: f64,
    tp_normal: f64,
    lead: Ts,
    cancel_after: Ts,
    max_hold: Ts,
    qty: i64,
    tier_filter: i64,
    active: Option<Active>,
    tick_mode: bool,
    last_px: f64,
    last_ts: Ts,
    pub armed_count: u32,
}

impl EventClip {
    pub const PARAMS: &'static [(&'static str, f64, &'static str)] = &[
        ("clip", 20.0, "夾子: 上下各幾點掛觸價單"),
        ("sl", 40.0, "停損 (點)"),
        ("tp_big", 150.0, "大事件停利 (點) — 非農/失業率/FOMC/CPI"),
        ("tp_normal", 100.0, "一般事件停利 (點) — JOLTS/PPI/MSCI/富時/台積電營收"),
        ("lead_sec", 1.0, "tick 模式: 公布前幾秒掛出夾子"),
        ("cancel_min", 5.0, "公布後幾分鐘沒觸發就取消夾子 (假設)"),
        ("max_hold_min", 0.0, "最長持倉分鐘, 0 = 只看停損停利 / 收盤前平倉 (假設)"),
        ("qty", 1.0, "口數"),
        ("tier", 0.0, "0 全部事件 / 1 只做大事件 / 2 只做一般事件"),
    ];

    pub fn new(p: &Params, events: Arc<Vec<Event>>) -> Self {
        Self {
            events,
            next: 0,
            clip: p.get("clip", 20.0),
            sl: p.get("sl", 40.0),
            tp_big: p.get("tp_big", 150.0),
            tp_normal: p.get("tp_normal", 100.0),
            lead: (p.get("lead_sec", 1.0) * US_PER_SEC as f64) as Ts,
            cancel_after: (p.get("cancel_min", 5.0) * US_PER_MIN as f64) as Ts,
            max_hold: (p.get("max_hold_min", 0.0) * US_PER_MIN as f64) as Ts,
            qty: p.get("qty", 1.0) as i64,
            tier_filter: p.get("tier", 0.0) as i64,
            active: None,
            tick_mode: false,
            last_px: f64::NAN,
            last_ts: Ts::MIN,
            armed_count: 0,
        }
    }

    fn wanted(&self, e: &Event) -> bool {
        match self.tier_filter {
            1 => e.tier == Tier::Big,
            2 => e.tier == Tier::Normal,
            _ => true,
        }
    }

    /// `price` = latest trade, `pre` = latest trade strictly before the next release.
    fn step(&mut self, now: Ts, price: f64, pre: f64, ctx: &mut Ctx) {
        // manage the armed / open event trade
        if let Some(mut a) = self.active {
            let finished = if ctx.position() != 0 {
                let entered = *a.entered_at.get_or_insert(now);
                if self.max_hold > 0 && now >= entered + self.max_hold {
                    ctx.flatten(); // held too long without hitting SL / TP
                    true
                } else {
                    false
                }
            } else if a.entered_at.is_some() || now >= a.ev_ts + self.cancel_after {
                // trade closed by SL / TP, or the release did not move price enough
                ctx.cancel_all();
                true
            } else {
                false
            };
            self.active = if finished { None } else { Some(a) };
        }
        // skip releases we can no longer act on (data gaps, filtered tiers)
        while let Some(e) = self.events.get(self.next) {
            if now > e.ts + US_PER_MIN || !self.wanted(e) {
                self.next += 1;
            } else {
                break;
            }
        }
        let Some(e) = self.events.get(self.next) else { return };
        if now < e.ts - self.lead || self.active.is_some() || !ctx.is_flat() || ctx.has_working_orders() {
            return;
        }
        self.next += 1;
        let reference = if now < e.ts || !pre.is_finite() { price } else { pre };
        if !reference.is_finite() {
            return;
        }
        let tp = match e.tier {
            Tier::Big => self.tp_big,
            Tier::Normal => self.tp_normal,
        };
        let br = Bracket { stop_dist: Some(self.sl), take_dist: (tp > 0.0).then_some(tp) };
        let oco = ctx.new_oco();
        let up = ctx.submit(Side::Buy, self.qty, OrderKind::Stop(reference + self.clip), Tif::Rod, oco, e.name);
        ctx.attach_bracket(up, br);
        let dn = ctx.submit(Side::Sell, self.qty, OrderKind::Stop(reference - self.clip), Tif::Rod, oco, e.name);
        ctx.attach_bracket(dn, br);
        self.active = Some(Active { ev_ts: e.ts, entered_at: None });
        self.armed_count += 1;
    }
}

impl Strategy for EventClip {
    fn on_bar(&mut self, bar: &Bar, ctx: &mut Ctx) {
        let end = ctx.bar_end(bar);
        if near_session_close(bar.ts, end, 1) {
            if !ctx.is_flat() || ctx.has_working_orders() {
                ctx.flatten();
            }
            self.active = None;
            return;
        }
        if !self.tick_mode {
            // the bar close is the last trade before `end`
            self.step(end, bar.close, bar.close, ctx);
        }
    }

    fn on_tick(&mut self, tick: &Tick, ctx: &mut Ctx) {
        self.tick_mode = true;
        let next_ts = self.events.get(self.next).map(|e| e.ts).unwrap_or(Ts::MAX);
        let pre = if self.last_ts < next_ts { self.last_px } else { f64::NAN };
        self.step(tick.ts, tick.price, pre, ctx);
        self.last_px = tick.price;
        self.last_ts = tick.ts;
    }

    fn wants_ticks(&self) -> bool {
        true
    }
}
