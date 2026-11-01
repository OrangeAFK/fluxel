//! CBOR LUT bundles. SPEC §3.
//!
//! Grid coordinates are integers: `bias_frac` in micro-units (10⁻⁶) and
//! `*_fs` axes in femtoseconds. Multilinear weights are `f64`. The only
//! rounding to [`Femtos`] is [`Femtos::from_fs_f64_rounded`] inside
//! [`LutBundle::lookup`].

use std::collections::BTreeMap;

use fx_time::{Femtos, TimeError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Why a bundle or a lookup was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LutError {
    #[error("invalid LUT CBOR: {0}")]
    Cbor(String),
    #[error("invalid LUT JSON: {0}")]
    Json(String),
    #[error("unknown LUT entry {name}")]
    UnknownEntry { name: String },
    #[error("LUT point is missing axis {axis}")]
    MissingAxis { axis: String },
    #[error("LUT point has an extra axis {axis}")]
    ExtraAxis { axis: String },
    #[error("LUT axis {axis} was given the wrong kind of value")]
    QueryKind { axis: String },
    #[error("invalid LUT bundle: {0}")]
    Invalid(String),
}

/// A query coordinate. Bias is a fraction such as `1.0`. Time axes are femtoseconds.
#[derive(Debug, Clone, Copy)]
pub enum AxisQuery {
    Fraction(f64),
    Time(Femtos),
}

/// Inclusive valid bound. `lo` and `hi` use the same decimal text as axis points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidBound {
    pub lo: String,
    pub hi: String,
}

/// One interpolation axis. Points are decimal strings, strictly increasing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LutAxis {
    pub name: String,
    /// Decimal text. `bias_frac` is a fraction. `*_fs` axes are integer femtoseconds.
    pub points: Vec<String>,
}

/// The only interpolation this bundle stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Interpolation {
    Multilinear,
}

/// Where the grid was measured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LutSource {
    pub josim_version: String,
    pub netlist_sha256: String,
    pub sweep_sha256: String,
    /// Integration step in femtoseconds.
    pub timestep_fs: i64,
}

/// One delay or window table.
///
/// `values` are femtoseconds, row-major, last axis fastest.
/// `sigma_fs` is the Monte Carlo sigma. It is 0 until that sweep exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LutEntry {
    pub axes: Vec<LutAxis>,
    pub values: Vec<i64>,
    pub interpolation: Interpolation,
    pub valid: BTreeMap<String, ValidBound>,
    pub sigma_fs: i64,
}

/// One cell's timing tables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LutBundle {
    pub cell: String,
    pub entries: BTreeMap<String, LutEntry>,
    pub source: LutSource,
}

impl LutBundle {
    /// Decode CBOR and reject a grid that cannot be interpolated.
    pub fn from_cbor(bytes: &[u8]) -> Result<Self, LutError> {
        let bundle: Self =
            ciborium::de::from_reader(bytes).map_err(|err| LutError::Cbor(err.to_string()))?;
        bundle.check()?;
        Ok(bundle)
    }

    /// Decode the JSON document that matches `schemas/lut.schema.json`.
    pub fn from_json(text: &str) -> Result<Self, LutError> {
        let bundle: Self =
            serde_json::from_str(text).map_err(|err| LutError::Json(err.to_string()))?;
        bundle.check()?;
        Ok(bundle)
    }

    /// Interpolate `name` at `point`.
    ///
    /// A coordinate outside the valid box is clamped. `in_valid` is false in
    /// that case, and the returned time is the clamped sample rounded to 1 fs.
    pub fn lookup(
        &self,
        name: &str,
        point: &BTreeMap<String, AxisQuery>,
    ) -> Result<(Femtos, bool), LutError> {
        let entry = self.entry(name)?;
        let mut units = BTreeMap::new();
        for axis in &entry.axes {
            let query = point.get(&axis.name).ok_or_else(|| LutError::MissingAxis {
                axis: axis.name.clone(),
            })?;
            units.insert(axis.name.clone(), query_units(&axis.name, query)?);
        }
        for axis in point.keys() {
            if !entry.axes.iter().any(|item| item.name == *axis) {
                return Err(LutError::ExtraAxis { axis: axis.clone() });
            }
        }
        self.lookup_units(name, &units)
    }

