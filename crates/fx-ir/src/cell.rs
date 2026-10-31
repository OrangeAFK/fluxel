use std::collections::{BTreeMap, BTreeSet};

use fx_time::Femtos;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Where a cell's timing numbers come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Provenance {
    Characterized,
    SelfCharacterized,
    Behavioral,
}

/// Compass and vertical faces. Serialized as `N`, `E`, `S`, `W`, `U`, `D`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub enum Face {
    N,
    E,
    S,
    W,
    U,
    D,
}

/// Input and output port names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Ports {
    #[serde(rename = "in")]
    pub inputs: Vec<String>,
    #[serde(rename = "out")]
    pub outputs: Vec<String>,
}

/// One output pulse produced by a transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Fire {
    pub port: String,
    pub delay: String,
}

/// A state-machine edge. `fire` names output ports and delay entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    pub from: String,
    pub on: String,
    pub to: String,
    pub fire: Vec<Fire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Setup, hold, or collision window between two ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintKind {
    Setup,
    Hold,
    Collision,
}

/// A timing constraint. `window` names a LUT entry, or a `delays` key on a behavioral cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Constraint {
    pub kind: ConstraintKind,
    pub first: String,
    pub then: String,
    pub window: String,
}

/// One cell's state machine, in the §3 JSON shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellDef {
    pub name: String,
    pub family: String,
    pub provenance: Provenance,
    pub ports: Ports,
    pub faces: BTreeMap<String, Face>,
    pub states: Vec<String>,
    pub initial: String,
    pub transitions: Vec<Transition>,
    pub constraints: Vec<Constraint>,
    /// Junctions in the library netlist. Absent when no netlist exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jj_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bias_ua: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub netlist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lut: Option<String>,
    /// Reference jjmit inside the RSFQlib subckt, one name per port.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ref_jj: BTreeMap<String, String>,
    /// Which library file this machine models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Constant femtosecond delays and windows. Only behavioral cells may set this.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub delays: BTreeMap<String, Femtos>,
}

/// Why a cell document was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CellError {
    #[error("invalid cell JSON: {0}")]
    Json(String),
    #[error("unknown state {state}")]
    UnknownState { state: String },
    #[error("unknown port {port}")]
    UnknownPort { port: String },
    #[error("undefined delay {name}")]
    UndefinedDelay { name: String },
    #[error("undefined window {name}")]
    UndefinedWindow { name: String },
    #[error("nondeterministic transition from {state} on {input}")]
    Nondeterministic { state: String, input: String },
    #[error("output port {port} is never fired")]
    UnfiredOutput { port: String },
    #[error("only behavioral cells may carry literal delays")]
    LiteralTiming,
    #[error("characterized cell is missing jj_count, netlist, or lut")]
    MissingPath,
    #[error("ref_jj is missing port {port}")]
    MissingRefJj { port: String },
    #[error("ref_jj for port {port} is empty")]
    EmptyRefJj { port: String },
}

impl CellDef {
    /// Deserialize `json`, then reject an inconsistent state machine.
    pub fn parse(json: &str) -> Result<Self, CellError> {
        let cell: Self =
            serde_json::from_str(json).map_err(|err| CellError::Json(err.to_string()))?;
        cell.validate()?;
        Ok(cell)
    }

    fn validate(&self) -> Result<(), CellError> {
        if self.provenance != Provenance::Behavioral && !self.delays.is_empty() {
            return Err(CellError::LiteralTiming);
        }
        if self.provenance == Provenance::Characterized
            && (self.jj_count.is_none() || !path_present(&self.netlist) || !path_present(&self.lut))
        {
            return Err(CellError::MissingPath);
        }

        let states: BTreeSet<&str> = self.states.iter().map(String::as_str).collect();
        if !states.contains(self.initial.as_str()) {
            return Err(CellError::UnknownState {
                state: self.initial.clone(),
            });
        }
        for transition in &self.transitions {
            if !states.contains(transition.from.as_str()) {
                return Err(CellError::UnknownState {
                    state: transition.from.clone(),
                });
            }
            if !states.contains(transition.to.as_str()) {
                return Err(CellError::UnknownState {
                    state: transition.to.clone(),
                });
            }
        }

        let inputs: BTreeSet<&str> = self.ports.inputs.iter().map(String::as_str).collect();
        let outputs: BTreeSet<&str> = self.ports.outputs.iter().map(String::as_str).collect();
        for port in self.faces.keys() {
            if !inputs.contains(port.as_str()) && !outputs.contains(port.as_str()) {
                return Err(CellError::UnknownPort { port: port.clone() });
            }
        }
        for transition in &self.transitions {
            if !inputs.contains(transition.on.as_str()) {
                return Err(CellError::UnknownPort {
                    port: transition.on.clone(),
                });
            }
            for fire in &transition.fire {
                if !outputs.contains(fire.port.as_str()) {
                    return Err(CellError::UnknownPort {
                        port: fire.port.clone(),
                    });
                }
            }
        }
        for constraint in &self.constraints {
            if !inputs.contains(constraint.first.as_str()) {
                return Err(CellError::UnknownPort {
                    port: constraint.first.clone(),
                });
            }
            if !inputs.contains(constraint.then.as_str()) {
                return Err(CellError::UnknownPort {
                    port: constraint.then.clone(),
                });
            }
        }

        if self.provenance == Provenance::Behavioral {
            for transition in &self.transitions {
                for fire in &transition.fire {
                    if !self.delays.contains_key(&fire.delay) {
                        return Err(CellError::UndefinedDelay {
                            name: fire.delay.clone(),
                        });
                    }
                }
            }
            for constraint in &self.constraints {
                if !self.delays.contains_key(&constraint.window) {
                    return Err(CellError::UndefinedWindow {
                        name: constraint.window.clone(),
                    });
                }
            }
        }

        let mut seen = BTreeSet::new();
        for transition in &self.transitions {
            if !seen.insert((transition.from.as_str(), transition.on.as_str())) {
                return Err(CellError::Nondeterministic {
                    state: transition.from.clone(),
                    input: transition.on.clone(),
                });
            }
        }

        let mut fired = BTreeSet::new();
        for transition in &self.transitions {
            for fire in &transition.fire {
                fired.insert(fire.port.as_str());
            }
        }
        for port in &self.ports.outputs {
            if !fired.contains(port.as_str()) {
                return Err(CellError::UnfiredOutput { port: port.clone() });
            }
        }

        if !self.ref_jj.is_empty() {
            for port in self.ref_jj.keys() {
                if !inputs.contains(port.as_str()) && !outputs.contains(port.as_str()) {
                    return Err(CellError::UnknownPort { port: port.clone() });
                }
            }
            for port in inputs.iter().chain(outputs.iter()) {
                match self.ref_jj.get(*port) {
                    None => {
                        return Err(CellError::MissingRefJj {
                            port: (*port).to_string(),
                        });
                    }
                    Some(device) if device.is_empty() => {
                        return Err(CellError::EmptyRefJj {
                            port: (*port).to_string(),
                        });
                    }
                    Some(_) => {}
                }
            }
        }
        Ok(())
    }
}

fn path_present(path: &Option<String>) -> bool {
    match path {
        Some(value) => !value.is_empty(),
        None => false,
    }
}
