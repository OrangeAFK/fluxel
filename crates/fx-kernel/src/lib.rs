//! Timing-constraint checks and a rewinding design runner.
//!
//! Separation is `t_then - t_first` femtoseconds. Slack is `separation - window`.
//! A breach is `slack <= 0`: a separation of exactly `window` is a violation.
//! Collision uses the same slack and is logged as information. [`ViolationPolicy::Halt`]
//! stops only on setup or hold.

use std::collections::BTreeMap;

use fx_ir::cell::{CellDef, Constraint, ConstraintKind, Provenance};
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
    } = request;
    let mut events = breaches(inst, cell, arrivals, port, t)?;
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
        let at = checked_add(t, constant(cell, &output.delay)?)?;
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

fn breaches(
    inst: &str,
    cell: &CellDef,
    arrivals: &BTreeMap<String, Femtos>,
    port: &str,
    t: Femtos,
) -> Result<Vec<LogEvent>, KernelError> {
    let mut events = Vec::new();
    for constraint in &cell.constraints {
        if let Some(event) = breach(inst, cell, arrivals, constraint, port, t)? {
            events.push(event);
        }
    }
    Ok(events)
}

fn breach(
    inst: &str,
    cell: &CellDef,
    arrivals: &BTreeMap<String, Femtos>,
    constraint: &Constraint,
    port: &str,
    t: Femtos,
) -> Result<Option<LogEvent>, KernelError> {
    let Some(partner) = partner(constraint, port) else {
        return Ok(None);
    };
    let Some(arrived) = arrivals.get(partner) else {
        return Ok(None);
    };
    let separation = t.0.checked_sub(arrived.0).ok_or(KernelError::Overflow)?;
    let window = constant(cell, &constraint.window)?.0;
    let slack = separation
        .checked_sub(window)
        .ok_or(KernelError::Overflow)?;
    if slack > 0 {
        return Ok(None);
    }
    let slack = Femtos(slack);
    let inst = inst.to_string();
    Ok(Some(match constraint.kind {
        ConstraintKind::Setup => LogEvent::SetupViolation { t, inst, slack },
        ConstraintKind::Hold => LogEvent::HoldViolation { t, inst, slack },
        ConstraintKind::Collision => LogEvent::Collision { t, inst, slack },
    }))
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

fn checked_add(left: Femtos, right: Femtos) -> Result<Femtos, KernelError> {
    left.0
        .checked_add(right.0)
        .map(Femtos)
        .ok_or(KernelError::Overflow)
}

#[cfg(test)]
mod sim_tests;
#[cfg(test)]
mod tests;
