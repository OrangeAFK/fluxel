//! Timing-constraint checks and a rewinding design runner.
//!
//! Separation is `t_then - t_first` femtoseconds. Slack is `separation - window`.
//! A breach is `slack <= 0`: a separation of exactly `window` is a violation.
//! Collision uses the same slack and is logged as information. [`ViolationPolicy::Halt`]
//! stops only on setup or hold.

use std::collections::BTreeMap;

use fx_ir::cell::{CellDef, Constraint, ConstraintKind, Provenance};
use fx_ir::lut::{AxisQuery, LutBundle, LutEntry};
use fx_time::Femtos;
use thiserror::Error;

mod log;
mod sim;

pub use log::{EventLog, LogEvent};
pub use sim::Simulator;

/// `Record` keeps simulating after a breach. `Halt` stops on setup or hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViolationPolicy {
    #[default]
    Record,
    Halt,
}

/// Whether the input was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    Ran,
    Halted,
}

/// Why a kernel step was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum KernelError {
    #[error("constraint checking needs a behavioral cell with constant delays")]
    NeedsConstantDelays,
    #[error("input time went backwards")]
    TimeWentBackwards,
    #[error("femtosecond arithmetic overflowed")]
    Overflow,
    #[error("event log encoding failed: {0}")]
    Cbor(String),
    #[error("unknown cell {name}")]
    UnknownCell { name: String },
    #[error("unknown instance {instance}")]
    UnknownInstance { instance: String },
    #[error("snapshot interval must be at least 1")]
    BadSnapshotInterval,
    #[error("stimulus port {port} is not connected")]
    UnconnectedPort { port: String },
    #[error("cell {cell} has no LUT bundle")]
    MissingLut { cell: String },
    #[error("LUT entry {name} is missing on cell {cell}")]
    MissingLutEntry { cell: String, name: String },
    #[error("unknown bias domain {domain}")]
    UnknownBiasDomain { domain: String },
    #[error("bias fraction is not finite")]
    BadBias,
    #[error("LUT entry {entry} has unknown axis {axis}")]
    UnknownAxis { entry: String, axis: String },
    #[error("LUT lookup failed: {0}")]
    Lut(String),
}

/// One instance, its last input time on each port, and the append-only log.
#[derive(Debug, Clone)]
pub struct Kernel {
    inst: String,
    cell: CellDef,
    policy: ViolationPolicy,
    state: String,
    last_arrival: BTreeMap<String, Femtos>,
    last_input_time: Option<Femtos>,
    log: Vec<LogEvent>,
    halted: bool,
}

pub(crate) struct Applied {
    events: Vec<LogEvent>,
    next_state: Option<String>,
    halt: bool,
}

impl Kernel {
    /// `inst` is copied onto every log entry. `cell` must be behavioral, with every
    /// referenced delay and window present in `delays`.
    pub fn new(
        inst: impl Into<String>,
        cell: CellDef,
        policy: ViolationPolicy,
    ) -> Result<Self, KernelError> {
        if !constants_ready(&cell) {
            return Err(KernelError::NeedsConstantDelays);
        }
        let state = cell.initial.clone();
        Ok(Self {
            inst: inst.into(),
            cell,
            policy,
            state,
            last_arrival: BTreeMap::new(),
            last_input_time: None,
            log: Vec::new(),
            halted: false,
        })
    }

    /// Apply one input at `t`. Inputs must be in non-decreasing time order.
    ///
    /// A setup or hold breach under [`ViolationPolicy::Halt`] is logged and the
    /// input does not fire. Later calls return [`StepOutcome::Halted`] without
    /// appending. Collision never halts.
    pub fn push_input(&mut self, port: &str, t: Femtos) -> Result<StepOutcome, KernelError> {
        if self.halted {
            return Ok(StepOutcome::Halted);
        }
        if let Some(prev) = self.last_input_time
            && t.0 < prev.0
        {
            return Err(KernelError::TimeWentBackwards);
        }
        let applied = apply_input(ApplyRequest {
            inst: &self.inst,
            cell: &self.cell,
            state: &self.state,
            arrivals: &self.last_arrival,
            policy: self.policy,
            port,
            t,
            log_illegal: false,
            last_input: self.last_input_time,
            bias: 1.0,
            bundle: None,
        })?;
        if applied.halt {
            self.log.extend(applied.events);
            self.halted = true;
            return Ok(StepOutcome::Halted);
        }
        self.log.extend(applied.events);
        if let Some(next) = applied.next_state {
            self.state = next;
        }
        self.last_arrival.insert(port.to_string(), t);
        self.last_input_time = Some(t);
        Ok(StepOutcome::Ran)
    }

