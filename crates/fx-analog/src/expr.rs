//! JoSIM parameter expressions.
//!
//! Suffixes follow `Misc::modifier` (JoSIM `src/Misc.cpp`): after the number
//! `std::stod` would consume, the next letter scales and any further letters
//! are a unit and are ignored. `M` is milli unless the next two letters are
//! `EG`. `X` is mega.
//!
//! Names are resolved by fixed point, the way `parse_parameters` walks the
//! map until a pass resolves nothing (`src/Parameters.cpp`). A subcircuit
//! parameter hides a global one. `LOG10` is in JoSIM's function list, but
//! `parse_operator` falls through and returns 0; this evaluator uses log10.

use std::collections::BTreeMap;

use crate::error::{Loc, NetlistError};

/// JoSIM `Constants.hpp`. `PHI_ZERO` is not RSFQlib's `Phi0` parameter and not spec Φ₀.
/// JoSIM prints π with extra digits; at `f64` it is [`std::f64::consts::PI`].
const PI: f64 = std::f64::consts::PI;
const PHI_ZERO: f64 = 2.067833831170082e-15;
const BOLTZMANN: f64 = 1.38064852e-23;
const EV: f64 = 1.6021766208e-19;
const HBAR: f64 = 1.0545718001391127e-34;
const C_LIGHT: f64 = 299792458.0;
const MU0: f64 = 12.566370614e-7;
const EPS0: f64 = 8.854187817e-12;
const SIGMA: f64 = 3.291059757e-16;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    name: String,
    scope: Option<String>,
}

struct Stored {
    expr: String,
    value: Option<f64>,
    loc: Loc,
}

/// Parameters for one deck. Scopes are subcircuit names; `None` is global.
#[derive(Default)]
pub(crate) struct ParamTable {
    map: BTreeMap<Key, Stored>,
}

enum Lookup {
    Value(f64),
    Unresolved,
    Unknown,
}

enum Fail {
    Unresolved,
    Bad(String),
}

enum Tok {
    Num(f64),
    Ident(String),
    Op(char),
}

impl ParamTable {
    pub(crate) fn insert(
        &mut self,
        name: String,
        scope: Option<String>,
        expr: String,
        loc: Loc,
    ) -> Result<(), NetlistError> {
        let key = Key { name, scope };
        if self.map.contains_key(&key) {
            return Err(NetlistError::at(
                &loc,
                format!("parameter {} is already defined", key.name),
            ));
        }
        self.map.insert(
            key,
            Stored {
                expr,
                value: None,
                loc,
            },
        );
        Ok(())
    }

