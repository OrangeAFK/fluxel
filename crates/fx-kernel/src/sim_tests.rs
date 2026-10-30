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
