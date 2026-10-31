//! JoSIM deck for a direct-wired design.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use fx_ir::cell::CellDef;
use fx_ir::design::{Design, PinRef, Wire};
use fx_ir::stimulus::Stimulus;
use thiserror::Error;

const SETTLE_FS: i64 = 20_000;
const EDGE_FS: i64 = 3_000;
const PULSE_SPAN_FS: i64 = 6_000;
const TAIL_FS: i64 = 500_000;

/// Why a design could not be written as a JoSIM deck.
#[derive(Debug, Error)]
pub enum FlowError {
    #[error("{wire} wires are not exported")]
    NotDirect { wire: String },
    #[error("cell {cell} cannot be exported to JoSIM")]
    NotExportable { cell: String },
    #[error("unknown cell {cell}")]
    UnknownCell { cell: String },
    #[error("ref_jj {device} is not a jjmit in {cell}")]
    UnknownDevice { cell: String, device: String },
    #[error("cell {cell} port {port} is not on its subckt")]
    MissingPin { cell: String, port: String },
    #[error("instance {instance} port {port} is not connected")]
    Unconnected { instance: String, port: String },
    #[error("stimulus pulses on {port} overlap")]
    OverlappingPulses { port: String },
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("netlist {path} has no .subckt")]
    NoSubckt { path: String },
    #[error("bias multiplier {value} is not positive and finite")]
    BadBias { value: String },
}

/// Bias multiplier, timestep, stop time, and whether the exporter wraps I/O.
///
/// Defaults are the deck `export_josim` already writes: `bias_mul` 1, a 25 fs
/// step, a stop of settle + last pulse + tail, a DCSFQ on every input, and an
/// SFQDC on every output.
#[derive(Debug, Clone, Copy)]
pub struct DeckOpts {
    pub bias_mul: f64,
    pub step_fs: i64,
    pub stop_fs: Option<i64>,
    pub current_inputs: bool,
    pub resistor_outputs: bool,
}

impl Default for DeckOpts {
    fn default() -> Self {
        Self {
            bias_mul: 1.0,
            step_fs: 25,
            stop_fs: None,
            current_inputs: false,
            resistor_outputs: false,
        }
    }
}

struct Subckt {
    name: String,
    pins: Vec<String>,
    body: String,
}

/// A JoSIM deck: inlined RSFQlib subckts, one DCSFQ per input, one SFQDC per output.
pub fn export_josim(
    design: &Design,
    stimulus: &Stimulus,
    cells: &BTreeMap<String, CellDef>,
    root: &Path,
) -> Result<String, FlowError> {
    export_josim_with(design, stimulus, cells, root, &DeckOpts::default())
}

