use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fx_ir::cell::CellDef;
use fx_ir::design::Design;
use fx_ir::stimulus::Stimulus;

use crate::{DeckOpts, FlowError, export_josim, export_josim_with};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_cells(root: &Path) -> BTreeMap<String, CellDef> {
    let dir = root.join("lib/families/rsfq");
    let mut cells = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".cell.json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let cell = CellDef::parse(&text).unwrap();
        cells.insert(cell.name.clone(), cell);
    }
    cells
}

fn export_example(name: &str) -> String {
    let root = repo_root();
    let dir = root.join("examples").join(name);
    let design = Design::parse(&std::fs::read_to_string(dir.join("design.json")).unwrap()).unwrap();
    let stimulus =
        Stimulus::parse(&std::fs::read_to_string(dir.join("stimulus.json")).unwrap()).unwrap();
    export_josim(&design, &stimulus, &load_cells(&root), &root).unwrap()
}

fn subckt_count(deck: &str, name: &str) -> usize {
    deck.lines()
        .filter(|line| line.split_whitespace().take(2).eq([".subckt", name]))
        .count()
}

fn x_line<'a>(deck: &'a str, label: &str) -> &'a str {
    let prefix = format!("X{label} ");
    deck.lines()
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| panic!("missing {prefix}"))
}

fn pins(line: &str) -> Vec<&str> {
    line.split_whitespace().skip(2).collect()
}

#[test]
fn jtl_chain_deck() {
    let deck = export_example("jtl_chain");
    assert_eq!(subckt_count(&deck, "THmitll_JTL"), 1);
    assert_eq!(subckt_count(&deck, "THmitll_DCSFQ"), 1);
    assert_eq!(subckt_count(&deck, "THmitll_SFQDC"), 1);
    assert_eq!(deck.matches(".param bias_mul=1").count(), 1);
    assert!(deck.contains("BiasCoef=0.7*bias_mul"));
    assert!(
        !deck.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.starts_with(".param BiasCoef=0.7") && !trimmed.contains("bias_mul")
        }),
        "bare BiasCoef survived"
    );
    assert!(deck.contains("IB1=(275u)*bias_mul"));
    assert!(deck.contains("IB2=B3*Ic0*BiasCoef"));
    assert!(deck.contains("IB1=(280u)*bias_mul"));
    assert!(deck.contains("IB2=(150u)*bias_mul"));
    assert!(deck.contains("IB3=(220u)*bias_mul"));
    assert!(deck.contains("IB4=(80u)*bias_mul"));
    assert!(deck.contains(".tran 0.025p 620p"));
    assert!(deck.contains("pwl(0 0 20p 0 23p 600u 26p 0 120p 0 123p 600u 126p 0)"));
    assert!(
        deck.lines()
            .any(|line| line.starts_with('R') && line.split_whitespace().nth(3) == Some("2"))
    );

    for stage in 0..8 {
        let line = x_line(&deck, &format!("j{stage}"));
        assert!(line.contains(" THmitll_JTL "));
        assert!(deck.contains(&format!("* fluxel-map j{stage}.a P(B1|XJ{stage})")));
        assert!(deck.contains(&format!("* fluxel-map j{stage}.q P(B2|XJ{stage})")));
        assert!(deck.contains(&format!("p(B1.Xj{stage})")));
        assert!(deck.contains(&format!("p(B2.Xj{stage})")));
    }
    let j0 = pins(x_line(&deck, "j0"));
    let j1 = pins(x_line(&deck, "j1"));
    assert_eq!(j0.len(), 2);
    assert_eq!(j0[1], j1[0], "j0.q and j1.a share a node");
    assert!(deck.contains("Xfxsrc_in THmitll_DCSFQ "));
    assert!(deck.contains("Xfxload_out THmitll_SFQDC "));
    assert!(deck.contains("* fluxel-map fxsrc_in.q P(B3|XFXSRC_IN)"));
    assert!(deck.contains("* fluxel-map fxload_out.a P(B1|XFXLOAD_OUT)"));
}

#[test]
fn shift_register_sources_loads_and_ports() {
    let deck = export_example("shift_register");
    assert!(subckt_count(&deck, "THmitll_SPLIT") >= 1);
    assert!(subckt_count(&deck, "THmitll_DFF") >= 1);
    assert_eq!(subckt_count(&deck, "THmitll_SPLIT"), 1);
    assert_eq!(subckt_count(&deck, "THmitll_DFF"), 1);
    assert!(deck.contains("Xfxsrc_d THmitll_DCSFQ "));
    assert!(deck.contains("Xfxsrc_clk THmitll_DCSFQ "));
    assert!(deck.contains("Xfxload_q THmitll_SFQDC "));
    for name in ["s0", "s1", "s2"] {
        for port in ["a", "q0", "q1"] {
            assert!(
                deck.contains(&format!("* fluxel-map {name}.{port} ")),
                "{name}.{port}"
            );
        }
    }
    for stage in 0..4 {
        for port in ["a", "clk", "q"] {
            assert!(
                deck.contains(&format!("* fluxel-map dff{stage}.{port} ")),
                "dff{stage}.{port}"
            );
        }
    }
}

