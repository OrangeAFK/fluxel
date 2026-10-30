use std::collections::BTreeMap;

use fx_ir::cell::{CellDef, Constraint, ConstraintKind, Face, Fire, Ports, Provenance, Transition};
use fx_time::Femtos;
use proptest::prelude::*;

use crate::{Kernel, KernelError, LogEvent, StepOutcome, ViolationPolicy};

fn delays(pairs: &[(&str, i64)]) -> BTreeMap<String, Femtos> {
    pairs
        .iter()
        .map(|(name, fs)| ((*name).to_string(), Femtos(*fs)))
        .collect()
}

fn faces(pairs: &[(&str, Face)]) -> BTreeMap<String, Face> {
    pairs
        .iter()
        .map(|(name, face)| ((*name).to_string(), *face))
        .collect()
}

fn dff(t_setup: i64, t_hold: i64) -> CellDef {
    CellDef {
        name: "DFF".into(),
        family: "rsfq".into(),
        provenance: Provenance::Behavioral,
        ports: Ports {
            inputs: vec!["d".into(), "clk".into()],
            outputs: vec!["q".into()],
        },
        faces: faces(&[("d", Face::W), ("clk", Face::S), ("q", Face::E)]),
        states: vec!["empty".into(), "stored".into()],
        initial: "empty".into(),
        transitions: vec![
            Transition {
                from: "empty".into(),
                on: "d".into(),
                to: "stored".into(),
                fire: vec![],
                note: None,
            },
            Transition {
                from: "stored".into(),
                on: "d".into(),
                to: "stored".into(),
                fire: vec![],
                note: Some("second pulse escapes".into()),
            },
            Transition {
                from: "stored".into(),
                on: "clk".into(),
                to: "empty".into(),
                fire: vec![Fire {
                    port: "q".into(),
                    delay: "t_clk_q".into(),
                }],
                note: None,
            },
            Transition {
                from: "empty".into(),
                on: "clk".into(),
                to: "empty".into(),
                fire: vec![],
                note: None,
            },
        ],
        constraints: vec![
            Constraint {
                kind: ConstraintKind::Setup,
                first: "d".into(),
                then: "clk".into(),
                window: "t_setup".into(),
            },
            Constraint {
                kind: ConstraintKind::Hold,
                first: "clk".into(),
                then: "d".into(),
                window: "t_hold".into(),
            },
        ],
        jj_count: Some(10),
        bias_ua: None,
        netlist: None,
        lut: None,
        notes: None,
        delays: delays(&[("t_clk_q", 5000), ("t_setup", t_setup), ("t_hold", t_hold)]),
    }
}

fn merger() -> CellDef {
    CellDef {
        name: "MERGE".into(),
        family: "rsfq".into(),
        provenance: Provenance::Behavioral,
        ports: Ports {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["q".into()],
        },
        faces: faces(&[("a", Face::W), ("b", Face::S), ("q", Face::E)]),
        states: vec!["s".into()],
        initial: "s".into(),
        transitions: vec![
            Transition {
                from: "s".into(),
                on: "a".into(),
                to: "s".into(),
                fire: vec![Fire {
                    port: "q".into(),
                    delay: "d".into(),
                }],
                note: None,
            },
            Transition {
                from: "s".into(),
                on: "b".into(),
                to: "s".into(),
                fire: vec![Fire {
                    port: "q".into(),
                    delay: "d".into(),
                }],
                note: None,
            },
        ],
        constraints: vec![Constraint {
            kind: ConstraintKind::Collision,
            first: "a".into(),
            then: "b".into(),
            window: "w".into(),
        }],
        jj_count: Some(4),
        bias_ua: None,
        netlist: None,
        lut: None,
        notes: None,
        delays: delays(&[("d", 1000), ("w", 500)]),
    }
}

fn kernel(cell: CellDef, policy: ViolationPolicy) -> Kernel {
    Kernel::new("dut", cell, policy).unwrap()
}

fn fired(t: i64, port: &str) -> LogEvent {
    LogEvent::Fired {
        t: Femtos(t),
        inst: "dut".into(),
        port: port.into(),
    }
}

fn setup(t: i64, slack: i64) -> LogEvent {
    LogEvent::SetupViolation {
        t: Femtos(t),
        inst: "dut".into(),
        slack: Femtos(slack),
    }
}

#[test]
fn setup_window_minus_one_is_violation() {
    let mut kernel = kernel(dff(1000, 400), ViolationPolicy::Record);
    kernel.push_input("d", Femtos(0)).unwrap();
    assert_eq!(
        kernel.push_input("clk", Femtos(999)).unwrap(),
        StepOutcome::Ran
    );
    assert_eq!(kernel.log(), &[setup(999, -1), fired(5999, "q")]);
}

#[test]
fn setup_window_exact_is_violation() {
    let mut kernel = kernel(dff(1000, 400), ViolationPolicy::Record);
    kernel.push_input("d", Femtos(0)).unwrap();
    kernel.push_input("clk", Femtos(1000)).unwrap();
    assert_eq!(kernel.log(), &[setup(1000, 0), fired(6000, "q")]);
}

