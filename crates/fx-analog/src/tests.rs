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

const PI: f64 = std::f64::consts::PI;

fn assert_crossings(got: &PhaseCrossings, pulses: &[(f64, i32)], antipulses: &[(f64, i32)]) {
    let up: Vec<(f64, i32)> = got.pulses.iter().map(|c| (c.t, c.k)).collect();
    let down: Vec<(f64, i32)> = got.antipulses.iter().map(|c| (c.t, c.k)).collect();
    assert_eq!(up, pulses);
    assert_eq!(down, antipulses);
}

#[test]
fn single_2pi_step_crosses_pi_halfway() {
    // Same samples as tools/char/tests/test_extract.py::test_single_2pi_step.
    let got = extract_crossings(&[0.0, 1.0, 2.0, 3.0], &[0.0, 0.0, 2.0 * PI, 2.0 * PI]).unwrap();
    assert_crossings(&got, &[(1.5, 0)], &[]);
}

#[test]
fn pulse_train_crosses_each_odd_pi() {
    let phase = [0.0, 2.0 * PI, 2.0 * PI, 4.0 * PI, 4.0 * PI, 6.0 * PI];
    let got = extract_crossings(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0], &phase).unwrap();
    assert_crossings(&got, &[(0.5, 0), (2.5, 1), (4.5, 2)], &[]);
}

#[test]
fn noisy_step_shifts_the_crossing_and_adds_none() {
    let phase = [
        PI / 10.0,
        -PI / 5.0,
        2.0 * PI + PI / 10.0,
        2.0 * PI + PI / 5.0,
    ];
    let got = extract_crossings(&[0.0, 1.0, 2.0, 3.0], &phase).unwrap();
    assert_crossings(&got, &[(1.0 + 12.0 / 23.0, 0)], &[]);
}

#[test]
fn a_level_that_lands_on_a_sample_belongs_to_the_arriving_interval() {
    let up = extract_crossings(&[0.0, 2.0, 3.0], &[0.0, PI, 2.0 * PI]).unwrap();
    assert_crossings(&up, &[(2.0, 0)], &[]);
    let down = extract_crossings(&[0.0, 2.0, 3.0], &[2.0 * PI, PI, 0.0]).unwrap();
    assert_crossings(&down, &[], &[(2.0, 0)]);
}

#[test]
fn femtoseconds_match_python_half_even() {
    // pulse_log uses int(round(t * 1e15)), and Python 3 rounds halves to even.
    assert_eq!(femtoseconds(0.5e-15), 0);
    assert_eq!(femtoseconds(1.5e-15), 2);
    assert_eq!(femtoseconds(2.5e-15), 2);
    assert_eq!(femtoseconds(1.2e-15), 1);
}

fn gap_jj(rtype: i64, r0: f64, rn: f64) -> Jj {
    Jj {
        name: "B1".into(),
        n1: "j".into(),
        n2: "0".into(),
        ic: 1e-3,
        vg: 2.8e-3,
        c: 0.0,
        r0,
        rn,
        rtype,
        deltav: 1e-4,
        icfct: PI / 4.0,
        phi: 0.0,
    }
}

fn qp_current(jj: &Jj, voltage: f64) -> f64 {
    let qp = quasiparticle(jj, voltage).unwrap();
    qp.g * voltage + qp.it
}

#[test]
fn rtype_0_is_ohmic_through_the_gap() {
    let jj = gap_jj(0, 5.0, 5.0);
    for voltage in [1e-3, 2.8e-3, 3e-3, -2.8e-3] {
        let qp = quasiparticle(&jj, voltage).unwrap();
        assert!((qp.g - 0.2).abs() < 1e-12, "G {}", qp.g);
        assert_eq!(qp.it, 0.0);
        assert!((qp_current(&jj, voltage) - voltage / 5.0).abs() < 1e-15);
    }
}

