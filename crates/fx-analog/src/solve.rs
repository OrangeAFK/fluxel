//! Mode B: trapezoidal phase-based modified nodal analysis.
//!
//! Unknowns are node phases [rad], ground fixed at 0, plus one branch current
//! [A] per voltage source. SPEC §4.3. `Φ₀ = 2.067833848e-15` Wb,
//! `α = Φ₀ / 2π` [Wb/rad]. JoSIM's Gear stamps are not used; the difference is
//! documented on [`crate::jj`].

use std::collections::BTreeMap;

use faer::linalg::solvers::Solve;
use faer::sparse::linalg::solvers::{Lu, SymbolicLu};
use faer::sparse::{SparseColMat, Triplet};

use crate::error::SolveError;
use crate::jj::{self, Qp};
use crate::parse::{Element, Jj, Wave};
use crate::wave;

/// Flux quantum from SPEC §4.3 [Wb]. Not JoSIM `PHI_ZERO`.
pub const PHI0: f64 = 2.067833848e-15;

/// Default transient step, 0.025 ps [s].
pub const DEFAULT_STEP: f64 = 0.025e-12;

const PI: f64 = std::f64::consts::PI;

/// What to integrate and which traces to keep.
#[derive(Debug, Clone)]
pub struct SolveConfig {
    /// Fixed step [s].
    pub h: f64,
    /// Last sample time [s]. Must be an integer number of steps.
    pub t_stop: f64,
    /// Node names whose phase and voltage are returned.
    pub nodes: Vec<String>,
    /// Junction names whose phase difference and voltage are returned.
    pub junctions: Vec<String>,
    /// Newton stops when every phase correction is below this [rad].
    pub phase_tol: f64,
    /// Newton stops when every voltage-source current correction is below this [A].
    pub current_tol: f64,
    /// Newton iterations per step. One iteration that still moves is a failure.
    pub max_newton: u32,
}

/// Samples in seconds, radians, and volts, including the t = 0 state.
#[derive(Debug, Clone, PartialEq)]
pub struct Trace {
    pub time_s: Vec<f64>,
    pub phase: Vec<f64>,
    pub voltage: Vec<f64>,
}

/// Traces keyed by the requested names.
#[derive(Debug, Clone, PartialEq)]
pub struct Solution {
    pub nodes: BTreeMap<String, Trace>,
    pub junctions: BTreeMap<String, Trace>,
}

/// Integrate `elements` with Mode B.
pub fn solve(elements: &[Element], config: &SolveConfig) -> Result<Solution, SolveError> {
    let prepared = prepare(elements, config)?;
    let mut state = State::new(prepared)?;
    state.prime_currents()?;
    state.record(0.0);
    for step in 1..=state.steps {
        let t = step as f64 * config.h;
        state.newton(t, config)?;
        state.accept(config.h);
        state.record(t);
    }
    Ok(state.solution())
}

fn alpha() -> f64 {
    PHI0 / (2.0 * PI)
}

struct Prepared {
    names: Vec<String>,
    devices: Vec<Device>,
    n_nodes: usize,
    n_vs: usize,
    steps: usize,
    watch_nodes: Vec<(String, Option<usize>)>,
    watch_jjs: Vec<(String, Option<usize>, Option<usize>)>,
}

struct Device {
    kind: Kind,
}

enum Kind {
    R {
        a: Option<usize>,
        b: Option<usize>,
        ohms: f64,
    },
    L {
        a: Option<usize>,
        b: Option<usize>,
        henries: f64,
    },
    C {
        a: Option<usize>,
        b: Option<usize>,
        farads: f64,
        i_prev: f64,
    },
    I {
        name: String,
        a: Option<usize>,
        b: Option<usize>,
        wave: Wave,
    },
    V {
        name: String,
        a: Option<usize>,
        b: Option<usize>,
        wave: Wave,
        branch: usize,
    },
    J {
        a: Option<usize>,
        b: Option<usize>,
        jj: Jj,
        i_cap: f64,
    },
}

struct State {
    prep: Prepared,
    /// Accepted node phases [rad].
    phi: Vec<f64>,
    /// Accepted node voltages [V].
    v: Vec<f64>,
    /// Newton unknown: phases, then voltage-source currents.
    x: Vec<f64>,
    symbolic: Option<SymbolicLu<usize>>,
    node_traces: Vec<Trace>,
    jj_traces: Vec<Trace>,
    steps: usize,
}

