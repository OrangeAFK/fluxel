//! JoSIM deck reader.
//!
//! Line handling follows `Input::read_input`: `*` and `#` comments, `+`
//! continuation, uppercase tokens, `.include` relative to the including file,
//! and `.end` stops that file. `$` is not a comment.
//!
//! `X` expansion follows `Netlist::expand_io`: the instance name is appended
//! after `|`, pins are replaced, and every other node (and a `K` inductor
//! name) gets the same suffix. `0` and `GND` stay put.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Loc, NetlistError};
use crate::expr::{self, ParamTable};

/// Transient card. Times are seconds.
///
/// Omitted print-start is 0 and omitted print-step copies the step, from
/// `Transient::identify_simulation`. A missing `.tran` stays unset so a
/// library subcircuit is still a deck.
#[derive(Debug, Clone, PartialEq)]
pub struct Tran {
    pub tstep: f64,
    pub tstop: f64,
    pub prstart: f64,
    pub prstep: f64,
}

/// Independent source waveform. Times are seconds. Amplitudes are amperes or volts.
#[derive(Debug, Clone, PartialEq)]
pub enum Wave {
    Dc(f64),
    Pwl(Vec<(f64, f64)>),
    Pulse {
        v1: f64,
        v2: f64,
        td: f64,
        tr: f64,
        tf: f64,
        pw: f64,
        per: f64,
    },
    Sin {
        vo: f64,
        va: f64,
        freq: f64,
        td: f64,
        theta: f64,
    },
}

/// Josephson junction after `JJ::set_model` area scaling. SI units.
#[derive(Debug, Clone, PartialEq)]
pub struct Jj {
    pub name: String,
    pub n1: String,
    pub n2: String,
    pub ic: f64,
    pub vg: f64,
    pub c: f64,
    pub r0: f64,
    pub rn: f64,
    pub rtype: i64,
    pub deltav: f64,
    pub icfct: f64,
    pub phi: f64,
}

/// Lossless transmission line. `z0` is ohms and `td` is seconds.
///
/// Card shape from `TransmissionLine.cpp`: `Tlabel Vi+ Vi- Vo+ Vo- td= z0=`.
/// `LOSSLESS` is accepted. JoSIM ignores any other extra token; this parser
/// reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Tx {
    pub name: String,
    pub n1: String,
    pub n2: String,
    pub n3: String,
    pub n4: String,
    pub z0: f64,
    pub td: f64,
}

/// One flattened device.
#[derive(Debug, Clone, PartialEq)]
pub enum Element {
    R {
        name: String,
        n1: String,
        n2: String,
        ohms: f64,
    },
    L {
        name: String,
        n1: String,
        n2: String,
        henries: f64,
    },
    C {
        name: String,
        n1: String,
        n2: String,
        farads: f64,
    },
    K {
        name: String,
        l1: String,
        l2: String,
        k: f64,
    },
    Jj(Jj),
    I {
        name: String,
        n1: String,
        n2: String,
        wave: Wave,
    },
    V {
        name: String,
        n1: String,
        n2: String,
        wave: Wave,
    },
    Tx(Tx),
}

/// A parsed deck. `main` is the flattened top level, which is empty when the
/// file is only a subcircuit definition. JoSIM would then report `MISSING_MAIN`.
pub struct Deck {
    pub main: Vec<Element>,
    pub tran: Option<Tran>,
    pub prints: Vec<String>,
    pub temp_k: Option<f64>,
    subckts: BTreeMap<String, Subckt>,
    models: Vec<Model>,
    params: ParamTable,
}

impl std::fmt::Debug for Deck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Deck")
            .field("main", &self.main)
            .field("tran", &self.tran)
            .field("prints", &self.prints)
            .field("temp_k", &self.temp_k)
            .finish()
    }
}

impl Deck {
    /// Expand one subcircuit as a single unnamed instance.
    ///
    /// Pins keep their names and devices are not suffixed, so a cell file can
    /// be checked directly. Nested `X` instances still get `|` names.
    pub fn flatten_subckt(&self, name: &str) -> Result<Vec<Element>, NetlistError> {
        let key = name.to_ascii_uppercase();
        let Some(sub) = self.subckts.get(&key) else {
            return Err(NetlistError::At {
                file: name.to_string(),
                line: 0,
                message: format!("unknown subcircuit {name}"),
            });
        };
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        let place = Place {
            scope: Some(key.as_str()),
            pins: &sub.pins,
            parent: &sub.pins,
            path: "",
        };
        self.expand(&sub.lines, &place, &mut out, &mut seen)?;
        Ok(out)
    }
}