#[test]
fn rtype_1_matches_subgap_transition_and_normal() {
    let jj = gap_jj(1, 30.0, 5.0);
    let sub = quasiparticle(&jj, 1e-3).unwrap();
    assert!((sub.g - 1.0 / 30.0).abs() < 1e-12);
    assert_eq!(sub.it, 0.0);

    let normal = quasiparticle(&jj, 3e-3).unwrap();
    assert!((normal.g - 0.2).abs() < 1e-12);
    assert_eq!(normal.it, 0.0);
    assert!((qp_current(&jj, 3e-3) - 6e-4).abs() < 1e-15);

    // Inside (vg - ΔV/2, vg + ΔV/2). G and it are the JoSIM transition arm.
    let up = quasiparticle(&jj, 2.8e-3).unwrap();
    assert!((up.g - 12.732395447351628).abs() < 1e-9);
    assert!((up.it - -0.034922420813550306).abs() < 1e-12);
    assert!((qp_current(&jj, 2.8e-3) - 7.28286439034255e-4).abs() < 1e-12);

    let down = quasiparticle(&jj, -2.8e-3).unwrap();
    assert!((down.g - up.g).abs() < 1e-12);
    assert!((down.it + up.it).abs() < 1e-12);
    assert!((qp_current(&jj, -2.8e-3) + 7.28286439034255e-4).abs() < 1e-12);
}

#[test]
fn empty_gap_window_does_not_divide_by_deltav() {
    let mut jj = gap_jj(1, 30.0, 5.0);
    jj.deltav = 0.0;
    let below = quasiparticle(&jj, 2.8e-3 - 1e-9).unwrap();
    assert!((below.g - 1.0 / 30.0).abs() < 1e-12 && below.it == 0.0);
    let above = quasiparticle(&jj, 2.8e-3).unwrap();
    assert!((above.g - 0.2).abs() < 1e-12 && above.it == 0.0);
}

#[test]
fn transition_with_nonpositive_icfct_is_an_error() {
    let mut jj = gap_jj(1, 30.0, 5.0);
    jj.icfct = 0.0;
    assert!(quasiparticle(&jj, 2.8e-3).is_err());
}

fn config(h: f64, t_stop: f64, nodes: &[&str], junctions: &[&str]) -> SolveConfig {
    SolveConfig {
        h,
        t_stop,
        nodes: nodes.iter().map(|name| (*name).to_string()).collect(),
        junctions: junctions.iter().map(|name| (*name).to_string()).collect(),
        phase_tol: 1e-10,
        current_tol: 1e-15,
        max_newton: 32,
    }
}

fn overdamped(current: f64, h: f64, t_stop: f64) -> Solution {
    let ic = 100e-6;
    let r = 2.0;
    let elements = [
        Element::I {
            name: "IB".into(),
            n1: "0".into(),
            n2: "j".into(),
            wave: Wave::Dc(current),
        },
        Element::Jj(Jj {
            name: "B1".into(),
            n1: "j".into(),
            n2: "0".into(),
            ic,
            vg: 2.8e-3,
            c: 0.0,
            r0: r,
            rn: r,
            rtype: 0,
            deltav: 1e-4,
            icfct: PI / 4.0,
            phi: 0.0,
        }),
    ];
    solve(&elements, &config(h, t_stop, &["j"], &["B1"])).unwrap()
}

fn trap_between(time: &[f64], value: &[f64], t0: f64, t1: f64) -> f64 {
    let mut samples = Vec::new();
    samples.push((t0, interp(time, value, t0)));
    for (t, v) in time.iter().zip(value) {
        if *t > t0 && *t < t1 {
            samples.push((*t, *v));
        }
    }
    samples.push((t1, interp(time, value, t1)));
    let mut area = 0.0;
    for pair in samples.windows(2) {
        area += 0.5 * (pair[1].0 - pair[0].0) * (pair[0].1 + pair[1].1);
    }
    area
}

fn interp(time: &[f64], value: &[f64], t: f64) -> f64 {
    if t <= time[0] {
        return value[0];
    }
    for i in 0..time.len() - 1 {
        if time[i] <= t && t <= time[i + 1] {
            let span = time[i + 1] - time[i];
            if span == 0.0 {
                return value[i + 1];
            }
            let w = (t - time[i]) / span;
            return value[i] + w * (value[i + 1] - value[i]);
        }
    }
    value[value.len() - 1]
}