/// [`export_josim`] with a bias multiplier, timestep, stop time, and source/load wrap.
pub fn export_josim_with(
    design: &Design,
    stimulus: &Stimulus,
    cells: &BTreeMap<String, CellDef>,
    root: &Path,
    opts: &DeckOpts,
) -> Result<String, FlowError> {
    for net in &design.nets {
        match &net.wire {
            Wire::Direct => {}
            Wire::Jtl { .. } => {
                return Err(FlowError::NotDirect { wire: "jtl".into() });
            }
            Wire::Ptl { .. } => {
                return Err(FlowError::NotDirect { wire: "ptl".into() });
            }
        }
    }

    let mut needed = BTreeSet::new();
    for instance in design.instances.values() {
        needed.insert(instance.cell.as_str());
    }
    if !opts.current_inputs {
        needed.insert("DCSFQ");
    }
    if !opts.resistor_outputs {
        needed.insert("SFQDC");
    }

    let mut by_cell = BTreeMap::new();
    for name in needed {
        let cell = cells.get(name).ok_or_else(|| FlowError::UnknownCell {
            cell: name.to_string(),
        })?;
        by_cell.insert(name.to_string(), load_subckt(root, cell)?);
    }

    let mut node_of = BTreeMap::new();
    for net in &design.nets {
        let node = match &net.driver {
            PinRef::Port { port } => format!("n_{port}"),
            PinRef::Instance { instance, port } => format!("n_{instance}_{port}"),
        };
        node_of.insert(net.driver.clone(), node.clone());
        node_of.insert(net.sink.clone(), node);
    }
    for (id, instance) in &design.instances {
        let cell = &cells[&instance.cell];
        for port in cell.ports.inputs.iter().chain(cell.ports.outputs.iter()) {
            let pin = PinRef::Instance {
                instance: id.clone(),
                port: port.clone(),
            };
            if !node_of.contains_key(&pin) {
                return Err(FlowError::Unconnected {
                    instance: id.clone(),
                    port: port.clone(),
                });
            }
        }
    }

    let mut inputs: Vec<&str> = design.ports.inputs.iter().map(String::as_str).collect();
    inputs.sort_unstable();
    let mut outputs: Vec<&str> = design.ports.outputs.iter().map(String::as_str).collect();
    outputs.sort_unstable();
    let mut schedules = BTreeMap::new();
    let mut last = 0_i64;
    for port in &inputs {
        let raw: Vec<i64> = stimulus
            .0
            .get(*port)
            .map(|times| times.iter().map(|time| time.0).collect())
            .unwrap_or_default();
        let scheduled = schedule(port, &raw)?;
        if let Some(end) = scheduled.last() {
            last = last.max(*end);
        }
        schedules.insert(*port, scheduled);
    }

    let bias = format_mul(opts.bias_mul)?;
    let mut deck = format!("* Fluxel JoSIM export\n.param bias_mul={bias}\n\n");
    let mut ordered: Vec<&Subckt> = by_cell.values().collect();
    ordered.sort_by(|left, right| left.name.cmp(&right.name));
    for sub in ordered {
        deck.push_str(&sub.body);
        if !sub.body.ends_with('\n') {
            deck.push('\n');
        }
        deck.push('\n');
    }

    for (id, instance) in &design.instances {
        let Some(sub) = by_cell.get(&instance.cell) else {
            return Err(FlowError::UnknownCell {
                cell: instance.cell.clone(),
            });
        };
        let mut pins = Vec::new();
        for pin in &sub.pins {
            let node = node_of
                .get(&PinRef::Instance {
                    instance: id.clone(),
                    port: pin.clone(),
                })
                .ok_or_else(|| FlowError::Unconnected {
                    instance: id.clone(),
                    port: pin.clone(),
                })?;
            pins.push(node.as_str());
        }
        deck.push_str(&format!("X{id} {} {}\n", sub.name, pins.join(" ")));
    }
    deck.push('\n');

    if opts.current_inputs {
        for port in &inputs {
            let node = connected(&node_of, port)?;
            let times = schedules.get(port).map(Vec::as_slice).unwrap_or(&[]);
            deck.push_str(&format!("Ifxsrc_{port} 0 {node} {}\n", pwl(times)));
        }
    } else {
        let dcsfq = cell_subckt(&by_cell, "DCSFQ")?;
        for port in &inputs {
            let source = format!("n_fxsrc_{port}_a");
            let out = connected(&node_of, port)?;
            let pins = ordered_pins(dcsfq, &[("a", source.as_str()), ("q", out)])?;
            deck.push_str(&format!("Xfxsrc_{port} {} {pins}\n", dcsfq.name));
            let times = schedules.get(port).map(Vec::as_slice).unwrap_or(&[]);
            deck.push_str(&format!("Ifxsrc_{port} 0 {source} {}\n", pwl(times)));
        }
    }
    deck.push('\n');

    if opts.resistor_outputs {
        for port in &outputs {
            let node = connected(&node_of, port)?;
            deck.push_str(&format!("Rfxload_{port} {node} 0 2\n"));
        }
    } else {
        let sfqdc = cell_subckt(&by_cell, "SFQDC")?;
        for port in &outputs {
            let load = format!("n_fxload_{port}_q");
            let inp = connected(&node_of, port)?;
            let pins = ordered_pins(sfqdc, &[("a", inp), ("q", load.as_str())])?;
            deck.push_str(&format!("Xfxload_{port} {} {pins}\n", sfqdc.name));
            deck.push_str(&format!("Rfxload_{port} {load} 0 2\n"));
        }
    }
    deck.push('\n');

    let stop = opts.stop_fs.unwrap_or(SETTLE_FS + last + TAIL_FS);
    deck.push_str(&format!(
        ".tran {} {} 0\n",
        format_fs(opts.step_fs),
        format_fs(stop)
    ));

    let mut prints = Vec::new();
    for (id, instance) in &design.instances {
        let Some(cell) = cells.get(&instance.cell) else {
            return Err(FlowError::UnknownCell {
                cell: instance.cell.clone(),
            });
        };
        for (port, device) in &cell.ref_jj {
            prints.push((id.clone(), port.clone(), device.clone()));
        }
    }
    if !opts.current_inputs {
        let dcsfq_cell = cells.get("DCSFQ").ok_or_else(|| FlowError::UnknownCell {
            cell: "DCSFQ".into(),
        })?;
        for port in &inputs {
            for (pin, device) in &dcsfq_cell.ref_jj {
                prints.push((format!("fxsrc_{port}"), pin.clone(), device.clone()));
            }
        }
    }
    if !opts.resistor_outputs {
        let sfqdc_cell = cells.get("SFQDC").ok_or_else(|| FlowError::UnknownCell {
            cell: "SFQDC".into(),
        })?;
        for port in &outputs {
            for (pin, device) in &sfqdc_cell.ref_jj {
                prints.push((format!("fxload_{port}"), pin.clone(), device.clone()));
            }
        }
    }
    prints.sort();
    let mut signals = Vec::new();
    for (instance, port, device) in &prints {
        // JoSIM keeps the leading X on the instance token and uppercases it.
        // `Xj0` is stored as `P(B1|XJ0)`.
        let spice = format!("X{instance}");
        deck.push_str(&format!(
            "* fluxel-map {instance}.{port} P({}|{})\n",
            device,
            spice.to_ascii_uppercase()
        ));
        signals.push(format!("p({device}.{spice})"));
    }
    deck.push_str(".print");
    for signal in &signals {
        deck.push(' ');
        deck.push_str(signal);
    }
    deck.push_str("\n.end\n");
    Ok(deck)
}

