//! Satellites in view (assembled from GSV cycles) and in use (from GSA),
//! merged across talkers and signals.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use super::sentence::{f, gsa_layout, u, Sys};

/// Satellite data older than this is dropped from the display.
pub const SAT_STALE: Duration = Duration::from_secs(10);
/// "Used in fix" sets older than this are ignored.
pub const USED_STALE: Duration = Duration::from_secs(5);

// Resource limits: every table keyed by received data is bounded.
/// GSV cycles in flight / completed, keyed by (talker, signal ID).
pub(super) const MAX_GSV_KEYS: usize = 64;
/// NMEA allows 1..9 GSV messages per cycle; be lenient but bounded.
pub(super) const MAX_GSV_MSGS: u32 = 32;
/// Satellites accepted in one GSV cycle.
pub(super) const MAX_GSV_SATS: usize = 256;
/// "Used in fix" groups (one per GSA talker / system ID).
pub(super) const MAX_USED_KEYS: usize = 64;

#[derive(Debug, Clone)]
pub struct SatView {
    pub sys: Sys,
    pub prn: u16,
    pub el: Option<i32>,
    pub az: Option<i32>,
    /// C/N0 per signal ID (0 = not given / single signal).
    pub snr: BTreeMap<u8, Option<u32>>,
    pub used: bool,
}

#[derive(Debug, Clone)]
pub(super) struct GsvSat {
    sys: Sys,
    prn: u16,
    el: Option<i32>,
    az: Option<i32>,
    snr: Option<u32>,
}

/// GSV and GSA state, separate from the fix so it can be inspected on its own.
#[derive(Debug, Default)]
pub struct SatTracker {
    pub(super) gsv_stage: HashMap<(String, u8), (u32, Vec<GsvSat>)>,
    pub(super) gsv_done: HashMap<(String, u8), (Instant, Vec<GsvSat>)>,
    pub(super) used: HashMap<String, (Instant, HashSet<(Sys, u16)>)>,
}

impl SatTracker {
    /// Record the PRNs a GSA sentence lists as used in the fix.
    pub(super) fn gsa(&mut self, talker: &str, fl: &[&str], now: Instant) {
        let Some(lay) = gsa_layout(fl) else { return };
        let sysid = lay.sysid;
        let hint = sysid
            .and_then(Sys::from_system_id)
            .or_else(|| Sys::from_talker(talker))
            .unwrap_or(Sys::Unknown);
        let prns: Vec<u16> = lay
            .prns
            .filter_map(|i| u(fl, i))
            .filter_map(|p| u16::try_from(p).ok())
            .collect();
        let set: HashSet<(Sys, u16)> = prns.iter().map(|&p| (Sys::refine(hint, p), p)).collect();
        let group = match (sysid, talker) {
            (Some(id), _) => format!("id{id}"),
            (None, "GN") => match prns.first() {
                Some(&p) => Sys::from_prn(p).label().to_string(),
                None => return, // empty GN GSA without system id: nothing attributable
            },
            (None, t) => t.to_string(),
        };
        if !self.used.contains_key(&group) && self.used.len() >= MAX_USED_KEYS {
            self.used
                .retain(|_, (t, _)| now.duration_since(*t) < USED_STALE);
            if self.used.len() >= MAX_USED_KEYS {
                return;
            }
        }
        self.used.insert(group, (now, set));
    }

