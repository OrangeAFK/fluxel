use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use fx_ir::cell::{CellDef, Constraint, ConstraintKind, Face, Fire, Ports, Provenance, Transition};
use fx_ir::design::{Design, Instance, Net, PinRef, Wire};
use fx_ir::lut::{AxisQuery, LutBundle};
use fx_ir::stimulus::Stimulus;
use fx_time::Femtos;
use proptest::prelude::*;
use sha2::{Digest, Sha256};

use crate::{KernelError, LogEvent, Simulator, ViolationPolicy};

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
    Simulator::new(
        design,
        cells,
        BTreeMap::new(),
        stimulus,
        ViolationPolicy::Record,
        case.every,
    )
    .unwrap()
}

fn event_time(event: &LogEvent) -> Femtos {
    match event {
        LogEvent::Fired { t, .. }
        | LogEvent::SetupViolation { t, .. }
        | LogEvent::HoldViolation { t, .. }
        | LogEvent::Collision { t, .. }
        | LogEvent::IllegalInput { t, .. }
        | LogEvent::Uncharacterized { t, .. } => *t,
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
        BTreeMap::new(),
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
            BTreeMap::new(),
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

fn source() -> String {
    r#""source": {
        "josim_version": "josim-test",
        "netlist_sha256": "abababababababababababababababababababababababababababababababab",
        "sweep_sha256": "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
        "timestep_fs": 25
    }"#
    .to_string()
}

fn delay_bundle() -> LutBundle {
    let json = format!(
        r#"{{
        "cell": "GATE",
        "entries": {{
            "t_a_q": {{
                "axes": [
                    {{"name": "bias_frac", "points": ["0.80", "1.00", "1.20"]}},
                    {{"name": "dt_prev_fs", "points": ["6000", "200000"]}}
                ],
                "values": [1000, 3000, 2000, 4000, 5000, 7000],
                "interpolation": "multilinear",
                "valid": {{
                    "bias_frac": {{"lo": "0.80", "hi": "1.20"}},
                    "dt_prev_fs": {{"lo": "6000", "hi": "200000"}}
                }},
                "sigma_fs": 0
            }}
        }},
        {}
    }}"#,
        source()
    );
    LutBundle::from_json(&json).unwrap()
}

fn window_bundle() -> LutBundle {
    let json = format!(
        r#"{{
        "cell": "DFF",
        "entries": {{
            "t_clk_q": {{
                "axes": [{{"name": "bias_frac", "points": ["0.80", "1.20"]}}],
                "values": [5000, 5000],
                "interpolation": "multilinear",
                "valid": {{"bias_frac": {{"lo": "0.80", "hi": "1.20"}}}},
                "sigma_fs": 0
            }},
            "t_setup": {{
                "axes": [{{"name": "bias_frac", "points": ["0.80", "1.20"]}}],
                "values": [1000, 3000],
                "interpolation": "multilinear",
                "valid": {{"bias_frac": {{"lo": "0.80", "hi": "1.20"}}}},
                "sigma_fs": 0
            }}
        }},
        {}
    }}"#,
        source()
    );
    LutBundle::from_json(&json).unwrap()
}

fn faces(pairs: &[(&str, Face)]) -> BTreeMap<String, Face> {
    pairs
        .iter()
        .map(|(name, face)| ((*name).to_string(), *face))
        .collect()
}

fn lut_buffer(name: &str, delay: &str) -> CellDef {
    CellDef {
        name: name.to_string(),
        family: "rsfq".into(),
        provenance: Provenance::Characterized,
        ports: Ports {
            inputs: vec!["a".into()],
            outputs: vec!["q".into()],
        },
        faces: faces(&[("a", Face::W), ("q", Face::E)]),
        states: vec!["s".into()],
        initial: "s".into(),
        transitions: vec![Transition {
            from: "s".into(),
            on: "a".into(),
            to: "s".into(),
            fire: vec![Fire {
                port: "q".into(),
                delay: delay.to_string(),
            }],
            note: None,
        }],
        constraints: vec![],
        jj_count: Some(1),
        bias_ua: None,
        netlist: Some("n".into()),
        lut: Some("l".into()),
        ref_jj: BTreeMap::new(),
        notes: None,
        delays: BTreeMap::new(),
    }
}

