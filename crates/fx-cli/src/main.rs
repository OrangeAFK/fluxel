//! `fx-cli sim` and `fx-cli hash`.

use std::path::PathBuf;

use fx_cli::{
    CliError, hash_hex, load_cells, parse_until, read_bytes, read_text, simulate, write_bytes,
};

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

fn run_hash(args: &[String]) -> Result<u8, CliError> {
    if args.len() != 2 || args[0] != "--log" {
        return Err(CliError::Usage);
    }
    let path = PathBuf::from(&args[1]);
    let bytes = read_bytes(&path)?;
    println!("{}", hash_hex(&bytes));
    Ok(0)
}
