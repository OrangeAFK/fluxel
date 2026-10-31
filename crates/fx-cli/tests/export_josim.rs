//! `fx-cli export-josim` writes the deck from `fx-flow`.

use std::path::PathBuf;
use std::process::Command;

use fx_ir::design::Design;
use fx_ir::stimulus::Stimulus;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fx_cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_fx-cli"))
        .args(args)
        .output()
        .expect("spawn fx-cli")
}

#[test]
fn export_josim_requires_every_flag() {
    let output = fx_cli(&["export-josim"]);
    assert_eq!(output.status.code(), Some(1));
    let err = String::from_utf8(output.stderr).unwrap();
    for flag in ["export-josim", "--design", "--stimulus", "--lib", "--out"] {
        assert!(err.contains(flag), "{err}");
    }

    let root = repo_root();
    let design = root.join("examples/jtl_chain/design.json");
    let stimulus = root.join("examples/jtl_chain/stimulus.json");
    let lib = root.join("lib/families/rsfq");
    let partial = [
        vec!["export-josim"],
        vec!["export-josim", "--design", design.to_str().unwrap()],
        vec![
            "export-josim",
            "--design",
            design.to_str().unwrap(),
            "--stimulus",
            stimulus.to_str().unwrap(),
        ],
        vec![
            "export-josim",
            "--design",
            design.to_str().unwrap(),
            "--stimulus",
            stimulus.to_str().unwrap(),
            "--lib",
            lib.to_str().unwrap(),
        ],
    ];
    for args in partial {
        let output = fx_cli(&args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
    }
}

#[test]
fn export_josim_matches_library() {
    let root = repo_root();
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("jtl_chain_export.cir");
    let design_path = root.join("examples/jtl_chain/design.json");
    let stimulus_path = root.join("examples/jtl_chain/stimulus.json");
    let lib = root.join("lib/families/rsfq");
    let output = fx_cli(&[
        "export-josim",
        "--design",
        design_path.to_str().unwrap(),
        "--stimulus",
        stimulus_path.to_str().unwrap(),
        "--lib",
        lib.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let written = std::fs::read_to_string(&out).unwrap();
    let cells = fx_cli::load_cells(&lib).unwrap();
    let design = Design::parse(&std::fs::read_to_string(&design_path).unwrap()).unwrap();
    let stimulus = Stimulus::parse(&std::fs::read_to_string(&stimulus_path).unwrap()).unwrap();
    let expected = fx_flow::export_josim(&design, &stimulus, &cells, &root).unwrap();
    assert_eq!(written, expected);
}

#[test]
fn export_josim_deck_opts_reach_the_file() {
    let root = repo_root();
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("jtl_opts.cir");
    let output = fx_cli(&[
        "export-josim",
        "--design",
        root.join("examples/jtl_chain/design.json")
            .to_str()
            .unwrap(),
        "--stimulus",
        root.join("examples/jtl_chain/stimulus.json")
            .to_str()
            .unwrap(),
        "--lib",
        root.join("lib/families/rsfq").to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--bias",
        "0.8",
        "--step",
        "25",
        "--stop",
        "200000",
        "--current-in",
        "--resistor-out",
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let written = std::fs::read_to_string(&out).unwrap();
    assert!(written.contains(".param bias_mul=0.8\n"), "{written}");
    assert!(written.contains(".tran 0.025p 200p 0\n"));
    assert!(!written.contains("Xfxsrc_"));
    assert!(!written.contains("Xfxload_"));
    assert!(written.contains("Rfxload_out "));
}