#[test]
fn setup_window_plus_one_ok() {
    let mut kernel = kernel(dff(1000, 400), ViolationPolicy::Record);
    kernel.push_input("d", Femtos(0)).unwrap();
    kernel.push_input("clk", Femtos(1001)).unwrap();
    assert_eq!(kernel.log(), &[fired(6001, "q")]);
}

#[test]
fn setup_skipped_when_data_never_arrived() {
    let mut kernel = kernel(dff(1000, 400), ViolationPolicy::Record);
    kernel.push_input("clk", Femtos(0)).unwrap();
    assert!(kernel.log().is_empty());
}

#[test]
fn setup_uses_latest_data_arrival() {
    let mut kernel = kernel(dff(1000, 400), ViolationPolicy::Record);
    kernel.push_input("d", Femtos(0)).unwrap();
    kernel.push_input("d", Femtos(500)).unwrap();
    kernel.push_input("clk", Femtos(1499)).unwrap();
    assert_eq!(kernel.log(), &[setup(1499, -1), fired(6499, "q")]);
}

#[test]
fn hold_inside_window_is_violation() {
    let mut kernel = kernel(dff(1000, 400), ViolationPolicy::Record);
    kernel.push_input("clk", Femtos(0)).unwrap();
    kernel.push_input("d", Femtos(399)).unwrap();
    assert_eq!(
        kernel.log(),
        &[LogEvent::HoldViolation {
            t: Femtos(399),
            inst: "dut".into(),
            slack: Femtos(-1),
        }]
    );
}

#[test]
fn merger_collision_is_info() {
    let mut kernel = kernel(merger(), ViolationPolicy::Halt);
    kernel.push_input("a", Femtos(0)).unwrap();
    assert_eq!(
        kernel.push_input("b", Femtos(499)).unwrap(),
        StepOutcome::Ran
    );
    assert_eq!(
        kernel.log(),
        &[
            fired(1000, "q"),
            LogEvent::Collision {
                t: Femtos(499),
                inst: "dut".into(),
                slack: Femtos(-1),
            },
            fired(1499, "q"),
        ]
    );
}

#[test]
fn halt_stops_after_setup_violation() {
    let mut kernel = kernel(dff(1000, 400), ViolationPolicy::Halt);
    kernel.push_input("d", Femtos(0)).unwrap();
    assert_eq!(
        kernel.push_input("clk", Femtos(999)).unwrap(),
        StepOutcome::Halted
    );
    assert_eq!(kernel.log(), &[setup(999, -1)]);
    assert_eq!(
        kernel.push_input("d", Femtos(2000)).unwrap(),
        StepOutcome::Halted
    );
    assert_eq!(kernel.log(), &[setup(999, -1)]);
}

#[test]
fn record_continues_after_setup_violation() {
    let mut kernel = kernel(dff(1000, 400), ViolationPolicy::Record);
    kernel.push_input("d", Femtos(0)).unwrap();
    assert_eq!(
        kernel.push_input("clk", Femtos(999)).unwrap(),
        StepOutcome::Ran
    );
    assert_eq!(kernel.log(), &[setup(999, -1), fired(5999, "q")]);
    assert_eq!(
        kernel.push_input("d", Femtos(2000)).unwrap(),
        StepOutcome::Ran
    );
    assert_eq!(
        kernel.push_input("clk", Femtos(3001)).unwrap(),
        StepOutcome::Ran
    );
    assert_eq!(
        kernel.log(),
        &[setup(999, -1), fired(5999, "q"), fired(8001, "q")]
    );
}

#[test]
fn new_rejects_characterized_cell() {
    let mut cell = dff(1000, 400);
    cell.provenance = Provenance::Characterized;
    assert_eq!(
        Kernel::new("dut", cell, ViolationPolicy::Record).unwrap_err(),
        KernelError::NeedsConstantDelays
    );
}

#[test]
fn rejects_time_going_backwards() {
    let mut kernel = kernel(dff(1000, 400), ViolationPolicy::Record);
    kernel.push_input("d", Femtos(10)).unwrap();
    assert_eq!(
        kernel.push_input("d", Femtos(9)).unwrap_err(),
        KernelError::TimeWentBackwards
    );
}

proptest! {
    #[test]
    fn prop_slack_matches_closed_window(
        pair in (1i64..10_000).prop_flat_map(|w| (Just(w), 0i64..=(w + 2)))
    ) {
        let (w, sep) = pair;
        let mut kernel = kernel(dff(w, 400), ViolationPolicy::Record);
        kernel.push_input("d", Femtos(0)).unwrap();
        kernel.push_input("clk", Femtos(sep)).unwrap();
        let slacks: Vec<Femtos> = kernel
            .log()
            .iter()
            .filter_map(|event| match event {
                LogEvent::SetupViolation { slack, .. } => Some(*slack),
                _ => None,
            })
            .collect();
        if sep <= w {
            assert_eq!(slacks, vec![Femtos(sep - w)]);
        } else {
            assert!(slacks.is_empty());
        }
    }
}