fn one_design(cell: &str, inputs: &[&str], output: &str) -> Design {
    let mut nets = Vec::new();
    for port in inputs {
        nets.push(Net {
            driver: PinRef::Port {
                port: (*port).to_string(),
            },
            sink: PinRef::Instance {
                instance: "u".into(),
                port: (*port).to_string(),
            },
            wire: Wire::Direct,
        });
    }
    nets.push(Net {
        driver: PinRef::Instance {
            instance: "u".into(),
            port: output.to_string(),
        },
        sink: PinRef::Port {
            port: output.to_string(),
        },
        wire: Wire::Direct,
    });
    Design {
        instances: BTreeMap::from([(
            "u".into(),
            Instance {
                family: "rsfq".into(),
                cell: cell.to_string(),
                bias_domain: "b".into(),
                macro_parent: None,
            },
        )]),
        nets,
        ports: Ports {
            inputs: inputs.iter().map(|port| (*port).to_string()).collect(),
            outputs: vec![output.to_string()],
        },
        clock_domains: BTreeMap::new(),
        bias_domains: BTreeSet::from(["b".into()]),
    }
}

fn stim(pairs: &[(&str, &[i64])]) -> Stimulus {
    let mut pulses = BTreeMap::new();
    for (port, times) in pairs {
        pulses.insert(
            (*port).to_string(),
            times.iter().copied().map(Femtos).collect(),
        );
    }
    Stimulus(pulses)
}

fn lookup_at(bundle: &LutBundle, entry: &str, bias: f64, dt: i64) -> (i64, bool) {
    let mut point = BTreeMap::new();
    point.insert("bias_frac".into(), AxisQuery::Fraction(bias));
    point.insert("dt_prev_fs".into(), AxisQuery::Time(Femtos(dt)));
    let (delay, valid) = bundle.lookup(entry, &point).unwrap();
    (delay.0, valid)
}

fn jtl_bundle() -> LutBundle {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/families/rsfq/JTL.lut.cbor");
    LutBundle::from_cbor(&std::fs::read(&path).expect("read JTL lut")).unwrap()
}

fn bound_f64(bundle: &LutBundle, entry: &str, axis: &str, hi: bool) -> f64 {
    let bound = &bundle.entries[entry].valid[axis];
    let text = if hi { &bound.hi } else { &bound.lo };
    text.parse().unwrap()
}

fn bound_fs(bundle: &LutBundle, entry: &str, axis: &str, hi: bool) -> i64 {
    let bound = &bundle.entries[entry].valid[axis];
    let text = if hi { &bound.hi } else { &bound.lo };
    text.parse().unwrap()
}

#[test]
fn lut_delay_uses_settled_then_gap() {
    let bundle = delay_bundle();
    let mut cells = BTreeMap::new();
    cells.insert("GATE".into(), lut_buffer("GATE", "t_a_q"));
    let mut luts = BTreeMap::new();
    luts.insert("GATE".into(), bundle.clone());
    let mut sim = Simulator::new(
        one_design("GATE", &["a"], "q"),
        cells,
        luts,
        stim(&[("a", &[0, 103_000])]),
        ViolationPolicy::Record,
        1,
    )
    .unwrap();
    sim.run().unwrap();
    let (first, first_ok) = lookup_at(&bundle, "t_a_q", 1.0, 200_000);
    let (second, second_ok) = lookup_at(&bundle, "t_a_q", 1.0, 103_000);
    assert!(first_ok && second_ok);
    assert_eq!(
        fired_triples(sim.log().events()),
        vec![
            (first, "u".into(), "q".into()),
            (103_000 + second, "u".into(), "q".into()),
        ]
    );
}