fn load_subckt(root: &Path, cell: &CellDef) -> Result<Subckt, FlowError> {
    let rel = match cell.netlist.as_deref() {
        Some(path) if !path.is_empty() && !cell.ref_jj.is_empty() => path,
        _ => {
            return Err(FlowError::NotExportable {
                cell: cell.name.clone(),
            });
        }
    };
    let path = root.join(rel);
    let text = std::fs::read_to_string(&path).map_err(|source| FlowError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let body = rewrite_bias(&text);
    let (name, pins) = parse_subckt(&body, rel)?;
    let devices = jjmit_devices(&body);
    for (port, device) in &cell.ref_jj {
        if !devices.contains(device) {
            return Err(FlowError::UnknownDevice {
                cell: cell.name.clone(),
                device: device.clone(),
            });
        }
        if !pins.iter().any(|pin| pin == port) {
            return Err(FlowError::MissingPin {
                cell: cell.name.clone(),
                port: port.clone(),
            });
        }
    }
    Ok(Subckt { name, pins, body })
}

fn parse_subckt(body: &str, path: &str) -> Result<(String, Vec<String>), FlowError> {
    for line in body.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens
            .first()
            .is_some_and(|word| word.eq_ignore_ascii_case(".subckt"))
            && tokens.len() >= 2
        {
            let name = tokens[1].to_string();
            let pins = tokens.into_iter().skip(2).map(str::to_string).collect();
            return Ok((name, pins));
        }
    }
    Err(FlowError::NoSubckt {
        path: path.to_string(),
    })
}

