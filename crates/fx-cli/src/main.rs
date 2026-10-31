//! `fx-cli sim`, `fx-cli hash`, and `fx-cli export-josim`.

use std::path::{Path, PathBuf};

use fx_cli::{
    CliError, hash_hex, load_cells, parse_until, read_bytes, read_text, simulate, write_bytes,
};
use fx_flow::{DeckOpts, export_josim_with};
use fx_ir::design::Design;
use fx_ir::stimulus::Stimulus;

fn main() -> std::process::ExitCode {
    match dispatch(std::env::args().skip(1)) {
        Ok(code) => std::process::ExitCode::from(code),
        Err(err) => {
            eprintln!("{err}");
            std::process::ExitCode::from(1)
        }
    }
}

fn dispatch(args: impl IntoIterator<Item = String>) -> Result<u8, CliError> {
    let args: Vec<String> = args.into_iter().collect();
    match args.first().map(String::as_str) {
        Some("sim") => run_sim(&args[1..]),
        Some("hash") => run_hash(&args[1..]),
        Some("export-josim") => run_export(&args[1..]),
        _ => Err(CliError::Usage),
    }
}

struct SimArgs {
    design: PathBuf,
    stimulus: PathBuf,
    lib: PathBuf,
    until: Option<i64>,
    out: Option<PathBuf>,
    log: Option<PathBuf>,
    violations: bool,
}

fn run_sim(args: &[String]) -> Result<u8, CliError> {
    let parsed = parse_sim(args)?;
    let cells = load_cells(&parsed.lib)?;
    let design = read_text(&parsed.design)?;
    let stimulus = read_text(&parsed.stimulus)?;
    let report = simulate(&design, &stimulus, cells, parsed.until)?;
    if let Some(path) = &parsed.out {
        write_bytes(path, report.pulses.as_bytes())?;
    }
    if let Some(path) = &parsed.log {
        write_bytes(path, &report.log)?;
    }
    eprint!("{}", report.stderr_text(parsed.violations));
    if report.violations > 0 { Ok(2) } else { Ok(0) }
}

fn parse_sim(args: &[String]) -> Result<SimArgs, CliError> {
    let mut design = None;
    let mut stimulus = None;
    let mut lib = None;
    let mut until = None;
    let mut out = None;
    let mut log = None;
    let mut violations = false;
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--design" => set_path(&mut design, next_value(&mut rest)?)?,
            "--stimulus" => set_path(&mut stimulus, next_value(&mut rest)?)?,
            "--lib" => set_path(&mut lib, next_value(&mut rest)?)?,
            "--out" => set_path(&mut out, next_value(&mut rest)?)?,
            "--log" => set_path(&mut log, next_value(&mut rest)?)?,
            "--until" => {
                let value = next_value(&mut rest)?;
                if until.is_some() {
                    return Err(CliError::Usage);
                }
                until = Some(parse_until(value)?);
            }
            "--violations" => {
                if violations {
                    return Err(CliError::Usage);
                }
                violations = true;
            }
            _ => return Err(CliError::Usage),
        }
    }
    Ok(SimArgs {
        design: design.ok_or(CliError::Usage)?,
        stimulus: stimulus.ok_or(CliError::Usage)?,
        lib: lib.ok_or(CliError::Usage)?,
        until,
        out,
        log,
        violations,
    })
}

fn set_path(slot: &mut Option<PathBuf>, value: &str) -> Result<(), CliError> {
    if slot.is_some() {
        return Err(CliError::Usage);
    }
    *slot = Some(PathBuf::from(value));
    Ok(())
}

fn next_value<'a>(rest: &mut impl Iterator<Item = &'a String>) -> Result<&'a str, CliError> {
    rest.next().map(String::as_str).ok_or(CliError::Usage)
}