    /// [`Self::lookup`] for the decimal strings stored in the bundle and the fixture.
    pub fn lookup_text(
        &self,
        name: &str,
        point: &BTreeMap<String, String>,
    ) -> Result<(Femtos, bool), LutError> {
        let entry = self.entry(name)?;
        let mut units = BTreeMap::new();
        for axis in &entry.axes {
            let text = point.get(&axis.name).ok_or_else(|| LutError::MissingAxis {
                axis: axis.name.clone(),
            })?;
            units.insert(axis.name.clone(), parse_units(&axis.name, text)?);
        }
        for axis in point.keys() {
            if !entry.axes.iter().any(|item| item.name == *axis) {
                return Err(LutError::ExtraAxis { axis: axis.clone() });
            }
        }
        self.lookup_units(name, &units)
    }

    fn entry(&self, name: &str) -> Result<&LutEntry, LutError> {
        self.entries
            .get(name)
            .ok_or_else(|| LutError::UnknownEntry {
                name: name.to_string(),
            })
    }

    fn lookup_units(
        &self,
        name: &str,
        point: &BTreeMap<String, i64>,
    ) -> Result<(Femtos, bool), LutError> {
        let entry = self.entry(name)?;
        let mut axes = Vec::with_capacity(entry.axes.len());
        let mut query = Vec::with_capacity(entry.axes.len());
        let mut in_valid = true;
        for axis in &entry.axes {
            let grid = axis_units(axis)?;
            let bound = entry.valid.get(&axis.name).ok_or_else(|| {
                LutError::Invalid(format!("{} is missing a valid bound", axis.name))
            })?;
            let lo = parse_units(&axis.name, &bound.lo)?;
            let hi = parse_units(&axis.name, &bound.hi)?;
            let raw = point.get(&axis.name).ok_or_else(|| LutError::MissingAxis {
                axis: axis.name.clone(),
            })?;
            let mut coord = *raw;
            if coord < lo || coord > hi {
                in_valid = false;
                coord = coord.clamp(lo, hi);
            }
            axes.push(grid);
            query.push(coord);
        }
        let sample = interpolate(&axes, &entry.values, &query)?;
        let rounded = Femtos::from_fs_f64_rounded(sample).map_err(time_to_lut)?;
        Ok((rounded, in_valid))
    }

    fn check(&self) -> Result<(), LutError> {
        if self.cell.is_empty() {
            return Err(LutError::Invalid("cell name is empty".to_string()));
        }
        if self.entries.is_empty() {
            return Err(LutError::Invalid("bundle has no entries".to_string()));
        }
        if self.source.josim_version.is_empty() {
            return Err(LutError::Invalid("josim_version is empty".to_string()));
        }
        if self.source.timestep_fs <= 0 {
            return Err(LutError::Invalid(
                "timestep_fs must be positive".to_string(),
            ));
        }
        for (name, entry) in &self.entries {
            check_entry(name, entry)?;
        }
        Ok(())
    }
}

