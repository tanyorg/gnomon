//! Per-address sentence statistics: counts and the output interval measured
//! per NMEA epoch.

use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct SentenceStat {
    pub count: u64,
    pub bad: u64,
    pub last: Option<Instant>,
    /// Smoothed interval between epochs in which this sentence appeared (s).
    pub period: Option<f64>,
    burst_epoch: u64,
    burst_start: Option<Instant>,
    outliers: u32,
}

impl SentenceStat {
    /// How long without this sentence before it is considered missing.
    pub fn stale_after(&self) -> Duration {
        let p = self.period.unwrap_or(1.0);
        Duration::from_secs_f64((p * 3.0).max(3.0))
    }

    pub(super) fn mark(&mut self, now: Instant, epoch_seq: u64) {
        self.count += 1;
        // A new "burst" is the first arrival in a new NMEA epoch. Without any
        // time-bearing sentences fall back to a plain inter-arrival gap.
        let new_burst = if epoch_seq > 0 {
            self.burst_epoch != epoch_seq
        } else {
            self.last
                .map_or(true, |l| now.duration_since(l) > Duration::from_millis(300))
        };
        if new_burst {
            if let Some(bs) = self.burst_start {
                let iv = now.duration_since(bs).as_secs_f64();
                if iv > 0.0 && iv < 60.0 {
                    self.period = Some(match self.period {
                        None => iv,
                        // Missed epochs: ignore, unless the rate really changed.
                        Some(p) if iv > p * 1.8 && self.outliers < 3 => {
                            self.outliers += 1;
                            p
                        }
                        Some(p) if iv > p * 1.8 => {
                            self.outliers = 0;
                            iv
                        }
                        Some(p) => {
                            self.outliers = 0;
                            p + 0.2 * (iv - p)
                        }
                    });
                }
            }
            self.burst_start = Some(now);
            self.burst_epoch = epoch_seq;
        }
        self.last = Some(now);
    }
}
