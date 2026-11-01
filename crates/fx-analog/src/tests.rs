use std::fs;
use std::path::{Path, PathBuf};

use super::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn close(got: f64, expect: f64) {
    let scale = expect.abs().max(1e-30);
    assert!((got - expect).abs() <= scale * 1e-9, "{got} != {expect}");
}

fn resistor(elements: &[Element], name: &str) -> f64 {
    elements
        .iter()
        .find_map(|element| match element {
            Element::R {
                name: got, ohms, ..
            } if got == name => Some(*ohms),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing resistor {name}"))
}

fn inductor(elements: &[Element], name: &str) -> f64 {
    elements
        .iter()
        .find_map(|element| match element {
            Element::L {
                name: got, henries, ..
            } if got == name => Some(*henries),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing inductor {name}"))
}

fn junction<'a>(elements: &'a [Element], name: &str) -> &'a Jj {
    elements
        .iter()
        .find_map(|element| match element {
            Element::Jj(jj) if jj.name == name => Some(jj),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing junction {name}"))
}

#[test]
fn suffixes_expressions_and_forward_references() {
    // Closed forms, not a second trip through the expression parser.
    let phi0 = 2.067833848e-15;
    let lval = phi0 / (4.0 * 2.5 * 0.0001);
    let sum = (2.5 + 2.5) * 0.0001 * 0.7;
    let icrs = 100e-6 * 6.859904418;
    let text = "\
.param SUM=(B1+B2)*IC0*BIASCOEF
.param PHI0=2.067833848E-15
.param LVAL=PHI0/(4*2.5*0.0001)
.param B1=2.5
.param B2=2.5
.param IC0=0.0001
.param BIASCOEF=0.7
.param ICRS=100u*6.859904418
.param MV=2.8mV
.param PF=0.07pF
.param MA=0.1mA
.param MEGV=1MEG
.param XV=1X
.param SC=1.13e-12
.param PZ=PHI_ZERO
.param LG=LOG10(100)
.param POW=2^3^2
.param QUOTE=0.7'
RL 1 0 LVAL
RS 1 0 SUM
RI 1 0 ICRS
RM 1 0 MV
RP 1 0 PF
RA 1 0 MA
RG 1 0 MEGV
RX 1 0 XV
RE 1 0 SC
RZ 1 0 PZ
RLOG 1 0 LG
RPW 1 0 POW
RQ 1 0 QUOTE
";
    let deck = parse_source("expr.cir", text).expect("expression deck");
    close(resistor(&deck.main, "RL"), lval);
    assert!((lval - 2.067833848e-12).abs() < 1e-18);
    close(resistor(&deck.main, "RS"), sum);
    close(resistor(&deck.main, "RI"), icrs);
    close(resistor(&deck.main, "RM"), 2.8e-3);
    close(resistor(&deck.main, "RP"), 0.07e-12);
    close(resistor(&deck.main, "RA"), 0.1e-3);
    close(resistor(&deck.main, "RG"), 1e6);
    close(resistor(&deck.main, "RX"), 1e6);
    close(resistor(&deck.main, "RE"), 1.13e-12);
    // JoSIM Constants.hpp PHI_ZERO, not RSFQlib's Phi0 and not spec Φ₀.
    close(resistor(&deck.main, "RZ"), 2.067833831170082e-15);
    // JoSIM lists LOG10 but parse_operator returns 0. This value is log10.
    close(resistor(&deck.main, "RLOG"), 2.0);
    close(resistor(&deck.main, "RPW"), 512.0);
    close(resistor(&deck.main, "RQ"), 0.7);
}

#[test]
fn parameter_cycle_names_the_unknowns() {
    let err = parse_source("cycle.cir", ".param A=B\n.param B=A\nR1 1 0 A\n").expect_err("cycle");
    let text = err.to_string();
    assert!(text.contains("cycle.cir"), "{text}");
    assert!(text.contains('A') && text.contains('B'), "{text}");
}

#[test]
fn jtl_base_matches_the_closed_form() {
    let path = root().join("lib/rsfqlib/RSFQlib/mitll_JTL/THmitll_JTL_v3p0_base.cir");
    let deck = parse_netlist(&path).expect("jtl base");
    assert!(
        deck.main.is_empty(),
        "a library cell has no top-level instance"
    );
    let elements = deck.flatten_subckt("THmitll_JTL").expect("flatten");
    let phi0 = 2.067833848e-15;
    let area = 2.5;
    close(inductor(&elements, "L1"), phi0 / (4.0 * area * 0.0001));
    let jj = junction(&elements, "B1");
    // JJ::set_model: Ic *= area, C *= area, Rn /= area, R0 /= area.
    close(jj.ic, 0.1e-3 * area);
    close(jj.c, 0.07e-12 * area);
    close(jj.rn, 16.0 / area);
    close(jj.r0, 160.0 / area);
    assert_eq!(jj.rtype, 1);
}

#[test]
fn exporter_fixture_continuation_and_sources() {
    let text = "\
* Fluxel JoSIM export
.param bias_mul=1.2
.param BiasCoef=0.7*bias_mul
.subckt THmitll_JTL a q
.model jjmit jj(rtype=1, vg=2.8mV, cap=0.07pF, r0=160, rn=16, icrit=0.1mA)
.param IC=2.5
.param B1=IC
B1 1 2 jjmit area=B1
L1 a 1 1p
.ends
Xj0 THmitll_JTL n_a n_q
Ibias 0 n_a pwl(0 0
+ 5p 1u)
Rfxload_q n_q 0 2
C1 n_a 0 1pF
V1 n_a 0 SIN(0 1m 1G)
Ltop n_a 0 2p
Lbot n_a 0 3p
Kcoup Ltop Lbot 0.5
.tran 0.025p 10p 0
.print p(B1.Xj0)
.temp 4.2
.end
.plot v(1)
";
    let deck = parse_source("export.cir", text).expect("exporter fixture");
    assert_eq!(deck.temp_k, Some(4.2));
    let tran = deck.tran.expect("tran");
    close(tran.tstep, 0.025e-12);
    close(tran.tstop, 10e-12);
    close(tran.prstart, 0.0);
    assert_eq!(deck.prints, vec!["P(B1.XJ0)".to_string()]);
    let jj = junction(&deck.main, "B1|XJ0");
    close(jj.ic, 0.1e-3 * 2.5);
    close(inductor(&deck.main, "L1|XJ0"), 1e-12);
    let ibias = deck.main.iter().find_map(|element| match element {
        Element::I { name, wave, .. } if name == "IBIAS" => Some(wave),
        _ => None,
    });
    match ibias {
        Some(Wave::Pwl(points)) => {
            assert_eq!(points.len(), 2);
            close(points[1].0, 5e-12);
            close(points[1].1, 1e-6);
        }
        other => panic!("pwl missing: {other:?}"),
    }
    close(resistor(&deck.main, "RFXLOAD_Q"), 2.0);
    let cap = deck.main.iter().find_map(|element| match element {
        Element::C { name, farads, .. } if name == "C1" => Some(*farads),
        _ => None,
    });
    close(cap.expect("C1"), 1e-12);
    let sin = deck.main.iter().find_map(|element| match element {
        Element::V { name, wave, .. } if name == "V1" => Some(wave),
        _ => None,
    });
    match sin {
        Some(Wave::Sin {
            vo,
            va,
            freq,
            td,
            theta,
        }) => {
            close(*vo, 0.0);
            close(*va, 1e-3);
            close(*freq, 1e9);
            close(*td, 0.0);
            close(*theta, 0.0);
        }
        other => panic!("sin missing: {other:?}"),
    }
    let coupled = deck.main.iter().find_map(|element| match element {
        Element::K { name, l1, l2, k } if name == "KCOUP" => Some((l1, l2, *k)),
        _ => None,
    });
    let (l1, l2, k) = coupled.expect("K");
    assert_eq!(l1, "LTOP");
    assert_eq!(l2, "LBOT");
    close(k, 0.5);
}

#[test]
fn lossless_line_keeps_z0_and_delay() {
    let text = "Tload 1 0 2 0 LOSSLESS Z0=5.3 TD=10p\n";
    let deck = parse_source("tx.cir", text).expect("tx");
    match deck.main.as_slice() {
        [Element::Tx(tx)] => {
            assert_eq!(tx.n1, "1");
            assert_eq!(tx.n3, "2");
            close(tx.z0, 5.3);
            close(tx.td, 10e-12);
        }
        other => panic!("expected one line, got {other:?}"),
    }
}

#[test]
fn unsupported_card_reports_file_and_line() {
    let err = parse_source("fixture.cir", "R1 1 0 1\n.plot v(1)\n").expect_err("plot");
    let text = err.to_string();
    assert!(text.contains("fixture.cir:2"), "{text}");
}

#[test]
fn instance_parameter_is_an_error() {
    let text = "\
.subckt S A
R1 A 0 1
.ends
X1 S 1 R1=10
";
    let err = parse_source("xeq.cir", text).expect_err("equals");
    let text = err.to_string();
    assert!(text.contains("xeq.cir:4"), "{text}");
}

#[test]
fn control_block_is_not_a_simulation_netlist() {
    let text = "\
.control
back-annotate foo.cir
.endc
THISSHOULDNOTAPPEAR 1 0 1
";
    let err = parse_source("control.cir", text).expect_err("control");
    assert!(matches!(err, NetlistError::NotASimulationNetlist { .. }));
    let text = err.to_string();
    assert!(text.contains("control.cir"), "{text}");
    assert!(!text.contains("THISSHOULDNOTAPPEAR"), "{text}");
}

#[test]
fn idx_filename_is_rejected_without_reading_devices() {
    let path = root().join("lib/rsfqlib/RSFQlib/mitll_JTL/THmitll_JTL_v3p0_idx.cir");
    let err = parse_netlist(&path).expect_err("idx");
    assert!(
        matches!(err, NetlistError::NotASimulationNetlist { .. }),
        "{err}"
    );
    assert!(!err.to_string().contains("BACK-ANNOTATE") || err.to_string().contains("idx"));
}

#[test]
fn every_josim_deck_parses_and_every_idx_is_rejected() {
    let lib = root().join("lib/rsfqlib");
    let mut cir = Vec::new();
    collect_cir(&lib, &mut cir);
    assert!(cir.len() > 100, "found {}", cir.len());
    let mut idx = 0;
    let mut josim = 0;
    for path in cir {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.to_ascii_lowercase().ends_with("_idx.cir") {
            idx += 1;
            let err = parse_netlist(&path).expect_err(name);
            assert!(
                matches!(err, NetlistError::NotASimulationNetlist { .. }),
                "{name}: {err}"
            );
        } else {
            josim += 1;
            parse_netlist(&path).unwrap_or_else(|err| panic!("{name}: {err}"));
        }
    }
    assert_eq!(idx, 35);
    assert_eq!(josim, 97);
    let mut golden = Vec::new();
    collect_named(&root().join("golden"), "netlist.cir", &mut golden);
    assert!(!golden.is_empty());
    for path in golden {
        parse_netlist(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    }
}

#[test]
fn five_decks_match_josim_verbose_counts() {
    // josim-cli v2.7.02a34ee, `josim-cli -o /tmp/josim-m31.csv -V 1`, 2026-11-01.
    // Counts are JJ, inductor, resistor, current source.
    let cases = [
        ("golden/jtl_chain/netlist.cir", (9, 40, 10, 6)),
        (
            "lib/rsfqlib/RSFQlib/mitll_JTL/THmitll_JTL_v3p0_testbench.cir",
            (9, 40, 10, 6),
        ),
        (
            "lib/rsfqlib/RSFQlib/mitll_DCSFQ/THmitll_DCSFQ_v3p0_testbench.cir",
            (5, 22, 6, 4),
        ),
        (
            "lib/rsfqlib/RSFQlib/mitll_XOR/THmitll_XOR_v3p0_testbench.cir",
            (28, 109, 29, 18),
        ),
        (
            "lib/rsfqlib/RSFQlib/mitll_AND2/THmitll_AND2_v3p0_testbench.cir",
            (32, 121, 33, 20),
        ),
    ];
    for (rel, (jj, l, r, i)) in cases {
        let deck = parse_netlist(&root().join(rel)).unwrap_or_else(|err| panic!("{rel}: {err}"));
        let got = count_kinds(&deck.main);
        assert_eq!(got, (jj, l, r, i, 0, 0, 0, 0), "{rel}");
    }
}

fn count_kinds(elements: &[Element]) -> (usize, usize, usize, usize, usize, usize, usize, usize) {
    let mut jj = 0;
    let mut l = 0;
    let mut r = 0;
    let mut i = 0;
    let mut c = 0;
    let mut k = 0;
    let mut v = 0;
    let mut tx = 0;
    for element in elements {
        match element {
            Element::Jj(_) => jj += 1,
            Element::L { .. } => l += 1,
            Element::R { .. } => r += 1,
            Element::I { .. } => i += 1,
            Element::C { .. } => c += 1,
            Element::K { .. } => k += 1,
            Element::V { .. } => v += 1,
            Element::Tx(_) => tx += 1,
        }
    }
    (jj, l, r, i, c, k, v, tx)
}

fn collect_cir(dir: &Path, out: &mut Vec<PathBuf>) {
    collect_named(dir, ".cir", out);
}

fn collect_named(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|err| panic!("{}: {err}", dir.display()));
    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_named(&path, suffix, out);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(suffix))
        {
            out.push(path);
        }
    }
}
