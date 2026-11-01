//! Independent source waveforms. Amplitudes are amperes or volts, times are seconds.

use crate::error::SolveError;
use crate::parse::Wave;

const PI: f64 = std::f64::consts::PI;

pub(crate) fn at(name: &str, wave: &Wave, t: f64) -> Result<f64, SolveError> {
    let value = match wave {
        Wave::Dc(value) => *value,
        Wave::Pwl(points) => pwl(points, t),
        Wave::Pulse {
            v1,
            v2,
            td,
            tr,
            tf,
            pw,
            per,
        } => pulse(
            &Pulse {
                v1: *v1,
                v2: *v2,
                td: *td,
                tr: *tr,
                tf: *tf,
                pw: *pw,
                per: *per,
            },
            t,
        ),
        Wave::Sin {
            vo,
            va,
            freq,
            td,
            theta,
        } => sin(*vo, *va, *freq, *td, *theta, t),
    };
    if value.is_finite() {
        Ok(value)
    } else {
        Err(SolveError::Element {
            name: name.to_string(),
            message: "waveform value is not finite".to_string(),
        })
    }
}

fn pwl(points: &[(f64, f64)], t: f64) -> f64 {
    let Some((t0, v0)) = points.first().copied() else {
        return 0.0;
    };
    if t <= t0 {
        return v0;
    }
    for pair in points.windows(2) {
        let (t0, v0) = pair[0];
        let (t1, v1) = pair[1];
        if t <= t1 {
            let span = t1 - t0;
            if span == 0.0 {
                return v1;
            }
            return v0 + (t - t0) / span * (v1 - v0);
        }
    }
    points[points.len() - 1].1
}

struct Pulse {
    v1: f64,
    v2: f64,
    td: f64,
    tr: f64,
    tf: f64,
    pw: f64,
    per: f64,
}

/// SPICE pulse. `per == 0` is a single shot. A zero rise or fall is a step.
fn pulse(shape: &Pulse, t: f64) -> f64 {
    if t < shape.td {
        return shape.v1;
    }
    let mut tau = t - shape.td;
    if shape.per > 0.0 {
        tau = tau.rem_euclid(shape.per);
    }
    let rise = shape.tr.max(0.0);
    if rise > 0.0 && tau < rise {
        return shape.v1 + (shape.v2 - shape.v1) * tau / rise;
    }
    if tau < rise + shape.pw.max(0.0) {
        return shape.v2;
    }
    let fall = shape.tf.max(0.0);
    if fall > 0.0 && tau < rise + shape.pw.max(0.0) + fall {
        return shape.v2 + (shape.v1 - shape.v2) * (tau - rise - shape.pw.max(0.0)) / fall;
    }
    shape.v1
}

/// Time derivative [amplitude/s] of [`at`]. A held level has derivative 0.
pub(crate) fn deriv(name: &str, wave: &Wave, t: f64) -> Result<f64, SolveError> {
    let value = match wave {
        Wave::Dc(_) => 0.0,
        Wave::Pwl(points) => pwl_deriv(points, t),
        Wave::Pulse {
            v1,
            v2,
            td,
            tr,
            tf,
            pw,
            per,
        } => pulse_deriv(
            &Pulse {
                v1: *v1,
                v2: *v2,
                td: *td,
                tr: *tr,
                tf: *tf,
                pw: *pw,
                per: *per,
            },
            t,
        ),
        Wave::Sin {
            va,
            freq,
            td,
            theta,
            ..
        } => sin_deriv(*va, *freq, *td, *theta, t),
    };
    if value.is_finite() {
        Ok(value)
    } else {
        Err(SolveError::Element {
            name: name.to_string(),
            message: "waveform derivative is not finite".to_string(),
        })
    }
}

fn pwl_deriv(points: &[(f64, f64)], t: f64) -> f64 {
    for pair in points.windows(2) {
        let (t0, v0) = pair[0];
        let (t1, v1) = pair[1];
        if t0 <= t && t < t1 {
            let span = t1 - t0;
            if span == 0.0 {
                return 0.0;
            }
            return (v1 - v0) / span;
        }
    }
    0.0
}

fn pulse_deriv(shape: &Pulse, t: f64) -> f64 {
    if t < shape.td {
        return 0.0;
    }
    let mut tau = t - shape.td;
    if shape.per > 0.0 {
        tau = tau.rem_euclid(shape.per);
    }
    let rise = shape.tr.max(0.0);
    if rise > 0.0 && tau < rise {
        return (shape.v2 - shape.v1) / rise;
    }
    let width = shape.pw.max(0.0);
    if tau < rise + width {
        return 0.0;
    }
    let fall = shape.tf.max(0.0);
    if fall > 0.0 && tau < rise + width + fall {
        return (shape.v1 - shape.v2) / fall;
    }
    0.0
}

fn sin_deriv(va: f64, freq: f64, td: f64, theta: f64, t: f64) -> f64 {
    if t < td {
        return 0.0;
    }
    let tau = t - td;
    let omega = 2.0 * PI * freq;
    let decay = (-theta * tau).exp();
    va * decay * (omega * (omega * tau).cos() - theta * (omega * tau).sin())
}

/// `vo + va e^{−θ(t−td)} sin(2π f (t−td))` for `t ≥ td`, otherwise `vo`.
fn sin(vo: f64, va: f64, freq: f64, td: f64, theta: f64, t: f64) -> f64 {
    if t < td {
        return vo;
    }
    let tau = t - td;
    vo + va * (-theta * tau).exp() * (2.0 * PI * freq * tau).sin()
}
