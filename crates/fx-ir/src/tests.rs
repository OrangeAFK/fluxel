use std::collections::{BTreeMap, BTreeSet};

use fx_time::Femtos;
use proptest::prelude::*;
use serde_json::{Value, json};

use crate::cell::{
    CellDef, CellError, Constraint, ConstraintKind, Face, Fire, Ports, Provenance, Transition,
};
use crate::design::{Design, DesignError, Instance, Net, PinRef, Wire};
use crate::schema::{cell_schema, design_schema, stimulus_schema};
use crate::stimulus::Stimulus;

fn parse_cell(value: &Value) -> Result<CellDef, CellError> {
    CellDef::parse(&value.to_string())
}

fn spec_dff() -> Value {
    json!({
        "name": "DFF",
        "family": "rsfq",
        "provenance": "characterized",
        "ports": { "in": ["d", "clk"], "out": ["q"] },
        "faces": { "d": "W", "clk": "S", "q": "E" },
        "states": ["empty", "stored"],
        "initial": "empty",
        "transitions": [
            { "from": "empty", "on": "d", "to": "stored", "fire": [] },
            {
                "from": "stored",
                "on": "d",
                "to": "stored",
                "fire": [],
                "note": "second pulse escapes"
            },
            {
                "from": "stored",
                "on": "clk",
                "to": "empty",
                "fire": [{ "port": "q", "delay": "t_clk_q" }]
            },
            { "from": "empty", "on": "clk", "to": "empty", "fire": [] }
        ],
        "constraints": [
            { "kind": "setup", "first": "d", "then": "clk", "window": "t_setup" },
            { "kind": "hold", "first": "clk", "then": "d", "window": "t_hold" }
        ],
        "jj_count": 10,
        "bias_ua": 100.0,
        "netlist": "lib/rsfqlib/dff.cir",
        "lut": "lib/families/rsfq/DFF.lut.cbor"
    })
}

fn behavioral_buf() -> Value {
    json!({
        "name": "BUF",
        "family": "rsfq",
        "provenance": "behavioral",
        "ports": { "in": ["a"], "out": ["q"] },
        "faces": { "a": "W", "q": "E" },
        "states": ["s"],
        "initial": "s",
        "transitions": [
            { "from": "s", "on": "a", "to": "s", "fire": [{ "port": "q", "delay": "d" }] }
        ],
        "constraints": [
            { "kind": "setup", "first": "a", "then": "a", "window": "w" }
        ],
        "jj_count": 1,
        "delays": { "d": 1000, "w": 200 }
    })
}

fn behavioral_dff() -> Value {
    let mut cell = spec_dff();
    cell["provenance"] = json!("behavioral");
    cell.as_object_mut().unwrap().remove("netlist");
    cell.as_object_mut().unwrap().remove("lut");
    cell["delays"] = json!({ "t_clk_q": 5000, "t_setup": 1000, "t_hold": 500 });
    cell
}

#[test]
fn parse_spec_dff() {
    let cell = parse_cell(&spec_dff()).unwrap();
    assert_eq!(cell.provenance, Provenance::Characterized);
    assert!(cell.delays.is_empty());
    assert_eq!(
        cell.transitions[1].note.as_deref(),
        Some("second pulse escapes")
    );
}

#[test]
fn parse_behavioral_cell() {
    let cell = parse_cell(&behavioral_dff()).unwrap();
    assert_eq!(cell.provenance, Provenance::Behavioral);
    assert_eq!(cell.netlist, None);
    assert_eq!(cell.lut, None);
    assert_eq!(cell.delays.get("t_clk_q"), Some(&Femtos(5000)));
    assert_eq!(cell.delays.get("t_setup"), Some(&Femtos(1000)));
    assert_eq!(cell.delays.get("t_hold"), Some(&Femtos(500)));
}

#[test]
fn parse_collision_constraint() {
    let mut cell = behavioral_buf();
    cell["ports"] = json!({ "in": ["a", "b"], "out": ["q"] });
    cell["faces"] = json!({ "a": "W", "b": "S", "q": "E" });
    cell["transitions"] = json!([
        { "from": "s", "on": "a", "to": "s", "fire": [{ "port": "q", "delay": "d" }] },
        { "from": "s", "on": "b", "to": "s", "fire": [{ "port": "q", "delay": "d" }] }
    ]);
    cell["constraints"] = json!([
        { "kind": "collision", "first": "a", "then": "b", "window": "w" }
    ]);
    let cell = parse_cell(&cell).unwrap();
    assert_eq!(cell.constraints[0].kind, ConstraintKind::Collision);
}

