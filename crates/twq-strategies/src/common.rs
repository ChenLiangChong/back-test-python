//! Small helpers shared by the bundled strategies.

use twq_core::time::{day_of, futures_session, hhmm_to_min, minute_of_day, trading_day, FuturesSession, Ts};
use twq_core::{Ctx, Params};

/// Which TAIFEX session(s) a strategy trades.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionFilter {
    All,
    Day,
    Night,
}

impl SessionFilter {
    /// `session` param: 0 = all, 1 = day (08:45–13:45), 2 = night (15:00–05:00).
    pub fn from_params(p: &Params, default: f64) -> Self {
        match p.get("session", default) as i64 {
            1 => Self::Day,
            2 => Self::Night,
            _ => Self::All,
        }
    }

    #[inline]
    pub fn allows(self, ts: Ts) -> bool {
        match (self, futures_session(ts)) {
            (_, FuturesSession::Closed) => false,
            (Self::All, _) => true,
            (Self::Day, FuturesSession::Day) | (Self::Night, FuturesSession::Night) => true,
            _ => false,
        }
    }
}

/// Session-end flattening: returns true when the bar *closing* at `end` is within the
/// last `buffer_min` minutes of its session (day 13:45, night 05:00).
#[inline]
pub fn near_session_close(bar_start: Ts, end: Ts, buffer_min: u32) -> bool {
    let m_end = minute_of_day(end);
    match futures_session(bar_start) {
        FuturesSession::Day => m_end + buffer_min >= hhmm_to_min(1345),
        FuturesSession::Night => m_end < hhmm_to_min(500) && m_end + buffer_min >= hhmm_to_min(500),
        FuturesSession::Closed => true,
    }
}

/// Order size: fixed `qty`, or risk-based when `risk_pct` > 0
/// (risk_pct % of equity lost if the stop `stop_pts` away is hit).
#[inline]
pub fn size(ctx: &Ctx, qty: i64, risk_pct: f64, stop_pts: f64, max_qty: i64) -> i64 {
    if risk_pct > 0.0 {
        ctx.qty_for_risk(ctx.equity() * risk_pct / 100.0, stop_pts).min(max_qty)
    } else {
        qty
    }
}

/// Tracks day changes.
#[derive(Clone, Copy, Debug, Default)]
pub struct DayTracker {
    day: i64,
    /// Use the calendar date instead of the TAIFEX trading day. Day-session-only
    /// strategies want this: it also works for markets whose session runs past 14:50
    /// (where the TAIFEX night-session roll would otherwise split the day).
    calendar: bool,
}

impl DayTracker {
    pub fn calendar() -> Self {
        Self { day: i64::MIN, calendar: true }
    }

    /// Returns true on the first bar of a new day.
    #[inline]
    pub fn is_new_day(&mut self, ts: Ts) -> bool {
        let d = if self.calendar { day_of(ts) } else { trading_day(ts) };
        if d != self.day {
            self.day = d;
            true
        } else {
            false
        }
    }
}