    /// Append-only record of fired outputs and timing notes.
    pub fn log(&self) -> &[LogEvent] {
        &self.log
    }
}

pub(crate) fn constants_ready(cell: &CellDef) -> bool {
    cell.provenance == Provenance::Behavioral
        && cell.transitions.iter().all(|transition| {
            transition
                .fire
                .iter()
                .all(|fire| cell.delays.contains_key(&fire.delay))
        })
        && cell
            .constraints
            .iter()
            .all(|constraint| cell.delays.contains_key(&constraint.window))
}

pub(crate) struct ApplyRequest<'a> {
    inst: &'a str,
    cell: &'a CellDef,
    state: &'a str,
    arrivals: &'a BTreeMap<String, Femtos>,
    policy: ViolationPolicy,
    port: &'a str,
    t: Femtos,
    log_illegal: bool,
    last_input: Option<Femtos>,
    bias: f64,
    bundle: Option<&'a LutBundle>,
}

pub(crate) fn apply_input(request: ApplyRequest<'_>) -> Result<Applied, KernelError> {
    let ApplyRequest {
        inst,
        cell,
        state,
        arrivals,
        policy,
        port,
        t,
        log_illegal,
        last_input,
        bias,
        bundle,
    } = request;
    let clock = Clock {
        inst,
        cell,
        arrivals,
        port,
        t,
        last_input,
        bias,
        bundle,
    };
    let mut events = breaches(&clock)?;
    let halt = policy == ViolationPolicy::Halt
        && events.iter().any(|event| {
            matches!(
                event,
                LogEvent::SetupViolation { .. } | LogEvent::HoldViolation { .. }
            )
        });
    if halt {
        return Ok(Applied {
            events,
            next_state: None,
            halt: true,
        });
    }
    let Some(transition) = cell
        .transitions
        .iter()
        .find(|transition| transition.from == state && transition.on == port)
    else {
        if log_illegal {
            events.push(LogEvent::IllegalInput {
                t,
                inst: inst.to_string(),
                port: port.to_string(),
            });
        }
        return Ok(Applied {
            events,
            next_state: None,
            halt: false,
        });
    };
    for output in &transition.fire {
        let (delay, notes) = clock.sample(&output.delay)?;
        events.extend(notes);
        let at = checked_add(t, delay)?;
        events.push(LogEvent::Fired {
            t: at,
            inst: inst.to_string(),
            port: output.port.clone(),
        });
    }
    Ok(Applied {
        events,
        next_state: Some(transition.to.clone()),
        halt: false,
    })
}

struct Clock<'a> {
    inst: &'a str,
    cell: &'a CellDef,
    arrivals: &'a BTreeMap<String, Femtos>,
    port: &'a str,
    t: Femtos,
    last_input: Option<Femtos>,
    bias: f64,
    bundle: Option<&'a LutBundle>,
}

impl Clock<'_> {
    /// Delay or window. An out-of-range axis is logged and the value is clamped.
    fn sample(&self, name: &str) -> Result<(Femtos, Vec<LogEvent>), KernelError> {
        let Some(bundle) = self.bundle else {
            return Ok((constant(self.cell, name)?, Vec::new()));
        };
        let entry = bundle
            .entries
            .get(name)
            .ok_or_else(|| KernelError::MissingLutEntry {
                cell: self.cell.name.clone(),
                name: name.to_string(),
            })?;
        let mut query = BTreeMap::new();
        let mut point = BTreeMap::new();
        for axis in &entry.axes {
            let coord = self.coordinate(name, entry, &axis.name)?;
            let units = coord
                .units(&axis.name)
                .map_err(|err| KernelError::Lut(err.to_string()))?;
            point.insert(axis.name.clone(), units);
            query.insert(axis.name.clone(), coord);
        }
        let (value, valid) = bundle
            .lookup(name, &query)
            .map_err(|err| KernelError::Lut(err.to_string()))?;
        let mut notes = Vec::new();
        if !valid {
            notes.push(LogEvent::Uncharacterized {
                t: self.t,
                inst: self.inst.to_string(),
                entry: name.to_string(),
                point,
            });
        }
        Ok((value, notes))
    }

    fn coordinate(
        &self,
        entry_name: &str,
        entry: &LutEntry,
        axis: &str,
    ) -> Result<AxisQuery, KernelError> {
        match axis {
            "bias_frac" => Ok(AxisQuery::Fraction(self.bias)),
            "dt_prev_fs" => Ok(AxisQuery::Time(self.dt_prev(entry)?)),
            "data_clock_offset_fs" => Ok(AxisQuery::Time(self.data_clock()?)),
            "in_in_offset_fs" => Ok(AxisQuery::Time(self.in_in()?)),
            other => Err(KernelError::UnknownAxis {
                entry: entry_name.to_string(),
                axis: other.to_string(),
            }),
        }
    }

    fn dt_prev(&self, entry: &LutEntry) -> Result<Femtos, KernelError> {
        if let Some(prev) = self.last_input {
            return checked_sub(self.t, prev);
        }
        valid_hi(entry, "dt_prev_fs")
    }

    fn data_clock(&self) -> Result<Femtos, KernelError> {
        if self.port != "clk" {
            return Ok(Femtos(0));
        }
        let latest = self
            .arrivals
            .iter()
            .filter(|(name, _)| name.as_str() != "clk")
            .map(|(_, time)| time.0)
            .max();
        match latest {
            Some(prev) => checked_sub(self.t, Femtos(prev)),
            None => Ok(Femtos(0)),
        }
    }

    fn in_in(&self) -> Result<Femtos, KernelError> {
        let a = if self.port == "a" {
            Some(self.t)
        } else {
            self.arrivals.get("a").copied()
        };
        let b = if self.port == "b" {
            Some(self.t)
        } else {
            self.arrivals.get("b").copied()
        };
        match (a, b) {
            (Some(left), Some(right)) => checked_sub(right, left),
            _ => Ok(Femtos(0)),
        }
    }
}

