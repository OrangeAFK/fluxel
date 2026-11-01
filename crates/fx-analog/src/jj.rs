//! JoSIM RCSJ quasiparticle branch, evaluated at a branch voltage.
//!
//! JoSIM `JJ::set_model` and `JJ::update_value` (`src/JJ.cpp`, master 2025)
//! and `Simulation::handle_jj` (`src/Simulation.cpp`). The physical current is
//! the SPEC §4.3 RCSJ law
//!
//! ```text
//! i [A] = I_c sin(δ − φ_off) + G(v) v + i_t(v) + C dv/dt
//! v [V] = (Φ₀ / 2π) dδ/dt
//! ```
//!
//! with `δ = φ_a − φ_b` in radians and `φ_off` the model `PHI`. `G` and `i_t`
//! are the piecewise quasiparticle arm below. JoSIM folds `C` into the same
//! branch; this solver does too. The capacitor companion itself is trapezoidal
//! and lives next to the junction stamp in the solver, not here.
//!
//! ## Gap law (`JJ::update_value`)
//!
//! `lower = V_g − ΔV/2`, `upper = V_g + ΔV/2`,
//! `G_large = I_c / (I_cfct · ΔV)`.
//!
//! - `rtype == 0`: `set_model` assigns `R0 = Rn` and `handle_jj` never calls
//!   `update_value`. `G = 1/R0`, `i_t = 0` at every voltage.
//! - `rtype == 1`, `|v| < lower`: subgap, `G = 1/R0`, `i_t = 0`.
//! - `rtype == 1`, `lower ≤ |v| < upper`: transition,
//!   `G = G_large`, `i_t = sign(v) · lower · (1/R0 − G_large)`.
//! - `rtype == 1`, `|v| ≥ upper`: normal, `G = 1/Rn`, `i_t = 0`.
//!
//! `ΔV = 0` makes `lower == upper`, so the transition arm is never taken.
//! JoSIM still evaluates `G_large` and divides by zero; this function skips
//! that division. A positive window with `I_cfct ≤ 0` is an error.
//!
//! ## Where this discretization differs from JoSIM
//!
//! JoSIM 2.5 replaced trapezoidal integration with second-order Gear. The
//! phase-mode capacitor stamp in `src/Capacitor.cpp` keeps four previous
//! phases and a factor `4 h² / (9 C σ)`. The junction impedance in
//! `subgap_impedance` is `1/R + 3C/(2h)`, the BDF2 companion, and the
//! supercurrent is `I_c sin(φ₀ − φ_off)` moved to the right-hand side at the
//! Gear predictor `φ₀` (`handle_jj`). There is no Newton iteration.
//!
//! Mode B keeps the same `G(v)` and `i_t(v)`, integrates with the trapezoidal
//! rule (companion `2C/h`, one previous step), and Newton-iterates
//! `sin(δ − φ_off)`. The arm is chosen from the trapezoidal voltage at the
//! current iterate. JoSIM freezes the arm on its Gear predictor for the whole
//! step. Away from the gap window, and wherever `R0 = Rn`, the quasiparticle
//! law is the same function of voltage. Inside the window the two simulators
//! can pick different arms for about one step. Both integrators are order 2;
//! the local difference is the `O(h²)` gap between the trapezoidal and Gear
//! companions, plus the difference between a converged `sin` and JoSIM's
//! explicit predictor. `Φ₀` here is the spec value `2.067833848e-15`, not
//! JoSIM `PHI_ZERO` (`2.067833831170082e-15` in `Constants.hpp`). The relative
//! difference is about `8e-9`.
//!
//! CPR harmonics and the transparency / `tDep` supercurrent formula are not
//! fields on [`Jj`](crate::Jj). Temperature scaling of `Rn` is already applied
//! by the parser.

use crate::error::SolveError;
use crate::parse::Jj;

/// Quasiparticle conductance `G` [S] and offset current `i_t` [A].
///
/// The branch current is `G * v + i_t`, plus the supercurrent and the
/// capacitor current, which this function does not include.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Qp {
    pub g: f64,
    pub it: f64,
}

/// JoSIM `JJ::update_value` at `voltage` [V]. See the module comment.
pub fn quasiparticle(jj: &Jj, voltage: f64) -> Result<Qp, SolveError> {
    if !voltage.is_finite() {
        return Err(bad(jj, "voltage is not finite"));
    }
    if jj.rtype != 0 && jj.rtype != 1 {
        return Err(bad(jj, "rtype must be 0 or 1"));
    }
    if !jj.r0.is_finite() || jj.r0 <= 0.0 {
        return Err(bad(jj, "subgap resistance must be finite and positive"));
    }
    // rtype 0: constant subgap conductance, gap ignored (JJ::set_model).
    if jj.rtype == 0 {
        return Ok(Qp {
            g: 1.0 / jj.r0,
            it: 0.0,
        });
    }
    if !jj.rn.is_finite() || jj.rn <= 0.0 {
        return Err(bad(jj, "normal resistance must be finite and positive"));
    }
    if !jj.vg.is_finite() || !jj.deltav.is_finite() || jj.deltav < 0.0 {
        return Err(bad(jj, "gap voltage and DELV must be finite, DELV >= 0"));
    }
    // ΔV = 0 makes lower == upper, so the transition arm is unreachable
    // and G_large is never computed.
    let lower = jj.vg - 0.5 * jj.deltav;
    let upper = jj.vg + 0.5 * jj.deltav;
    let av = voltage.abs();
    if av < lower {
        return Ok(Qp {
            g: 1.0 / jj.r0,
            it: 0.0,
        });
    }
    if av < upper {
        if !jj.ic.is_finite() || !jj.icfct.is_finite() || jj.icfct <= 0.0 {
            return Err(bad(
                jj,
                "ICFCT must be positive when the gap window is positive",
            ));
        }
        // G_large [S] = I_c / (I_cfct · ΔV), JoSIM JJ::set_model.
        let g_large = jj.ic / (jj.icfct * jj.deltav);
        let mut it = lower * (1.0 / jj.r0 - g_large);
        if voltage < 0.0 {
            it = -it;
        }
        return Ok(Qp { g: g_large, it });
    }
    Ok(Qp {
        g: 1.0 / jj.rn,
        it: 0.0,
    })
}

fn bad(jj: &Jj, message: &str) -> SolveError {
    SolveError::Element {
        name: jj.name.clone(),
        message: message.to_string(),
    }
}
