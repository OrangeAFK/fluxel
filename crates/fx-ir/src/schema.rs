use schemars::schema_for;
use serde_json::Value;

use crate::cell::CellDef;
use crate::design::Design;
use crate::stimulus::Stimulus;

/// JSON Schema for a cell definition.
pub fn cell_schema() -> Result<Value, serde_json::Error> {
    serde_json::to_value(schema_for!(CellDef))
}

/// JSON Schema for a design.
pub fn design_schema() -> Result<Value, serde_json::Error> {
    serde_json::to_value(schema_for!(Design))
}

/// JSON Schema for a stimulus map.
pub fn stimulus_schema() -> Result<Value, serde_json::Error> {
    serde_json::to_value(schema_for!(Stimulus))
}
