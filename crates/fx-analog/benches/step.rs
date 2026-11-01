//! One RC step and one junction step. No pass/fail threshold.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use fx_analog::{Element, Jj, SolveConfig, Wave, solve};

const H: f64 = 0.025e-12;

fn step(c: &mut Criterion) {
    let mut group = c.benchmark_group("analog");
    let rc_elements = rc();
    let rc_cfg = config(&["b"], &[]);
    group.bench_function("rc_step", |b| {
        b.iter(|| {
            black_box(solve(black_box(&rc_elements), black_box(&rc_cfg)).expect("rc step"));
        });
    });
    let jj_elements = junction();
    let jj_cfg = config(&["j"], &["B1"]);
    group.bench_function("junction_step", |b| {
        b.iter(|| {
            black_box(solve(black_box(&jj_elements), black_box(&jj_cfg)).expect("junction step"));
        });
    });
    group.finish();
}

fn config(nodes: &[&str], junctions: &[&str]) -> SolveConfig {
    SolveConfig {
        h: H,
        t_stop: H,
        nodes: nodes.iter().map(|name| (*name).to_string()).collect(),
        junctions: junctions.iter().map(|name| (*name).to_string()).collect(),
        phase_tol: 1e-10,
        current_tol: 1e-15,
        max_newton: 32,
    }
}

fn rc() -> [Element; 3] {
    [
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
    ]
}

fn junction() -> [Element; 2] {
    let ic = 100e-6;
    [
        Element::I {
            name: "IB".into(),
            n1: "0".into(),
            n2: "j".into(),
            wave: Wave::Dc(2.0 * ic),
        },
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
            icfct: std::f64::consts::PI / 4.0,
            phi: 0.0,
        }),
    ]
}

criterion_group!(benches, step);
criterion_main!(benches);
