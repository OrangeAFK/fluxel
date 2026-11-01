//! Design runner: a binary heap, constraint checks, and snapshot rewind.
//!
//! Wire delay is 0. A behavioral cell uses its constant delay. Any other cell
//! uses its LUT at the bias fraction in effect when the input is applied.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};

use fx_ir::cell::{CellDef, Provenance};
use fx_ir::design::{Design, PinRef};
use fx_ir::lut::LutBundle;
use fx_ir::stimulus::Stimulus;
use fx_time::Femtos;

use crate::log::EventLog;
use crate::{
    Applied, ApplyRequest, KernelError, LogEvent, ViolationPolicy, apply_input, constants_ready,
};

/// One fanout-1 design. `seek` restores a snapshot and replays events at times `<= t`.
#[derive(Debug)]
pub struct Simulator {
    design: Design,
    cells: BTreeMap<String, CellDef>,
    luts: BTreeMap<String, LutBundle>,
    bias: BTreeMap<String, f64>,
    policy: ViolationPolicy,
    snapshot_every: u64,
    instances: BTreeMap<String, InstanceRun>,
    queue: BinaryHeap<Queued>,
    seq: u64,
    log: EventLog,
    halted: bool,
    steps: u64,
    covered_until: Option<Femtos>,
    snapshots: Vec<Snapshot>,
}

#[derive(Debug, Clone)]
struct InstanceRun {
    state: String,
    arrivals: BTreeMap<String, Femtos>,
    last_input: Option<Femtos>,
}

#[derive(Debug, Clone)]
struct Snapshot {
    steps: u64,
    covered_until: Option<Femtos>,
    instances: BTreeMap<String, InstanceRun>,
    queue: Vec<Queued>,
    seq: u64,
    log_len: usize,
    halted: bool,
    bias: BTreeMap<String, f64>,
}

/// Heap order is the reverse of `(t, target, port, seq)`, so the earliest event pops first.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Queued {
    t: Femtos,
    target: String,
    port: String,
    seq: u64,
}

impl Queued {
    fn key(&self) -> (Femtos, &str, &str, u64) {
        (self.t, &self.target, &self.port, self.seq)
    }
}

impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Queued {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key().cmp(&other.key()).reverse()
    }
}

impl Simulator {
    /// Queue `stimulus` and take the initial snapshot. `snapshot_every` is at least 1.
    ///
    /// Each bias domain starts at fraction `1.0`. A behavioral cell keeps its
    /// constant delays. Any other cell reads `luts`.
    pub fn new(
        design: Design,
        cells: BTreeMap<String, CellDef>,
        luts: BTreeMap<String, LutBundle>,
        stimulus: Stimulus,
        policy: ViolationPolicy,
        snapshot_every: u64,
    ) -> Result<Self, KernelError> {
        if snapshot_every == 0 {
            return Err(KernelError::BadSnapshotInterval);
        }
        let mut instances = BTreeMap::new();
        for (id, instance) in &design.instances {
            let Some(cell) = cells.get(&instance.cell) else {
                return Err(KernelError::UnknownCell {
                    name: instance.cell.clone(),
                });
            };
            if cell.provenance == Provenance::Behavioral {
                if !constants_ready(cell) {
                    return Err(KernelError::NeedsConstantDelays);
                }
            } else {
                let Some(bundle) = luts.get(&instance.cell) else {
                    return Err(KernelError::MissingLut {
                        cell: instance.cell.clone(),
                    });
                };
                for name in timing_names(cell) {
                    if !bundle.entries.contains_key(&name) {
                        return Err(KernelError::MissingLutEntry {
                            cell: instance.cell.clone(),
                            name,
                        });
                    }
                }
            }
            instances.insert(
                id.clone(),
                InstanceRun {
                    state: cell.initial.clone(),
                    arrivals: BTreeMap::new(),
                    last_input: None,
                },
            );
        }
        let bias = design
            .bias_domains
            .iter()
            .map(|domain| (domain.clone(), 1.0))
            .collect();
        let mut sim = Self {
            design,
            cells,
            luts,
            bias,
            policy,
            snapshot_every,
            instances,
            queue: BinaryHeap::new(),
            seq: 0,
            log: EventLog::default(),
            halted: false,
            steps: 0,
            covered_until: None,
            snapshots: Vec::new(),
        };
        sim.enqueue_stimulus(&stimulus)?;
        sim.capture();
        Ok(sim)
    }