struct Subckt {
    pins: Vec<String>,
    lines: Vec<SrcLine>,
}

struct Model {
    name: String,
    scope: Option<String>,
    vg: f64,
    ic: f64,
    rtype: i64,
    rn: f64,
    r0: f64,
    c: f64,
    t: f64,
    tc: f64,
    deltav: f64,
    icfct: f64,
    phi: f64,
    t_dep: bool,
}

#[derive(Clone)]
struct SrcLine {
    loc: Loc,
    tokens: Vec<String>,
}

struct Place<'a> {
    scope: Option<&'a str>,
    pins: &'a [String],
    parent: &'a [String],
    path: &'a str,
}

/// Read a netlist file. `.include` paths are relative to the including file.
pub fn parse_netlist(path: &Path) -> Result<Deck, NetlistError> {
    reject_idx(path)?;
    let mut stack = Vec::new();
    let lines = read_path(path, &mut stack)?;
    assemble(lines)
}

/// Parse a deck held in memory. `name` is the file name used in errors.
pub fn parse_source(name: &str, text: &str) -> Result<Deck, NetlistError> {
    let path = Path::new(name);
    reject_idx(path)?;
    let dir = directory_of(path);
    let mut stack = Vec::new();
    let lines = read_text(name, &dir, text, &mut stack)?;
    assemble(lines)
}

fn reject_idx(path: &Path) -> Result<(), NetlistError> {
    if is_idx(path) {
        Err(NetlistError::NotASimulationNetlist {
            path: path.display().to_string(),
            trigger: "filename ends with _idx.cir".into(),
        })
    } else {
        Ok(())
    }
}

fn is_idx(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with("_idx.cir"))
}