struct ExportArgs {
    design: PathBuf,
    stimulus: PathBuf,
    lib: PathBuf,
    out: PathBuf,
    bias: Option<f64>,
    step: Option<i64>,
    stop: Option<i64>,
    current_inputs: bool,
    resistor_outputs: bool,
}

fn run_export(args: &[String]) -> Result<u8, CliError> {
    let parsed = parse_export(args)?;
    let cells = load_cells(&parsed.lib)?;
    let design = Design::parse(&read_text(&parsed.design)?)?;
    let stimulus = Stimulus::parse(&read_text(&parsed.stimulus)?)?;
    let root = find_root(&parsed.lib)?;
    let mut opts = DeckOpts::default();
    if let Some(bias) = parsed.bias {
        opts.bias_mul = bias;
    }
    if let Some(step) = parsed.step {
        opts.step_fs = step;
    }
    opts.stop_fs = parsed.stop;
    opts.current_inputs = parsed.current_inputs;
    opts.resistor_outputs = parsed.resistor_outputs;
    let deck = export_josim_with(&design, &stimulus, &cells, &root, &opts)?;
    write_bytes(&parsed.out, deck.as_bytes())?;
    Ok(0)
}

fn parse_export(args: &[String]) -> Result<ExportArgs, CliError> {
    let mut design = None;
    let mut stimulus = None;
    let mut lib = None;
    let mut out = None;
    let mut bias = None;
    let mut step = None;
    let mut stop = None;
    let mut current_inputs = false;
    let mut resistor_outputs = false;
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--design" => set_path(&mut design, next_value(&mut rest)?)?,
            "--stimulus" => set_path(&mut stimulus, next_value(&mut rest)?)?,
            "--lib" => set_path(&mut lib, next_value(&mut rest)?)?,
            "--out" => set_path(&mut out, next_value(&mut rest)?)?,
            "--bias" => {
                if bias.is_some() {
                    return Err(CliError::Usage);
                }
                let value = next_value(&mut rest)?;
                bias = Some(value.parse().map_err(|_| CliError::BadBias {
                    value: value.to_string(),
                })?);
            }
            "--step" => {
                if step.is_some() {
                    return Err(CliError::Usage);
                }
                let value = next_value(&mut rest)?;
                step = Some(value.parse().map_err(|_| CliError::BadStep {
                    value: value.to_string(),
                })?);
            }
            "--stop" => {
                if stop.is_some() {
                    return Err(CliError::Usage);
                }
                let value = next_value(&mut rest)?;
                stop = Some(value.parse().map_err(|_| CliError::BadStop {
                    value: value.to_string(),
                })?);
            }
            "--current-in" => {
                if current_inputs {
                    return Err(CliError::Usage);
                }
                current_inputs = true;
            }
            "--resistor-out" => {
                if resistor_outputs {
                    return Err(CliError::Usage);
                }
                resistor_outputs = true;
            }
            _ => return Err(CliError::Usage),
        }
    }
    Ok(ExportArgs {
        design: design.ok_or(CliError::Usage)?,
        stimulus: stimulus.ok_or(CliError::Usage)?,
        lib: lib.ok_or(CliError::Usage)?,
        out: out.ok_or(CliError::Usage)?,
        bias,
        step,
        stop,
        current_inputs,
        resistor_outputs,
    })
}

fn find_root(lib: &Path) -> Result<PathBuf, CliError> {
    let mut dir = lib.canonicalize().map_err(|source| CliError::Read {
        path: lib.display().to_string(),
        source,
    })?;
    loop {
        if dir.join("lib/rsfqlib").is_dir() {
            return Ok(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    Err(CliError::NoRoot {
        path: lib.display().to_string(),
    })
}

fn run_hash(args: &[String]) -> Result<u8, CliError> {
    if args.len() != 2 || args[0] != "--log" {
        return Err(CliError::Usage);
    }
    let path = PathBuf::from(&args[1]);
    let bytes = read_bytes(&path)?;
    println!("{}", hash_hex(&bytes));
    Ok(0)
}