    /// Process every queued event.
    pub fn run(&mut self) -> Result<(), KernelError> {
        while !self.halted && !self.queue.is_empty() {
            self.step()?;
        }
        Ok(())
    }

    /// Process queued events whose time is `<= t`.
    pub fn run_until(&mut self, t: Femtos) -> Result<(), KernelError> {
        while !self.halted {
            let Some(next) = self.queue.peek() else {
                break;
            };
            if next.t > t {
                break;
            }
            self.step()?;
        }
        Ok(())
    }

    /// Restore the latest snapshot still at or before `t`, then replay through `t`.
    pub fn seek(&mut self, t: Femtos) -> Result<(), KernelError> {
        let index = self.snapshot_index(t);
        self.restore(index);
        self.run_until(t)
    }

    /// Append-only record. [`Self::seek`] is what truncates it.
    pub fn log(&self) -> &EventLog {
        &self.log
    }

    /// Set one domain's bias fraction. Later evaluations use it. Events already
    /// queued keep the delay they were given. Before the first step, the initial
    /// snapshot is updated so a seek back to the start keeps this fraction.
    pub fn set_bias(&mut self, domain: &str, frac: f64) -> Result<(), KernelError> {
        if !self.design.bias_domains.contains(domain) {
            return Err(KernelError::UnknownBiasDomain {
                domain: domain.to_string(),
            });
        }
        if !frac.is_finite() {
            return Err(KernelError::BadBias);
        }
        self.bias.insert(domain.to_string(), frac);
        if self.steps == 0
            && let Some(snap) = self.snapshots.first_mut()
        {
            snap.bias = self.bias.clone();
        }
        Ok(())
    }

    fn enqueue_stimulus(&mut self, stimulus: &Stimulus) -> Result<(), KernelError> {
        let mut pending = Vec::new();
        for (port, times) in &stimulus.0 {
            let driver = PinRef::Port { port: port.clone() };
            let Some(net) = self.design.nets.iter().find(|net| net.driver == driver) else {
                return Err(KernelError::UnconnectedPort { port: port.clone() });
            };
            let PinRef::Instance {
                instance,
                port: sink,
            } = &net.sink
            else {
                continue;
            };
            for time in times {
                pending.push((*time, port.clone(), instance.clone(), sink.clone()));
            }
        }
        pending.sort();
        for (t, _source, instance, port) in pending {
            self.enqueue(t, instance, port);
        }
        Ok(())
    }

    fn enqueue(&mut self, t: Femtos, target: String, port: String) {
        let seq = self.seq;
        self.seq += 1;
        self.queue.push(Queued {
            t,
            target,
            port,
            seq,
        });
    }

    fn step(&mut self) -> Result<(), KernelError> {
        let Some(event) = self.queue.pop() else {
            return Ok(());
        };
        let at = event.t;
        let outcome = match self.evaluate(&event) {
            Ok(outcome) => outcome,
            Err(err) => {
                self.queue.push(event);
                return Err(err);
            }
        };
        self.commit(event, outcome)?;
        self.steps += 1;
        self.covered_until = Some(at);
        if self.steps.is_multiple_of(self.snapshot_every) {
            self.capture();
        }
        Ok(())
    }