fn directory_of(path: &Path) -> PathBuf {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn read_path(path: &Path, stack: &mut Vec<PathBuf>) -> Result<Vec<SrcLine>, NetlistError> {
    let label = path.display().to_string();
    let canon = path.canonicalize().map_err(|source| NetlistError::Io {
        path: label.clone(),
        source,
    })?;
    if stack.iter().any(|seen| seen == &canon) {
        return Err(NetlistError::At {
            file: label,
            line: 1,
            message: "cyclic .include".into(),
        });
    }
    let text = fs::read_to_string(&canon).map_err(|source| NetlistError::Io {
        path: label.clone(),
        source,
    })?;
    stack.push(canon);
    let lines = read_text(&label, &directory_of(path), &text, stack);
    stack.pop();
    lines
}

fn read_text(
    label: &str,
    dir: &Path,
    text: &str,
    stack: &mut Vec<PathBuf>,
) -> Result<Vec<SrcLine>, NetlistError> {
    let mut out = Vec::new();
    for (index, raw_line) in text.lines().enumerate() {
        let line_no = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let loc = Loc {
            file: label.to_string(),
            line: line_no,
        };
        let raw = raw_line.trim();
        let Some(first) = raw.chars().next() else {
            continue;
        };
        if first == '*' || first == '#' {
            continue;
        }
        let parts: Vec<&str> = raw.split_whitespace().collect();
        let Some(head_raw) = parts.first() else {
            continue;
        };
        let head = head_raw.to_ascii_uppercase();
        if head == ".INCLUDE" || head == "INCLUDE" {
            let Some(include) = parts.get(1) else {
                return Err(NetlistError::at(&loc, "missing .include path"));
            };
            let path = dir.join(include);
            if is_idx(&path) {
                return Err(NetlistError::NotASimulationNetlist {
                    path: path.display().to_string(),
                    trigger: "filename ends with _idx.cir".into(),
                });
            }
            out.extend(read_path(&path, stack)?);
            continue;
        }
        let mut tokens = Vec::with_capacity(parts.len());
        tokens.push(head);
        for part in parts.iter().skip(1) {
            tokens.push(part.to_ascii_uppercase());
        }
        if tokens.iter().any(|token| token.contains('$')) {
            return Err(NetlistError::at(
                &loc,
                "'$' is not a comment in this subset",
            ));
        }
        if tokens[0].starts_with('+') {
            if tokens[0] == "+" {
                tokens.remove(0);
            } else {
                tokens[0] = tokens[0][1..].to_string();
            }
            let Some(prev) = out.last_mut() else {
                return Err(NetlistError::at(
                    &loc,
                    "line continuation has no previous line",
                ));
            };
            prev.tokens.extend(tokens);
            continue;
        }
        if tokens[0] == ".END" || tokens[0] == "END" {
            break;
        }
        if tokens[0] == ".CONTROL" || tokens[0] == "CONTROL" || tokens[0] == "BACK-ANNOTATE" {
            return Err(NetlistError::NotASimulationNetlist {
                path: label.to_string(),
                trigger: format!("line {line_no}: {}", tokens[0]),
            });
        }
        out.push(SrcLine { loc, tokens });
    }
    Ok(out)
}

fn assemble(lines: Vec<SrcLine>) -> Result<Deck, NetlistError> {
    let mut subckts: BTreeMap<String, Subckt> = BTreeMap::new();
    let mut params = ParamTable::default();
    let mut model_lines: Vec<(SrcLine, Option<String>)> = Vec::new();
    let mut main: Vec<SrcLine> = Vec::new();
    let mut tran_lines: Vec<SrcLine> = Vec::new();
    let mut print_lines: Vec<SrcLine> = Vec::new();
    let mut temp_line: Option<SrcLine> = None;
    let mut current: Option<String> = None;

    for line in lines {
        let kind = line.tokens.first().map(String::as_str).unwrap_or("");
        match kind {
            ".SUBCKT" => {
                if current.is_some() {
                    return Err(NetlistError::at(&line.loc, "nested .subckt"));
                }
                if line.tokens.len() < 3 {
                    return Err(NetlistError::at(
                        &line.loc,
                        ".subckt needs a name and at least one pin",
                    ));
                }
                let name = line.tokens[1].clone();
                let pins: Vec<String> = line.tokens[2..].to_vec();
                if pins.iter().any(|pin| pin.contains('=')) {
                    return Err(NetlistError::at(
                        &line.loc,
                        "subcircuit parameter on .subckt is not supported",
                    ));
                }
                if subckts.contains_key(&name) {
                    return Err(NetlistError::at(
                        &line.loc,
                        format!("duplicate subcircuit {name}"),
                    ));
                }
                subckts.insert(
                    name.clone(),
                    Subckt {
                        pins,
                        lines: Vec::new(),
                    },
                );
                current = Some(name);
            }
            ".ENDS" => {
                let Some(name) = current.take() else {
                    return Err(NetlistError::at(&line.loc, ".ends without .subckt"));
                };
                if line.tokens.len() > 2 {
                    return Err(NetlistError::at(&line.loc, "extra tokens on .ends"));
                }
                if line.tokens.len() == 2 && line.tokens[1] != name {
                    return Err(NetlistError::at(
                        &line.loc,
                        format!(".ends {} does not close {name}", line.tokens[1]),
                    ));
                }
            }
            ".PARAM" => {
                let body = line.tokens[1..].join("");
                let Some((name, expr)) = body.split_once('=') else {
                    return Err(NetlistError::at(&line.loc, ".param needs name=expression"));
                };
                if name.is_empty() || expr.is_empty() {
                    return Err(NetlistError::at(&line.loc, ".param needs name=expression"));
                }
                params.insert(
                    name.to_string(),
                    current.clone(),
                    expr.to_string(),
                    line.loc,
                )?;
            }
            ".MODEL" => model_lines.push((line, current.clone())),
            ".TRAN" => {
                if current.is_some() {
                    return Err(NetlistError::at(&line.loc, "control inside a subcircuit"));
                }
                tran_lines.push(line);
            }
            ".PRINT" => {
                if current.is_some() {
                    return Err(NetlistError::at(&line.loc, "control inside a subcircuit"));
                }
                print_lines.push(line);
            }
            ".TEMP" => {
                if current.is_some() {
                    return Err(NetlistError::at(&line.loc, "control inside a subcircuit"));
                }
                temp_line = Some(line);
            }
            _ if kind.starts_with('.') => {
                return Err(NetlistError::at(
                    &line.loc,
                    format!("unsupported card {kind}"),
                ));
            }
            _ => {
                if let Some(name) = current.clone() {
                    let Some(sub) = subckts.get_mut(&name) else {
                        return Err(NetlistError::at(&line.loc, "missing subcircuit"));
                    };
                    sub.lines.push(line);
                } else {
                    main.push(line);
                }
            }
        }
    }
    if let Some(name) = current {
        return Err(NetlistError::At {
            file: String::new(),
            line: 0,
            message: format!("unclosed .subckt {name}"),
        });
    }

    params.resolve()?;
    let mut models = Vec::new();
    for (line, scope) in model_lines {
        models.push(parse_model(&line, scope, &params)?);
    }
    let tran = tran_lines
        .last()
        .map(|line| parse_tran(line, &params))
        .transpose()?;
    let mut prints = Vec::new();
    for line in &print_lines {
        if line.tokens.len() < 2 {
            return Err(NetlistError::at(&line.loc, ".print needs a signal"));
        }
        prints.extend(line.tokens.iter().skip(1).cloned());
    }
    let temp_k = temp_line
        .as_ref()
        .map(|line| parse_temp(line, &params))
        .transpose()?;

    let deck = Deck {
        main: Vec::new(),
        tran,
        prints,
        temp_k,
        subckts,
        models,
        params,
    };
    let mut elements = Vec::new();
    let mut seen = BTreeSet::new();
    let place = Place {
        scope: None,
        pins: &[],
        parent: &[],
        path: "",
    };
    deck.expand(&main, &place, &mut elements, &mut seen)?;
    Ok(Deck {
        main: elements,
        ..deck
    })
}

fn parse_tran(line: &SrcLine, params: &ParamTable) -> Result<Tran, NetlistError> {
    if line.tokens.len() < 3 || line.tokens.len() > 5 {
        return Err(NetlistError::at(
            &line.loc,
            ".tran needs tstep and tstop, and optionally print start and print step",
        ));
    }
    let tstep = params.eval(&line.tokens[1], None, &line.loc)?;
    let tstop = params.eval(&line.tokens[2], None, &line.loc)?;
    let prstart = if line.tokens.len() >= 4 {
        params.eval(&line.tokens[3], None, &line.loc)?
    } else {
        0.0
    };
    let mut prstep = if line.tokens.len() >= 5 {
        params.eval(&line.tokens[4], None, &line.loc)?
    } else {
        tstep
    };
    if tstep == 0.0 || tstop == 0.0 {
        return Err(NetlistError::at(
            &line.loc,
            ".tran step and stop must be non-zero",
        ));
    }
    let mut tstep = tstep;
    if tstep > prstep {
        tstep = prstep;
    }
    if prstep == 0.0 {
        prstep = tstep;
    }
    Ok(Tran {
        tstep,
        tstop,
        prstart,
        prstep,
    })
}

fn parse_temp(line: &SrcLine, params: &ParamTable) -> Result<f64, NetlistError> {
    if line.tokens.len() != 2 {
        return Err(NetlistError::at(
            &line.loc,
            ".temp needs one value in kelvin",
        ));
    }
    params.eval(&line.tokens[1], None, &line.loc)
}

fn parse_model(
    line: &SrcLine,
    scope: Option<String>,
    params: &ParamTable,
) -> Result<Model, NetlistError> {
    if line.tokens.len() < 3 {
        return Err(NetlistError::at(
            &line.loc,
            ".model needs a name and a type",
        ));
    }
    let name = line.tokens[1].clone();
    let body = line.tokens[2..].join("");
    let Some(rest) = body.strip_prefix("JJ") else {
        return Err(NetlistError::at(&line.loc, "only jj models are supported"));
    };
    let rest = rest.trim_matches(|ch| ch == '(' || ch == ')');
    // Defaults from JoSIM Model.hpp.
    let mut model = Model {
        name,
        scope,
        vg: 2.8e-3,
        ic: 1e-3,
        rtype: 1,
        rn: 5.0,
        r0: 30.0,
        c: 2.5e-12,
        t: 4.2,
        tc: 9.1,
        deltav: 0.1e-3,
        icfct: expr::pi() / 4.0,
        phi: 0.0,
        t_dep: false,
    };
    if rest.is_empty() {
        return Ok(model);
    }
    for piece in rest.split(',') {
        if piece.is_empty() {
            continue;
        }
        let Some((key, value_src)) = piece.split_once('=') else {
            return Err(NetlistError::at(
                &line.loc,
                format!("model parameter {piece} needs a value"),
            ));
        };
        let value = params.eval(value_src, model.scope.as_deref(), &line.loc)?;
        match key {
            "VG" | "VGAP" => model.vg = value,
            "IC" | "ICRIT" => model.ic = value,
            "RTYPE" => model.rtype = integer(value, &line.loc)?,
            "RN" => model.rn = value,
            "R0" => model.r0 = value,
            "CAP" | "C" => model.c = value,
            "T" => {
                model.t = value;
                model.t_dep = true;
            }
            "TC" => {
                model.tc = value;
                model.t_dep = true;
            }
            "DELV" => model.deltav = value,
            "D" => model.t_dep = true,
            "ICFACT" | "ICFCT" => model.icfct = value,
            "PHI" => model.phi = value,
            _ => {
                return Err(NetlistError::at(
                    &line.loc,
                    format!("unknown model parameter {key}"),
                ));
            }
        }
    }
    Ok(model)
}

fn integer(value: f64, loc: &Loc) -> Result<i64, NetlistError> {
    if !value.is_finite() || (value - value.round()).abs() > 1e-6 {
        return Err(NetlistError::at(loc, "model value is not an integer"));
    }
    Ok(value.round() as i64)
}

impl Deck {
    fn expand(
        &self,
        lines: &[SrcLine],
        place: &Place<'_>,
        out: &mut Vec<Element>,
        seen: &mut BTreeSet<String>,
    ) -> Result<(), NetlistError> {
        for line in lines {
            let Some(kind) = line.tokens.first().and_then(|token| token.chars().next()) else {
                return Err(NetlistError::at(&line.loc, "empty device line"));
            };
            match kind {
                'X' => self.expand_x(line, place, out, seen)?,
                'R' | 'L' | 'C' => self.expand_passive(line, place, out, seen)?,
                'K' => self.expand_mutual(line, place, out, seen)?,
                'B' => self.expand_jj(line, place, out, seen)?,
                'I' | 'V' => self.expand_source(line, place, out, seen)?,
                'T' => self.expand_tx(line, place, out, seen)?,
                _ => {
                    return Err(NetlistError::at(
                        &line.loc,
                        format!("unsupported device {}", line.tokens[0]),
                    ));
                }
            }
        }
        Ok(())
    }

    fn expand_x(
        &self,
        line: &SrcLine,
        place: &Place<'_>,
        out: &mut Vec<Element>,
        seen: &mut BTreeSet<String>,
    ) -> Result<(), NetlistError> {
        if line.tokens.len() < 2 {
            return Err(NetlistError::at(&line.loc, "X needs a subcircuit"));
        }
        let mut io = Vec::new();
        let mut sub_name: Option<String> = None;
        for token in line.tokens.iter().skip(1) {
            if token.contains('=') {
                return Err(NetlistError::at(
                    &line.loc,
                    format!("instance parameter {token} is not supported"),
                ));
            }
            if self.subckts.contains_key(token) {
                if sub_name.is_some() {
                    return Err(NetlistError::at(
                        &line.loc,
                        "X names more than one subcircuit",
                    ));
                }
                sub_name = Some(token.clone());
            } else {
                io.push(token.clone());
            }
        }
        let Some(sub_name) = sub_name else {
            return Err(NetlistError::at(&line.loc, "unknown subcircuit on X"));
        };
        let (pins, lines) = {
            let Some(sub) = self.subckts.get(&sub_name) else {
                return Err(NetlistError::at(&line.loc, "unknown subcircuit on X"));
            };
            if io.len() != sub.pins.len() {
                return Err(NetlistError::at(
                    &line.loc,
                    format!(
                        "{} has {} pins, X passed {}",
                        sub_name,
                        sub.pins.len(),
                        io.len()
                    ),
                ));
            }
            (sub.pins.clone(), sub.lines.clone())
        };
        let parent: Vec<String> = io.iter().map(|node| map_node(node, place)).collect();
        let path = map_name(&line.tokens[0], place.path);
        let child = Place {
            scope: Some(sub_name.as_str()),
            pins: &pins,
            parent: &parent,
            path: &path,
        };
        self.expand(&lines, &child, out, seen)
    }

    fn expand_passive(
        &self,
        line: &SrcLine,
        place: &Place<'_>,
        out: &mut Vec<Element>,
        seen: &mut BTreeSet<String>,
    ) -> Result<(), NetlistError> {
        if line.tokens.len() < 4 {
            return Err(NetlistError::at(
                &line.loc,
                "device needs two nodes and a value",
            ));
        }
        let name = self.claim(&line.tokens[0], place, &line.loc, seen)?;
        let n1 = map_node(&line.tokens[1], place);
        let n2 = map_node(&line.tokens[2], place);
        let value = self.value(&line.tokens[3..].join(""), place, &line.loc)?;
        let element = match line.tokens[0].chars().next() {
            Some('R') => Element::R {
                name,
                n1,
                n2,
                ohms: value,
            },
            Some('L') => Element::L {
                name,
                n1,
                n2,
                henries: value,
            },
            Some('C') => Element::C {
                name,
                n1,
                n2,
                farads: value,
            },
            _ => return Err(NetlistError::at(&line.loc, "not a passive device")),
        };
        out.push(element);
        Ok(())
    }

    fn expand_mutual(
        &self,
        line: &SrcLine,
        place: &Place<'_>,
        out: &mut Vec<Element>,
        seen: &mut BTreeSet<String>,
    ) -> Result<(), NetlistError> {
        if line.tokens.len() < 4 {
            return Err(NetlistError::at(
                &line.loc,
                "K needs two inductors and a coupling",
            ));
        }
        let name = self.claim(&line.tokens[0], place, &line.loc, seen)?;
        let value = self.value(&line.tokens[3..].join(""), place, &line.loc)?;
        out.push(Element::K {
            name,
            l1: map_node(&line.tokens[1], place),
            l2: map_node(&line.tokens[2], place),
            k: value,
        });
        Ok(())
    }

    fn expand_jj(
        &self,
        line: &SrcLine,
        place: &Place<'_>,
        out: &mut Vec<Element>,
        seen: &mut BTreeSet<String>,
    ) -> Result<(), NetlistError> {
        if line.tokens.len() < 4 {
            return Err(NetlistError::at(&line.loc, "B needs two nodes and a model"));
        }
        let name = self.claim(&line.tokens[0], place, &line.loc, seen)?;
        let n1 = map_node(&line.tokens[1], place);
        let n2 = map_node(&line.tokens[2], place);
        let mut model_name: Option<&str> = None;
        let mut area: Option<f64> = None;
        let mut ic_set: Option<f64> = None;
        for token in line.tokens.iter().skip(3) {
            if let Some(value) = token.strip_prefix("AREA=") {
                area = Some(self.value(value, place, &line.loc)?);
            } else if let Some(value) = token.strip_prefix("IC=") {
                ic_set = Some(self.value(value, place, &line.loc)?);
            } else if token.contains('=') {
                return Err(NetlistError::at(
                    &line.loc,
                    format!("unsupported junction parameter {token}"),
                ));
            } else if model_name.is_some() {
                return Err(NetlistError::at(&line.loc, "extra model name on B"));
            } else {
                model_name = Some(token);
            }
        }
        let Some(model_name) = model_name else {
            return Err(NetlistError::at(&line.loc, "B needs a model"));
        };
        let model = self.find_model(model_name, place.scope, &line.loc)?;
        let mut area = area.unwrap_or(1.0);
        if let Some(ic) = ic_set {
            if model.ic == 0.0 {
                return Err(NetlistError::at(&line.loc, "IC= on a model with Ic = 0"));
            }
            area = ic / model.ic;
        }
        if !area.is_finite() || area == 0.0 {
            return Err(NetlistError::at(
                &line.loc,
                "junction area must be finite and non-zero",
            ));
        }
        // JoSIM JJ::set_model. Area scales the RCSJ branch
        // I_JJ [A] = Ic sin φ + (Φ₀/(2π R)) φ̇ + (Φ₀ C /(2π)) φ̈:
        // Ic [A] *= area, C [F] *= area, Rn [Ω] /= area, R0 [Ω] /= area.
        // IC= sets area = Ic_instance / Ic_model before that scaling.
        let ic = model.ic * area;
        let c = model.c * area;
        let rn = if model.t_dep {
            bcs_rn(model, ic, &line.loc)?
        } else {
            model.rn / area
        };
        let mut r0 = model.r0 / area;
        if model.rtype == 0 {
            r0 = rn;
        }
        out.push(Element::Jj(Jj {
            name,
            n1,
            n2,
            ic,
            vg: model.vg,
            c,
            r0,
            rn,
            rtype: model.rtype,
            deltav: model.deltav,
            icfct: model.icfct,
            phi: model.phi,
        }));
        Ok(())
    }

    fn expand_source(
        &self,
        line: &SrcLine,
        place: &Place<'_>,
        out: &mut Vec<Element>,
        seen: &mut BTreeSet<String>,
    ) -> Result<(), NetlistError> {
        if line.tokens.len() < 4 {
            return Err(NetlistError::at(
                &line.loc,
                "source needs two nodes and a waveform",
            ));
        }
        let name = self.claim(&line.tokens[0], place, &line.loc, seen)?;
        let n1 = map_node(&line.tokens[1], place);
        let n2 = map_node(&line.tokens[2], place);
        let spec = line.tokens[3..].join(" ");
        let wave = self.wave(&spec, place, &line.loc)?;
        if line.tokens[0].starts_with('V') {
            out.push(Element::V { name, n1, n2, wave });
        } else {
            out.push(Element::I { name, n1, n2, wave });
        }
        Ok(())
    }

    fn expand_tx(
        &self,
        line: &SrcLine,
        place: &Place<'_>,
        out: &mut Vec<Element>,
        seen: &mut BTreeSet<String>,
    ) -> Result<(), NetlistError> {
        if line.tokens.len() < 5 {
            return Err(NetlistError::at(&line.loc, "T needs four nodes"));
        }
        let name = self.claim(&line.tokens[0], place, &line.loc, seen)?;
        let mut z0 = None;
        let mut td = None;
        for token in line.tokens.iter().skip(5) {
            if token == "LOSSLESS" {
                continue;
            } else if let Some(value) = token.strip_prefix("Z0=") {
                z0 = Some(self.value(value, place, &line.loc)?);
            } else if let Some(value) = token.strip_prefix("TD=") {
                td = Some(self.value(value, place, &line.loc)?);
            } else {
                return Err(NetlistError::at(
                    &line.loc,
                    format!("unsupported transmission-line token {token}"),
                ));
            }
        }
        let (Some(z0), Some(td)) = (z0, td) else {
            return Err(NetlistError::at(&line.loc, "T needs Z0= and TD="));
        };
        out.push(Element::Tx(Tx {
            name,
            n1: map_node(&line.tokens[1], place),
            n2: map_node(&line.tokens[2], place),
            n3: map_node(&line.tokens[3], place),
            n4: map_node(&line.tokens[4], place),
            z0,
            td,
        }));
        Ok(())
    }

    fn wave(&self, spec: &str, place: &Place<'_>, loc: &Loc) -> Result<Wave, NetlistError> {
        if let Some(inner) = paren_inner(spec) {
            if spec.contains("PWL") || spec.contains("PWS") {
                return self.pwl(inner, place, loc);
            }
            if spec.contains("PULSE") {
                return self.pulse(inner, place, loc);
            }
            if spec.contains("SIN") {
                return self.sin(inner, place, loc);
            }
            return Err(NetlistError::at(
                loc,
                format!("unsupported waveform {spec}"),
            ));
        }
        let parts: Vec<&str> = spec.split_whitespace().collect();
        let expr = if parts.first().copied() == Some("DC") {
            parts.last().copied().unwrap_or("")
        } else {
            spec
        };
        if expr.is_empty() {
            return Err(NetlistError::at(loc, "source is missing a value"));
        }
        Ok(Wave::Dc(self.value(expr, place, loc)?))
    }

    /// `Function::parse_pwl`. A first time other than 0 is kept and also
    /// prepended at time 0 with the same amplitude, which is what JoSIM does
    /// after its initial-value warning.
    fn pwl(&self, inner: &str, place: &Place<'_>, loc: &Loc) -> Result<Wave, NetlistError> {
        let parts = pieces(inner);
        if parts.len() < 2 || !parts.len().is_multiple_of(2) {
            return Err(NetlistError::at(loc, "PWL needs time/value pairs"));
        }
        let mut points = Vec::new();
        let t0 = self.value(parts[0], place, loc)?;
        let v0 = self.value(parts[1], place, loc)?;
        if t0 != 0.0 {
            points.push((0.0, v0));
        }
        points.push((t0, v0));
        let mut index = 2;
        while index + 1 < parts.len() {
            let time = self.value(parts[index], place, loc)?;
            let value = self.value(parts[index + 1], place, loc)?;
            let Some(prev) = points.last().map(|point| point.0) else {
                return Err(NetlistError::at(loc, "PWL is empty"));
            };
            if time <= prev {
                return Err(NetlistError::at(loc, "PWL times must increase"));
            }
            points.push((time, value));
            index += 2;
        }
        Ok(Wave::Pwl(points))
    }

    /// `Function::parse_pulse`. Missing delay is 0. Missing rise, fall, width,
    /// and period copy the transient step or stop.
    fn pulse(&self, inner: &str, place: &Place<'_>, loc: &Loc) -> Result<Wave, NetlistError> {
        let parts = pieces(inner);
        if parts.len() < 2 || parts.len() > 7 {
            return Err(NetlistError::at(loc, "PULSE needs 2 to 7 values"));
        }
        let v1 = self.value(parts[0], place, loc)?;
        let v2 = self.value(parts[1], place, loc)?;
        let td = self.wave_arg(&parts, 2, place, loc, |_| 0.0)?;
        let tr = self.wave_arg(&parts, 3, place, loc, |tran| tran.tstep)?;
        let tf = self.wave_arg(&parts, 4, place, loc, |tran| tran.tstep)?;
        let pw = self.wave_arg(&parts, 5, place, loc, |tran| tran.tstop)?;
        let per = self.wave_arg(&parts, 6, place, loc, |tran| tran.tstop)?;
        Ok(Wave::Pulse {
            v1,
            v2,
            td,
            tr,
            tf,
            pw,
            per,
        })
    }

    fn wave_arg(
        &self,
        parts: &[&str],
        index: usize,
        place: &Place<'_>,
        loc: &Loc,
        default: impl Fn(&Tran) -> f64,
    ) -> Result<f64, NetlistError> {
        if let Some(part) = parts.get(index) {
            return self.value(part, place, loc);
        }
        if index == 2 {
            return Ok(0.0);
        }
        let Some(tran) = self.tran.as_ref() else {
            return Err(NetlistError::at(loc, "PULSE defaults need a .tran card"));
        };
        Ok(default(tran))
    }

    fn sin(&self, inner: &str, place: &Place<'_>, loc: &Loc) -> Result<Wave, NetlistError> {
        let parts = pieces(inner);
        if parts.len() < 2 || parts.len() > 5 {
            return Err(NetlistError::at(
                loc,
                "SIN needs offset, amplitude, and optionally frequency, delay, and theta",
            ));
        }
        let vo = self.value(parts[0], place, loc)?;
        let va = self.value(parts[1], place, loc)?;
        let freq = if parts.len() >= 3 {
            self.value(parts[2], place, loc)?
        } else {
            let stop =
                self.tran.as_ref().map(|tran| tran.tstop).ok_or_else(|| {
                    NetlistError::at(loc, "SIN frequency defaults from .tran stop")
                })?;
            if stop == 0.0 { 0.0 } else { 1.0 / stop }
        };
        let td = if parts.len() >= 4 {
            self.value(parts[3], place, loc)?
        } else {
            0.0
        };
        let theta = if parts.len() >= 5 {
            self.value(parts[4], place, loc)?
        } else {
            0.0
        };
        Ok(Wave::Sin {
            vo,
            va,
            freq,
            td,
            theta,
        })
    }

    fn value(&self, expr: &str, place: &Place<'_>, loc: &Loc) -> Result<f64, NetlistError> {
        self.params.eval(expr, place.scope, loc)
    }

    fn claim(
        &self,
        name: &str,
        place: &Place<'_>,
        loc: &Loc,
        seen: &mut BTreeSet<String>,
    ) -> Result<String, NetlistError> {
        let name = map_name(name, place.path);
        if !seen.insert(name.clone()) {
            return Err(NetlistError::at(loc, format!("duplicate device {name}")));
        }
        Ok(name)
    }

    fn find_model<'a>(
        &'a self,
        name: &str,
        scope: Option<&str>,
        loc: &Loc,
    ) -> Result<&'a Model, NetlistError> {
        if let Some(scope) = scope
            && let Some(model) = self
                .models
                .iter()
                .find(|model| model.name == name && model.scope.as_deref() == Some(scope))
        {
            return Ok(model);
        }
        if let Some(model) = self
            .models
            .iter()
            .find(|model| model.name == name && model.scope.is_none())
        {
            return Ok(model);
        }
        Err(NetlistError::at(loc, format!("unknown model {name}")))
    }
}

