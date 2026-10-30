use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::cell::Ports;

/// An instance pin, or a port on the design itself.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(untagged, deny_unknown_fields)]
pub enum PinRef {
    Instance { instance: String, port: String },
    Port { port: String },
}

/// How a net is wired. Delays come from interconnect characterization, not from this value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Wire {
    Direct,
    Jtl { stages: u32 },
    Ptl { length_units: u32 },
}

/// One placed cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    pub family: String,
    pub cell: String,
    pub bias_domain: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub macro_parent: Option<String>,
}

/// A single driver-to-sink connection. Fanout greater than one is two nets, and [`Design::parse`] rejects it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Net {
    pub driver: PinRef,
    pub sink: PinRef,
    pub wire: Wire,
}

/// A fanout-1 netlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Design {
    pub instances: BTreeMap<String, Instance>,
    pub nets: Vec<Net>,
    pub ports: Ports,
    pub clock_domains: BTreeMap<String, Vec<String>>,
    pub bias_domains: BTreeSet<String>,
}

/// Why a design document was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DesignError {
    #[error("invalid design JSON: {0}")]
    Json(String),
    #[error("fanout exceeds 1 at {driver:?}")]
    Fanout { driver: PinRef },
    #[error("more than one driver at {sink:?}")]
    MultipleDrivers { sink: PinRef },
    #[error("unknown instance {instance}")]
    UnknownInstance { instance: String },
    #[error("unknown bias domain {domain}")]
    UnknownBiasDomain { domain: String },
}

impl Design {
    /// Deserialize `json`, then reject fanout, multiple drivers, and dangling names.
    pub fn parse(json: &str) -> Result<Self, DesignError> {
        let design: Self =
            serde_json::from_str(json).map_err(|err| DesignError::Json(err.to_string()))?;
        design.validate()?;
        Ok(design)
    }

    fn validate(&self) -> Result<(), DesignError> {
        for net in &self.nets {
            for pin in [&net.driver, &net.sink] {
                if let Some(instance) = pin_instance(pin)
                    && !self.instances.contains_key(instance)
                {
                    return Err(DesignError::UnknownInstance {
                        instance: instance.to_string(),
                    });
                }
            }
        }
        for members in self.clock_domains.values() {
            for instance in members {
                if !self.instances.contains_key(instance) {
                    return Err(DesignError::UnknownInstance {
                        instance: instance.clone(),
                    });
                }
            }
        }
        for instance in self.instances.values() {
            if !self.bias_domains.contains(&instance.bias_domain) {
                return Err(DesignError::UnknownBiasDomain {
                    domain: instance.bias_domain.clone(),
                });
            }
        }

        let mut drivers = BTreeSet::new();
        for net in &self.nets {
            if !drivers.insert(net.driver.clone()) {
                return Err(DesignError::Fanout {
                    driver: net.driver.clone(),
                });
            }
        }
        let mut sinks = BTreeSet::new();
        for net in &self.nets {
            if !sinks.insert(net.sink.clone()) {
                return Err(DesignError::MultipleDrivers {
                    sink: net.sink.clone(),
                });
            }
        }
        Ok(())
    }
}

fn pin_instance(pin: &PinRef) -> Option<&str> {
    match pin {
        PinRef::Instance { instance, .. } => Some(instance.as_str()),
        PinRef::Port { .. } => None,
    }
}