impl State {
    fn new(prep: Prepared) -> Result<Self, SolveError> {
        let n_nodes = prep.n_nodes;
        let n = n_nodes + prep.n_vs;
        let mut v = vec![0.0; n_nodes];
        seed_source_voltages(&prep.devices, &mut v)?;
        let samples = prep.steps + 1;
        let node_traces = prep
            .watch_nodes
            .iter()
            .map(|_| empty_trace(samples))
            .collect();
        let jj_traces = prep
            .watch_jjs
            .iter()
            .map(|_| empty_trace(samples))
            .collect();
        Ok(Self {
            phi: vec![0.0; n_nodes],
            v,
            x: vec![0.0; n],
            symbolic: None,
            node_traces,
            jj_traces,
            steps: prep.steps,
            prep,
        })
    }

    fn newton(&mut self, t: f64, config: &SolveConfig) -> Result<(), SolveError> {
        let n = self.x.len();
        if n == 0 {
            return Ok(());
        }
        let t_prev = t - config.h;
        let mut blame = self
            .prep
            .names
            .first()
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        for _ in 0..config.max_newton {
            let system = assemble(self, t, t_prev, config.h)?;
            let delta = solve_delta(n, &system.triplets, &system.residual, &mut self.symbolic, t)?;
            let (phase_over, current_over, who) = corrections(&delta, &self.prep, config);
            blame = who;
            for (slot, change) in self.x.iter_mut().zip(&delta) {
                *slot += *change;
            }
            if !phase_over && !current_over {
                return Ok(());
            }
        }
        Err(SolveError::Diverged {
            time_s: t,
            node: blame,
        })
    }

