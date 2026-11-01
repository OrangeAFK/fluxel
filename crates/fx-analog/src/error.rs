use std::fmt;

use thiserror::Error;

/// Where a netlist line came from, after `.include` expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loc {
    pub file: String,
    pub line: u32,
}

impl fmt::Display for Loc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.file, self.line)
    }
}

/// The reference solver could not finish a transient.
#[derive(Debug, Error)]
pub enum SolveError {
    /// Newton stopped at `max_newton` with a correction still over tolerance.
    #[error("newton did not converge at t = {time_s} s on node {node}")]
    Diverged { time_s: f64, node: String },
    /// The sparse factorization failed.
    #[error("singular matrix at t = {time_s} s")]
    Singular { time_s: f64 },
    /// A device value or an unsupported card.
    #[error("{name}: {message}")]
    Element { name: String, message: String },
    /// The request itself is not a transient the solver can run.
    #[error("{message}")]
    Input { message: String },
    /// `extract_crossings` was given unequal slices.
    #[error("time and phase traces differ in length")]
    TraceLength,
}

/// A deck could not be parsed or flattened.
#[derive(Debug, Error)]
pub enum NetlistError {
    #[error("{file}:{line}: {message}")]
    At {
        file: String,
        line: u32,
        message: String,
    },
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// InductEx `*_idx.cir` decks, or a `.control` / `back-annotate` block.
    /// JoSIM rejects those as well. Parsing stops at the trigger.
    #[error("{path}: not a simulation netlist ({trigger})")]
    NotASimulationNetlist { path: String, trigger: String },
}

impl NetlistError {
    pub(crate) fn at(loc: &Loc, message: impl Into<String>) -> Self {
        Self::At {
            file: loc.file.clone(),
            line: loc.line,
            message: message.into(),
        }
    }
}
