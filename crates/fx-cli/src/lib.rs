//! Headless simulation and event-log hashing.

use std::collections::BTreeMap;
use std::path::Path;

use fx_ir::cell::{CellDef, CellError};
use fx_ir::design::{Design, DesignError};
use fx_ir::stimulus::{Stimulus, StimulusError};
use fx_kernel::{EventLog, KernelError, LogEvent, Simulator, ViolationPolicy};
use fx_time::Femtos;
use sha2::{Digest, Sha256};
use thiserror::Error;

const SNAPSHOT_EVERY: u64 = 1024;

/// Why a command was rejected.
#[derive(Debug, Error)]
pub enum CliError {
    #[error(
        "usage: fx-cli sim --design <file> --stimulus <file> --lib <dir> [--until <fs>] [--out <file>] [--log <file>] [--violations]\n       fx-cli hash --log <file>\n       fx-cli export-josim --design <file> --stimulus <file> --lib <dir> --out <file> [--bias <mul>] [--step <fs>] [--stop <fs>] [--current-in] [--resistor-out]"
    )]
    Usage,
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot read library {path}: {source}")]
    LibraryIo {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("no cell files in {path}")]
    EmptyLibrary { path: String },
    #[error("duplicate cell {name}")]
    DuplicateCell { name: String },
    #[error("invalid cell {path}: {source}")]
    Cell {
        path: String,
        #[source]
        source: CellError,
    },
    #[error(transparent)]
    Design(#[from] DesignError),
    #[error(transparent)]
    Stimulus(#[from] StimulusError),
    #[error(transparent)]
    Kernel(#[from] KernelError),
    #[error("invalid --until {value}")]
    BadUntil { value: String },
    #[error("invalid --bias {value}")]
    BadBias { value: String },
    #[error("invalid --step {value}")]
    BadStep { value: String },
    #[error("invalid --stop {value}")]
    BadStop { value: String },
    #[error("cannot find repository root from {path}")]
    NoRoot { path: String },
    #[error(transparent)]
    Flow(#[from] fx_flow::FlowError),
}

/// Pulse JSON, CBOR log, and the counts printed on stderr.
pub struct SimReport {
    pub events: usize,
    pub violations: usize,
    pub sim_time_fs: i64,
    pub pulses: String,
    pub log: Vec<u8>,
    pub violation_lines: Vec<String>,
}

impl SimReport {
    /// Three summary lines, then one line per violation when `list_violations` is set.
    pub fn stderr_text(&self, list_violations: bool) -> String {
        let mut text = format!(
            "events: {}\nviolations: {}\nsim_time_fs: {}\n",
            self.events, self.violations, self.sim_time_fs
        );
        if list_violations {
            for line in &self.violation_lines {
                text.push_str(line);
                text.push('\n');
            }
        }
        text
    }
}

/// Read a text file.
pub fn read_text(path: &Path) -> Result<String, CliError> {
    std::fs::read_to_string(path).map_err(|source| CliError::Read {
        path: path.display().to_string(),
        source,
    })
}

/// Read a binary file.
pub fn read_bytes(path: &Path) -> Result<Vec<u8>, CliError> {
    std::fs::read(path).map_err(|source| CliError::Read {
        path: path.display().to_string(),
        source,
    })
}

/// Write bytes to `path`.
pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    std::fs::write(path, bytes).map_err(|source| CliError::Write {
        path: path.display().to_string(),
        source,
    })
}

/// Parse an `--until` femtosecond timestamp.
pub fn parse_until(value: &str) -> Result<i64, CliError> {
    value.parse().map_err(|_| CliError::BadUntil {
        value: value.to_string(),
    })
}

/// Load every `*.cell.json` in `dir`, keyed by the cell's name.
pub fn load_cells(dir: &Path) -> Result<BTreeMap<String, CellDef>, CliError> {
    let entries = std::fs::read_dir(dir).map_err(|source| CliError::LibraryIo {
        path: dir.display().to_string(),
        source,
    })?;
    let mut cells = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|source| CliError::LibraryIo {
            path: dir.display().to_string(),
            source,
        })?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.ends_with(".cell.json") {
            continue;
        }
        let text = read_text(&path)?;
        let cell = CellDef::parse(&text).map_err(|source| CliError::Cell {
            path: path.display().to_string(),
            source,
        })?;
        let name = cell.name.clone();
        if cells.insert(name.clone(), cell).is_some() {
            return Err(CliError::DuplicateCell { name });
        }
    }
    if cells.is_empty() {
        return Err(CliError::EmptyLibrary {
            path: dir.display().to_string(),
        });
    }
    Ok(cells)
}

/// Run `design_json` with `stimulus_json` until `until`, or to completion.
pub fn simulate(
    design_json: &str,
    stimulus_json: &str,
    cells: BTreeMap<String, CellDef>,
    until: Option<i64>,
) -> Result<SimReport, CliError> {
    let design = Design::parse(design_json)?;
    let stimulus = Stimulus::parse(stimulus_json)?;
    let mut sim = Simulator::new(
        design,
        cells,
        stimulus,
        ViolationPolicy::Record,
        SNAPSHOT_EVERY,
    )?;
    match until {
        Some(t) => sim.run_until(Femtos(t))?,
        None => sim.run()?,
    }
    report(sim.log())
}

/// Lowercase SHA-256 hex of `bytes`.
pub fn hash_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

fn report(log: &EventLog) -> Result<SimReport, CliError> {
    let mut violations = 0;
    let mut latest: Option<i64> = None;
    let mut violation_lines = Vec::new();
    for event in log.events() {
        let t = event_time(event);
        latest = Some(match latest {
            Some(prev) if prev > t => prev,
            _ => t,
        });
        if let Some(line) = violation_line(event) {
            violations += 1;
            violation_lines.push(line);
        }
    }
    Ok(SimReport {
        events: log.events().len(),
        violations,
        sim_time_fs: latest.unwrap_or_default(),
        pulses: log.pulse_json(),
        log: log.encode()?,
        violation_lines,
    })
}

fn event_time(event: &LogEvent) -> i64 {
    match event {
        LogEvent::Fired { t, .. }
        | LogEvent::SetupViolation { t, .. }
        | LogEvent::HoldViolation { t, .. }
        | LogEvent::Collision { t, .. }
        | LogEvent::IllegalInput { t, .. } => t.0,
    }
}

fn violation_line(event: &LogEvent) -> Option<String> {
    match event {
        LogEvent::SetupViolation { t, inst, slack } => {
            Some(format!("setup {inst} t={} slack={}", t.0, slack.0))
        }
        LogEvent::HoldViolation { t, inst, slack } => {
            Some(format!("hold {inst} t={} slack={}", t.0, slack.0))
        }
        LogEvent::IllegalInput { t, inst, port } => {
            Some(format!("illegal {inst} port={port} t={}", t.0))
        }
        LogEvent::Fired { .. } | LogEvent::Collision { .. } => None,
    }
}
