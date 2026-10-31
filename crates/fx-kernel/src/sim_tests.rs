use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use fx_ir::cell::{CellDef, Face, Fire, Ports, Provenance, Transition};
use fx_ir::design::{Design, Instance, Net, PinRef, Wire};
use fx_ir::stimulus::Stimulus;
use fx_time::Femtos;
use proptest::prelude::*;
use sha2::{Digest, Sha256};

use crate::{LogEvent, Simulator, ViolationPolicy};

#[derive(Clone, Debug)]
struct Case {
    n: usize,
    delay: i64,
    times: Vec<i64>,
    every: u64,
    pick: usize,
}

fn buffer(delay: i64) -> CellDef {
    CellDef {
        name: "BUF".into(),
        family: "rsfq".into(),
        provenance: Provenance::Behavioral,
        ports: Ports {
            inputs: vec!["a".into()],
            outputs: vec!["q".into()],
        },
        faces: [("a", Face::W), ("q", Face::E)]
            .into_iter()
            .map(|(name, face)| (name.to_string(), face))
            .collect(),
        states: vec!["s".into()],
        initial: "s".into(),
        transitions: vec![Transition {
            from: "s".into(),
            on: "a".into(),
            to: "s".into(),
            fire: vec![Fire {
                port: "q".into(),
                delay: "d".into(),
            }],
            note: None,
        }],
        constraints: vec![],
        jj_count: Some(1),
        bias_ua: None,
        netlist: None,
        lut: None,
        ref_jj: BTreeMap::new(),
        notes: None,
        delays: BTreeMap::from([("d".into(), Femtos(delay))]),
    }
}

fn chain(case: &Case) -> (Design, BTreeMap<String, CellDef>, Stimulus) {
    let mut instances = BTreeMap::new();
    for i in 0..case.n {
        instances.insert(
            format!("u{i}"),
            Instance {
                family: "rsfq".into(),
                cell: "BUF".into(),
                bias_domain: "b".into(),
                macro_parent: None,
            },
        );
    }
    let mut nets = vec![Net {
        driver: PinRef::Port { port: "in".into() },
        sink: PinRef::Instance {
            instance: "u0".into(),
            port: "a".into(),
        },
        wire: Wire::Direct,
    }];
    for i in 0..case.n - 1 {
        nets.push(Net {
            driver: PinRef::Instance {
                instance: format!("u{i}"),
                port: "q".into(),
            },
            sink: PinRef::Instance {
                instance: format!("u{}", i + 1),
                port: "a".into(),
            },
            wire: Wire::Direct,
        });
    }
    let last = case.n - 1;
    nets.push(Net {
        driver: PinRef::Instance {
            instance: format!("u{last}"),
            port: "q".into(),
        },
        sink: PinRef::Port { port: "out".into() },
        wire: Wire::Direct,
    });
    let design = Design {
        instances,
        nets,
        ports: Ports {
            inputs: vec!["in".into()],
            outputs: vec!["out".into()],
        },
        clock_domains: BTreeMap::new(),
        bias_domains: BTreeSet::from(["b".into()]),
    };
    let mut cells = BTreeMap::new();
    cells.insert("BUF".into(), buffer(case.delay));
    let mut pulses = BTreeMap::new();
    pulses.insert(
        "in".into(),
        case.times.iter().copied().map(Femtos).collect(),
    );
    (design, cells, Stimulus(pulses))
}

fn started(case: &Case) -> Simulator {
    let (design, cells, stimulus) = chain(case);
    Simulator::new(design, cells, stimulus, ViolationPolicy::Record, case.every).unwrap()
}

fn event_time(event: &LogEvent) -> Femtos {
    match event {
        LogEvent::Fired { t, .. }
        | LogEvent::SetupViolation { t, .. }
        | LogEvent::HoldViolation { t, .. }
        | LogEvent::Collision { t, .. }
        | LogEvent::IllegalInput { t, .. } => *t,
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(bytes);
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&digest);
    hash
}

