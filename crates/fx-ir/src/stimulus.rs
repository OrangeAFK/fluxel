use std::collections::BTreeMap;

use fx_time::Femtos;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Pulse arrival times, in femtoseconds, for each source port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Stimulus(pub BTreeMap<String, Vec<Femtos>>);

/// Why a stimulus document was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StimulusError {
    #[error("invalid stimulus JSON: {0}")]
    Json(String),
}

impl Stimulus {
    /// Deserialize `{ source_port: [t_fs, ...] }`.
    pub fn parse(json: &str) -> Result<Self, StimulusError> {
        serde_json::from_str(json).map_err(|err| StimulusError::Json(err.to_string()))
    }
}
