//! 10_000-instance clocked design. Reports events/s. No pass/fail threshold.

use std::collections::{BTreeMap, BTreeSet};
use std::hint::black_box;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use fx_ir::cell::{CellDef, Face, Fire, Ports, Provenance, Transition};
use fx_ir::design::{Design, Instance, Net, PinRef, Wire};
use fx_ir::stimulus::Stimulus;
use fx_kernel::{KernelError, Simulator, ViolationPolicy};
use fx_time::Femtos;

const DFFS: usize = 5_000;
const SPLITS: usize = 4_999;
const CYCLES: i64 = 8;
const PERIOD: i64 = 1_000_000;
const MARGIN: i64 = 100_000;
const SNAPSHOT_EVERY: u64 = 1024;

fn clocked_10k(c: &mut Criterion) {
    let built = build();
    let events = count_events(&built);
    let mut group = c.benchmark_group("kernel");
    group.throughput(Throughput::Elements(events));
    group.bench_function("clocked_10k", |b| {
        b.iter_batched(
            || ready(new_sim(&built)),
            |mut sim| {
                finish(&mut sim);
                black_box(sim.log().events().len())
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

struct Built {
    design: Design,
    cells: BTreeMap<String, CellDef>,
    stimulus: Stimulus,
}

fn build() -> Built {
    let mut instances = BTreeMap::new();
    let mut nets = Vec::new();
    instances.insert("j0".to_string(), instance("JTL"));
    for index in 0..DFFS {
        instances.insert(format!("d{index}"), instance("DFF"));
    }
    nets.push(drive_port("d", "j0", "a"));
    nets.push(drive_inst("j0", "q", "d0", "a"));
    for index in 0..DFFS - 1 {
        nets.push(drive_inst(
            &format!("d{index}"),
            "q",
            &format!("d{}", index + 1),
            "a",
        ));
    }
    nets.push(Net {
        driver: PinRef::Instance {
            instance: format!("d{}", DFFS - 1),
            port: "q".to_string(),
        },
        sink: PinRef::Port {
            port: "out".to_string(),
        },
        wire: Wire::Direct,
    });

    let mut need: Vec<PinRef> = (0..DFFS)
        .map(|index| PinRef::Instance {
            instance: format!("d{index}"),
            port: "clk".to_string(),
        })
        .collect();
    let mut split_index = 0usize;
    while need.len() > 1 {
        let mut next = Vec::new();
        while need.len() >= 2 {
            let right = match need.pop() {
                Some(pin) => pin,
                None => panic!("clock tree pair missing a pin"),
            };
            let left = match need.pop() {
                Some(pin) => pin,
                None => panic!("clock tree pair missing a pin"),
            };
            let name = format!("s{split_index}");
            split_index += 1;
            instances.insert(name.clone(), instance("SPLIT"));
            nets.push(Net {
                driver: PinRef::Instance {
                    instance: name.clone(),
                    port: "q0".to_string(),
                },
                sink: left,
                wire: Wire::Direct,
            });
            nets.push(Net {
                driver: PinRef::Instance {
                    instance: name.clone(),
                    port: "q1".to_string(),
                },
                sink: right,
                wire: Wire::Direct,
            });
            next.push(PinRef::Instance {
                instance: name,
                port: "a".to_string(),
            });
        }
        if let Some(rest) = need.pop() {
            next.push(rest);
        }
        need = next;
    }
    let root = match need.pop() {
        Some(pin) => pin,
        None => panic!("clock tree has no root"),
    };
    if need.pop().is_some() {
        panic!("clock tree has more than one root");
    }
    nets.push(Net {
        driver: PinRef::Port {
            port: "clk".to_string(),
        },
        sink: root,
        wire: Wire::Direct,
    });
    if split_index != SPLITS {
        panic!("expected {SPLITS} splitters, built {split_index}");
    }
    if instances.len() != 10_000 {
        panic!("expected 10000 instances, built {}", instances.len());
    }

    let mut cells = BTreeMap::new();
    cells.insert("JTL".to_string(), jtl());
    cells.insert("SPLIT".to_string(), split());
    cells.insert("DFF".to_string(), dff());
    Built {
        design: Design {
            instances,
            nets,
            ports: Ports {
                inputs: vec!["d".to_string(), "clk".to_string()],
                outputs: vec!["out".to_string()],
            },
            clock_domains: BTreeMap::new(),
            bias_domains: BTreeSet::from(["b".to_string()]),
        },
        cells,
        stimulus: stimulus(),
    }
}

fn stimulus() -> Stimulus {
    let mut state = 0x5eed_u64;
    let mut data = Vec::new();
    let mut clocks = Vec::new();
    for cycle in 1..=CYCLES {
        let clock = cycle * PERIOD;
        clocks.push(Femtos(clock));
        if lcg(&mut state) & 1 == 1 {
            data.push(Femtos(clock - MARGIN));
        }
    }
    let mut pulses = BTreeMap::new();
    pulses.insert("d".to_string(), data);
    pulses.insert("clk".to_string(), clocks);
    Stimulus(pulses)
}

fn lcg(state: &mut u64) -> u64 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *state
}

fn count_events(built: &Built) -> u64 {
    let mut sim = ready(new_sim(built));
    finish(&mut sim);
    let events = sim.log().events().len() as u64;
    if events == 0 {
        panic!("10k bench produced no events");
    }
    events
}

fn new_sim(built: &Built) -> Result<Simulator, KernelError> {
    Simulator::new(
        built.design.clone(),
        built.cells.clone(),
        built.stimulus.clone(),
        ViolationPolicy::Record,
        SNAPSHOT_EVERY,
    )
}

fn ready(sim: Result<Simulator, KernelError>) -> Simulator {
    match sim {
        Ok(sim) => sim,
        Err(err) => panic!("10k bench setup failed: {err}"),
    }
}

fn finish(sim: &mut Simulator) {
    match sim.run() {
        Ok(()) => {}
        Err(err) => panic!("10k bench run failed: {err}"),
    }
}

fn instance(cell: &str) -> Instance {
    Instance {
        family: "rsfq".to_string(),
        cell: cell.to_string(),
        bias_domain: "b".to_string(),
        macro_parent: None,
    }
}

fn drive_port(port: &str, instance: &str, sink: &str) -> Net {
    Net {
        driver: PinRef::Port {
            port: port.to_string(),
        },
        sink: PinRef::Instance {
            instance: instance.to_string(),
            port: sink.to_string(),
        },
        wire: Wire::Direct,
    }
}

fn drive_inst(from: &str, from_port: &str, to: &str, to_port: &str) -> Net {
    Net {
        driver: PinRef::Instance {
            instance: from.to_string(),
            port: from_port.to_string(),
        },
        sink: PinRef::Instance {
            instance: to.to_string(),
            port: to_port.to_string(),
        },
        wire: Wire::Direct,
    }
}

fn jtl() -> CellDef {
    buffer_cell("JTL", &[("q", "d")])
}

fn split() -> CellDef {
    buffer_cell("SPLIT", &[("q0", "d0"), ("q1", "d1")])
}

fn buffer_cell(name: &str, fires: &[(&str, &str)]) -> CellDef {
    let mut outputs = Vec::new();
    let mut fire = Vec::new();
    let mut delays = BTreeMap::new();
    let mut faces = BTreeMap::from([("a".to_string(), Face::W)]);
    for (index, (port, delay)) in fires.iter().enumerate() {
        outputs.push((*port).to_string());
        fire.push(Fire {
            port: (*port).to_string(),
            delay: (*delay).to_string(),
        });
        delays.insert((*delay).to_string(), Femtos(1_000));
        let face = if index == 0 { Face::E } else { Face::N };
        faces.insert((*port).to_string(), face);
    }
    CellDef {
        name: name.to_string(),
        family: "rsfq".to_string(),
        provenance: Provenance::Behavioral,
        ports: Ports {
            inputs: vec!["a".to_string()],
            outputs,
        },
        faces,
        states: vec!["s".to_string()],
        initial: "s".to_string(),
        transitions: vec![Transition {
            from: "s".to_string(),
            on: "a".to_string(),
            to: "s".to_string(),
            fire,
            note: None,
        }],
        constraints: vec![],
        jj_count: Some(1),
        bias_ua: None,
        netlist: None,
        lut: None,
        ref_jj: BTreeMap::new(),
        notes: None,
        delays,
    }
}

fn dff() -> CellDef {
    CellDef {
        name: "DFF".to_string(),
        family: "rsfq".to_string(),
        provenance: Provenance::Behavioral,
        ports: Ports {
            inputs: vec!["a".to_string(), "clk".to_string()],
            outputs: vec!["q".to_string()],
        },
        faces: BTreeMap::from([
            ("a".to_string(), Face::W),
            ("clk".to_string(), Face::S),
            ("q".to_string(), Face::E),
        ]),
        states: vec!["empty".to_string(), "stored".to_string()],
        initial: "empty".to_string(),
        transitions: vec![
            Transition {
                from: "empty".to_string(),
                on: "a".to_string(),
                to: "stored".to_string(),
                fire: vec![],
                note: None,
            },
            Transition {
                from: "empty".to_string(),
                on: "clk".to_string(),
                to: "empty".to_string(),
                fire: vec![],
                note: None,
            },
            Transition {
                from: "stored".to_string(),
                on: "a".to_string(),
                to: "stored".to_string(),
                fire: vec![],
                note: None,
            },
            Transition {
                from: "stored".to_string(),
                on: "clk".to_string(),
                to: "empty".to_string(),
                fire: vec![Fire {
                    port: "q".to_string(),
                    delay: "t_clk_q".to_string(),
                }],
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
        delays: BTreeMap::from([("t_clk_q".to_string(), Femtos(5_000))]),
    }
}

criterion_group!(benches, clocked_10k);
criterion_main!(benches);
