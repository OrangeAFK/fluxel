//! JoSIM netlist subset and the Mode B analog solver.
//!
//! Values are SI. A library `.subckt` with no top-level instance parses with an
//! empty [`Deck::main`]; call [`Deck::flatten_subckt`] for that cell.
//! [`solve`] integrates that element list with trapezoidal phase MNA.

mod error;
mod expr;
mod jj;
mod parse;
mod pulse;
mod solve;
mod wave;

pub use error::{NetlistError, SolveError};
pub use jj::{Qp, quasiparticle};
pub use parse::{Deck, Element, Jj, Tran, Tx, Wave, parse_netlist, parse_source};
pub use pulse::{Crossing, PhaseCrossings, extract_crossings, femtoseconds};
pub use solve::{DEFAULT_STEP, PHI0, Solution, SolveConfig, Trace, solve};

#[cfg(test)]
mod tests;