fn arb_case() -> impl Strategy<Value = Case> {
    (
        1usize..=3,
        1i64..=30,
        proptest::collection::btree_set(0i64..200, 0..=3),
        1u64..=8,
        0usize..16,
    )
        .prop_map(|(n, delay, times, every, pick)| Case {
            n,
            delay,
            times: times.into_iter().collect(),
            every,
            pick,
        })
}

#[test]
fn empty_and_one_pulse_hash_differ() {
    let empty = Case {
        n: 1,
        delay: 10,
        times: vec![],
        every: 1,
        pick: 0,
    };
    let one = Case {
        n: 1,
        delay: 10,
        times: vec![0],
        every: 1,
        pick: 0,
    };
    let mut quiet = started(&empty);
    let mut pulsed = started(&one);
    quiet.run().unwrap();
    pulsed.run().unwrap();
    assert_ne!(quiet.log().hash().unwrap(), pulsed.log().hash().unwrap());
}

#[test]
fn pulse_json_matches_fixture() {
    let case = Case {
        n: 1,
        delay: 10,
        times: vec![0],
        every: 1,
        pick: 0,
    };
    let mut sim = started(&case);
    sim.run().unwrap();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/char/tests/fixtures/kernel_pulses.json");
    let fixture = std::fs::read_to_string(path).unwrap();
    assert_eq!(sim.log().pulse_json(), fixture);
}

proptest! {
    #[test]
    fn prop_two_runs_same_hash(case in arb_case()) {
        let mut first = started(&case);
        let mut second = started(&case);
        first.run().unwrap();
        second.run().unwrap();
        let hash = first.log().hash().unwrap();
        assert_eq!(hash, second.log().hash().unwrap());
        assert_eq!(hash, sha256(&first.log().encode().unwrap()));
    }

    #[test]
    fn prop_seek_then_continue_matches(case in arb_case()) {
        let mut finished = started(&case);
        finished.run().unwrap();
        let end_hash = finished.log().hash().unwrap();

        let mut candidates = vec![Femtos(-1)];
        for event in finished.log().events() {
            candidates.push(event_time(event));
        }
        let after = candidates
            .iter()
            .map(|time| time.0)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        candidates.push(Femtos(after));
        let t = candidates[case.pick % candidates.len()];

        let mut rewound = started(&case);
        rewound.run().unwrap();
        rewound.seek(t).unwrap();
        let mut straight = started(&case);
        straight.run_until(t).unwrap();
        assert_eq!(rewound.log(), straight.log());
        rewound.run().unwrap();
        assert_eq!(rewound.log().hash().unwrap(), end_hash);
    }
}

#[derive(Clone, Debug)]
enum Shape {
    Chain {
        delays: Vec<i64>,
    },
    Split {
        q0: i64,
        q1: i64,
        left: Vec<i64>,
        right: Vec<i64>,
    },
}

#[derive(Clone, Debug)]
struct CauseCase {
    shape: Shape,
    times: Vec<i64>,
}

struct FireSite {
    inst: String,
    port: String,
    delay: i64,
    cause_inst: Option<String>,
    cause_port: String,
}

fn named_buffer(name: &str, delay: i64) -> CellDef {
    let mut cell = buffer(delay);
    cell.name = name.to_string();
    cell
}

fn splitter(q0: i64, q1: i64) -> CellDef {
    CellDef {
        name: "SPL".into(),
        family: "rsfq".into(),
        provenance: Provenance::Behavioral,
        ports: Ports {
            inputs: vec!["a".into()],
            outputs: vec!["q0".into(), "q1".into()],
        },
        faces: [("a", Face::W), ("q0", Face::E), ("q1", Face::N)]
            .into_iter()
            .map(|(name, face)| (name.to_string(), face))
            .collect(),
        states: vec!["s".into()],
        initial: "s".into(),
        transitions: vec![Transition {
            from: "s".into(),
            on: "a".into(),
            to: "s".into(),
            fire: vec![
                Fire {
                    port: "q0".into(),
                    delay: "d0".into(),
                },
                Fire {
                    port: "q1".into(),
                    delay: "d1".into(),
                },
            ],
            note: None,
        }],
        constraints: vec![],
        jj_count: Some(1),
        bias_ua: None,
        netlist: None,
        lut: None,
        ref_jj: BTreeMap::new(),
        notes: None,
        delays: BTreeMap::from([("d0".into(), Femtos(q0)), ("d1".into(), Femtos(q1))]),
    }
}