#[test]
fn bias_moves_jtl_delay_with_the_sweep() {
    let bundle = jtl_bundle();
    let lo = bound_f64(&bundle, "t_a_q", "bias_frac", false);
    let hi = bound_f64(&bundle, "t_a_q", "bias_frac", true);
    let dt = bound_fs(&bundle, "t_a_q", "dt_prev_fs", true);
    let low = jtl_fire(&bundle, lo, &[0]);
    let high = jtl_fire(&bundle, hi, &[0]);
    let (d_lo, ok_lo) = lookup_at(&bundle, "t_a_q", lo, dt);
    let (d_hi, ok_hi) = lookup_at(&bundle, "t_a_q", hi, dt);
    assert!(ok_lo && ok_hi);
    assert_ne!(d_lo, d_hi);
    assert_eq!(low, vec![d_lo]);
    assert_eq!(high, vec![d_hi]);
    assert_eq!((high[0] - low[0]).signum(), (d_hi - d_lo).signum());
}

#[test]
fn set_bias_affects_only_later_pulses() {
    let bundle = jtl_bundle();
    let lo = bound_f64(&bundle, "t_a_q", "bias_frac", false);
    let hi = bound_f64(&bundle, "t_a_q", "bias_frac", true);
    let dt_lo = bound_fs(&bundle, "t_a_q", "dt_prev_fs", false);
    let dt_hi = bound_fs(&bundle, "t_a_q", "dt_prev_fs", true);
    let gap = dt_lo + (dt_hi - dt_lo) / 2;
    let mut sim = jtl_sim(&bundle, &[0, gap]);
    sim.set_bias("b", lo).unwrap();
    sim.run_until(Femtos(0)).unwrap();
    sim.set_bias("b", hi).unwrap();
    sim.run().unwrap();
    let (d0, ok0) = lookup_at(&bundle, "t_a_q", lo, dt_hi);
    let (d1, ok1) = lookup_at(&bundle, "t_a_q", hi, gap);
    assert!(ok0 && ok1);
    assert_eq!(
        fired_triples(sim.log().events()),
        vec![
            (d0, "u".into(), "q".into()),
            (gap + d1, "u".into(), "q".into()),
        ]
    );
    let (if_early_used_hi, _) = lookup_at(&bundle, "t_a_q", hi, dt_hi);
    assert_ne!(d0, if_early_used_hi);
}

#[test]
fn bias_outside_box_logs_uncharacterized_and_clamps() {
    let bundle = delay_bundle();
    let mut cells = BTreeMap::new();
    cells.insert("GATE".into(), lut_buffer("GATE", "t_a_q"));
    let mut luts = BTreeMap::new();
    luts.insert("GATE".into(), bundle.clone());
    let mut sim = Simulator::new(
        one_design("GATE", &["a"], "q"),
        cells,
        luts,
        stim(&[("a", &[0])]),
        ViolationPolicy::Record,
        1,
    )
    .unwrap();
    sim.set_bias("b", 0.5).unwrap();
    sim.run().unwrap();
    let (clamped, valid) = lookup_at(&bundle, "t_a_q", 0.5, 200_000);
    let (edge, edge_ok) = lookup_at(&bundle, "t_a_q", 0.80, 200_000);
    assert!(!valid);
    assert!(edge_ok);
    assert_eq!(clamped, edge);
    let mut point = BTreeMap::new();
    point.insert("bias_frac".into(), 500_000);
    point.insert("dt_prev_fs".into(), 200_000);
    assert_eq!(
        sim.log().events(),
        &[
            LogEvent::Uncharacterized {
                t: Femtos(0),
                inst: "u".into(),
                entry: "t_a_q".into(),
                point,
            },
            LogEvent::Fired {
                t: Femtos(clamped),
                inst: "u".into(),
                port: "q".into(),
            },
        ]
    );
}