    /// Resolve every parameter. A pass that resolves nothing is an error.
    pub(crate) fn resolve(&mut self) -> Result<(), NetlistError> {
        let keys: Vec<Key> = self.map.keys().cloned().collect();
        loop {
            let mut progress = false;
            let mut pending: Vec<(Key, Loc)> = Vec::new();
            for key in &keys {
                let Some(stored) = self.map.get(key) else {
                    continue;
                };
                if stored.value.is_some() {
                    continue;
                }
                let expr = stored.expr.clone();
                let loc = stored.loc.clone();
                let scope = key.scope.clone();
                match self.eval_inner(&expr, scope.as_deref()) {
                    Ok(value) => {
                        if let Some(stored) = self.map.get_mut(key) {
                            stored.value = Some(value);
                        }
                        progress = true;
                    }
                    Err(Fail::Unresolved) => pending.push((key.clone(), loc)),
                    Err(Fail::Bad(message)) => {
                        return Err(NetlistError::at(
                            &loc,
                            format!("parameter {} : {message}", key.name),
                        ));
                    }
                }
            }
            if pending.is_empty() {
                return Ok(());
            }
            if !progress {
                let Some((_, loc)) = pending.first() else {
                    return Ok(());
                };
                let names = pending
                    .iter()
                    .map(|(key, _)| match &key.scope {
                        Some(scope) => format!("{} ({scope})", key.name),
                        None => key.name.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(NetlistError::at(
                    loc,
                    format!("unresolved parameters: {names}"),
                ));
            }
        }
    }

    pub(crate) fn eval(
        &self,
        expr: &str,
        scope: Option<&str>,
        loc: &Loc,
    ) -> Result<f64, NetlistError> {
        self.eval_inner(expr, scope).map_err(|fail| match fail {
            Fail::Unresolved => NetlistError::at(loc, format!("unresolved expression {expr}")),
            Fail::Bad(message) => NetlistError::at(loc, message),
        })
    }

    fn eval_inner(&self, expr: &str, scope: Option<&str>) -> Result<f64, Fail> {
        let upper = expr.to_ascii_uppercase();
        let toks = lex(&upper)?;
        if toks.is_empty() {
            return Err(Fail::Bad("empty expression".into()));
        }
        let mut parser = Parser {
            toks: &toks,
            index: 0,
            lookup: &|name| self.lookup(name, scope),
        };
        let value = parser.expr()?;
        if parser.index != toks.len() {
            return Err(Fail::Bad(format!("trailing input in {expr}")));
        }
        finite(value)
    }

    fn lookup(&self, name: &str, scope: Option<&str>) -> Lookup {
        if let Some(stored) = self.get(name, scope) {
            return match stored.value {
                Some(value) => Lookup::Value(value),
                None => Lookup::Unresolved,
            };
        }
        match constant(name) {
            Some(value) => Lookup::Value(value),
            None => Lookup::Unknown,
        }
    }

    fn get(&self, name: &str, scope: Option<&str>) -> Option<&Stored> {
        if let Some(scope) = scope
            && let Some(stored) = self.map.get(&Key {
                name: name.to_string(),
                scope: Some(scope.to_string()),
            })
        {
            return Some(stored);
        }
        self.map.get(&Key {
            name: name.to_string(),
            scope: None,
        })
    }
}

fn constant(name: &str) -> Option<f64> {
    match name {
        "PI" => Some(PI),
        "PHI_ZERO" => Some(PHI_ZERO),
        "BOLTZMANN" => Some(BOLTZMANN),
        "EV" => Some(EV),
        "HBAR" => Some(HBAR),
        "C" => Some(C_LIGHT),
        "MU0" => Some(MU0),
        "EPS0" => Some(EPS0),
        "SIGMA" => Some(SIGMA),
        _ => None,
    }
}

pub(crate) fn pi() -> f64 {
    PI
}

pub(crate) fn boltzmann() -> f64 {
    BOLTZMANN
}

pub(crate) fn ev() -> f64 {
    EV
}

struct Parser<'a> {
    toks: &'a [Tok],
    index: usize,
    lookup: &'a dyn Fn(&str) -> Lookup,
}

impl Parser<'_> {
    fn expr(&mut self) -> Result<f64, Fail> {
        let mut value = self.term()?;
        loop {
            if self.eat('+') {
                value = finite(value + self.term()?)?;
            } else if self.eat('-') {
                value = finite(value - self.term()?)?;
            } else {
                break;
            }
        }
        Ok(value)
    }

    fn term(&mut self) -> Result<f64, Fail> {
        let mut value = self.power()?;
        loop {
            if self.eat('*') {
                value = finite(value * self.power()?)?;
            } else if self.eat('/') {
                value = finite(value / self.power()?)?;
            } else {
                break;
            }
        }
        Ok(value)
    }