struct PartialNet {
    instances: BTreeMap<String, Instance>,
    cells: BTreeMap<String, CellDef>,
    nets: Vec<Net>,
    sites: Vec<FireSite>,
    outputs: Vec<String>,
}

fn place_buffer(net: &mut PartialNet, inst: &str, delay: i64, driver: PinRef) {
    let cell_name = format!("B{inst}");
    let (cause_inst, cause_port) = match &driver {
        PinRef::Instance { instance, port } => (Some(instance.clone()), port.clone()),
        PinRef::Port { port } => (None, port.clone()),
    };
    net.instances.insert(
        inst.to_string(),
        Instance {
            family: "rsfq".into(),
            cell: cell_name.clone(),
            bias_domain: "b".into(),
            macro_parent: None,
        },
    );
    net.cells
        .insert(cell_name.clone(), named_buffer(&cell_name, delay));
    net.nets.push(Net {
        driver,
        sink: PinRef::Instance {
            instance: inst.to_string(),
            port: "a".into(),
        },
        wire: Wire::Direct,
    });
    net.sites.push(FireSite {
        inst: inst.to_string(),
        port: "q".into(),
        delay,
        cause_inst,
        cause_port,
    });
}

fn cause_setup(case: &CauseCase) -> (Design, BTreeMap<String, CellDef>, Stimulus, Vec<FireSite>) {
    let mut net = PartialNet {
        instances: BTreeMap::new(),
        cells: BTreeMap::new(),
        nets: Vec::new(),
        sites: Vec::new(),
        outputs: Vec::new(),
    };
    match &case.shape {
        Shape::Chain { delays } => {
            let mut driver = PinRef::Port { port: "in".into() };
            for (index, delay) in delays.iter().enumerate() {
                let inst = format!("u{index}");
                place_buffer(&mut net, &inst, *delay, driver);
                driver = PinRef::Instance {
                    instance: inst,
                    port: "q".into(),
                };
            }
            net.outputs.push("out".to_string());
            net.nets.push(Net {
                driver,
                sink: PinRef::Port { port: "out".into() },
                wire: Wire::Direct,
            });
        }
        Shape::Split {
            q0,
            q1,
            left,
            right,
        } => {
            net.instances.insert(
                "s".into(),
                Instance {
                    family: "rsfq".into(),
                    cell: "SPL".into(),
                    bias_domain: "b".into(),
                    macro_parent: None,
                },
            );
            net.cells.insert("SPL".into(), splitter(*q0, *q1));
            net.nets.push(Net {
                driver: PinRef::Port { port: "in".into() },
                sink: PinRef::Instance {
                    instance: "s".into(),
                    port: "a".into(),
                },
                wire: Wire::Direct,
            });
            net.sites.push(FireSite {
                inst: "s".into(),
                port: "q0".into(),
                delay: *q0,
                cause_inst: None,
                cause_port: "in".into(),
            });
            net.sites.push(FireSite {
                inst: "s".into(),
                port: "q1".into(),
                delay: *q1,
                cause_inst: None,
                cause_port: "in".into(),
            });
            feed_chain(
                &mut net,
                left,
                PinRef::Instance {
                    instance: "s".into(),
                    port: "q0".into(),
                },
                "l",
                "out0",
            );
            feed_chain(
                &mut net,
                right,
                PinRef::Instance {
                    instance: "s".into(),
                    port: "q1".into(),
                },
                "r",
                "out1",
            );
        }
    }
    let mut pulses = BTreeMap::new();
    pulses.insert(
        "in".into(),
        case.times.iter().copied().map(Femtos).collect(),
    );
    let design = Design {
        instances: net.instances,
        nets: net.nets,
        ports: Ports {
            inputs: vec!["in".into()],
            outputs: net.outputs,
        },
        clock_domains: BTreeMap::new(),
        bias_domains: BTreeSet::from(["b".into()]),
    };
    (design, net.cells, Stimulus(pulses), net.sites)
}