#[test]
fn setup_window_follows_bias() {
    let bundle = window_bundle();
    let at_nominal = run_window(&bundle, 1.0);
    let at_low = run_window(&bundle, 0.80);
    assert_eq!(
        at_nominal,
        vec![LogEvent::SetupViolation {
            t: Femtos(1999),
            inst: "u".into(),
            slack: Femtos(-1),
        }]
    );
    assert!(
        at_low
            .iter()
            .all(|event| !matches!(event, LogEvent::SetupViolation { .. }))
    );
}

#[test]
fn mixed_behavioral_then_lut() {
    let bundle = delay_bundle();
    let mut cells = BTreeMap::new();
    cells.insert("BUF".into(), buffer(1_000));
    cells.insert("GATE".into(), lut_buffer("GATE", "t_a_q"));
    let mut luts = BTreeMap::new();
    luts.insert("GATE".into(), bundle.clone());
    luts.insert("BUF".into(), delay_bundle());
    let design = Design {
        instances: BTreeMap::from([
            (
                "u0".into(),
                Instance {
                    family: "rsfq".into(),
                    cell: "BUF".into(),
                    bias_domain: "b".into(),
                    macro_parent: None,
                },
            ),
            (
                "u1".into(),
                Instance {
                    family: "rsfq".into(),
                    cell: "GATE".into(),
                    bias_domain: "b".into(),
                    macro_parent: None,
                },
            ),
        ]),
        nets: vec![
            Net {
                driver: PinRef::Port { port: "a".into() },
                sink: PinRef::Instance {
                    instance: "u0".into(),
                    port: "a".into(),
                },
                wire: Wire::Direct,
            },
            Net {
                driver: PinRef::Instance {
                    instance: "u0".into(),
                    port: "q".into(),
                },
                sink: PinRef::Instance {
                    instance: "u1".into(),
                    port: "a".into(),
                },
                wire: Wire::Direct,
            },
            Net {
                driver: PinRef::Instance {
                    instance: "u1".into(),
                    port: "q".into(),
                },
                sink: PinRef::Port { port: "q".into() },
                wire: Wire::Direct,
            },
        ],
        ports: Ports {
            inputs: vec!["a".into()],
            outputs: vec!["q".into()],
        },
        clock_domains: BTreeMap::new(),
        bias_domains: BTreeSet::from(["b".into()]),
    };
    let mut sim = Simulator::new(
        design,
        cells,
        luts,
        stim(&[("a", &[0])]),
        ViolationPolicy::Record,
        1,
    )
    .unwrap();
    sim.run().unwrap();
    let (delay, ok) = lookup_at(&bundle, "t_a_q", 1.0, 200_000);
    assert!(ok);
    assert_eq!(
        fired_triples(sim.log().events()),
        vec![
            (1_000, "u0".into(), "q".into()),
            (1_000 + delay, "u1".into(), "q".into()),
        ]
    );
}

#[test]
fn lut_runs_hash_equal() {
    let first = jtl_fire(&jtl_bundle(), 1.0, &[0, 100_000]);
    let second = jtl_fire(&jtl_bundle(), 1.0, &[0, 100_000]);
    assert_eq!(first, second);
    let mut left = jtl_sim(&jtl_bundle(), &[0, 100_000]);
    let mut right = jtl_sim(&jtl_bundle(), &[0, 100_000]);
    left.set_bias("b", 1.0).unwrap();
    right.set_bias("b", 1.0).unwrap();
    left.run().unwrap();
    right.run().unwrap();
    assert_eq!(left.log().hash().unwrap(), right.log().hash().unwrap());
}

#[test]
fn set_bias_before_run_survives_seek() {
    let bundle = delay_bundle();
    let mut cells = BTreeMap::new();
    cells.insert("GATE".into(), lut_buffer("GATE", "t_a_q"));
    let mut luts = BTreeMap::new();
    luts.insert("GATE".into(), bundle);
    let mut sim = Simulator::new(
        one_design("GATE", &["a"], "q"),
        cells,
        luts,
        stim(&[("a", &[0])]),
        ViolationPolicy::Record,
        8,
    )
    .unwrap();
    sim.set_bias("b", 1.2).unwrap();
    sim.run().unwrap();
    let hash = sim.log().hash().unwrap();
    sim.seek(Femtos(10_000)).unwrap();
    assert_eq!(sim.log().hash().unwrap(), hash);
}