#[test]
fn instances_are_sorted() {
    let deck = export_example("jtl_chain");
    let j0 = deck.find("Xj0 ").unwrap();
    let j1 = deck.find("Xj1 ").unwrap();
    assert!(j0 < j1);
    let root = repo_root();
    let design = Design::parse(
        r#"{
          "instances": {
            "b": { "family": "rsfq", "cell": "JTL", "bias_domain": "b" },
            "a": { "family": "rsfq", "cell": "JTL", "bias_domain": "b" }
          },
          "nets": [
            { "driver": { "port": "in" }, "sink": { "instance": "a", "port": "a" }, "wire": { "kind": "direct" } },
            { "driver": { "instance": "a", "port": "q" }, "sink": { "instance": "b", "port": "a" }, "wire": { "kind": "direct" } },
            { "driver": { "instance": "b", "port": "q" }, "sink": { "port": "out" }, "wire": { "kind": "direct" } }
          ],
          "ports": { "in": ["in"], "out": ["out"] },
          "clock_domains": {},
          "bias_domains": ["b"]
        }"#,
    )
    .unwrap();
    let stimulus = Stimulus::parse(r#"{ "in": [0] }"#).unwrap();
    let deck = export_josim(&design, &stimulus, &load_cells(&root), &root).unwrap();
    assert!(deck.find("Xa ").unwrap() < deck.find("Xb ").unwrap());
}

#[test]
fn indirect_wires_are_rejected() {
    let root = repo_root();
    let cells = load_cells(&root);
    let stimulus = Stimulus::parse(r#"{ "in": [0] }"#).unwrap();
    for wire in [
        r#"{ "kind": "jtl", "stages": 1 }"#,
        r#"{ "kind": "ptl", "length_units": 1 }"#,
    ] {
        let design = Design::parse(&format!(
            r#"{{
              "instances": {{
                "j0": {{ "family": "rsfq", "cell": "JTL", "bias_domain": "b" }}
              }},
              "nets": [
                {{ "driver": {{ "port": "in" }}, "sink": {{ "instance": "j0", "port": "a" }}, "wire": {wire} }}
              ],
              "ports": {{ "in": ["in"], "out": ["out"] }},
              "clock_domains": {{}},
              "bias_domains": ["b"]
            }}"#
        ))
        .unwrap();
        let err = export_josim(&design, &stimulus, &cells, &root).unwrap_err();
        assert!(matches!(err, FlowError::NotDirect { .. }), "{err}");
    }
}

#[test]
fn tff_is_not_exportable() {
    let root = repo_root();
    let dir = root.join("examples/tff_divider");
    let design = Design::parse(&std::fs::read_to_string(dir.join("design.json")).unwrap()).unwrap();
    let stimulus =
        Stimulus::parse(&std::fs::read_to_string(dir.join("stimulus.json")).unwrap()).unwrap();
    let err = export_josim(&design, &stimulus, &load_cells(&root), &root).unwrap_err();
    assert!(matches!(err, FlowError::NotExportable { ref cell } if cell == "TFF"));
}

#[test]
fn unknown_ref_jj_is_rejected() {
    let root = repo_root();
    let mut cells = load_cells(&root);
    cells
        .get_mut("JTL")
        .unwrap()
        .ref_jj
        .insert("a".into(), "B99".into());
    let design = Design::parse(
        r#"{
          "instances": {
            "j0": { "family": "rsfq", "cell": "JTL", "bias_domain": "b" }
          },
          "nets": [
            { "driver": { "port": "in" }, "sink": { "instance": "j0", "port": "a" }, "wire": { "kind": "direct" } },
            { "driver": { "instance": "j0", "port": "q" }, "sink": { "port": "out" }, "wire": { "kind": "direct" } }
          ],
          "ports": { "in": ["in"], "out": ["out"] },
          "clock_domains": {},
          "bias_domains": ["b"]
        }"#,
    )
    .unwrap();
    let stimulus = Stimulus::parse(r#"{ "in": [0] }"#).unwrap();
    let err = export_josim(&design, &stimulus, &cells, &root).unwrap_err();
    assert!(
        matches!(err, FlowError::UnknownDevice { ref device, .. } if device == "B99"),
        "{err}"
    );
}