fn feed_chain(net: &mut PartialNet, delays: &[i64], mut driver: PinRef, prefix: &str, out: &str) {
    for (index, delay) in delays.iter().enumerate() {
        let inst = format!("{prefix}{index}");
        place_buffer(net, &inst, *delay, driver);
        driver = PinRef::Instance {
            instance: inst,
            port: "q".into(),
        };
    }
    net.outputs.push(out.to_string());
    net.nets.push(Net {
        driver,
        sink: PinRef::Port {
            port: out.to_string(),
        },
        wire: Wire::Direct,
    });
}

fn arb_cause() -> impl Strategy<Value = CauseCase> {
    prop_oneof![
        (1usize..=4).prop_flat_map(|n| {
            (
                proptest::collection::vec(0i64..=40, n..=n),
                proptest::collection::btree_set(0i64..200, 0..=4),
            )
                .prop_map(|(delays, times)| CauseCase {
                    shape: Shape::Chain { delays },
                    times: times.into_iter().collect(),
                })
        }),
        (0i64..=40, 0i64..=40, 0usize..=3, 0usize..=3).prop_flat_map(
            |(q0, q1, left_n, right_n)| {
                (
                    Just(q0),
                    Just(q1),
                    proptest::collection::vec(0i64..=40, left_n..=left_n),
                    proptest::collection::vec(0i64..=40, right_n..=right_n),
                    proptest::collection::btree_set(0i64..200, 0..=4),
                )
                    .prop_map(|(q0, q1, left, right, times)| CauseCase {
                        shape: Shape::Split {
                            q0,
                            q1,
                            left,
                            right,
                        },
                        times: times.into_iter().collect(),
                    })
            }
        ),
    ]
}

fn fired_triples(events: &[LogEvent]) -> Vec<(i64, String, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            LogEvent::Fired { t, inst, port } => Some((t.0, inst.clone(), port.clone())),
            _ => None,
        })
        .collect()
}

fn toggle(delay: i64) -> CellDef {
    CellDef {
        name: "TOG".into(),
        family: "rsfq".into(),
        provenance: Provenance::Behavioral,
        ports: Ports {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["q".into()],
        },
        faces: [("a", Face::W), ("b", Face::S), ("q", Face::E)]
            .into_iter()
            .map(|(name, face)| (name.to_string(), face))
            .collect(),
        states: vec!["s0".into(), "s1".into()],
        initial: "s0".into(),
        transitions: vec![
            Transition {
                from: "s0".into(),
                on: "a".into(),
                to: "s1".into(),
                fire: vec![Fire {
                    port: "q".into(),
                    delay: "d".into(),
                }],
                note: None,
            },
            Transition {
                from: "s1".into(),
                on: "a".into(),
                to: "s0".into(),
                fire: vec![],
                note: None,
            },
        ],
        constraints: vec![],
        jj_count: Some(1),
        bias_ua: None,
        netlist: None,
        lut: None,
        ref_jj: BTreeMap::new(),
        notes: None,
        delays: BTreeMap::from([("d".into(), Femtos(delay))]),
    }
}

fn toggle_design() -> Design {
    Design {
        instances: BTreeMap::from([(
            "u".into(),
            Instance {
                family: "rsfq".into(),
                cell: "TOG".into(),
                bias_domain: "b".into(),
                macro_parent: None,
            },
        )]),
        nets: vec![
            Net {
                driver: PinRef::Port { port: "a".into() },
                sink: PinRef::Instance {
                    instance: "u".into(),
                    port: "a".into(),
                },
                wire: Wire::Direct,
            },
            Net {
                driver: PinRef::Port { port: "b".into() },
                sink: PinRef::Instance {
                    instance: "u".into(),
                    port: "b".into(),
                },
                wire: Wire::Direct,
            },
            Net {
                driver: PinRef::Instance {
                    instance: "u".into(),
                    port: "q".into(),
                },
                sink: PinRef::Port { port: "q".into() },
                wire: Wire::Direct,
            },
        ],
        ports: Ports {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["q".into()],
        },
        clock_domains: BTreeMap::new(),
        bias_domains: BTreeSet::from(["b".into()]),
    }
}