#[test]
fn parse_stimulus() {
    let stim = Stimulus::parse(r#"{"d":[0,1500,-10],"clk":[]}"#).unwrap();
    assert_eq!(
        stim.0.get("d").unwrap(),
        &vec![Femtos(0), Femtos(1500), Femtos(-10)]
    );
    assert_eq!(stim.0.get("clk").unwrap(), &Vec::<Femtos>::new());
    let again = Stimulus::parse(&serde_json::to_string(&stim).unwrap()).unwrap();
    assert_eq!(again, stim);
}

fn design_wires() -> Value {
    json!({
        "instances": {
            "dff0": {
                "family": "rsfq",
                "cell": "DFF",
                "bias_domain": "bias0",
                "macro_parent": null
            }
        },
        "nets": [
            {
                "driver": { "port": "d" },
                "sink": { "instance": "dff0", "port": "d" },
                "wire": { "kind": "direct" }
            },
            {
                "driver": { "instance": "dff0", "port": "q" },
                "sink": { "port": "q" },
                "wire": { "kind": "jtl", "stages": 3 }
            },
            {
                "driver": { "port": "clk" },
                "sink": { "instance": "dff0", "port": "clk" },
                "wire": { "kind": "ptl", "length_units": 10 }
            }
        ],
        "ports": { "in": ["d", "clk"], "out": ["q"] },
        "clock_domains": { "clk": ["dff0"] },
        "bias_domains": ["bias0"]
    })
}

#[test]
fn parse_design_wires() {
    let design = Design::parse(&design_wires().to_string()).unwrap();
    assert_eq!(design.nets[0].wire, Wire::Direct);
    assert_eq!(design.nets[1].wire, Wire::Jtl { stages: 3 });
    assert_eq!(design.nets[2].wire, Wire::Ptl { length_units: 10 });
    assert_eq!(
        design.clock_domains.get("clk"),
        Some(&vec!["dff0".to_string()])
    );
    assert!(design.bias_domains.contains("bias0"));
    let again = Design::parse(&serde_json::to_string(&design).unwrap()).unwrap();
    assert_eq!(again, design);
}

#[test]
fn unknown_state_initial() {
    let mut cell = behavioral_buf();
    cell["initial"] = json!("nope");
    assert_eq!(
        parse_cell(&cell).unwrap_err(),
        CellError::UnknownState {
            state: "nope".into()
        }
    );
}

#[test]
fn unknown_state_from() {
    let mut cell = behavioral_buf();
    cell["transitions"][0]["from"] = json!("nope");
    assert_eq!(
        parse_cell(&cell).unwrap_err(),
        CellError::UnknownState {
            state: "nope".into()
        }
    );
}

#[test]
fn unknown_state_to() {
    let mut cell = behavioral_buf();
    cell["transitions"][0]["to"] = json!("nope");
    assert_eq!(
        parse_cell(&cell).unwrap_err(),
        CellError::UnknownState {
            state: "nope".into()
        }
    );
}

#[test]
fn unknown_port_on() {
    let mut cell = behavioral_buf();
    cell["transitions"][0]["on"] = json!("nope");
    assert_eq!(
        parse_cell(&cell).unwrap_err(),
        CellError::UnknownPort {
            port: "nope".into()
        }
    );
}

#[test]
fn unknown_port_fire() {
    let mut cell = behavioral_buf();
    cell["transitions"][0]["fire"][0]["port"] = json!("nope");
    assert_eq!(
        parse_cell(&cell).unwrap_err(),
        CellError::UnknownPort {
            port: "nope".into()
        }
    );
}

#[test]
fn unknown_port_constraint() {
    let mut cell = behavioral_buf();
    cell["constraints"][0]["first"] = json!("nope");
    assert_eq!(
        parse_cell(&cell).unwrap_err(),
        CellError::UnknownPort {
            port: "nope".into()
        }
    );
}

#[test]
fn unknown_port_face() {
    let mut cell = behavioral_buf();
    cell["faces"]["nope"] = json!("N");
    assert_eq!(
        parse_cell(&cell).unwrap_err(),
        CellError::UnknownPort {
            port: "nope".into()
        }
    );
}

#[test]
fn undefined_delay() {
    let mut cell = behavioral_buf();
    cell["transitions"][0]["fire"][0]["delay"] = json!("missing");
    assert_eq!(
        parse_cell(&cell).unwrap_err(),
        CellError::UndefinedDelay {
            name: "missing".into()
        }
    );
}

#[test]
fn undefined_window() {
    let mut cell = behavioral_buf();
    cell["constraints"][0]["window"] = json!("missing");
    assert_eq!(
        parse_cell(&cell).unwrap_err(),
        CellError::UndefinedWindow {
            name: "missing".into()
        }
    );
}

#[test]
fn nondeterministic_pair() {
    let mut cell = behavioral_buf();
    let extra = cell["transitions"][0].clone();
    cell["transitions"].as_array_mut().unwrap().push(extra);
    assert_eq!(
        parse_cell(&cell).unwrap_err(),
        CellError::Nondeterministic {
            state: "s".into(),
            input: "a".into(),
        }
    );
}

#[test]
fn unfired_output() {
    let mut cell = behavioral_buf();
    cell["transitions"][0]["fire"] = json!([]);
    assert_eq!(
        parse_cell(&cell).unwrap_err(),
        CellError::UnfiredOutput { port: "q".into() }
    );
}

#[test]
fn literal_timing_on_characterized() {
    let mut cell = spec_dff();
    cell["delays"] = json!({ "t_clk_q": 5000 });
    assert_eq!(parse_cell(&cell).unwrap_err(), CellError::LiteralTiming);
}

#[test]
fn characterized_missing_lut() {
    let mut cell = spec_dff();
    cell.as_object_mut().unwrap().remove("lut");
    assert_eq!(parse_cell(&cell).unwrap_err(), CellError::MissingPath);
}

#[test]
fn characterized_null_jj_count() {
    let mut cell = spec_dff();
    cell["jj_count"] = json!(null);
    assert_eq!(parse_cell(&cell).unwrap_err(), CellError::MissingPath);
}

#[test]
fn behavioral_null_jj_count_and_paths() {
    let mut cell = behavioral_buf();
    cell["jj_count"] = json!(null);
    cell["netlist"] = json!(null);
    cell["lut"] = json!(null);
    let cell = parse_cell(&cell).unwrap();
    assert_eq!(cell.provenance, Provenance::Behavioral);
    assert_eq!(cell.jj_count, None);
    assert_eq!(cell.netlist, None);
    assert_eq!(cell.lut, None);
}

#[test]
fn self_characterized_null_jj_count_and_paths() {
    let mut cell = behavioral_buf();
    cell["provenance"] = json!("self-characterized");
    cell["jj_count"] = json!(null);
    cell["netlist"] = json!(null);
    cell["lut"] = json!(null);
    cell.as_object_mut().unwrap().remove("delays");
    let cell = parse_cell(&cell).unwrap();
    assert_eq!(cell.provenance, Provenance::SelfCharacterized);
    assert_eq!(cell.jj_count, None);
    assert_eq!(cell.netlist, None);
    assert_eq!(cell.lut, None);
}

fn two_instances(nets: Value, bias: &str) -> Value {
    json!({
        "instances": {
            "a": { "family": "rsfq", "cell": "BUF", "bias_domain": bias },
            "b": { "family": "rsfq", "cell": "BUF", "bias_domain": "b" },
            "c": { "family": "rsfq", "cell": "BUF", "bias_domain": "b" }
        },
        "nets": nets,
        "ports": { "in": [], "out": [] },
        "clock_domains": {},
        "bias_domains": ["b"]
    })
}

#[test]
fn fanout_rejected() {
    let driver = json!({ "instance": "a", "port": "q" });
    let nets = json!([
        {
            "driver": driver,
            "sink": { "instance": "b", "port": "d" },
            "wire": { "kind": "direct" }
        },
        {
            "driver": { "instance": "a", "port": "q" },
            "sink": { "instance": "c", "port": "d" },
            "wire": { "kind": "direct" }
        }
    ]);
    assert_eq!(
        Design::parse(&two_instances(nets, "b").to_string()).unwrap_err(),
        DesignError::Fanout {
            driver: PinRef::Instance {
                instance: "a".into(),
                port: "q".into(),
            },
        }
    );
}

#[test]
fn multiple_drivers_rejected() {
    let nets = json!([
        {
            "driver": { "instance": "a", "port": "q" },
            "sink": { "instance": "c", "port": "d" },
            "wire": { "kind": "direct" }
        },
        {
            "driver": { "instance": "b", "port": "q" },
            "sink": { "instance": "c", "port": "d" },
            "wire": { "kind": "direct" }
        }
    ]);
    assert_eq!(
        Design::parse(&two_instances(nets, "b").to_string()).unwrap_err(),
        DesignError::MultipleDrivers {
            sink: PinRef::Instance {
                instance: "c".into(),
                port: "d".into(),
            },
        }
    );
}

#[test]
fn unknown_instance_rejected() {
    let nets = json!([
        {
            "driver": { "instance": "missing", "port": "q" },
            "sink": { "instance": "a", "port": "d" },
            "wire": { "kind": "direct" }
        }
    ]);
    assert_eq!(
        Design::parse(&two_instances(nets, "b").to_string()).unwrap_err(),
        DesignError::UnknownInstance {
            instance: "missing".into(),
        }
    );
}

#[test]
fn unknown_bias_domain_rejected() {
    let nets = json!([]);
    assert_eq!(
        Design::parse(&two_instances(nets, "nope").to_string()).unwrap_err(),
        DesignError::UnknownBiasDomain {
            domain: "nope".into(),
        }
    );
}

fn arb_wire() -> impl Strategy<Value = Wire> {
    prop_oneof![
        Just(Wire::Direct),
        (0u32..8).prop_map(|stages| Wire::Jtl { stages }),
        (0u32..8).prop_map(|length_units| Wire::Ptl { length_units }),
    ]
}

fn arb_cell() -> impl Strategy<Value = CellDef> {
    (1usize..4, 1usize..3, 1usize..3).prop_flat_map(|(n_states, n_in, n_out)| {
        let n_trans = n_states * n_in;
        (
            proptest::collection::vec(0..n_states, n_trans),
            any::<u32>(),
            any::<i64>(),
            any::<i64>(),
        )
            .prop_map(move |(tos, jj, delay, window)| {
                let states: Vec<String> = (0..n_states).map(|i| format!("s{i}")).collect();
                let inputs: Vec<String> = (0..n_in).map(|i| format!("i{i}")).collect();
                let outputs: Vec<String> = (0..n_out).map(|i| format!("o{i}")).collect();
                let cycle = [Face::N, Face::E, Face::S, Face::W, Face::U, Face::D];
                let mut faces = BTreeMap::new();
                for (i, port) in inputs.iter().chain(outputs.iter()).enumerate() {
                    faces.insert(port.clone(), cycle[i % cycle.len()]);
                }
                let mut transitions = Vec::new();
                let mut idx = 0;
                for state in &states {
                    for input in &inputs {
                        let to = states[tos[idx]].clone();
                        idx += 1;
                        let fire = outputs
                            .iter()
                            .map(|port| Fire {
                                port: port.clone(),
                                delay: "d".into(),
                            })
                            .collect();
                        transitions.push(Transition {
                            from: state.clone(),
                            on: input.clone(),
                            to,
                            fire,
                            note: None,
                        });
                    }
                }
                let mut delays = BTreeMap::new();
                delays.insert("d".into(), Femtos(delay));
                delays.insert("w".into(), Femtos(window));
                CellDef {
                    name: "cell".into(),
                    family: "rsfq".into(),
                    provenance: Provenance::Behavioral,
                    ports: Ports { inputs, outputs },
                    faces,
                    initial: states[0].clone(),
                    states,
                    transitions,
                    constraints: vec![Constraint {
                        kind: ConstraintKind::Setup,
                        first: "i0".into(),
                        then: "i0".into(),
                        window: "w".into(),
                    }],
                    jj_count: Some(jj),
                    bias_ua: None,
                    netlist: None,
                    lut: None,
                    notes: None,
                    delays,
                }
            })
    })
}

fn arb_design() -> impl Strategy<Value = Design> {
    (2usize..6).prop_flat_map(|n| {
        proptest::collection::vec(arb_wire(), n - 1).prop_map(move |wires| {
            let mut instances = BTreeMap::new();
            for i in 0..n {
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
            let nets = wires
                .into_iter()
                .enumerate()
                .map(|(i, wire)| Net {
                    driver: PinRef::Instance {
                        instance: format!("u{i}"),
                        port: "q".into(),
                    },
                    sink: PinRef::Instance {
                        instance: format!("u{}", i + 1),
                        port: "d".into(),
                    },
                    wire,
                })
                .collect();
            let mut clock_domains = BTreeMap::new();
            clock_domains.insert("clk".into(), (0..n).map(|i| format!("u{i}")).collect());
            let mut bias_domains = BTreeSet::new();
            bias_domains.insert("b".into());
            Design {
                instances,
                nets,
                ports: Ports {
                    inputs: vec!["in".into()],
                    outputs: vec!["out".into()],
                },
                clock_domains,
                bias_domains,
            }
        })
    })
}

fn arb_stimulus() -> impl Strategy<Value = Stimulus> {
    let keys = proptest::string::string_regex("[a-z]{1,4}").unwrap();
    proptest::collection::btree_map(keys, proptest::collection::vec(any::<i64>(), 0..6), 0..5)
        .prop_map(|pulses| {
            Stimulus(
                pulses
                    .into_iter()
                    .map(|(k, times)| (k, times.into_iter().map(Femtos).collect()))
                    .collect(),
            )
        })
}

proptest! {
    #[test]
    fn prop_cell_json_round_trip(cell in arb_cell()) {
        let json = serde_json::to_string(&cell).unwrap();
        assert_eq!(CellDef::parse(&json).unwrap(), cell);
    }

    #[test]
    fn prop_design_json_round_trip(design in arb_design()) {
        let json = serde_json::to_string(&design).unwrap();
        assert_eq!(Design::parse(&json).unwrap(), design);
    }

    #[test]
    fn prop_stimulus_json_round_trip(stim in arb_stimulus()) {
        let json = serde_json::to_string(&stim).unwrap();
        assert_eq!(Stimulus::parse(&json).unwrap(), stim);
    }
}

#[test]
fn rsfq_library_cells_parse() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/families/rsfq");
    let mut found = BTreeMap::new();
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|err| {
        panic!("read {}: {err}", dir.display());
    });
    for entry in entries {
        let path = entry.unwrap().path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(name) = file_name.strip_suffix(".cell.json") else {
            continue;
        };
        let text = std::fs::read_to_string(&path).unwrap();
        let cell = CellDef::parse(&text).unwrap_or_else(|err| panic!("parse {file_name}: {err}"));
        assert_eq!(cell.name, name, "{file_name}");
        assert_eq!(cell.family, "rsfq");
        assert_eq!(cell.provenance, Provenance::Behavioral);
        assert_eq!(cell.netlist, None);
        assert_eq!(cell.lut, None);
        assert!(cell.notes.is_some(), "{name} notes");
        found.insert(name.to_string(), cell);
    }

    let expected = [
        ("AND", Some(15u32)),
        ("DCSFQ", Some(3)),
        ("DFF", Some(7)),
        ("JTL", Some(2)),
        ("MERGE", Some(7)),
        ("NDRO", Some(11)),
        ("NOT", Some(8)),
        ("OR", Some(12)),
        ("SFQDC", Some(8)),
        ("SPLIT", Some(3)),
        ("TFF", None),
        ("XOR", Some(11)),
    ];
    assert_eq!(
        found.keys().map(String::as_str).collect::<Vec<_>>(),
        expected.map(|(name, _)| name)
    );
    for (name, jj) in expected {
        let cell = &found[name];
        assert_eq!(cell.jj_count, jj, "{name}");
        if name == "JTL" {
            assert_eq!(cell.delays.get("t_a_q"), Some(&Femtos(1819)));
            assert_eq!(cell.delays.len(), 1);
        } else {
            for (key, delay) in &cell.delays {
                assert!(
                    delay.0 == 5_000 || delay.0 == 1_000,
                    "{name} {key} is {}",
                    delay.0
                );
            }
        }
    }
    assert_eq!(
        found["TFF"].notes.as_deref(),
        Some(
            "No RSFQlib v3.0 source. Behavioral model of standard RSFQ TFF: 2 states, output on every second input. Placeholder delay. Not characterizable until a netlist is sourced."
        )
    );
}

#[test]
fn committed_schemas_match_generated() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas");
    for (name, generated) in [
        ("cell.json", cell_schema().unwrap()),
        ("design.json", design_schema().unwrap()),
        ("stimulus.json", stimulus_schema().unwrap()),
    ] {
        let text = std::fs::read_to_string(root.join(name)).unwrap();
        let committed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(committed, generated, "{name}");
    }
}
