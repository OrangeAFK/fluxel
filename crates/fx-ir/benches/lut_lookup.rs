//! One 3-axis multilinear lookup. No pass/fail threshold.

use std::collections::BTreeMap;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use fx_ir::{AxisQuery, LutBundle};
use fx_time::Femtos;

fn sample() -> LutBundle {
    LutBundle::from_json(
        r#"{
            "cell": "BENCH",
            "entries": {
                "t_a_q": {
                    "axes": [
                        {"name": "bias_frac", "points": ["0.80", "1.20"]},
                        {"name": "dt_prev_fs", "points": ["6000", "200000"]},
                        {"name": "data_clock_offset_fs", "points": ["-10000", "10000"]}
                    ],
                    "values": [1000, 1100, 1200, 1300, 1400, 1500, 1600, 1700],
                    "interpolation": "multilinear",
                    "valid": {
                        "bias_frac": {"lo": "0.80", "hi": "1.20"},
                        "dt_prev_fs": {"lo": "6000", "hi": "200000"},
                        "data_clock_offset_fs": {"lo": "-10000", "hi": "10000"}
                    },
                    "sigma_fs": 0
                }
            },
            "source": {
                "josim_version": "bench",
                "netlist_sha256": "aa",
                "sweep_sha256": "bb",
                "timestep_fs": 25
            }
        }"#,
    )
    .unwrap()
}

fn lut_lookup(c: &mut Criterion) {
    let bundle = sample();
    let mut point = BTreeMap::new();
    point.insert("bias_frac".to_string(), AxisQuery::Fraction(1.0));
    point.insert("dt_prev_fs".to_string(), AxisQuery::Time(Femtos(100_000)));
    point.insert(
        "data_clock_offset_fs".to_string(),
        AxisQuery::Time(Femtos(0)),
    );
    c.bench_function("lut_lookup_3d", |b| {
        b.iter(|| black_box(bundle.lookup("t_a_q", black_box(&point)).unwrap()))
    });
}

criterion_group!(benches, lut_lookup);
criterion_main!(benches);