fn check_entry(name: &str, entry: &LutEntry) -> Result<(), LutError> {
    if entry.axes.is_empty() {
        return Err(LutError::Invalid(format!("{name} has no axes")));
    }
    if entry.sigma_fs < 0 {
        return Err(LutError::Invalid(format!("{name} has a negative sigma_fs")));
    }
    let mut names = BTreeMap::new();
    let mut length = 1usize;
    for axis in &entry.axes {
        if names.insert(axis.name.clone(), ()).is_some() {
            return Err(LutError::Invalid(format!(
                "{name} repeats axis {}",
                axis.name
            )));
        }
        if axis.name != "bias_frac" && !axis.name.ends_with("_fs") {
            return Err(LutError::Invalid(format!(
                "{name} has unknown axis {}",
                axis.name
            )));
        }
        let units = axis_units(axis)?;
        if units.is_empty() {
            return Err(LutError::Invalid(format!(
                "{name} axis {} has no points",
                axis.name
            )));
        }
        for pair in units.windows(2) {
            if pair[0] >= pair[1] {
                return Err(LutError::Invalid(format!(
                    "{name} axis {} is not strictly increasing",
                    axis.name
                )));
            }
        }
        length = length
            .checked_mul(units.len())
            .ok_or_else(|| LutError::Invalid(format!("{name} grid is too large")))?;
    }
    if entry.values.len() != length {
        return Err(LutError::Invalid(format!(
            "{name} has {} values for a grid of {length}",
            entry.values.len()
        )));
    }
    if entry.valid.len() != entry.axes.len() {
        return Err(LutError::Invalid(format!(
            "{name} valid box does not match its axes"
        )));
    }
    for axis in &entry.axes {
        let bound = entry.valid.get(&axis.name).ok_or_else(|| {
            LutError::Invalid(format!("{name} is missing a valid bound for {}", axis.name))
        })?;
        let units = axis_units(axis)?;
        let lo = parse_units(&axis.name, &bound.lo)?;
        let hi = parse_units(&axis.name, &bound.hi)?;
        if !units.contains(&lo) || !units.contains(&hi) || lo > hi {
            return Err(LutError::Invalid(format!(
                "{name} valid box for {} is not on the grid",
                axis.name
            )));
        }
    }
    Ok(())
}

fn axis_units(axis: &LutAxis) -> Result<Vec<i64>, LutError> {
    axis.points
        .iter()
        .map(|text| parse_units(&axis.name, text))
        .collect()
}

fn query_units(axis: &str, query: &AxisQuery) -> Result<i64, LutError> {
    match (axis == "bias_frac", axis.ends_with("_fs"), query) {
        (true, _, AxisQuery::Fraction(value)) => fraction_units(*value),
        (false, true, AxisQuery::Time(time)) => Ok(time.0),
        _ => Err(LutError::QueryKind {
            axis: axis.to_string(),
        }),
    }
}

fn fraction_units(value: f64) -> Result<i64, LutError> {
    if !value.is_finite() {
        return Err(LutError::Invalid("bias is not finite".to_string()));
    }
    let scaled = value * 1_000_000.0;
    if !scaled.is_finite() {
        return Err(LutError::Invalid("bias is out of range".to_string()));
    }
    let rounded = scaled.round();
    if rounded < (i64::MIN as f64) || rounded >= (1u64 << 63) as f64 {
        return Err(LutError::Invalid("bias is out of range".to_string()));
    }
    Ok(rounded as i64)
}

fn parse_units(name: &str, text: &str) -> Result<i64, LutError> {
    if name == "bias_frac" {
        return parse_micro(text);
    }
    if name.ends_with("_fs") {
        return parse_fs(text);
    }
    Err(LutError::Invalid(format!("unknown axis {name}")))
}

fn parse_fs(text: &str) -> Result<i64, LutError> {
    let (neg, body) = split_sign(text)?;
    if body.is_empty() || !body.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(LutError::Invalid(format!("bad femtosecond count {text}")));
    }
    let magnitude: i64 = body
        .parse()
        .map_err(|_| LutError::Invalid(format!("bad femtosecond count {text}")))?;
    apply_sign(neg, magnitude)
}