    /// Sets each capacitor current so the first trapezoidal step sees i(0).
    ///
    /// `i = C du/dt`. `du/dt` is the node-voltage rate that satisfies KCL at the
    /// stored voltages, with a source's rate equal to its waveform derivative.
    /// Leaving `i(0)` at 0 when the source is already on makes that first step
    /// first-order.
    fn prime_currents(&mut self) -> Result<(), SolveError> {
        let dynamic = self.prep.devices.iter().any(|device| match &device.kind {
            Kind::C { farads, .. } => *farads > 0.0,
            Kind::J { jj, .. } => jj.c > 0.0,
            _ => false,
        });
        if !dynamic || self.x.is_empty() {
            return Ok(());
        }
        let n = self.x.len();
        let mut rate = vec![0.0; n];
        let mut symbolic = None;
        for _ in 0..4 {
            let system = assemble_rate(self, &rate)?;
            let delta = solve_delta(n, &system.triplets, &system.residual, &mut symbolic, 0.0)?;
            for (slot, change) in rate.iter_mut().zip(&delta) {
                *slot += *change;
            }
        }
        for device in &mut self.prep.devices {
            match &mut device.kind {
                Kind::C {
                    a,
                    b,
                    farads,
                    i_prev,
                } => {
                    *i_prev = *farads * (phase_of(&rate, *a) - phase_of(&rate, *b));
                }
                Kind::J { a, b, jj, i_cap } => {
                    *i_cap = jj.c * (phase_of(&rate, *a) - phase_of(&rate, *b));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn accept(&mut self, h: f64) {
        let n_nodes = self.prep.n_nodes;
        let scale = 2.0 * alpha() / h;
        let mut v_new = vec![0.0; n_nodes];
        for (i, slot) in v_new.iter_mut().enumerate() {
            // v_n [V] = (2α/h) (φ_n − φ_{n−1}) − v_{n−1}.
            *slot = scale * (self.x[i] - self.phi[i]) - self.v[i];
        }
        for device in &mut self.prep.devices {
            match &mut device.kind {
                Kind::C {
                    a,
                    b,
                    farads,
                    i_prev,
                } => {
                    *i_prev = capacitor_current(
                        Ends { a: *a, b: *b },
                        *farads,
                        &Step {
                            x: &self.x,
                            phi: &self.phi,
                            v: &self.v,
                            h,
                        },
                        *i_prev,
                    );
                }
                Kind::J { a, b, jj, i_cap } => {
                    *i_cap = capacitor_current(
                        Ends { a: *a, b: *b },
                        jj.c,
                        &Step {
                            x: &self.x,
                            phi: &self.phi,
                            v: &self.v,
                            h,
                        },
                        *i_cap,
                    );
                }
                _ => {}
            }
        }
        self.phi.copy_from_slice(&self.x[..n_nodes]);
        self.v = v_new;
    }

    fn record(&mut self, t: f64) {
        for (slot, (_, node)) in self.node_traces.iter_mut().zip(&self.prep.watch_nodes) {
            let (phase, voltage) = match node {
                Some(i) => (self.phi[*i], self.v[*i]),
                None => (0.0, 0.0),
            };
            push_sample(slot, t, phase, voltage);
        }
        for (slot, (_, a, b)) in self.jj_traces.iter_mut().zip(&self.prep.watch_jjs) {
            push_sample(
                slot,
                t,
                phase_of(&self.phi, *a) - phase_of(&self.phi, *b),
                voltage_of(&self.v, *a) - voltage_of(&self.v, *b),
            );
        }
    }

    fn solution(self) -> Solution {
        let mut nodes = BTreeMap::new();
        for ((name, _), trace) in self.prep.watch_nodes.into_iter().zip(self.node_traces) {
            nodes.insert(name, trace);
        }
        let mut junctions = BTreeMap::new();
        for ((name, _, _), trace) in self.prep.watch_jjs.into_iter().zip(self.jj_traces) {
            junctions.insert(name, trace);
        }
        Solution { nodes, junctions }
    }
}

fn prepare(elements: &[Element], config: &SolveConfig) -> Result<Prepared, SolveError> {
    if !config.h.is_finite() || config.h <= 0.0 {
        return Err(input("step must be finite and positive"));
    }
    if !config.t_stop.is_finite() || config.t_stop < 0.0 {
        return Err(input("stop time must be finite and >= 0"));
    }
    if !(config.phase_tol.is_finite() && config.phase_tol > 0.0) {
        return Err(input("phase tolerance must be finite and positive"));
    }
    if !(config.current_tol.is_finite() && config.current_tol > 0.0) {
        return Err(input("current tolerance must be finite and positive"));
    }
    if !alpha().is_finite() || !(alpha() / config.h).is_finite() {
        return Err(input("step is too small for the phase scaling"));
    }
    let steps_f = (config.t_stop / config.h).round();
    if (steps_f * config.h - config.t_stop).abs() > 1e-6 * config.h {
        return Err(input("stop time is not an integer number of steps"));
    }
    let steps = steps_f as usize;

    let mut node_names = BTreeMap::new();
    for element in elements {
        reject_unsupported(element)?;
        for name in terminals(element) {
            if !is_ground(name) {
                node_names.entry(name.to_string()).or_insert(0);
            }
        }
    }
    let mut names: Vec<String> = node_names.keys().cloned().collect();
    names.sort();
    for (index, name) in names.iter().enumerate() {
        node_names.insert(name.clone(), index);
    }
    let n_nodes = names.len();

    let mut devices = Vec::new();
    let mut n_vs = 0;
    let mut jj_at: BTreeMap<String, (Option<usize>, Option<usize>)> = BTreeMap::new();
    for element in elements {
        match element {
            Element::K { .. } | Element::Tx(_) => return Err(unsupported(element)),
            Element::R { name, n1, n2, ohms } => {
                positive(name, *ohms, "resistance")?;
                devices.push(dev(Kind::R {
                    a: port(&node_names, n1)?,
                    b: port(&node_names, n2)?,
                    ohms: *ohms,
                }));
            }
            Element::L {
                name,
                n1,
                n2,
                henries,
            } => {
                positive(name, *henries, "inductance")?;
                devices.push(dev(Kind::L {
                    a: port(&node_names, n1)?,
                    b: port(&node_names, n2)?,
                    henries: *henries,
                }));
            }
            Element::C {
                name,
                n1,
                n2,
                farads,
            } => {
                nonnegative(name, *farads, "capacitance")?;
                devices.push(dev(Kind::C {
                    a: port(&node_names, n1)?,
                    b: port(&node_names, n2)?,
                    farads: *farads,
                    i_prev: 0.0,
                }));
            }
            Element::I { name, n1, n2, wave } => {
                devices.push(dev(Kind::I {
                    name: name.clone(),
                    a: port(&node_names, n1)?,
                    b: port(&node_names, n2)?,
                    wave: wave.clone(),
                }));
            }
            Element::V { name, n1, n2, wave } => {
                let a = port(&node_names, n1)?;
                let b = port(&node_names, n2)?;
                if a.is_none() && b.is_none() {
                    return Err(SolveError::Element {
                        name: name.clone(),
                        message: "voltage source has no free node".to_string(),
                    });
                }
                let branch = n_vs;
                n_vs += 1;
                devices.push(dev(Kind::V {
                    name: name.clone(),
                    a,
                    b,
                    wave: wave.clone(),
                    branch,
                }));
            }
            Element::Jj(jj) => {
                check_jj(jj)?;
                let a = port(&node_names, &jj.n1)?;
                let b = port(&node_names, &jj.n2)?;
                jj_at.insert(jj.name.clone(), (a, b));
                devices.push(dev(Kind::J {
                    a,
                    b,
                    jj: jj.clone(),
                    i_cap: 0.0,
                }));
            }
        }
    }

    let watch_nodes = config
        .nodes
        .iter()
        .map(|name| {
            let node = if is_ground(name) {
                None
            } else {
                Some(*node_names.get(name).ok_or_else(|| SolveError::Input {
                    message: format!("unknown node {name}"),
                })?)
            };
            Ok((name.clone(), node))
        })
        .collect::<Result<Vec<_>, SolveError>>()?;
    let watch_jjs = config
        .junctions
        .iter()
        .map(|name| {
            let (a, b) = jj_at.get(name).ok_or_else(|| SolveError::Input {
                message: format!("unknown junction {name}"),
            })?;
            Ok((name.clone(), *a, *b))
        })
        .collect::<Result<Vec<_>, SolveError>>()?;

    Ok(Prepared {
        names,
        devices,
        n_nodes,
        n_vs,
        steps,
        watch_nodes,
        watch_jjs,
    })
}

/// Jacobian triplets and the residual they belong to.
struct LinearSystem {
    triplets: Vec<Triplet<usize, usize, f64>>,
    residual: Vec<f64>,
}

/// KCL at the initial voltages. Unknowns are `du/dt` [V/s] and source currents.
fn assemble_rate(state: &State, rate: &[f64]) -> Result<LinearSystem, SolveError> {
    let mut residual = vec![0.0; rate.len()];
    let mut triplets = Vec::new();
    for device in &state.prep.devices {
        match &device.kind {
            Kind::R { a, b, ohms } => {
                let current = (voltage_of(&state.v, *a) - voltage_of(&state.v, *b)) / *ohms;
                stamp(
                    &mut triplets,
                    &mut residual,
                    *a,
                    *b,
                    Companion {
                        current,
                        slope: 0.0,
                        pattern: false,
                    },
                );
            }
            Kind::L { a, b, henries } => {
                // I_L = (α/L) δ at the initial phase, independent of du/dt.
                let current =
                    (alpha() / *henries) * (phase_of(&state.phi, *a) - phase_of(&state.phi, *b));
                stamp(
                    &mut triplets,
                    &mut residual,
                    *a,
                    *b,
                    Companion {
                        current,
                        slope: 0.0,
                        pattern: false,
                    },
                );
            }
            Kind::C { a, b, farads, .. } => {
                // i [A] = C [F] · (u_a − u_b) [V/s].
                if *farads == 0.0 {
                    continue;
                }
                let slope = *farads;
                stamp(
                    &mut triplets,
                    &mut residual,
                    *a,
                    *b,
                    Companion {
                        current: slope * (phase_of(rate, *a) - phase_of(rate, *b)),
                        slope,
                        pattern: false,
                    },
                );
            }
            Kind::I { name, a, b, wave } => {
                let current = wave::at(name, wave, 0.0)?;
                if let Some(i) = a {
                    residual[*i] += current;
                }
                if let Some(i) = b {
                    residual[*i] -= current;
                }
            }
            Kind::V {
                name,
                a,
                b,
                wave,
                branch,
            } => {
                let a = *a;
                let b = *b;
                let row = state.prep.n_nodes + *branch;
                let predicted = wave::deriv(name, wave, 0.0)?;
                residual[row] = (phase_of(rate, a) - phase_of(rate, b)) - predicted;
                if let Some(i) = a {
                    triplets.push(Triplet::new(row, i, 1.0));
                    residual[i] += rate[row];
                    triplets.push(Triplet::new(i, row, 1.0));
                }
                if let Some(j) = b {
                    triplets.push(Triplet::new(row, j, -1.0));
                    residual[j] -= rate[row];
                    triplets.push(Triplet::new(j, row, -1.0));
                }
            }
            Kind::J { a, b, jj, .. } => {
                let delta = phase_of(&state.phi, *a) - phase_of(&state.phi, *b);
                let voltage = voltage_of(&state.v, *a) - voltage_of(&state.v, *b);
                let Qp { g, it } = jj::quasiparticle(jj, voltage)?;
                let cap = jj.c * (phase_of(rate, *a) - phase_of(rate, *b));
                let current = jj.ic * (delta - jj.phi).sin() + g * voltage + it + cap;
                stamp(
                    &mut triplets,
                    &mut residual,
                    *a,
                    *b,
                    Companion {
                        current,
                        slope: jj.c,
                        pattern: jj.c != 0.0,
                    },
                );
            }
        }
    }
    Ok(LinearSystem { triplets, residual })
}

fn assemble(state: &State, t: f64, t_prev: f64, h: f64) -> Result<LinearSystem, SolveError> {
    let n = state.x.len();
    let mut residual = vec![0.0; n];
    let mut triplets = Vec::new();
    for device in &state.prep.devices {
        match &device.kind {
            Kind::R { a, b, ohms } => {
                let comp = resistor(*a, *b, *ohms, &state.x, &state.phi, &state.v, h);
                stamp(&mut triplets, &mut residual, *a, *b, comp);
            }
            Kind::L { a, b, henries } => {
                let comp = inductor(*a, *b, *henries, &state.x);
                stamp(&mut triplets, &mut residual, *a, *b, comp);
            }
            Kind::C {
                a,
                b,
                farads,
                i_prev,
            } => {
                let comp = capacitor(
                    Ends { a: *a, b: *b },
                    *farads,
                    &Step {
                        x: &state.x,
                        phi: &state.phi,
                        v: &state.v,
                        h,
                    },
                    *i_prev,
                );
                stamp(&mut triplets, &mut residual, *a, *b, comp);
            }
            Kind::I { name, a, b, wave } => {
                // Positive waveform current leaves n1 and enters n2.
                let current = wave::at(name, wave, t)?;
                if let Some(i) = a {
                    residual[*i] += current;
                }
                if let Some(i) = b {
                    residual[*i] -= current;
                }
            }
            Kind::V {
                name,
                a,
                b,
                wave,
                branch,
            } => {
                stamp_voltage(
                    &mut triplets,
                    &mut residual,
                    Ends { a: *a, b: *b },
                    state.prep.n_nodes + *branch,
                    state.x[state.prep.n_nodes + *branch],
                    &Step {
                        x: &state.x,
                        phi: &state.phi,
                        v: &state.v,
                        h,
                    },
                    Source {
                        now: wave::at(name, wave, t)?,
                        prev: wave::at(name, wave, t_prev)?,
                    },
                );
            }
            Kind::J { a, b, jj, i_cap } => {
                let comp = junction(
                    Ends { a: *a, b: *b },
                    jj,
                    &Step {
                        x: &state.x,
                        phi: &state.phi,
                        v: &state.v,
                        h,
                    },
                    *i_cap,
                )?;
                stamp(&mut triplets, &mut residual, *a, *b, comp);
            }
        }
    }
    Ok(LinearSystem { triplets, residual })
}

/// `i` leaves `a` and enters `b`. `slope` is `di / d(φ_a − φ_b)` [A/rad].
///
/// `pattern` keeps the node block when `slope` is 0. A junction needs that:
/// `cos` can cross zero between Newton iterations, and the symbolic factor is
/// reused only while the sparsity pattern stays put.
struct Companion {
    current: f64,
    slope: f64,
    pattern: bool,
}

/// `i = v / R`, `v = (2α/h)(δ − δ_prev) − v_prev`. Conductance `2α/(R h)`.
fn resistor(
    a: Option<usize>,
    b: Option<usize>,
    ohms: f64,
    x: &[f64],
    phi: &[f64],
    v: &[f64],
    h: f64,
) -> Companion {
    let slope = 2.0 * alpha() / (ohms * h);
    let delta = phase_of(x, a) - phase_of(x, b);
    let delta_prev = phase_of(phi, a) - phase_of(phi, b);
    let v_prev = voltage_of(v, a) - voltage_of(v, b);
    Companion {
        current: slope * (delta - delta_prev) - v_prev / ohms,
        slope,
        pattern: false,
    }
}

/// `I_L = (Φ₀/(2π L)) (φ_a − φ_b) = (α/L) δ`. Algebraic, as in JoSIM phase mode.
fn inductor(a: Option<usize>, b: Option<usize>, henries: f64, x: &[f64]) -> Companion {
    let slope = alpha() / henries;
    Companion {
        current: slope * (phase_of(x, a) - phase_of(x, b)),
        slope,
        pattern: false,
    }
}

/// Device terminals. Ground is `None`.
struct Ends {
    a: Option<usize>,
    b: Option<usize>,
}

/// Unknowns and history for one step. `x` is the current Newton guess.
struct Step<'a> {
    x: &'a [f64],
    phi: &'a [f64],
    v: &'a [f64],
    h: f64,
}

/// Voltage-source samples at the new time and the previous time [V].
struct Source {
    now: f64,
    prev: f64,
}

/// Trapezoidal capacitor. JoSIM's phase capacitor (`src/Capacitor.cpp`) is Gear.
///
/// `i_n = (2C/h) v_n − (2C/h) v_prev − i_prev`,
/// `v_n = (2α/h)(δ − δ_prev) − v_prev`,
/// so `i_n = (4αC/h²)(δ − δ_prev) − (4C/h) v_prev − i_prev`.
fn capacitor(ends: Ends, farads: f64, step: &Step<'_>, i_prev: f64) -> Companion {
    if farads == 0.0 {
        return Companion {
            current: 0.0,
            slope: 0.0,
            pattern: false,
        };
    }
    let slope = 4.0 * alpha() * farads / (step.h * step.h);
    Companion {
        current: capacitor_current(ends, farads, step, i_prev),
        slope,
        pattern: false,
    }
}

fn capacitor_current(ends: Ends, farads: f64, step: &Step<'_>, i_prev: f64) -> f64 {
    if farads == 0.0 {
        return 0.0;
    }
    let slope = 4.0 * alpha() * farads / (step.h * step.h);
    let delta = phase_of(step.x, ends.a) - phase_of(step.x, ends.b);
    let delta_prev = phase_of(step.phi, ends.a) - phase_of(step.phi, ends.b);
    let v_prev = voltage_of(step.v, ends.a) - voltage_of(step.v, ends.b);
    slope * (delta - delta_prev) - (4.0 * farads / step.h) * v_prev - i_prev
}

/// `i = I_c sin(δ − φ_off) + G v + i_t + i_C`.
///
/// Slope `I_c cos(δ − φ_off) + G (2α/h) + 4αC/h²`. `G` and `i_t` come from
/// [`jj::quasiparticle`] at the trapezoidal voltage and are not differentiated.
fn junction(ends: Ends, jj: &Jj, step: &Step<'_>, i_cap: f64) -> Result<Companion, SolveError> {
    let delta = phase_of(step.x, ends.a) - phase_of(step.x, ends.b);
    let delta_prev = phase_of(step.phi, ends.a) - phase_of(step.phi, ends.b);
    let v_prev = voltage_of(step.v, ends.a) - voltage_of(step.v, ends.b);
    // v [V] = (2α/h)(δ − δ_prev) − v_prev.
    let voltage = (2.0 * alpha() / step.h) * (delta - delta_prev) - v_prev;
    let Qp { g, it } = jj::quasiparticle(jj, voltage)?;
    let i_c = capacitor_current(ends, jj.c, step, i_cap);
    let angle = delta - jj.phi;
    let current = jj.ic * angle.sin() + g * voltage + it + i_c;
    let cap_slope = if jj.c == 0.0 {
        0.0
    } else {
        4.0 * alpha() * jj.c / (step.h * step.h)
    };
    let slope = jj.ic * angle.cos() + g * (2.0 * alpha() / step.h) + cap_slope;
    Ok(Companion {
        current,
        slope,
        pattern: true,
    })
}

fn stamp(
    triplets: &mut Vec<Triplet<usize, usize, f64>>,
    residual: &mut [f64],
    a: Option<usize>,
    b: Option<usize>,
    companion: Companion,
) {
    // KCL: current leaves a and enters b. A structurally zero slope adds no
    // matrix entry, so a missing capacitance does not change the pattern.
    if companion.current != 0.0 {
        if let Some(i) = a {
            residual[i] += companion.current;
        }
        if let Some(j) = b {
            residual[j] -= companion.current;
        }
    }
    if companion.slope == 0.0 && !companion.pattern {
        return;
    }
    if let Some(i) = a {
        triplets.push(Triplet::new(i, i, companion.slope));
        if let Some(j) = b {
            triplets.push(Triplet::new(i, j, -companion.slope));
        }
    }
    if let Some(j) = b {
        triplets.push(Triplet::new(j, j, companion.slope));
        if let Some(i) = a {
            triplets.push(Triplet::new(j, i, -companion.slope));
        }
    }
}

/// Trapezoidal voltage constraint, written in volts:
/// `(2α/h)(δ − δ_prev) − (V(t_n) + V(t_{n−1})) = 0`.
/// The branch current is a KCL unknown and does not appear in this row.
fn stamp_voltage(
    triplets: &mut Vec<Triplet<usize, usize, f64>>,
    residual: &mut [f64],
    ends: Ends,
    branch: usize,
    current: f64,
    step: &Step<'_>,
    source: Source,
) {
    let scale = 2.0 * alpha() / step.h;
    let delta = phase_of(step.x, ends.a) - phase_of(step.x, ends.b);
    let delta_prev = phase_of(step.phi, ends.a) - phase_of(step.phi, ends.b);
    residual[branch] = scale * (delta - delta_prev) - (source.now + source.prev);
    if let Some(i) = ends.a {
        triplets.push(Triplet::new(branch, i, scale));
        residual[i] += current;
        triplets.push(Triplet::new(i, branch, 1.0));
    }
    if let Some(j) = ends.b {
        triplets.push(Triplet::new(branch, j, -scale));
        residual[j] -= current;
        triplets.push(Triplet::new(j, branch, -1.0));
    }
}

fn solve_delta(
    n: usize,
    triplets: &[Triplet<usize, usize, f64>],
    residual: &[f64],
    symbolic: &mut Option<SymbolicLu<usize>>,
    time_s: f64,
) -> Result<Vec<f64>, SolveError> {
    let mat = SparseColMat::<usize, f64>::try_new_from_triplets(n, n, triplets).map_err(|err| {
        SolveError::Input {
            message: format!("sparse matrix: {err}"),
        }
    })?;
    let sym = if let Some(sym) = symbolic {
        sym.clone()
    } else {
        let sym =
            SymbolicLu::<usize>::try_new(mat.symbolic()).map_err(|err| SolveError::Input {
                message: format!("symbolic factorization: {err}"),
            })?;
        *symbolic = Some(sym.clone());
        sym
    };
    let lu = Lu::<usize, f64>::try_new_with_symbolic(sym, mat.as_ref())
        .map_err(|_| SolveError::Singular { time_s })?;
    let rhs = faer::Mat::from_fn(n, 1, |row, _| -residual[row]);
    let delta = lu.solve(&rhs);
    let mut out = Vec::with_capacity(n);
    for row in 0..n {
        let value = delta[(row, 0)];
        if !value.is_finite() {
            return Err(SolveError::Singular { time_s });
        }
        out.push(value);
    }
    Ok(out)
}

/// `(phase still over tolerance, current still over tolerance, worst unknown)`.
fn corrections(delta: &[f64], prep: &Prepared, config: &SolveConfig) -> (bool, bool, String) {
    let mut worst_phase = 0.0;
    let mut phase_name = prep
        .names
        .first()
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());
    for (index, name) in prep.names.iter().enumerate() {
        let change = delta[index].abs();
        if change >= worst_phase {
            worst_phase = change;
            phase_name = name.clone();
        }
    }
    let mut worst_current = 0.0;
    let mut current_name = phase_name.clone();
    for device in &prep.devices {
        if let Kind::V { name, branch, .. } = &device.kind {
            let change = delta[prep.n_nodes + *branch].abs();
            if change >= worst_current {
                worst_current = change;
                current_name = name.clone();
            }
        }
    }
    let phase_over = worst_phase >= config.phase_tol;
    let current_over = worst_current >= config.current_tol;
    let blame = if worst_current / config.current_tol > worst_phase / config.phase_tol {
        current_name
    } else {
        phase_name
    };
    (phase_over, current_over, blame)
}

fn seed_source_voltages(devices: &[Device], v: &mut [f64]) -> Result<(), SolveError> {
    for device in devices {
        let Kind::V {
            name, a, b, wave, ..
        } = &device.kind
        else {
            continue;
        };
        let target = wave::at(name, wave, 0.0)?;
        let va = voltage_of(v, *a);
        let vb = voltage_of(v, *b);
        let deficit = target - (va - vb);
        if let Some(i) = a {
            v[*i] += deficit;
        } else if let Some(i) = b {
            v[*i] -= deficit;
        }
    }
    Ok(())
}

fn reject_unsupported(element: &Element) -> Result<(), SolveError> {
    match element {
        Element::K { .. } | Element::Tx(_) => Err(unsupported(element)),
        _ => Ok(()),
    }
}

fn unsupported(element: &Element) -> SolveError {
    match element {
        Element::K { name, .. } => SolveError::Element {
            name: name.clone(),
            message: "mutual inductance is not in the Mode B solver".to_string(),
        },
        Element::Tx(tx) => SolveError::Element {
            name: tx.name.clone(),
            message: "transmission lines are not in the Mode B solver".to_string(),
        },
        _ => input("internal"),
    }
}

fn check_jj(jj: &Jj) -> Result<(), SolveError> {
    if !jj.ic.is_finite() {
        return Err(bad_jj(jj, "critical current must be finite"));
    }
    if !jj.c.is_finite() || jj.c < 0.0 {
        return Err(bad_jj(jj, "capacitance must be finite and >= 0"));
    }
    if !jj.phi.is_finite() {
        return Err(bad_jj(jj, "phase offset must be finite"));
    }
    // Resistances, gap, and ICFCT are checked when the arm is evaluated.
    jj::quasiparticle(jj, 0.0).map(|_| ())
}

fn terminals(element: &Element) -> Vec<&str> {
    match element {
        Element::R { n1, n2, .. }
        | Element::L { n1, n2, .. }
        | Element::C { n1, n2, .. }
        | Element::I { n1, n2, .. }
        | Element::V { n1, n2, .. } => vec![n1, n2],
        Element::Jj(jj) => vec![&jj.n1, &jj.n2],
        Element::K { .. } => Vec::new(),
        Element::Tx(tx) => vec![&tx.n1, &tx.n2, &tx.n3, &tx.n4],
    }
}

fn is_ground(name: &str) -> bool {
    name == "0" || name.eq_ignore_ascii_case("GND")
}

fn port(nodes: &BTreeMap<String, usize>, name: &str) -> Result<Option<usize>, SolveError> {
    if is_ground(name) {
        Ok(None)
    } else {
        Ok(Some(*nodes.get(name).ok_or_else(|| input("missing node"))?))
    }
}

fn phase_of(phi: &[f64], node: Option<usize>) -> f64 {
    node.map(|i| phi[i]).unwrap_or(0.0)
}

fn voltage_of(v: &[f64], node: Option<usize>) -> f64 {
    node.map(|i| v[i]).unwrap_or(0.0)
}

fn dev(kind: Kind) -> Device {
    Device { kind }
}

fn empty_trace(samples: usize) -> Trace {
    Trace {
        time_s: Vec::with_capacity(samples),
        phase: Vec::with_capacity(samples),
        voltage: Vec::with_capacity(samples),
    }
}

fn push_sample(trace: &mut Trace, t: f64, phase: f64, voltage: f64) {
    trace.time_s.push(t);
    trace.phase.push(phase);
    trace.voltage.push(voltage);
}

fn positive(name: &str, value: f64, what: &str) -> Result<(), SolveError> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(SolveError::Element {
            name: name.to_string(),
            message: format!("{what} must be finite and positive"),
        })
    }
}

fn nonnegative(name: &str, value: f64, what: &str) -> Result<(), SolveError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(SolveError::Element {
            name: name.to_string(),
            message: format!("{what} must be finite and >= 0"),
        })
    }
}

fn bad_jj(jj: &Jj, message: &str) -> SolveError {
    SolveError::Element {
        name: jj.name.clone(),
        message: message.to_string(),
    }
}

fn input(message: &str) -> SolveError {
    SolveError::Input {
        message: message.to_string(),
    }
}