#[test]
fn set_bias_rejects_unknown_domain_and_non_finite() {
    let bundle = delay_bundle();
    let mut cells = BTreeMap::new();
    cells.insert("GATE".into(), lut_buffer("GATE", "t_a_q"));
    let mut luts = BTreeMap::new();
    luts.insert("GATE".into(), bundle);
    let mut sim = Simulator::new(
        one_design("GATE", &["a"], "q"),
        cells,
        luts,
        stim(&[("a", &[])]),
        ViolationPolicy::Record,
        1,
    )
    .unwrap();
    assert_eq!(
        sim.set_bias("missing", 1.0).unwrap_err(),
        KernelError::UnknownBiasDomain {
            domain: "missing".into(),
        }
    );
    assert_eq!(
        sim.set_bias("b", f64::NAN).unwrap_err(),
        KernelError::BadBias
    );
    assert_eq!(
        sim.set_bias("b", f64::INFINITY).unwrap_err(),
        KernelError::BadBias
    );
}

#[test]
fn characterized_cell_without_bundle_is_rejected() {
    let mut cells = BTreeMap::new();
    cells.insert("GATE".into(), lut_buffer("GATE", "t_a_q"));
    let err = Simulator::new(
        one_design("GATE", &["a"], "q"),
        cells,
        BTreeMap::new(),
        stim(&[("a", &[0])]),
        ViolationPolicy::Record,
        1,
    )
    .unwrap_err();
    assert_eq!(
        err,
        KernelError::MissingLut {
            cell: "GATE".into(),
        }
    );
}

fn jtl_sim(bundle: &LutBundle, times: &[i64]) -> Simulator {
    let mut cells = BTreeMap::new();
    cells.insert("JTL".into(), lut_buffer("JTL", "t_a_q"));
    let mut luts = BTreeMap::new();
    luts.insert("JTL".into(), bundle.clone());
    Simulator::new(
        one_design("JTL", &["a"], "q"),
        cells,
        luts,
        stim(&[("a", times)]),
        ViolationPolicy::Record,
        1,
    )
    .unwrap()
}

fn jtl_fire(bundle: &LutBundle, bias: f64, times: &[i64]) -> Vec<i64> {
    let mut sim = jtl_sim(bundle, times);
    sim.set_bias("b", bias).unwrap();
    sim.run().unwrap();
    fired_triples(sim.log().events())
        .into_iter()
        .map(|(t, _, _)| t)
        .collect()
}

fn run_window(bundle: &LutBundle, bias: f64) -> Vec<LogEvent> {
    let cell = CellDef {
        name: "DFF".into(),
        family: "rsfq".into(),
        provenance: Provenance::Characterized,
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
        constraints: vec![Constraint {
            kind: ConstraintKind::Setup,
            first: "d".into(),
            then: "clk".into(),
            window: "t_setup".into(),
        }],
        jj_count: Some(1),
        bias_ua: None,
        netlist: Some("n".into()),
        lut: Some("l".into()),
        ref_jj: BTreeMap::new(),
        notes: None,
        delays: BTreeMap::new(),
    };
    let mut cells = BTreeMap::new();
    cells.insert("DFF".into(), cell);
    let mut luts = BTreeMap::new();
    luts.insert("DFF".into(), bundle.clone());
    let mut sim = Simulator::new(
        one_design("DFF", &["d", "clk"], "q"),
        cells,
        luts,
        stim(&[("d", &[0]), ("clk", &[1999])]),
        ViolationPolicy::Record,
        1,
    )
    .unwrap();
    sim.set_bias("b", bias).unwrap();
    sim.run().unwrap();
    sim.log()
        .events()
        .iter()
        .filter(|event| matches!(event, LogEvent::SetupViolation { .. }))
        .cloned()
        .collect()
}
