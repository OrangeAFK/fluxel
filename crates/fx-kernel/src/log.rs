//! Append-only log of fired pulses and timing notes.
//!
//! CBOR field order follows the struct declaration. The encoding uses only
//! strings and integers, so the same log is the same bytes on every platform.

use std::collections::BTreeMap;

use fx_time::Femtos;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::KernelError;

/// One fired output or timing note, in the order the kernel produced them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum LogEvent {
    Fired {
        t: Femtos,
        inst: String,
        port: String,
    },
    SetupViolation {
        t: Femtos,
        inst: String,
        slack: Femtos,
    },
    HoldViolation {
        t: Femtos,
        inst: String,
        slack: Femtos,
    },
    Collision {
        t: Femtos,
        inst: String,
        slack: Femtos,
    },
    IllegalInput {
        t: Femtos,
        inst: String,
        port: String,
    },
}

/// Fired pulses and violations. Forward simulation only appends.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EventLog {
    events: Vec<LogEvent>,
}

impl EventLog {
    pub(crate) fn extend(&mut self, events: impl IntoIterator<Item = LogEvent>) {
        self.events.extend(events);
    }

    pub(crate) fn truncate(&mut self, len: usize) {
        self.events.truncate(len);
    }

    pub(crate) fn len(&self) -> usize {
        self.events.len()
    }

    /// Events in append order.
    pub fn events(&self) -> &[LogEvent] {
        &self.events
    }

    /// Deterministic CBOR of the event list.
    pub fn encode(&self) -> Result<Vec<u8>, KernelError> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&self.events, &mut bytes)
            .map_err(|err| KernelError::Cbor(err.to_string()))?;
        Ok(bytes)
    }

    /// SHA-256 of [`Self::encode`].
    pub fn hash(&self) -> Result<[u8; 32], KernelError> {
        let digest = Sha256::digest(self.encode()?);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&digest);
        Ok(hash)
    }

    /// Fired pulses as `{ "instance.port": [t_fs, ...] }`, keys and times sorted.
    pub fn pulse_json(&self) -> String {
        let mut pulses: BTreeMap<String, Vec<i64>> = BTreeMap::new();
        for event in &self.events {
            if let LogEvent::Fired { t, inst, port } = event {
                pulses
                    .entry(format!("{inst}.{port}"))
                    .or_default()
                    .push(t.0);
            }
        }
        for times in pulses.values_mut() {
            times.sort_unstable();
        }
        let mut out = String::from("{");
        for (index, (port, times)) in pulses.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            push_json_string(&mut out, port);
            out.push(':');
            out.push('[');
            for (time_index, time) in times.iter().enumerate() {
                if time_index > 0 {
                    out.push(',');
                }
                out.push_str(&time.to_string());
            }
            out.push(']');
        }
        out.push('}');
        out
    }
}

fn push_json_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if other.is_control() => {
                out.push_str(&format!("\\u{:04x}", u32::from(other)));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}
