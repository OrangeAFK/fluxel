//! Each example circuit, checked against a hand-counted pulse map.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;

use sha2::{Digest, Sha256};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn lib_dir() -> PathBuf {
    repo_root().join("lib/families/rsfq")
}

fn sim(
    design: &Path,
    stimulus: &Path,
    out: &Path,
    log: &Path,
    until: Option<&str>,
    violations: bool,
) -> Output {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_fx-cli"));
    cmd.arg("sim")
        .arg("--design")
        .arg(design)
        .arg("--stimulus")
        .arg(stimulus)
        .arg("--lib")
        .arg(lib_dir())
        .arg("--out")
        .arg(out)
        .arg("--log")
        .arg(log);
    if let Some(until) = until {
        cmd.arg("--until").arg(until);
    }
    if violations {
        cmd.arg("--violations");
    }
    cmd.output().expect("spawn fx-cli")
}

fn counts(json: &str) -> BTreeMap<String, usize> {
    let pulses: BTreeMap<String, Vec<i64>> = serde_json::from_str(json).expect("pulse json");
    pulses
        .into_iter()
        .map(|(key, times)| (key, times.len()))
        .collect()
}

fn expect_map(pairs: &[(&str, usize)]) -> BTreeMap<String, usize> {
    pairs
        .iter()
        .map(|(key, count)| ((*key).to_string(), *count))
        .collect()
}

/// Two input pulses, each copied by all eight JTL stages (`t_a_q` = 1819).
fn jtl_counts() -> BTreeMap<String, usize> {
    (0..8).map(|stage| (format!("j{stage}.q"), 2)).collect()
}

/// One data pulse and four clocks. The 1→4 tree (`s0`–`s2`) fires on every clock.
/// Each DFF emits the bit once as it shifts to the next stage.
fn shift_counts() -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for name in ["s0", "s1", "s2"] {
        counts.insert(format!("{name}.q0"), 4);
        counts.insert(format!("{name}.q1"), 4);
    }
    for stage in 0..4 {
        counts.insert(format!("dff{stage}.q"), 1);
    }
    counts
}

/// Two input pulses. Root `s0` and leaves `s1`/`s2` each copy both pulses on `q0` and `q1`.
fn splitter_counts() -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for name in ["s0", "s1", "s2"] {
        counts.insert(format!("{name}.q0"), 2);
        counts.insert(format!("{name}.q1"), 2);
    }
    counts
}

/// Vector 1+1+1 over three clocks. Both data inputs cancel in `xor_ab` and store in `and_ab`.
/// `cin` is delayed one cycle; `and_ab` is delayed one cycle into the OR; `sum` is delayed
/// one cycle so it lines up with `cout`. Sum and cout are each one pulse.
fn adder_counts() -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for name in ["c0", "c1", "c2", "c3", "c4", "c5", "c6"] {
        counts.insert(format!("{name}.q0"), 3);
        counts.insert(format!("{name}.q1"), 3);
    }
    for name in ["split_a", "split_b", "split_cin"] {
        counts.insert(format!("{name}.q0"), 1);
        counts.insert(format!("{name}.q1"), 1);
    }
    for name in [
        "and_ab", "dff_cin", "xor_sum", "dff_and", "dff_sum", "or_cout",
    ] {
        counts.insert(format!("{name}.q"), 1);
    }
    counts
}

/// Eight inputs. A TFF emits on every second pulse, so `t0` fires 4 and `t1` fires 2.
fn tff_counts() -> BTreeMap<String, usize> {
    expect_map(&[("t0.q", 4), ("t1.q", 2)])
}

#[test]
fn tff_divider_pulse_times() {
    // TFF starts clear. t_a_q is 5000 fs and every wire is direct (delay 0).
    // clear + a -> set, and emits nothing. set + a -> clear, and q is at t + 5000.
    // in: 0, 20000, 40000, 60000, 80000, 100000, 120000, 140000.
    // t0 emits on the 2nd, 4th, 6th, and 8th inputs:
    //   20000 + 5000 = 25000
    //   60000 + 5000 = 65000
    //   100000 + 5000 = 105000
    //   140000 + 5000 = 145000
    // t1 starts clear and sees those four pulses, so it emits on its 2nd and 4th:
    //   65000 + 5000 = 70000
    //   145000 + 5000 = 150000
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let dir = repo_root().join("examples/tff_divider");
    let out = tmp.join("tff_times_pulses.json");
    let log = tmp.join("tff_times_events.cbor");
    let output = sim(
        &dir.join("design.json"),
        &dir.join("stimulus.json"),
        &out,
        &log,
        None,
        false,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pulses: BTreeMap<String, Vec<i64>> =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("read pulses"))
            .expect("pulse json");
    let expected: BTreeMap<String, Vec<i64>> = [
        ("t0.q".to_string(), vec![25000, 65000, 105000, 145000]),
        ("t1.q".to_string(), vec![70000, 150000]),
    ]
    .into_iter()
    .collect();
    assert_eq!(pulses, expected);
}