    fn evaluate(&self, event: &Queued) -> Result<Applied, KernelError> {
        let Some(instance) = self.design.instances.get(&event.target) else {
            return Err(KernelError::UnknownInstance {
                instance: event.target.clone(),
            });
        };
        let Some(cell) = self.cells.get(&instance.cell) else {
            return Err(KernelError::UnknownCell {
                name: instance.cell.clone(),
            });
        };
        let Some(run) = self.instances.get(&event.target) else {
            return Err(KernelError::UnknownInstance {
                instance: event.target.clone(),
            });
        };
        let bundle = if cell.provenance == Provenance::Behavioral {
            None
        } else {
            Some(
                self.luts
                    .get(&instance.cell)
                    .ok_or_else(|| KernelError::MissingLut {
                        cell: instance.cell.clone(),
                    })?,
            )
        };
        let bias = self
            .bias
            .get(&instance.bias_domain)
            .copied()
            .ok_or_else(|| KernelError::UnknownBiasDomain {
                domain: instance.bias_domain.clone(),
            })?;
        apply_input(ApplyRequest {
            inst: &event.target,
            cell,
            state: &run.state,
            arrivals: &run.arrivals,
            policy: self.policy,
            port: &event.port,
            t: event.t,
            log_illegal: true,
            last_input: run.last_input,
            bias,
            bundle,
        })
    }

    fn commit(&mut self, event: Queued, applied: Applied) -> Result<(), KernelError> {
        if applied.halt {
            self.log.extend(applied.events);
            self.halted = true;
            return Ok(());
        }
        let fires: Vec<(Femtos, String)> = applied
            .events
            .iter()
            .filter_map(|entry| match entry {
                LogEvent::Fired { t, port, .. } => Some((*t, port.clone())),
                _ => None,
            })
            .collect();
        self.log.extend(applied.events);
        if let Some(run) = self.instances.get_mut(&event.target) {
            if let Some(next) = applied.next_state {
                run.state = next;
            }
            run.arrivals.insert(event.port, event.t);
            run.last_input = Some(event.t);
        }
        for (t, port) in fires {
            self.schedule_output(&event.target, &port, t);
        }
        Ok(())
    }

    fn schedule_output(&mut self, instance: &str, port: &str, t: Femtos) {
        let driver = PinRef::Instance {
            instance: instance.to_string(),
            port: port.to_string(),
        };
        let Some(net) = self.design.nets.iter().find(|net| net.driver == driver) else {
            return;
        };
        if let PinRef::Instance { instance, port } = &net.sink {
            self.enqueue(t, instance.clone(), port.clone());
        }
    }

    fn capture(&mut self) {
        let mut queue: Vec<_> = self.queue.iter().cloned().collect();
        queue.sort_by(|left, right| left.key().cmp(&right.key()));
        self.snapshots.push(Snapshot {
            steps: self.steps,
            covered_until: self.covered_until,
            instances: self.instances.clone(),
            queue,
            seq: self.seq,
            log_len: self.log.len(),
            halted: self.halted,
            bias: self.bias.clone(),
        });
    }

    fn snapshot_index(&self, t: Femtos) -> usize {
        self.snapshots
            .iter()
            .enumerate()
            .rev()
            .find(|(_, snap)| match snap.covered_until {
                None => true,
                Some(until) => until <= t,
            })
            .map(|(index, _)| index)
            .unwrap_or(0)
    }

    fn restore(&mut self, index: usize) {
        let Some(snap) = self.snapshots.get(index).cloned() else {
            return;
        };
        self.steps = snap.steps;
        self.covered_until = snap.covered_until;
        self.instances = snap.instances;
        self.queue = BinaryHeap::from(snap.queue);
        self.seq = snap.seq;
        self.log.truncate(snap.log_len);
        self.halted = snap.halted;
        self.bias = snap.bias;
        self.snapshots.truncate(index + 1);
    }
}

fn timing_names(cell: &CellDef) -> Vec<String> {
    let mut names = Vec::new();
    for transition in &cell.transitions {
        for fire in &transition.fire {
            names.push(fire.delay.clone());
        }
    }
    for constraint in &cell.constraints {
        names.push(constraint.window.clone());
    }
    names
}
