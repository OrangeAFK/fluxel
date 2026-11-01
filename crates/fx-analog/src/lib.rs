//! JoSIM netlist subset for the analog tier.
//!
//! Values are SI. A library `.subckt` with no top-level instance parses with an
//! empty [`Deck::main`]; call [`Deck::flatten_subckt`] for that cell.

mod error;
mod expr;
mod parse;

pub use error::NetlistError;
pub use parse::{Deck, Element, Jj, Tran, Tx, Wave, parse_netlist, parse_source};

#[cfg(test)]
mod tests;