fn bcs_rn(model: &Model, ic: f64, loc: &Loc) -> Result<f64, NetlistError> {
    // JoSIM JJ::set_model temperature branch, after Ic has been scaled.
    if model.tc == 0.0 || model.t == 0.0 || ic == 0.0 {
        return Err(NetlistError::at(
            loc,
            "temperature-dependent Rn is undefined",
        ));
    }
    let del0 = 1.76 * expr::boltzmann() * model.tc;
    let ratio = (model.t / model.tc) * (model.t / model.tc);
    let inside = (expr::pi() / 2.0 * ratio).cos();
    if inside < 0.0 {
        return Err(NetlistError::at(loc, "temperature is above Tc"));
    }
    let del = del0 * inside.sqrt();
    let rn = (expr::pi() * del) / (2.0 * expr::ev() * ic)
        * (del / (2.0 * expr::boltzmann() * model.t)).tanh();
    if rn.is_finite() {
        Ok(rn)
    } else {
        Err(NetlistError::at(
            loc,
            "temperature-dependent Rn is not finite",
        ))
    }
}

fn map_name(name: &str, path: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{name}|{path}")
    }
}

fn map_node(node: &str, place: &Place<'_>) -> String {
    if node == "0" || node == "GND" {
        return node.to_string();
    }
    if let Some(index) = place.pins.iter().position(|pin| pin == node)
        && let Some(parent) = place.parent.get(index)
    {
        return parent.clone();
    }
    map_name(node, place.path)
}

fn paren_inner(spec: &str) -> Option<&str> {
    let open = spec.find('(')?;
    let close = spec.rfind(')')?;
    if close <= open {
        return None;
    }
    Some(&spec[open + 1..close])
}

fn pieces(inner: &str) -> Vec<&str> {
    inner
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .filter(|part| !part.is_empty())
        .collect()
}
