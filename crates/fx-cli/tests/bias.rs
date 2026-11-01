//! `--bias` reaches the kernel. 0.5 is outside the JTL valid box; 1.0 is not.

use std::path::Path;
use std::process::Output;

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(design: &Path, stimulus: &Path, log: &Path, bias: &str) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_fx-cli"))
        .arg("sim")
        .arg("--design")
        .arg(design)
        .arg("--stimulus")
        .arg(stimulus)
        .arg("--lib")
        .arg(repo_root().join("lib/families/rsfq"))
        .arg("--log")
        .arg(log)
        .arg("--bias")
        .arg(bias)
        .output()
        .expect("spawn fx-cli")
}

fn uncharacterized(log: &[u8]) -> usize {
    log.windows(b"Uncharacterized".len())
        .filter(|window| *window == b"Uncharacterized")
        .count()
}

#[test]
fn bias_half_is_uncharacterized_and_nominal_is_not() {
    let dir = std::env::temp_dir().join(format!("fluxel-bias-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let design = dir.join("design.json");
    let stimulus = dir.join("stimulus.json");
    std::fs::write(
        &design,
        r#"{
  "instances": {
    "j0": { "family": "rsfq", "cell": "JTL", "bias_domain": "b" }
  },
  "nets": [
    { "driver": { "port": "a" }, "sink": { "instance": "j0", "port": "a" }, "wire": { "kind": "direct" } },
    { "driver": { "instance": "j0", "port": "q" }, "sink": { "port": "q" }, "wire": { "kind": "direct" } }
  ],
  "ports": { "in": ["a"], "out": ["q"] },
  "clock_domains": {},
  "bias_domains": ["b"]
}
"#,
    )
    .expect("design");
    std::fs::write(&stimulus, r#"{ "a": [0] }"#).expect("stimulus");

    let low_log = dir.join("low.cbor");
    let nom_log = dir.join("nom.cbor");
    let low = run(&design, &stimulus, &low_log, "0.5");
    let nom = run(&design, &stimulus, &nom_log, "1.0");
    assert!(
        low.status.success(),
        "bias 0.5: {}",
        String::from_utf8_lossy(&low.stderr)
    );
    assert!(
        nom.status.success(),
        "bias 1.0: {}",
        String::from_utf8_lossy(&nom.stderr)
    );
    assert!(uncharacterized(&std::fs::read(&low_log).expect("low log")) >= 1);
    assert_eq!(
        uncharacterized(&std::fs::read(&nom_log).expect("nom log")),
        0
    );
}