#[test]
fn pulse_area_is_one_flux_quantum() {
    let ic = 100e-6;
    let h = 0.025e-12;
    let solved = overdamped(2.0 * ic, h, 40e-12);
    let junction = &solved.junctions["B1"];
    let crossings = extract_crossings(&junction.time_s, &junction.phase).unwrap();
    assert!(
        crossings.pulses.len() >= 2,
        "pulses {}",
        crossings.pulses.len()
    );
    let t0 = crossings.pulses[0].t;
    let t1 = crossings.pulses[1].t;
    let area = trap_between(&junction.time_s, &junction.voltage, t0, t1);
    assert!(
        (area - PHI0).abs() / PHI0 < 1e-3,
        "∫V dt = {area}, Φ₀ = {PHI0}"
    );
}

#[test]
fn overdamped_iv_matches_the_rcsj_curve() {
    let ic = 100e-6;
    let r = 2.0;
    let h = 0.025e-12;
    // The locking transient decays by only about 1% per 0.025 ps step, so the
    // second half has to start well after a few dozen time constants.
    let locked = overdamped(0.5 * ic, h, 80e-12);
    let junction = &locked.junctions["B1"];
    let mid = junction.time_s.len() / 2;
    let mean_abs = junction.voltage[mid..].iter().map(|v| v.abs()).sum::<f64>()
        / (junction.voltage.len() - mid) as f64;
    assert!(mean_abs < 1e-6 * ic * r, "locked mean |V| = {mean_abs}");

    for factor in [1.5, 2.0] {
        let solved = overdamped(factor * ic, h, 80e-12);
        let junction = &solved.junctions["B1"];
        let crossings = extract_crossings(&junction.time_s, &junction.phase).unwrap();
        let t_mid = 40e-12;
        let pulses: Vec<f64> = crossings
            .pulses
            .iter()
            .map(|pulse| pulse.t)
            .filter(|t| *t >= t_mid)
            .collect();
        assert!(pulses.len() >= 2, "factor {factor} pulses {}", pulses.len());
        let t_a = pulses[0];
        let t_b = *pulses.last().unwrap();
        let rate = (pulses.len() - 1) as f64 / (t_b - t_a);
        let mean = trap_between(&junction.time_s, &junction.voltage, t_a, t_b) / (t_b - t_a);
        let expect = r * ((factor * ic).powi(2) - ic * ic).sqrt();
        assert!(
            (mean - expect).abs() / expect < 1e-3,
            "I = {factor} Ic: ⟨V⟩ = {mean}, analytic {expect}"
        );
        assert!(
            (mean - PHI0 * rate).abs() / (PHI0 * rate) < 1e-3,
            "I = {factor} Ic: ⟨V⟩ = {mean}, Φ₀ f = {}",
            PHI0 * rate
        );
    }
}

fn series_rc(h: f64, t_stop: f64) -> Solution {
    let elements = [
        Element::V {
            name: "VS".into(),
            n1: "a".into(),
            n2: "0".into(),
            wave: Wave::Dc(1e-3),
        },
        Element::R {
            name: "R1".into(),
            n1: "a".into(),
            n2: "b".into(),
            ohms: 1.0,
        },
        Element::C {
            name: "C1".into(),
            n1: "b".into(),
            n2: "0".into(),
            farads: 1e-12,
        },
    ];
    solve(&elements, &config(h, t_stop, &["b"], &[])).unwrap()
}

fn rc_error(h: f64) -> f64 {
    let tau = 1e-12;
    let v0 = 1e-3;
    let solved = series_rc(h, tau);
    let node = &solved.nodes["b"];
    let mut worst: f64 = 0.0;
    for (t, v) in node.time_s.iter().zip(&node.voltage) {
        if *t < 0.05 * tau {
            continue;
        }
        let exact = v0 * (1.0 - (-t / tau).exp());
        worst = worst.max((v - exact).abs());
    }
    worst / v0
}

#[test]
fn series_rc_matches_the_exponential() {
    let tau = 1e-12;
    assert!(
        rc_error(tau / 2000.0) < 1e-3,
        "rel {}",
        rc_error(tau / 2000.0)
    );
}