    pub(super) fn gsv(&mut self, talker: &str, fl: &[&str], now: Instant) {
        let (Some(total), Some(num)) = (u(fl, 0), u(fl, 1)) else {
            return;
        };
        if total == 0 || total > MAX_GSV_MSGS || num == 0 || num > total {
            return;
        }
        // After the 3 header fields come 4-field blocks, optionally followed
        // by a signal ID (NMEA 4.10+).
        let body = fl.len().saturating_sub(3);
        let signal = if body % 4 == 1 {
            u(fl, fl.len() - 1)
                .and_then(|v| u8::try_from(v).ok())
                .unwrap_or(0)
        } else {
            0
        };
        let hint = Sys::from_talker(talker).unwrap_or(Sys::Unknown);
        let key = (talker.to_string(), signal);
        if num == 1 {
            if !self.gsv_stage.contains_key(&key) && self.gsv_stage.len() >= MAX_GSV_KEYS {
                // Too many parallel cycles: only real talkers survive this
                // long, so dropping the oldest partial state is fine.
                self.gsv_stage.clear();
            }
            self.gsv_stage.insert(key.clone(), (1, Vec::new()));
        }
        let Some(stage) = self.gsv_stage.get_mut(&key) else {
            return;
        };
        if num != stage.0 && num != 1 {
            // Out of sequence: discard the partial cycle.
            self.gsv_stage.remove(&key);
            return;
        }
        let blocks = body / 4;
        for b in 0..blocks {
            let i = 3 + b * 4;
            let Some(prn) = u(fl, i).and_then(|p| u16::try_from(p).ok()) else {
                continue;
            };
            if prn == 0 {
                continue;
            }
            if stage.1.len() >= MAX_GSV_SATS {
                self.gsv_stage.remove(&key);
                return;
            }
            stage.1.push(GsvSat {
                sys: Sys::refine(hint, prn),
                prn,
                el: f(fl, i + 1).map(|v| v as i32),
                az: f(fl, i + 2).map(|v| v as i32),
                snr: u(fl, i + 3),
            });
        }
        if num >= total {
            let (_, sats) = self.gsv_stage.remove(&key).unwrap();
            if !self.gsv_done.contains_key(&key) && self.gsv_done.len() >= MAX_GSV_KEYS {
                self.gsv_done
                    .retain(|_, (t, _)| now.duration_since(*t) < SAT_STALE);
                if self.gsv_done.len() >= MAX_GSV_KEYS {
                    return;
                }
            }
            self.gsv_done.insert(key, (now, sats));
        } else {
            // num < total <= MAX_GSV_MSGS, so this cannot overflow; keep the
            // check anyway in case the limits ever change.
            match num.checked_add(1) {
                Some(n) => stage.0 = n,
                None => {
                    self.gsv_stage.remove(&key);
                }
            }
        }
    }

    pub fn used_set(&self, now: Instant) -> HashSet<(Sys, u16)> {
        self.used
            .values()
            .filter(|(t, _)| now.duration_since(*t) < USED_STALE)
            .flat_map(|(_, s)| s.iter().copied())
            .collect()
    }

    /// Merged satellite view across talkers and signals, sorted for display.
    pub fn satellites(&self, now: Instant) -> Vec<SatView> {
        let used = self.used_set(now);
        let mut map: BTreeMap<(Sys, u16), SatView> = BTreeMap::new();
        for ((_, sig), (t, sats)) in &self.gsv_done {
            if now.duration_since(*t) >= SAT_STALE {
                continue;
            }
            for g in sats {
                let e = map.entry((g.sys, g.prn)).or_insert_with(|| SatView {
                    sys: g.sys,
                    prn: g.prn,
                    el: None,
                    az: None,
                    snr: BTreeMap::new(),
                    used: false,
                });
                e.el = e.el.or(g.el);
                e.az = e.az.or(g.az);
                e.snr.insert(*sig, g.snr);
            }
        }
        // A used satellite might not have been in view list (or vice versa).
        for &(sys, prn) in &used {
            map.entry((sys, prn)).or_insert_with(|| SatView {
                sys,
                prn,
                el: None,
                az: None,
                snr: BTreeMap::new(),
                used: true,
            });
        }
        let mut v: Vec<SatView> = map
            .into_values()
            .map(|mut s| {
                s.used = used.contains(&(s.sys, s.prn));
                s
            })
            .collect();
        v.sort_by_key(|s| (!s.used, s.sys, s.prn));
        v
    }
}