fn parse_micro(text: &str) -> Result<i64, LutError> {
    let (neg, body) = split_sign(text)?;
    if body.is_empty() {
        return Err(LutError::Invalid(format!("bad bias {text}")));
    }
    let (whole, frac) = match body.split_once('.') {
        Some((whole, frac)) => (whole, frac),
        None => (body, ""),
    };
    if whole.is_empty() && frac.is_empty() {
        return Err(LutError::Invalid(format!("bad bias {text}")));
    }
    if !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !frac.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(LutError::Invalid(format!("bad bias {text}")));
    }
    let whole_val: i64 = if whole.is_empty() {
        0
    } else {
        whole
            .parse()
            .map_err(|_| LutError::Invalid(format!("bad bias {text}")))?
    };
    let mut micro = whole_val
        .checked_mul(1_000_000)
        .ok_or_else(|| LutError::Invalid(format!("bias {text} is out of range")))?;
    let mut frac_micro = 0i64;
    for byte in frac.bytes().take(6) {
        frac_micro = frac_micro * 10 + i64::from(byte - b'0');
    }
    let mut padded = frac_micro;
    if frac.len() < 6 {
        for _ in frac.len()..6 {
            padded *= 10;
        }
    }
    if frac.len() > 6 && frac.as_bytes()[6] >= b'5' {
        padded = padded.saturating_add(1);
    }
    if padded == 1_000_000 {
        micro = micro
            .checked_add(1_000_000)
            .ok_or_else(|| LutError::Invalid(format!("bias {text} is out of range")))?;
    } else {
        micro = micro
            .checked_add(padded)
            .ok_or_else(|| LutError::Invalid(format!("bias {text} is out of range")))?;
    }
    apply_sign(neg, micro)
}

fn split_sign(text: &str) -> Result<(bool, &str), LutError> {
    if let Some(rest) = text.strip_prefix('-') {
        Ok((true, rest))
    } else if let Some(rest) = text.strip_prefix('+') {
        Ok((false, rest))
    } else {
        Ok((false, text))
    }
}

fn apply_sign(neg: bool, magnitude: i64) -> Result<i64, LutError> {
    if !neg {
        return Ok(magnitude);
    }
    magnitude
        .checked_neg()
        .ok_or_else(|| LutError::Invalid("coordinate is out of range".to_string()))
}

fn interpolate(axes: &[Vec<i64>], values: &[i64], query: &[i64]) -> Result<f64, LutError> {
    interp(axes, values, query, 0, 0)
}

fn interp(
    axes: &[Vec<i64>],
    values: &[i64],
    query: &[i64],
    dim: usize,
    base: usize,
) -> Result<f64, LutError> {
    let points = &axes[dim];
    let stride = axes[dim + 1..].iter().map(Vec::len).product::<usize>();
    if points.len() == 1 {
        if dim + 1 == axes.len() {
            return value_at(values, base);
        }
        return interp(axes, values, query, dim + 1, base);
    }
    let (index, weight) = bracket(points, query[dim]);
    let left_base = base + index * stride;
    let right_base = base + (index + 1) * stride;
    let left = if dim + 1 == axes.len() {
        value_at(values, left_base)?
    } else {
        interp(axes, values, query, dim + 1, left_base)?
    };
    let right = if dim + 1 == axes.len() {
        value_at(values, right_base)?
    } else {
        interp(axes, values, query, dim + 1, right_base)?
    };
    Ok((1.0 - weight) * left + weight * right)
}

fn bracket(points: &[i64], query: i64) -> (usize, f64) {
    if query <= points[0] {
        return (0, 0.0);
    }
    let last = points.len() - 1;
    if query >= points[last] {
        return (last - 1, 1.0);
    }
    let mut lo = 0;
    let mut hi = last;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if points[mid] <= query {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    if query == points[lo] {
        return (lo, 0.0);
    }
    if query == points[hi] {
        return (lo, 1.0);
    }
    let span = (points[hi] - points[lo]) as f64;
    let weight = (query - points[lo]) as f64 / span;
    (lo, weight)
}

fn value_at(values: &[i64], index: usize) -> Result<f64, LutError> {
    values.get(index).map(|value| *value as f64).ok_or_else(|| {
        LutError::Invalid("interpolation index is outside the value table".to_string())
    })
}

fn time_to_lut(err: TimeError) -> LutError {
    LutError::Invalid(err.to_string())
}
