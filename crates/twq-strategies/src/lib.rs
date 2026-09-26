//! Strategy library. Every strategy here runs unchanged in backtest, paper and live.
//!
//! To add your own: implement [`twq_core::Strategy`], give it a `PARAMS` table and a
//! `new(&Params)` constructor, and register it in [`REGISTRY`].

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Result};
use twq_core::events::{Event, Series};
use twq_core::{Params, Strategy};

pub mod bb_revert;
pub mod common;
pub mod donchian;
pub mod event_clip;
pub mod guard;
pub mod ma_cross;
pub mod orb;
pub mod orb_daylow;
pub mod tail_flow;
pub mod vol_breakdown;

pub use guard::DailyLossGuard;

pub type ParamSpec = (&'static str, f64, &'static str);

/// Extra inputs besides prices: an event calendar and named auxiliary series
/// (e.g. `taiex_vol` = 大盤累積成交). Cheap to clone (shared `Arc`s).
#[derive(Clone, Debug, Default)]
pub struct Inputs {
    pub events: Arc<Vec<Event>>,
    pub series: HashMap<String, Arc<Series>>,
}

pub struct StrategyInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub params: &'static [ParamSpec],
    build: fn(&Params, &Inputs) -> Result<Box<dyn Strategy>>,
}

pub const REGISTRY: &[StrategyInfo] = &[
    StrategyInfo {
        name: "ma_cross",
        description: "均線交叉 (黃金/死亡交叉)，可選 ATR 停損",
        params: ma_cross::MaCross::PARAMS,
        build: |p, _| Ok(Box::new(ma_cross::MaCross::new(p))),
    },
    StrategyInfo {
        name: "orb",
        description: "台指日盤開盤區間突破 (OCO 雙向停損單 + 括號停損停利)",
        params: orb::Orb::PARAMS,
        build: |p, _| Ok(Box::new(orb::Orb::new(p))),
    },
    StrategyInfo {
        name: "donchian",
        description: "唐奇安通道突破 (海龜)，ATR + 通道移動停損",
        params: donchian::Donchian::PARAMS,
        build: |p, _| Ok(Box::new(donchian::Donchian::new(p))),
    },
    StrategyInfo {
        name: "bb_revert",
        description: "布林通道均值回歸 + KD 濾網，中軌停利",
        params: bb_revert::BbRevert::PARAMS,
        build: |p, _| Ok(Box::new(bb_revert::BbRevert::new(p))),
    },
    StrategyInfo {
        name: "vol_breakdown",
        description: "馬克羊 夜盤: 爆量紅K低點 -1tick 跌破放空，停損紅K高點",
        params: vol_breakdown::VolBreakdown::PARAMS,
        build: |p, _| Ok(Box::new(vol_breakdown::VolBreakdown::new(p))),
    },
    StrategyInfo {
        name: "tail_flow",
        description: "馬克羊講座 (第三方筆記): 13:30 當沖強平順勢單",
        params: tail_flow::TailFlow::PARAMS,
        build: |p, _| Ok(Box::new(tail_flow::TailFlow::new(p))),
    },
    StrategyInfo {
        name: "event_clip",
        description: "事件盤夾子: 公布前上下 ±20 掛觸價單, 停損 40, 大事件停利 150 / 一般 100 (需 --events)",
        params: event_clip::EventClip::PARAMS,
        build: |p, inp| {
            if inp.events.is_empty() {
                bail!("event_clip 需要事件行事曆: --events data/events/events.csv");
            }
            Ok(Box::new(event_clip::EventClip::new(p, inp.events.clone())))
        },
    },
    StrategyInfo {
        name: "orb_daylow",
        description: "你的 ORB: 9:30 前高低差>100 且大盤量>昨量0.45倍 → 破 day low −1 放空, 停損 0.4% (需 --series taiex_vol=...)",
        params: orb_daylow::OrbDayLow::PARAMS,
        build: |p, inp| {
            let vol = if p.flag("use_vol", true) {
                match inp.series.get("taiex_vol") {
                    Some(s) => Some(s.clone()),
                    None => bail!(
                        "orb_daylow 需要大盤累積成交資料: --series taiex_vol=data/taiex_cumamt.csv (或 -p use_vol=0 只用條件1)"
                    ),
                }
            } else {
                None
            };
            Ok(Box::new(orb_daylow::OrbDayLow::new(p, vol)))
        },
    },
];

/// Common parameters understood by every strategy (applied as wrappers).
pub const COMMON_PARAMS: &[ParamSpec] =
    &[("max_daily_loss", 0.0, "計程車司機法則: 當日虧損達此金額 (NTD) 即平倉停手 (0 = 關閉)")];

pub fn find(name: &str) -> Option<&'static StrategyInfo> {
    REGISTRY.iter().find(|s| s.name == name)
}

/// Build a strategy by name without extra inputs.
pub fn build(name: &str, p: &Params) -> Result<Box<dyn Strategy>> {
    build_with(name, p, &Inputs::default())
}

/// Build a strategy by name. Unknown parameters are rejected to catch typos.
pub fn build_with(name: &str, p: &Params, inputs: &Inputs) -> Result<Box<dyn Strategy>> {
    let Some(info) = find(name) else {
        let names: Vec<_> = REGISTRY.iter().map(|s| s.name).collect();
        bail!("unknown strategy '{name}'. available: {}", names.join(", "));
    };
    for (k, _) in p.iter() {
        if !info.params.iter().chain(COMMON_PARAMS).any(|(n, _, _)| n == k) {
            bail!("strategy '{name}' has no parameter '{k}'");
        }
    }
    let inner = (info.build)(p, inputs)?;
    let max_loss = p.get("max_daily_loss", 0.0);
    Ok(if max_loss > 0.0 { Box::new(DailyLossGuard::new(inner, max_loss)) } else { inner })
}

/// Defaults of a strategy as `Params`.
pub fn defaults(name: &str) -> Params {
    let mut p = Params::new();
    if let Some(info) = find(name) {
        for (k, v, _) in info.params {
            p.set(k, *v);
        }
    }
    p
}