    /// `^` is right-associative, matching `precedence_lvl` in Parameters.cpp.
    fn power(&mut self) -> Result<f64, Fail> {
        let left = self.unary()?;
        if self.eat('^') {
            let right = self.power()?;
            return finite(left.powf(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<f64, Fail> {
        if self.eat('+') {
            return self.unary();
        }
        if self.eat('-') {
            return finite(-self.unary()?);
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<f64, Fail> {
        let Some(tok) = self.toks.get(self.index) else {
            return Err(Fail::Bad("truncated expression".into()));
        };
        match tok {
            Tok::Num(value) => {
                self.index += 1;
                Ok(*value)
            }
            Tok::Ident(name) => {
                let name = name.clone();
                self.index += 1;
                if self.eat('(') {
                    let arg = self.expr()?;
                    self.expect(')')?;
                    apply_fn(&name, arg)
                } else {
                    match (self.lookup)(&name) {
                        Lookup::Value(value) => Ok(value),
                        Lookup::Unresolved => Err(Fail::Unresolved),
                        Lookup::Unknown => Err(Fail::Bad(format!("unknown name {name}"))),
                    }
                }
            }
            Tok::Op('(') | Tok::Op('[') | Tok::Op('{') => {
                let close = match self.toks.get(self.index) {
                    Some(Tok::Op('(')) => ')',
                    Some(Tok::Op('[')) => ']',
                    Some(Tok::Op('{')) => '}',
                    _ => ')',
                };
                self.index += 1;
                let value = self.expr()?;
                self.expect(close)?;
                Ok(value)
            }
            Tok::Op(op) => Err(Fail::Bad(format!("unexpected '{op}'"))),
        }
    }

    fn eat(&mut self, op: char) -> bool {
        if matches!(self.toks.get(self.index), Some(Tok::Op(found)) if *found == op) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, op: char) -> Result<(), Fail> {
        if self.eat(op) {
            Ok(())
        } else {
            Err(Fail::Bad(format!("expected '{op}'")))
        }
    }
}

fn apply_fn(name: &str, arg: f64) -> Result<f64, Fail> {
    let value = match name {
        "SIN" => arg.sin(),
        "COS" => arg.cos(),
        "TAN" => arg.tan(),
        "ASIN" => arg.asin(),
        "ACOS" => arg.acos(),
        "ATAN" => arg.atan(),
        "SINH" => arg.sinh(),
        "COSH" => arg.cosh(),
        "TANH" => arg.tanh(),
        "ASINH" => arg.asinh(),
        "ACOSH" => arg.acosh(),
        "ATANH" => arg.atanh(),
        "EXP" => arg.exp(),
        "LOG" => arg.ln(),
        // JoSIM parse_operator does not handle LOG10 and returns 0.
        "LOG10" => arg.log10(),
        "SQRT" => arg.sqrt(),
        "CBRT" => arg.cbrt(),
        _ => return Err(Fail::Bad(format!("unknown function {name}"))),
    };
    finite(value)
}

fn finite(value: f64) -> Result<f64, Fail> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Fail::Bad(format!("non-finite value {value}")))
    }
}

fn lex(input: &str) -> Result<Vec<Tok>, Fail> {
    let bytes = input.as_bytes();
    let mut index = 0;
    let mut toks = Vec::new();
    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
            continue;
        }
        let ch = bytes[index] as char;
        if "+-*/^()[]{}".contains(ch) {
            toks.push(Tok::Op(ch));
            index += 1;
            continue;
        }
        if ch.is_ascii_digit() || ch == '.' {
            let (value, used) = scan_number(&input[index..])?;
            toks.push(Tok::Num(value));
            index += used;
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            toks.push(Tok::Ident(input[start..index].to_string()));
            continue;
        }
        return Err(Fail::Bad(format!("bad character '{ch}'")));
    }
    Ok(toks)
}

/// Number plus scale suffix. The numeric part is what `std::stod` consumes,
/// including an exponent. Cited from JoSIM `Misc::modifier`.
fn scan_number(input: &str) -> Result<(f64, usize), Fail> {
    let bytes = input.as_bytes();
    let mut index = 0;
    let mut saw_digit = false;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        saw_digit = true;
        index += 1;
    }
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            saw_digit = true;
            index += 1;
        }
    }
    if !saw_digit {
        return Err(Fail::Bad(format!("not a number: {input}")));
    }
    if index < bytes.len() && (bytes[index] == b'E' || bytes[index] == b'e') {
        let mut exp = index + 1;
        if exp < bytes.len() && (bytes[exp] == b'+' || bytes[exp] == b'-') {
            exp += 1;
        }
        let exp_digits = exp;
        while exp < bytes.len() && bytes[exp].is_ascii_digit() {
            exp += 1;
        }
        if exp == exp_digits {
            return Err(Fail::Bad(format!("bad exponent in {input}")));
        }
        index = exp;
    }
    let mut suffix = index;
    while suffix < bytes.len() && bytes[suffix].is_ascii_alphabetic() {
        suffix += 1;
    }
    // `Misc::modifier` returns the number unchanged when the first leftover
    // character is not a scale. RSFQlib writes `BiasCoef=0.7'`.
    while suffix < bytes.len() && !stops_number(bytes[suffix]) {
        if bytes[suffix] == b'$' {
            return Err(Fail::Bad("'$' is not a comment".into()));
        }
        suffix += 1;
    }
    let number: f64 = input[..index]
        .parse()
        .map_err(|_| Fail::Bad(format!("bad number {}", &input[..index])))?;
    let value = scale(&input[index..suffix], number)?;
    Ok((value, suffix))
}

fn stops_number(byte: u8) -> bool {
    byte.is_ascii_whitespace() || b"+-*/^()[]{}".contains(&byte)
}

fn scale(suffix: &str, number: f64) -> Result<f64, Fail> {
    let bytes = suffix.as_bytes();
    let value = if bytes.is_empty() {
        number
    } else {
        match bytes[0] {
            b'X' => number * 1e6,
            b'M' => {
                if bytes.len() >= 3 && bytes[1] == b'E' && bytes[2] == b'G' {
                    number * 1e6
                } else {
                    number * 1e-3
                }
            }
            b'U' => number * 1e-6,
            b'N' => number * 1e-9,
            b'P' => number * 1e-12,
            b'F' => number * 1e-15,
            b'K' => number * 1e3,
            b'G' => number * 1e9,
            b'T' => number * 1e12,
            // Any other letter is a unit (V, A, H, OHM, …) and does not scale.
            _ => number,
        }
    };
    finite(value)
}