fn run_toggle(delay: i64, pulses: &[(i64, bool)], include_bad: bool) -> Vec<LogEvent> {
    let mut cells = BTreeMap::new();
    cells.insert("TOG".into(), toggle(delay));
    let mut stimulus = BTreeMap::new();
    stimulus.insert(
        "a".into(),
        pulses
            .iter()
            .filter(|pair| !pair.1)
            .map(|pair| Femtos(pair.0))
            .collect(),
    );
    if include_bad {
        stimulus.insert(
            "b".into(),
            pulses
                .iter()
                .filter(|pair| pair.1)
                .map(|pair| Femtos(pair.0))
                .collect(),
        );
    }
    let mut sim = Simulator::new(
        toggle_design(),
        cells,
        Stimulus(stimulus),
        ViolationPolicy::Record,
        1,
    )
    .unwrap();
    sim.run().unwrap();
    sim.log().events().to_vec()
}

fn illegal_triples(events: &[LogEvent]) -> Vec<(i64, String, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            LogEvent::IllegalInput { t, inst, port } => Some((t.0, inst.clone(), port.clone())),
            _ => None,
        })
        .collect()
}

fn arb_illegal() -> impl Strategy<Value = (i64, Vec<(i64, bool)>)> {
    (0i64..=40, proptest::collection::btree_set(0i64..400, 1..=6))
        .prop_flat_map(|(delay, times)| {
            let n = times.len();
            let times: Vec<i64> = times.into_iter().collect();
            (
                Just(delay),
                Just(times),
                proptest::collection::vec(any::<bool>(), n),
            )
        })
        .prop_map(|(delay, times, flags)| {
            let mut pulses: Vec<(i64, bool)> = times.into_iter().zip(flags).collect();
            if pulses.iter().all(|pair| !pair.1) {
                pulses[0].1 = true;
            }
            (delay, pulses)
        })
}

proptest! {
    #[test]
    fn prop_fired_time_at_or_after_cause(case in arb_cause()) {
        let (design, cells, stimulus, sites) = cause_setup(&case);
        let mut sim = Simulator::new(
            design,
            cells,
            stimulus,
            ViolationPolicy::Record,
            1,
        )
        .unwrap();
        sim.run().unwrap();
        let fired = fired_triples(sim.log().events());
        prop_assert_eq!(fired.len(), case.times.len() * sites.len());
        let have: BTreeSet<(i64, String, String)> = fired.iter().cloned().collect();
        let mut by_output = BTreeMap::new();
        for site in &sites {
            by_output.insert((site.inst.as_str(), site.port.as_str()), site);
        }
        for (t, inst, port) in &fired {
            let Some(site) = by_output.get(&(inst.as_str(), port.as_str())) else {
                prop_assert!(false, "unexpected fire {inst}.{port} at {t}");
                unreachable!();
            };
            prop_assert!(site.delay >= 0);
            let t_cause = t - site.delay;
            prop_assert!(*t >= t_cause);
            match &site.cause_inst {
                None => prop_assert!(
                    case.times.contains(&t_cause),
                    "no stimulus cause for {inst}.{port} at {t}"
                ),
                Some(src) => prop_assert!(
                    have.contains(&(t_cause, src.clone(), site.cause_port.clone())),
                    "no cause {src}.{} at {t_cause} for {inst}.{port} at {t}",
                    site.cause_port
                ),
            }
        }
    }

    #[test]
    fn prop_illegal_input_logged_state_unchanged((delay, pulses) in arb_illegal()) {
        let full = run_toggle(delay, &pulses, true);
        let clean = run_toggle(delay, &pulses, false);
        prop_assert_eq!(fired_triples(&full), fired_triples(&clean));
        let expected: Vec<(i64, String, String)> = pulses
            .iter()
            .filter(|pair| pair.1)
            .map(|pair| (pair.0, "u".to_string(), "b".to_string()))
            .collect();
        prop_assert_eq!(illegal_triples(&full), expected);
        prop_assert!(illegal_triples(&clean).is_empty());
    }
}