#[test]
fn halving_the_step_quarters_the_rc_error() {
    let tau = 1e-12;
    let coarse = rc_error(tau / 100.0);
    let fine = rc_error(tau / 200.0);
    let ratio = coarse / fine;
    assert!(
        (3.5..=4.5).contains(&ratio),
        "err {coarse} -> {fine}, ratio {ratio}"
    );
}

#[test]
fn series_lc_matches_the_sinusoid() {
    let l: f64 = 1e-12;
    let c: f64 = 1e-12;
    let v0: f64 = 1e-3;
    let omega = 1.0 / (l * c).sqrt();
    let period = 2.0 * PI / omega;
    let h = period / 2000.0;
    let elements = [
        Element::V {
            name: "VS".into(),
            n1: "a".into(),
            n2: "0".into(),
            wave: Wave::Dc(v0),
        },
        Element::L {
            name: "L1".into(),
            n1: "a".into(),
            n2: "b".into(),
            henries: l,
        },
        Element::C {
            name: "C1".into(),
            n1: "b".into(),
            n2: "0".into(),
            farads: c,
        },
    ];
    let solved = solve(&elements, &config(h, period, &["a", "b"], &[])).unwrap();
    let a = &solved.nodes["a"];
    let b = &solved.nodes["b"];
    let amp = v0 * (c / l).sqrt();
    let mut worst: f64 = 0.0;
    let mut seen = 0;
    for i in 0..a.time_s.len() {
        let t = a.time_s[i];
        if t < 0.05 * period {
            continue;
        }
        let exact = amp * (omega * t).sin();
        if exact.abs() < 0.2 * amp {
            continue;
        }
        let current = (PHI0 / (2.0 * PI * l)) * (a.phase[i] - b.phase[i]);
        worst = worst.max((current - exact).abs() / exact.abs());
        seen += 1;
    }
    assert!(seen > 10, "compared {seen} samples");
    assert!(worst < 1e-3, "rel {worst}");
}

#[test]
fn one_newton_iteration_on_a_running_junction_diverges() {
    let ic = 100e-6;
    let mut cfg = config(0.025e-12, 0.025e-12, &[], &["B1"]);
    cfg.max_newton = 1;
    cfg.phase_tol = 1e-10;
    let elements = [
        Element::Jj(Jj {
            name: "B1".into(),
            n1: "j".into(),
            n2: "0".into(),
            ic,
            vg: 2.8e-3,
            c: 0.0,
            r0: 2.0,
            rn: 2.0,
            rtype: 0,
            deltav: 1e-4,
            icfct: PI / 4.0,
            phi: 0.0,
        }),
        Element::I {
            name: "IB".into(),
            n1: "0".into(),
            n2: "j".into(),
            wave: Wave::Dc(2.0 * ic),
        },
    ];
    match solve(&elements, &cfg) {
        Err(SolveError::Diverged { time_s, node }) => {
            assert!(time_s > 0.0, "{time_s}");
            assert_eq!(node, "j");
        }
        other => panic!("expected divergence, got {other:?}"),
    }
}

#[test]
fn mutual_inductance_and_transmission_lines_are_rejected() {
    let k = [Element::K {
        name: "K1".into(),
        l1: "L1".into(),
        l2: "L2".into(),
        k: 0.5,
    }];
    let err = solve(&k, &config(1e-12, 1e-12, &[], &[])).unwrap_err();
    assert!(err.to_string().contains("K1"), "{err}");

    let tx = [Element::Tx(Tx {
        name: "T1".into(),
        n1: "1".into(),
        n2: "0".into(),
        n3: "2".into(),
        n4: "0".into(),
        z0: 2.0,
        td: 1e-12,
    })];
    let err = solve(&tx, &config(1e-12, 1e-12, &[], &[])).unwrap_err();
    assert!(err.to_string().contains("T1"), "{err}");
}

#[test]
fn a_nonpositive_resistance_is_an_error() {
    let elements = [Element::R {
        name: "R1".into(),
        n1: "a".into(),
        n2: "0".into(),
        ohms: 0.0,
    }];
    let err = solve(&elements, &config(1e-12, 1e-12, &[], &[])).unwrap_err();
    assert!(err.to_string().contains("R1"), "{err}");
}
