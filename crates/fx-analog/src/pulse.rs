//! Pulse timestamps from a junction phase trace.
//!
//! Same rule as `tools/char/extract.py` and SPEC §4: a pulse is an upward
//! crossing of `(2k+1)π`, a downward crossing is an antipulse, and the time
//! is linear between samples. A level that lands on a sample belongs to the
//! interval that arrives there.

use crate::error::SolveError;

const PI: f64 = std::f64::consts::PI;

/// One `(2k+1)π` crossing. `t` is in the same units as the time trace.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crossing {
    pub t: f64,
    pub k: i32,
}

/// Upward crossings are pulses. Downward crossings are antipulses.
#[derive(Debug, Clone, PartialEq)]
pub struct PhaseCrossings {
    pub pulses: Vec<Crossing>,
    pub antipulses: Vec<Crossing>,
}

/// Crossings of `phase` [rad] sampled at `time`.
pub fn extract_crossings(time: &[f64], phase: &[f64]) -> Result<PhaseCrossings, SolveError> {
    if time.len() != phase.len() {
        return Err(SolveError::TraceLength);
    }
    let mut pulses = Vec::new();
    let mut antipulses = Vec::new();
    if time.len() < 2 {
        return Ok(PhaseCrossings { pulses, antipulses });
    }
    for i in 0..time.len() - 1 {
        let (t0, t1) = (time[i], time[i + 1]);
        let (p0, p1) = (phase[i], phase[i + 1]);
        if p1 == p0 {
            continue;
        }
        if p1 > p0 {
            for k in ks_up(p0, p1) {
                pulses.push(Crossing {
                    t: crossing_time(t0, t1, p0, p1, k),
                    k,
                });
            }
        } else {
            for k in ks_down(p0, p1) {
                antipulses.push(Crossing {
                    t: crossing_time(t0, t1, p0, p1, k),
                    k,
                });
            }
        }
    }
    Ok(PhaseCrossings { pulses, antipulses })
}

/// `int(round(seconds * 1e15))` with Python 3 half-to-even rounding.
pub fn femtoseconds(seconds: f64) -> i64 {
    let x = seconds * 1e15;
    if !x.is_finite() {
        return 0;
    }
    let floor = x.floor();
    let frac = x - floor;
    let even = (floor as i64).rem_euclid(2) == 0;
    let rounded = if frac > 0.5 || (frac == 0.5 && !even) {
        floor + 1.0
    } else {
        floor
    };
    rounded as i64
}

fn crossing_time(t0: f64, t1: f64, p0: f64, p1: f64, k: i32) -> f64 {
    // (2k+1)π, linear in time between samples.
    let level = f64::from(2 * k + 1) * PI;
    t0 + (t1 - t0) * (level - p0) / (p1 - p0)
}

fn ks_up(p0: f64, p1: f64) -> Vec<i32> {
    // Odd levels with p0 < (2k+1)π <= p1, low k first.
    let k0 = ((p0 / PI - 1.0) / 2.0).floor() as i32;
    let k1 = ((p1 / PI - 1.0) / 2.0).floor() as i32 + 1;
    let mut ks = Vec::new();
    let mut k = k0;
    while k <= k1 {
        let level = f64::from(2 * k + 1) * PI;
        if p0 < level && level <= p1 {
            ks.push(k);
        }
        k += 1;
    }
    ks
}

fn ks_down(p0: f64, p1: f64) -> Vec<i32> {
    // Odd levels with p1 <= (2k+1)π < p0, high k first.
    let k_hi = ((p0 / PI - 1.0) / 2.0).ceil() as i32;
    let k_lo = ((p1 / PI - 1.0) / 2.0).floor() as i32;
    if k_hi < k_lo {
        return Vec::new();
    }
    let mut ks = Vec::new();
    let mut k = k_hi;
    loop {
        let level = f64::from(2 * k + 1) * PI;
        if p1 <= level && level < p0 {
            ks.push(k);
        }
        if k == k_lo {
            break;
        }
        k -= 1;
    }
    ks
}