fn jjmit_devices(body: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in body.lines() {
        let mut tokens = line.split_whitespace();
        let Some(name) = tokens.next() else {
            continue;
        };
        if tokens.any(|token| token.eq_ignore_ascii_case("jjmit")) {
            names.insert(name.to_string());
        }
    }
    names
}

fn rewrite_bias(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(".param BiasCoef=0.7")
            && (rest.is_empty() || rest == "'")
        {
            out.push_str(".param BiasCoef=0.7*bias_mul\n");
            continue;
        }
        if let Some((name, value)) = literal_bias(trimmed) {
            out.push_str(&format!(".param {name}=({value})*bias_mul\n"));
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn literal_bias(trimmed: &str) -> Option<(&str, &str)> {
    let rest = trimmed.strip_prefix(".param ")?;
    let (name, value) = rest.split_once('=')?;
    let digits = name.strip_prefix("IB")?;
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let number = value.strip_suffix('u')?;
    if number.is_empty() || !number.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some((name, value))
}

fn cell_subckt<'a>(
    by_cell: &'a BTreeMap<String, Subckt>,
    name: &str,
) -> Result<&'a Subckt, FlowError> {
    by_cell.get(name).ok_or_else(|| FlowError::UnknownCell {
        cell: name.to_string(),
    })
}

fn connected<'a>(node_of: &'a BTreeMap<PinRef, String>, port: &str) -> Result<&'a str, FlowError> {
    node_of
        .get(&PinRef::Port {
            port: port.to_string(),
        })
        .map(String::as_str)
        .ok_or_else(|| FlowError::Unconnected {
            instance: port.to_string(),
            port: port.to_string(),
        })
}

fn ordered_pins(sub: &Subckt, bound: &[(&str, &str)]) -> Result<String, FlowError> {
    let map: BTreeMap<&str, &str> = bound.iter().copied().collect();
    let mut nodes = Vec::new();
    for pin in &sub.pins {
        let Some(node) = map.get(pin.as_str()) else {
            return Err(FlowError::MissingPin {
                cell: sub.name.clone(),
                port: pin.clone(),
            });
        };
        nodes.push(*node);
    }
    Ok(nodes.join(" "))
}

fn schedule(port: &str, times: &[i64]) -> Result<Vec<i64>, FlowError> {
    let mut times = times.to_vec();
    times.sort_unstable();
    for pair in times.windows(2) {
        if pair[1] - pair[0] < PULSE_SPAN_FS {
            return Err(FlowError::OverlappingPulses {
                port: port.to_string(),
            });
        }
    }
    Ok(times)
}

fn pwl(times: &[i64]) -> String {
    let mut parts = vec!["0".to_string(), "0".to_string()];
    for time in times {
        let start = time + SETTLE_FS;
        parts.push(format_fs(start));
        parts.push("0".to_string());
        parts.push(format_fs(start + EDGE_FS));
        parts.push("600u".to_string());
        parts.push(format_fs(start + PULSE_SPAN_FS));
        parts.push("0".to_string());
    }
    format!("pwl({})", parts.join(" "))
}

fn format_mul(value: f64) -> Result<String, FlowError> {
    if !value.is_finite() || value <= 0.0 {
        return Err(FlowError::BadBias {
            value: value.to_string(),
        });
    }
    let nearest = value.round();
    if (value - nearest).abs() < 1e-9 && nearest.abs() < 1e15 {
        return Ok(format!("{}", nearest as i64));
    }
    let text = format!("{value:.12}");
    Ok(text.trim_end_matches('0').trim_end_matches('.').to_string())
}

fn format_fs(fs: i64) -> String {
    let neg = if fs < 0 { "-" } else { "" };
    let fs = fs.unsigned_abs();
    let ps = fs / 1000;
    let rem = fs % 1000;
    if rem == 0 {
        format!("{neg}{ps}p")
    } else {
        let digits = format!("{rem:03}");
        let digits = digits.trim_end_matches('0');
        format!("{neg}{ps}.{digits}p")
    }
}
