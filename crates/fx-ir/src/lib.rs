//! Netlist IR, cell definitions, and their JSON schemas.

pub mod cell;
pub mod design;
pub mod schema;
pub mod stimulus;

pub use cell::{CellDef, CellError};
pub use design::{Design, DesignError};
pub use stimulus::{Stimulus, StimulusError};

#[cfg(test)]
mod tests;