fn breaches(clock: &Clock<'_>) -> Result<Vec<LogEvent>, KernelError> {
    let mut events = Vec::new();
    for constraint in &clock.cell.constraints {
        events.extend(breach(clock, constraint)?);
    }
    Ok(events)
}

fn breach(clock: &Clock<'_>, constraint: &Constraint) -> Result<Vec<LogEvent>, KernelError> {
    let Some(partner) = partner(constraint, clock.port) else {
        return Ok(Vec::new());
    };
    let Some(arrived) = clock.arrivals.get(partner) else {
        return Ok(Vec::new());
    };
    let separation = clock
        .t
        .0
        .checked_sub(arrived.0)
        .ok_or(KernelError::Overflow)?;
    let (window, mut events) = clock.sample(&constraint.window)?;
    let slack = separation
        .checked_sub(window.0)
        .ok_or(KernelError::Overflow)?;
    if slack > 0 {
        return Ok(events);
    }
    let slack = Femtos(slack);
    let t = clock.t;
    let inst = clock.inst.to_string();
    events.push(match constraint.kind {
        ConstraintKind::Setup => LogEvent::SetupViolation { t, inst, slack },
        ConstraintKind::Hold => LogEvent::HoldViolation { t, inst, slack },
        ConstraintKind::Collision => LogEvent::Collision { t, inst, slack },
    });
    Ok(events)
}

fn partner<'a>(constraint: &'a Constraint, port: &str) -> Option<&'a str> {
    match constraint.kind {
        ConstraintKind::Setup | ConstraintKind::Hold => {
            if port == constraint.then {
                Some(constraint.first.as_str())
            } else {
                None
            }
        }
        ConstraintKind::Collision => {
            if port == constraint.then {
                Some(constraint.first.as_str())
            } else if port == constraint.first {
                Some(constraint.then.as_str())
            } else {
                None
            }
        }
    }
}

fn constant(cell: &CellDef, name: &str) -> Result<Femtos, KernelError> {
    cell.delays
        .get(name)
        .copied()
        .ok_or(KernelError::NeedsConstantDelays)
}

fn valid_hi(entry: &LutEntry, axis: &str) -> Result<Femtos, KernelError> {
    let bound = entry
        .valid
        .get(axis)
        .ok_or_else(|| KernelError::Lut(format!("{axis} has no valid box")))?;
    let fs = bound
        .hi
        .parse::<i64>()
        .map_err(|_| KernelError::Lut(format!("bad {axis} bound {}", bound.hi)))?;
    Ok(Femtos(fs))
}

fn checked_add(left: Femtos, right: Femtos) -> Result<Femtos, KernelError> {
    left.0
        .checked_add(right.0)
        .map(Femtos)
        .ok_or(KernelError::Overflow)
}

fn checked_sub(left: Femtos, right: Femtos) -> Result<Femtos, KernelError> {
    left.0
        .checked_sub(right.0)
        .map(Femtos)
        .ok_or(KernelError::Overflow)
}

#[cfg(test)]
mod sim_tests;
#[cfg(test)]
mod tests;