fn assert_clean(output: &Output, out: &Path, log: &Path, expected: &BTreeMap<String, usize>) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let pulses = std::fs::read_to_string(out).expect("read pulses");
    assert_eq!(counts(&pulses), *expected);
    let bytes = std::fs::read(log).expect("read log");
    let digest = Sha256::digest(&bytes);
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let hashed = std::process::Command::new(env!("CARGO_BIN_EXE_fx-cli"))
        .arg("hash")
        .arg("--log")
        .arg(log)
        .output()
        .expect("spawn fx-cli hash");
    assert_eq!(hashed.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(hashed.stdout).expect("hash stdout"),
        format!("{hex}\n")
    );
}

#[test]
fn examples_match_hand_counts() {
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let cases = [
        ("jtl_chain", jtl_counts()),
        ("shift_register", shift_counts()),
        ("splitter_tree", splitter_counts()),
        ("full_adder", adder_counts()),
        ("tff_divider", tff_counts()),
    ];
    for (name, expected) in cases {
        let dir = repo_root().join("examples").join(name);
        let out = tmp.join(format!("{name}_pulses.json"));
        let log = tmp.join(format!("{name}_events.cbor"));
        let output = sim(
            &dir.join("design.json"),
            &dir.join("stimulus.json"),
            &out,
            &log,
            None,
            false,
        );
        assert_clean(&output, &out, &log, &expected);
    }
}

#[test]
fn jtl_until_zero_logs_first_stage_only() {
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let dir = repo_root().join("examples/jtl_chain");
    let out = tmp.join("jtl_until_pulses.json");
    let log = tmp.join("jtl_until_events.cbor");
    let output = sim(
        &dir.join("design.json"),
        &dir.join("stimulus.json"),
        &out,
        &log,
        Some("0"),
        false,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let pulses = std::fs::read_to_string(&out).expect("read pulses");
    assert_eq!(counts(&pulses), expect_map(&[("j0.q", 1)]));
}

const HOLD_DESIGN: &str = r#"{
  "instances": {
    "ff": { "family": "rsfq", "cell": "DFF", "bias_domain": "b" }
  },
  "nets": [
    { "driver": { "port": "d" }, "sink": { "instance": "ff", "port": "a" }, "wire": { "kind": "direct" } },
    { "driver": { "port": "clk" }, "sink": { "instance": "ff", "port": "clk" }, "wire": { "kind": "direct" } },
    { "driver": { "instance": "ff", "port": "q" }, "sink": { "port": "q" }, "wire": { "kind": "direct" } }
  ],
  "ports": { "in": ["d", "clk"], "out": ["q"] },
  "clock_domains": { "clk": ["ff"] },
  "bias_domains": ["b"]
}
"#;

#[test]
fn hold_violation_exits_2() {
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let design = tmp.join("hold_design.json");
    let stimulus = tmp.join("hold_stimulus.json");
    std::fs::write(&design, HOLD_DESIGN).expect("write design");
    std::fs::write(&stimulus, r#"{ "d": [500], "clk": [0] }"#).expect("write stimulus");
    let output = sim(
        &design,
        &stimulus,
        &tmp.join("hold_pulses.json"),
        &tmp.join("hold_events.cbor"),
        None,
        true,
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let err = String::from_utf8(output.stderr).expect("stderr");
    assert!(err.contains("violations: 1"), "{err}");
    assert!(err.contains("hold ff"), "{err}");
}

#[test]
fn missing_design_exits_1() {
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let output = sim(
        &tmp.join("no-such-design.json"),
        &repo_root().join("examples/jtl_chain/stimulus.json"),
        &tmp.join("missing_pulses.json"),
        &tmp.join("missing_events.cbor"),
        None,
        false,
    );
    assert_eq!(output.status.code(), Some(1));
}