#[test]
fn overlapping_pulses_are_rejected() {
    let root = repo_root();
    let design = Design::parse(
        r#"{
          "instances": {
            "j0": { "family": "rsfq", "cell": "JTL", "bias_domain": "b" }
          },
          "nets": [
            { "driver": { "port": "in" }, "sink": { "instance": "j0", "port": "a" }, "wire": { "kind": "direct" } },
            { "driver": { "instance": "j0", "port": "q" }, "sink": { "port": "out" }, "wire": { "kind": "direct" } }
          ],
          "ports": { "in": ["in"], "out": ["out"] },
          "clock_domains": {},
          "bias_domains": ["b"]
        }"#,
    )
    .unwrap();
    let stimulus = Stimulus::parse(r#"{ "in": [0, 5000] }"#).unwrap();
    let err = export_josim(&design, &stimulus, &load_cells(&root), &root).unwrap_err();
    assert!(matches!(err, FlowError::OverlappingPulses { .. }), "{err}");
}

#[test]
fn merge_quoted_bias_coef_is_rewritten() {
    let root = repo_root();
    let design = Design::parse(
        r#"{
          "instances": {
            "m0": { "family": "rsfq", "cell": "MERGE", "bias_domain": "b" }
          },
          "nets": [
            { "driver": { "port": "a" }, "sink": { "instance": "m0", "port": "a" }, "wire": { "kind": "direct" } },
            { "driver": { "port": "b" }, "sink": { "instance": "m0", "port": "b" }, "wire": { "kind": "direct" } },
            { "driver": { "instance": "m0", "port": "q" }, "sink": { "port": "q" }, "wire": { "kind": "direct" } }
          ],
          "ports": { "in": ["a", "b"], "out": ["q"] },
          "clock_domains": {},
          "bias_domains": ["b"]
        }"#,
    )
    .unwrap();
    let stimulus = Stimulus::parse(r#"{ "a": [0], "b": [10000] }"#).unwrap();
    let deck = export_josim(&design, &stimulus, &load_cells(&root), &root).unwrap();
    assert!(deck.contains(".param BiasCoef=0.7*bias_mul"));
    assert!(
        !deck.contains("BiasCoef=0.7'"),
        "MERGE's trailing quote survived"
    );
}

#[test]
fn default_deck_opts_match_export_josim() {
    let root = repo_root();
    let dir = root.join("examples/jtl_chain");
    let design = Design::parse(&std::fs::read_to_string(dir.join("design.json")).unwrap()).unwrap();
    let stimulus =
        Stimulus::parse(&std::fs::read_to_string(dir.join("stimulus.json")).unwrap()).unwrap();
    let cells = load_cells(&root);
    let with = export_josim_with(&design, &stimulus, &cells, &root, &DeckOpts::default()).unwrap();
    assert_eq!(
        with,
        export_josim(&design, &stimulus, &cells, &root).unwrap()
    );
}

#[test]
fn deck_opts_bias_step_stop_and_direct_io() {
    let root = repo_root();
    let design = Design::parse(
        r#"{
          "instances": {
            "j0": { "family": "rsfq", "cell": "JTL", "bias_domain": "b" }
          },
          "nets": [
            { "driver": { "port": "in" }, "sink": { "instance": "j0", "port": "a" }, "wire": { "kind": "direct" } },
            { "driver": { "instance": "j0", "port": "q" }, "sink": { "port": "out" }, "wire": { "kind": "direct" } }
          ],
          "ports": { "in": ["in"], "out": ["out"] },
          "clock_domains": {},
          "bias_domains": ["b"]
        }"#,
    )
    .unwrap();
    let stimulus = Stimulus::parse(r#"{ "in": [0] }"#).unwrap();
    let opts = DeckOpts {
        bias_mul: 0.8,
        step_fs: 25,
        stop_fs: Some(200_000),
        current_inputs: true,
        resistor_outputs: true,
    };
    let deck = export_josim_with(&design, &stimulus, &load_cells(&root), &root, &opts).unwrap();
    assert!(deck.contains(".param bias_mul=0.8\n"), "{deck}");
    assert!(!deck.contains(".param bias_mul=1\n"));
    assert!(deck.contains(".tran 0.025p 200p 0\n"));
    assert!(!deck.contains("Xfxsrc_"));
    assert!(!deck.contains("Xfxload_"));
    assert!(!deck.contains("THmitll_DCSFQ"));
    assert!(!deck.contains("THmitll_SFQDC"));
    assert!(!deck.contains("fluxel-map fxsrc_"));
    assert!(!deck.contains("fluxel-map fxload_"));
    let nodes = pins(x_line(&deck, "j0"));
    assert_eq!(nodes.len(), 2);
    assert!(deck.contains(&format!("Ifxsrc_in 0 {} ", nodes[0])));
    assert!(deck.contains(&format!("Rfxload_out {} 0 2\n", nodes[1])));
    assert!(deck.contains("Xj0 THmitll_JTL "));
}
