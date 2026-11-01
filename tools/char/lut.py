"""Assemble a cell LUT and measure held-out interpolation error.

SPEC §3 and §4.2. Grid coordinates are integers: ``bias_frac`` in micro-units
(10**-6) and ``*_fs`` axes in femtoseconds. Multilinear weights use float.
The returned sample is rounded half away from zero to 1 fs.
"""

from __future__ import annotations

import hashlib
import itertools
import json
import math
import random
import sys
from collections.abc import Mapping, Sequence
from decimal import ROUND_HALF_UP, Decimal
from pathlib import Path
from typing import Any

import yaml

from bench import BENCHES, LIB, render
from sweep import (
    OUT,
    SETTLED_DT_PREV_FS,
    SWEEPS,
    _append_jsonl,
    _eval_job,
    _eval_with,
    _load_cache,
    cache_key,
    characterized_inputs,
    patch_deck,
    pool_map,
    schedule_inputs,
    simulate_deck,
    stop_fs,
    worker_count,
)
from windows import WindowError, _load_bench, _probe, _probe_request, bisect_job

ROOT = Path(__file__).resolve().parents[2]
SCHEMA = ROOT / "schemas" / "lut.schema.json"
FIXTURE = ROOT / "tests" / "fixtures"

CHARACTERIZED = (
    "AND",
    "DCSFQ",
    "DFF",
    "JTL",
    "MERGE",
    "NDRO",
    "NOT",
    "OR",
    "SFQDC",
    "SPLIT",
    "XOR",
)
AXIS_ORDER = ("bias_frac", "dt_prev_fs", "data_clock_offset_fs", "in_in_offset_fs")
NOMINAL_UNITS = {
    "bias_frac": 1_000_000,
    "dt_prev_fs": SETTLED_DT_PREV_FS,
    "data_clock_offset_fs": 0,
    "in_in_offset_fs": 0,
}
HELD_OUT = 50
DRAW_LIMIT = 200
TABLE_COLUMNS = (
    "cell",
    "entry",
    "kind",
    "samples",
    "max_err_fs",
    "mean_err_fs",
    "max_pct_nominal",
    "mean_pct_nominal",
    "pass_rate",
    "note",
)


class LutError(RuntimeError):
    """A LUT grid, lookup, or held-out sample cannot be built."""


def round_fs(value: float) -> int:
    """Nearest integer femtosecond, half away from zero."""
    if not math.isfinite(value):
        raise LutError("non-finite femtosecond sample")
    if value >= 0:
        return int(math.floor(value + 0.5))
    return int(math.ceil(value - 0.5))


def round_decimal(value: Decimal) -> int:
    """Nearest integer, half away from zero."""
    return int(value.to_integral_value(rounding=ROUND_HALF_UP))


def parse_units(name: str, text: str) -> int:
    """Axis text to an integer coordinate."""
    if name == "bias_frac":
        return round_decimal(Decimal(text) * Decimal(1_000_000))
    if name.endswith("_fs"):
        value = Decimal(text)
        if value != value.to_integral_value():
            raise LutError(f"{name} point {text} is not an integer femtosecond count")
        return int(value)
    raise LutError(f"unknown axis {name}")


def format_units(name: str, units: int) -> str:
    """Inverse of [`parse_units`] for a coordinate this module created."""
    if name == "bias_frac":
        sign = "-" if units < 0 else ""
        magnitude = abs(units)
        whole, frac = divmod(magnitude, 1_000_000)
        if frac == 0:
            return f"{sign}{whole}"
        digits = f"{frac:06d}".rstrip("0")
        return f"{sign}{whole}.{digits}"
    return str(units)


def margin_fs(nominal_delay_fs: int) -> int:
    """Bracket half-width: max(20 fs, 2% of the nominal delay)."""
    percent = round_decimal(Decimal(abs(nominal_delay_fs)) * Decimal("0.02"))
    return max(20, percent)


def bracket_ok(*, low: str, high: str) -> bool:
    """True when the low offset fails and the high offset passes."""
    return low == "fail" and high == "pass"


def offset_pair(predicted: int, margin: int) -> tuple[int, int]:
    """Stimulus offsets for a window whose stored value is itself an offset."""
    return predicted - margin, predicted + margin


def gap_within_margin(predicted: int, measured: int | None, margin: int) -> bool:
    """A junction-gap window passes when the bisected gap is within ``margin``."""
    if measured is None:
        return False
    return abs(measured - predicted) <= margin


def bracket_plan(window: Mapping[str, Any], predicted: int, margin: int) -> tuple[Any, ...]:
    """How to check one interpolated window.

    Windows without ``measure`` store the first passing stimulus offset, so
    two probes at ``w±margin`` match that edge. Windows with ``measure`` store
    the junction gap at that offset, and the check is the bisected gap.
    """
    if window.get("measure") is None:
        low, high = offset_pair(predicted, margin)
        return ("offset", low, high)
    return ("gap",)


def lookup(bundle: Mapping[str, Any], name: str, point: Mapping[str, str]) -> tuple[int, bool]:
    """Interpolate one entry. Outside the valid box, clamp and return in_valid false."""
    try:
        entry = bundle["entries"][name]
    except KeyError as exc:
        raise LutError(f"unknown LUT entry {name}") from exc
    query: list[int] = []
    grids: list[list[int]] = []
    in_valid = True
    for axis in entry["axes"]:
        axis_name = axis["name"]
        if axis_name not in point:
            raise LutError(f"LUT point is missing axis {axis_name}")
        raw = parse_units(axis_name, point[axis_name])
        bound = entry["valid"][axis_name]
        lo = parse_units(axis_name, bound["lo"])
        hi = parse_units(axis_name, bound["hi"])
        coord = raw
        if coord < lo or coord > hi:
            in_valid = False
            coord = min(hi, max(lo, coord))
        grids.append([parse_units(axis_name, text) for text in axis["points"]])
        query.append(coord)
    extra = set(point) - {axis["name"] for axis in entry["axes"]}
    if extra:
        raise LutError(f"LUT point has an extra axis {sorted(extra)[0]}")
    return round_fs(_interpolate(grids, entry["values"], query)), in_valid


def validate(bundle: Mapping[str, Any]) -> None:
    """Reject a document that does not match ``schemas/lut.schema.json``."""
    import jsonschema

    schema = json.loads(SCHEMA.read_text(encoding="utf-8"))
    validator = jsonschema.Draft7Validator(schema)
    errors = sorted(validator.iter_errors(bundle), key=lambda err: list(err.absolute_path))
    if errors:
        raise LutError(errors[0].message)


def assemble(
    cell: str,
    delay_rows: Sequence[Mapping[str, Any]],
    window_rows: Sequence[Mapping[str, Any]],
    *,
    source: Mapping[str, Any],
) -> dict[str, Any]:
    """Build one bundle. A missing nominal point is an error."""
    entries: dict[str, dict[str, Any]] = {}
    if delay_rows:
        axes = _active_axes(delay_rows)
        levels, labels = _levels(delay_rows, axes)
        samples = _delay_samples(delay_rows, axes)
        nominal = tuple(NOMINAL_UNITS[axis] for axis in axes)
        # Nearest-input relabels that never occur at the nominal point are not
        # entries. If none of the measured arcs occur there, the cell has no
        # characterized delay and that is an error.
        present = {name: points for name, points in samples.items() if nominal in points}
        if samples and not present:
            missing = ", ".join(sorted(samples))
            raise LutError(f"{missing} is missing its nominal point")
        for name, points in present.items():
            entries[name] = _entry(name, axes, levels, labels, points)
    for name, samples in _window_samples(window_rows).items():
        axes = ["bias_frac"]
        levels, labels = _levels(window_rows, axes)
        # Levels are per window name, not every window of the cell.
        owned = [row for row in window_rows if str(row["window"]) == name]
        levels, labels = _levels(owned, axes)
        entries[name] = _entry(name, axes, levels, labels, samples)
    bundle = {"cell": cell, "entries": entries, "source": dict(source)}
    validate(bundle)
    return bundle


def write_cbor(bundle: Mapping[str, Any], path: Path | str) -> None:
    """Validate, then write CBOR with the schema's field names."""
    import cbor2

    validate(bundle)
    destination = Path(path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(cbor2.dumps(bundle))


def load_cbor(path: Path | str) -> dict[str, Any]:
    """Read a bundle written by [`write_cbor`]."""
    import cbor2

    loaded = cbor2.loads(Path(path).read_bytes())
    if not isinstance(loaded, dict):
        raise LutError("LUT CBOR root is not a map")
    return loaded


def format_heldout(rows: Sequence[Mapping[str, Any]]) -> str:
    """Tab-separated held-out table. The note is the last column."""
    lines = ["\t".join(TABLE_COLUMNS)]
    for row in rows:
        lines.append("\t".join(_table_cell(row.get(name, "")) for name in TABLE_COLUMNS))
    return "\n".join(lines) + "\n"


def write_sample_fixture(directory: Path | str | None = None) -> None:
    """Write the 100-point cross-language fixture."""
    destination = FIXTURE if directory is None else Path(directory)
    bundle = _fixture_bundle()
    validate(bundle)
    points = _fixture_points(bundle)
    destination.mkdir(parents=True, exist_ok=True)
    write_cbor(bundle, destination / "sample.lut.cbor")
    text = json.dumps(points, indent=2) + "\n"
    (destination / "sample.lut.points.json").write_text(text, encoding="utf-8")


def build_cell(cell: str, out_dir: Path | None = None) -> dict[str, Any]:
    """Assemble one cell from its sweep and window parquet files."""
    directory = (OUT if out_dir is None else Path(out_dir)) / cell
    delay_path = directory / "results.parquet"
    window_path = directory / "windows.parquet"
    if not delay_path.is_file() or not window_path.is_file():
        raise LutError(f"{cell} is missing results.parquet or windows.parquet")
    delay_rows = _read_parquet(delay_path)
    window_rows = _read_parquet(window_path)
    versions = {str(row["josim_version"]) for row in delay_rows}
    if len(versions) != 1:
        raise LutError(f"{cell} sweep has {len(versions)} JoSIM versions")
    cell_def = json.loads((LIB / f"{cell}.cell.json").read_text(encoding="utf-8"))
    netlist = ROOT / str(cell_def["netlist"])
    source = {
        "josim_version": versions.pop(),
        "netlist_sha256": hashlib.sha256(netlist.read_bytes()).hexdigest(),
        "sweep_sha256": hashlib.sha256((SWEEPS / f"{cell}.yaml").read_bytes()).hexdigest(),
        "timestep_fs": _timestep_fs(cell),
    }
    return assemble(cell, delay_rows, window_rows, source=source)


def heldout_rows(
    cell: str,
    bundle: Mapping[str, Any],
    *,
    out_dir: Path | None = None,
    workers: int | None = None,
) -> list[dict[str, Any]]:
    """50 off-grid delay points, and the window bracket plus a short bisection."""
    directory = OUT if out_dir is None else Path(out_dir)
    count = worker_count() if workers is None else workers
    nominal_delay = _nominal_delay(cell, bundle)
    rows: list[dict[str, Any]] = []
    rows.extend(_delay_heldout(cell, bundle, directory, count, nominal_delay))
    rows.extend(_window_heldout(cell, bundle, directory, count, nominal_delay))
    return rows


def point_cell(cell: str) -> None:
    """Record the bundle path on a characterized cell. TFF is left alone."""
    path = LIB / f"{cell}.cell.json"
    text = path.read_text(encoding="utf-8")
    new = f'"lut": "lib/families/rsfq/{cell}.lut.cbor"'
    old = '"lut": null'
    if new in text:
        return
    if old not in text:
        raise LutError(f"{cell} has no lut field to set")
    path.write_text(text.replace(old, new, 1), encoding="utf-8")


def main(argv: Sequence[str] | None = None) -> None:
    """Write every characterized bundle, its cell path, and the held-out table."""
    args = list(sys.argv[1:] if argv is None else argv)
    if args == ["--fixture"]:
        write_sample_fixture()
        print(FIXTURE / "sample.lut.cbor", flush=True)
        return
    cells = list(args) if args else list(CHARACTERIZED)
    table: list[dict[str, Any]] = []
    for cell in cells:
        print(f"{cell}: assemble", flush=True)
        bundle = build_cell(cell)
        destination = LIB / f"{cell}.lut.cbor"
        write_cbor(bundle, destination)
        point_cell(cell)
        print(f"{cell}: held-out", flush=True)
        table.extend(heldout_rows(cell, bundle))
        print(destination, flush=True)
    report = LIB / "lut_heldout.txt"
    report.write_text(format_heldout(table), encoding="utf-8")
    print(report, flush=True)


def _interpolate(axes: Sequence[Sequence[int]], values: Sequence[int], query: Sequence[int]) -> float:
    def rec(dim: int, base: int) -> float:
        points = axes[dim]
        stride = math.prod(len(axis) for axis in axes[dim + 1 :])
        if len(points) == 1:
            if dim + 1 == len(axes):
                return float(values[base])
            return rec(dim + 1, base)
        index, weight = _bracket(points, query[dim])
        left_base = base + index * stride
        right_base = base + (index + 1) * stride
        if dim + 1 == len(axes):
            left = float(values[left_base])
            right = float(values[right_base])
        else:
            left = rec(dim + 1, left_base)
            right = rec(dim + 1, right_base)
        return (1.0 - weight) * left + weight * right

    return rec(0, 0)


def _bracket(points: Sequence[int], query: int) -> tuple[int, float]:
    if query <= points[0]:
        return 0, 0.0
    last = len(points) - 1
    if query >= points[last]:
        return last - 1, 1.0
    lo = 0
    hi = last
    while hi - lo > 1:
        mid = (lo + hi) // 2
        if points[mid] <= query:
            lo = mid
        else:
            hi = mid
    if query == points[lo]:
        return lo, 0.0
    if query == points[hi]:
        return lo, 1.0
    span = points[hi] - points[lo]
    return lo, (query - points[lo]) / span


def _active_axes(rows: Sequence[Mapping[str, Any]]) -> list[str]:
    found = [name for name in AXIS_ORDER if any(row.get(name) is not None for row in rows)]
    if "bias_frac" not in found or "dt_prev_fs" not in found:
        raise LutError("delay rows are missing bias_frac or dt_prev_fs")
    return found


def _levels(
    rows: Sequence[Mapping[str, Any]],
    axes: Sequence[str],
) -> tuple[dict[str, list[int]], dict[str, dict[int, str]]]:
    units = {name: set() for name in axes}
    labels: dict[str, dict[int, str]] = {name: {} for name in axes}
    for row in rows:
        for name in axes:
            if row.get(name) is None:
                continue
            unit, text = _coord_text(name, row[name])
            units[name].add(unit)
            labels[name].setdefault(unit, text)
    return {name: sorted(units[name]) for name in axes}, labels


def _coord_text(name: str, value: Any) -> tuple[int, str]:
    if name == "bias_frac":
        text = format(Decimal(str(value)), "f")
        return parse_units(name, text), text
    unit = int(value)
    return unit, str(unit)


def _delay_samples(
    rows: Sequence[Mapping[str, Any]],
    axes: Sequence[str],
) -> dict[str, dict[tuple[int, ...], list[int]]]:
    found: dict[str, dict[tuple[int, ...], list[int]]] = {}
    for row in rows:
        if row.get("outcome") != "expected":
            continue
        coord = tuple(_coord_text(name, row[name])[0] for name in axes)
        for arc in _arcs(row):
            name = _arc_name(arc)
            found.setdefault(name, {}).setdefault(coord, []).append(int(arc["delay_fs"]))
    return found


def _window_samples(
    rows: Sequence[Mapping[str, Any]],
) -> dict[str, dict[tuple[int, ...], list[int]]]:
    found: dict[str, dict[tuple[int, ...], list[int]]] = {}
    for row in rows:
        if row.get("bracket") != "bracketed" or row.get("window_fs") is None:
            continue
        unit, _text = _coord_text("bias_frac", row["bias_frac"])
        name = str(row["window"])
        found.setdefault(name, {}).setdefault((unit,), []).append(int(row["window_fs"]))
    return found


def _entry(
    name: str,
    axes: Sequence[str],
    levels: Mapping[str, Sequence[int]],
    labels: Mapping[str, Mapping[int, str]],
    samples: Mapping[tuple[int, ...], Sequence[int]],
) -> dict[str, Any]:
    medians = {coord: _integer_median(list(values)) for coord, values in samples.items()}
    nominal = tuple(NOMINAL_UNITS[axis] for axis in axes)
    if nominal not in medians:
        raise LutError(f"{name} is missing its nominal point")
    chosen = _rectangle([list(levels[axis]) for axis in axes], medians, nominal)
    points = [[labels[axis][unit] for unit in chosen[index]] for index, axis in enumerate(axes)]
    values: list[int] = []
    for combo in itertools.product(*points_as_ranges(chosen)):
        key = tuple(chosen[index][combo[index]] for index in range(len(axes)))
        values.append(medians[key])
    return {
        "axes": [{"name": axis, "points": points[index]} for index, axis in enumerate(axes)],
        "values": values,
        "interpolation": "multilinear",
        "valid": {
            axis: {"lo": points[index][0], "hi": points[index][-1]}
            for index, axis in enumerate(axes)
        },
        "sigma_fs": 0,
    }


def points_as_ranges(chosen: Sequence[Sequence[int]]) -> list[range]:
    return [range(len(axis)) for axis in chosen]


def _rectangle(
    levels: Sequence[Sequence[int]],
    samples: Mapping[tuple[int, ...], int],
    nominal: tuple[int, ...],
) -> list[list[int]]:
    indexes = []
    for axis, unit in zip(levels, nominal, strict=True):
        try:
            indexes.append(list(axis).index(unit))
        except ValueError as exc:
            raise LutError("nominal coordinate is not on the sweep axis") from exc
    ranges = []
    for axis, index in zip(levels, indexes, strict=True):
        ranges.append([(lo, hi) for lo in range(index + 1) for hi in range(index, len(axis))])
    best: list[list[int]] | None = None
    best_key: tuple[int, tuple[int, ...]] | None = None
    for combo in itertools.product(*ranges):
        chosen = [list(levels[index][lo : hi + 1]) for index, (lo, hi) in enumerate(combo)]
        if any(len(axis) == 0 for axis in chosen):
            continue
        if not _covered(chosen, samples):
            continue
        count = math.prod(len(axis) for axis in chosen)
        spans = tuple(axis[-1] - axis[0] for axis in chosen)
        key = (count, spans)
        if best_key is None or key > best_key:
            best_key = key
            best = chosen
    if best is None:
        raise LutError("no full rectangle contains the nominal point")
    return best


def _covered(chosen: Sequence[Sequence[int]], samples: Mapping[tuple[int, ...], int]) -> bool:
    return all(combo in samples for combo in itertools.product(*chosen))


def _integer_median(values: Sequence[int]) -> int:
    ordered = sorted(values)
    if not ordered:
        raise LutError("median of no samples")
    mid = len(ordered) // 2
    if len(ordered) % 2 == 1:
        return ordered[mid]
    return round_decimal(Decimal(ordered[mid - 1] + ordered[mid]) / Decimal(2))


def _arc_name(arc: Mapping[str, Any]) -> str:
    tag = str(arc.get("tag") or "")
    if tag:
        return f"t_{arc['src']}_{arc['dst']}_{tag}"
    return f"t_{arc['src']}_{arc['dst']}"


def _arcs(row: Mapping[str, Any]) -> list[dict[str, Any]]:
    arcs = row.get("arcs") or []
    if isinstance(arcs, str):
        arcs = json.loads(arcs)
    return list(arcs)


def _table_cell(value: Any) -> str:
    if value is None or value == "":
        return ""
    if isinstance(value, float):
        return f"{value:.3f}"
    return str(value)


def _timestep_fs(cell: str) -> int:
    bench = yaml.safe_load((BENCHES / f"{cell}.yaml").read_text(encoding="utf-8"))
    step = Decimal(str(bench.get("timestep_ps", "0.025")))
    femto = step * Decimal(1000)
    if femto != femto.to_integral_value():
        raise LutError(f"{cell} timestep is not an integer number of femtoseconds")
    return int(femto)


def _read_parquet(path: Path) -> list[dict[str, Any]]:
    import pyarrow.parquet as pq

    rows = pq.read_table(path).to_pylist()
    for row in rows:
        arcs = row.get("arcs")
        if isinstance(arcs, str):
            row["arcs"] = json.loads(arcs)
    return rows


def _is_window(entry: Mapping[str, Any]) -> bool:
    return [axis["name"] for axis in entry["axes"]] == ["bias_frac"]


def _nominal_point(entry: Mapping[str, Any]) -> dict[str, str]:
    point: dict[str, str] = {}
    for axis in entry["axes"]:
        name = axis["name"]
        unit = NOMINAL_UNITS[name]
        labels = {parse_units(name, text): text for text in axis["points"]}
        if unit not in labels:
            raise LutError(f"{name} nominal is not on the grid")
        point[name] = labels[unit]
    return point


def _nominal_delay(cell: str, bundle: Mapping[str, Any]) -> int:
    best: int | None = None
    for name, entry in bundle["entries"].items():
        if _is_window(entry):
            continue
        value, valid = lookup(bundle, name, _nominal_point(entry))
        if not valid:
            raise LutError(f"{cell} {name} nominal point is outside the valid box")
        if best is None or abs(value) > abs(best):
            best = value
    if best is None:
        raise LutError(f"{cell} has no delay entry")
    return best


def _entry_percent(err: float, nominal: int) -> float:
    if nominal == 0:
        raise LutError("nominal value is 0")
    return 100.0 * err / abs(nominal)


def _delay_heldout(
    cell: str,
    bundle: Mapping[str, Any],
    out_dir: Path,
    workers: int,
    nominal_delay: int,
) -> list[dict[str, Any]]:
    del nominal_delay
    delay_entries = {
        name: entry for name, entry in bundle["entries"].items() if not _is_window(entry)
    }
    bench = yaml.safe_load((BENCHES / f"{cell}.yaml").read_text(encoding="utf-8"))
    cell_def = json.loads((LIB / f"{cell}.cell.json").read_text(encoding="utf-8"))
    nominal_deck = render(BENCHES / f"{cell}.yaml")
    version = str(bundle["source"]["josim_version"])
    cache_path = out_dir / cell / "cache.jsonl"
    cache_path.parent.mkdir(parents=True, exist_ok=True)
    cache = _load_cache(cache_path)
    groups: dict[tuple[Any, ...], list[str]] = {}
    for name, entry in delay_entries.items():
        groups.setdefault(_box_key(entry), []).append(name)
    rows = []
    for names in groups.values():
        names.sort()
        entry = delay_entries[names[0]]
        rng = random.Random(f"fluxel-lut-{cell}-{names[0]}")
        errors = {name: [] for name in names}
        attempts = {name: 0 for name in names}
        while any(len(errors[name]) < HELD_OUT for name in names):
            short = [name for name in names if len(errors[name]) < HELD_OUT]
            if any(attempts[name] >= DRAW_LIMIT for name in short):
                raise LutError(f"{cell} {short} had fewer than {HELD_OUT} measurable points")
            budget = min(HELD_OUT - len(errors[name]) for name in short)
            budget = min(budget, min(DRAW_LIMIT - attempts[name] for name in short))
            drawn = _draw_units(entry, rng, budget)
            for name in names:
                attempts[name] += len(drawn)
            measured = _measure_delays(
                cell,
                cell_def,
                characterized_inputs(bench),
                nominal_deck,
                entry,
                drawn,
                version,
                cache,
                cache_path,
                workers,
            )
            for units, sample in zip(drawn, measured, strict=True):
                if sample is None:
                    continue
                for name in names:
                    value = sample.get(name)
                    if value is None or len(errors[name]) >= HELD_OUT:
                        continue
                    predicted, valid = lookup(bundle, name, _units_text(entry, units))
                    if not valid:
                        raise LutError(f"{cell} {name} held-out point fell outside the valid box")
                    errors[name].append(abs(value - predicted))
        for name in names:
            nominal, _valid = lookup(bundle, name, _nominal_point(delay_entries[name]))
            samples = errors[name]
            max_err = max(samples)
            mean_err = sum(samples) / len(samples)
            rows.append(
                {
                    "cell": cell,
                    "entry": name,
                    "kind": "delay",
                    "samples": len(samples),
                    "max_err_fs": max_err,
                    "mean_err_fs": mean_err,
                    "max_pct_nominal": _entry_percent(max_err, nominal),
                    "mean_pct_nominal": _entry_percent(mean_err, nominal),
                    "pass_rate": "",
                    "note": "",
                }
            )
    return rows


def _window_heldout(
    cell: str,
    bundle: Mapping[str, Any],
    out_dir: Path,
    workers: int,
    nominal_delay: int,
) -> list[dict[str, Any]]:
    windows = {name: entry for name, entry in bundle["entries"].items() if _is_window(entry)}
    if not windows:
        return []
    bench = _load_bench(cell)
    by_name = {str(window["name"]): window for window in bench["windows"]}
    nominal_deck = render(BENCHES / f"{cell}.yaml")
    version = str(bundle["source"]["josim_version"])
    cache_path = out_dir / cell / "window_cache.jsonl"
    cache = _load_cache(cache_path)
    margin = margin_fs(nominal_delay)
    anchor = int(bench["anchor_fs"])
    lead = int(bench["lead_fs"])
    resolution = int(bench["resolution_fs"])
    rows = []
    for name, entry in windows.items():
        template = by_name[name]
        rng = random.Random(f"fluxel-lut-{cell}-{name}")
        draws = _draw_units(entry, rng, HELD_OUT)
        predictions: list[int] = []
        biases: list[Decimal] = []
        for units in draws:
            text = _units_text(entry, units)
            predicted, valid = lookup(bundle, name, text)
            if not valid:
                raise LutError(f"{cell} {name} held-out bias fell outside the valid box")
            predictions.append(predicted)
            biases.append(Decimal(units["bias_frac"]) / Decimal(1_000_000))
        plan = bracket_plan(template, predictions[0], margin)
        if plan[0] == "offset":
            pairs: list[tuple[Decimal, int]] = []
            for bias, predicted in zip(biases, predictions, strict=True):
                low, high = offset_pair(predicted, margin)
                pairs.append((bias, low))
                pairs.append((bias, high))
            statuses = _offset_statuses(
                nominal_deck, template, anchor, lead, pairs, version, cache, cache_path, workers
            )
            flags = [
                bracket_ok(low=statuses[2 * index], high=statuses[2 * index + 1])
                for index in range(len(draws))
            ]
            found_for = None
        else:
            found_for = _bisect_many(
                cell,
                template,
                anchor,
                lead,
                resolution,
                biases,
                nominal_deck,
                version,
                cache,
                cache_path,
                workers,
            )
            flags = [
                gap_within_margin(predicted, found, margin)
                for predicted, found in zip(predictions, found_for, strict=True)
            ]
        misses = [index for index, flag in enumerate(flags) if not flag]
        passes = [index for index, flag in enumerate(flags) if flag]
        rng.shuffle(passes)
        chosen = misses + passes[:10]
        bisected: list[int] = []
        unbracketed = 0
        if found_for is None:
            found_windows = _bisect_many(
                cell,
                template,
                anchor,
                lead,
                resolution,
                [biases[index] for index in chosen],
                nominal_deck,
                version,
                cache,
                cache_path,
                workers,
            )
        else:
            found_windows = [found_for[index] for index in chosen]
        for index, found in zip(chosen, found_windows, strict=True):
            if found is None:
                unbracketed += 1
                continue
            bisected.append(abs(found - predictions[index]))
        nominal, _valid = lookup(bundle, name, _nominal_point(entry))
        if bisected:
            max_err = max(bisected)
            mean_err = sum(bisected) / len(bisected)
            max_pct = _entry_percent(max_err, nominal)
            mean_pct = _entry_percent(mean_err, nominal)
        else:
            max_err = ""
            mean_err = ""
            max_pct = ""
            mean_pct = ""
        note = "window error resolution 10 fs"
        if unbracketed:
            note = f"{note}; {unbracketed} unbracketed"
        rows.append(
            {
                "cell": cell,
                "entry": name,
                "kind": "window",
                "samples": len(bisected),
                "max_err_fs": max_err,
                "mean_err_fs": mean_err,
                "max_pct_nominal": max_pct,
                "mean_pct_nominal": mean_pct,
                "pass_rate": f"{sum(flags)}/{HELD_OUT}",
                "note": note,
            }
        )
    return rows


def _box_key(entry: Mapping[str, Any]) -> tuple[Any, ...]:
    return tuple(
        (axis["name"], tuple(axis["points"]), entry["valid"][axis["name"]]["lo"], entry["valid"][axis["name"]]["hi"])
        for axis in entry["axes"]
    )


def _draw_units(entry: Mapping[str, Any], rng: random.Random, count: int = 1) -> list[dict[str, int]]:
    grids = {
        axis["name"]: {parse_units(axis["name"], text) for text in axis["points"]}
        for axis in entry["axes"]
    }
    bounds = {}
    for axis in entry["axes"]:
        name = axis["name"]
        bounds[name] = (
            parse_units(name, entry["valid"][name]["lo"]),
            parse_units(name, entry["valid"][name]["hi"]),
        )
    if all(lo == hi for lo, hi in bounds.values()):
        raise LutError("valid box has no off-grid point")
    found: list[dict[str, int]] = []
    seen: set[tuple[int, ...]] = set()
    guard = 0
    while len(found) < count:
        guard += 1
        if guard > 100_000:
            raise LutError("could not draw an off-grid point")
        point = {name: rng.randint(lo, hi) for name, (lo, hi) in bounds.items()}
        key = tuple(point[axis["name"]] for axis in entry["axes"])
        if key in seen or all(point[name] in grids[name] for name in point):
            continue
        seen.add(key)
        found.append(point)
    return found


def _units_text(entry: Mapping[str, Any], units: Mapping[str, int]) -> dict[str, str]:
    return {axis["name"]: format_units(axis["name"], units[axis["name"]]) for axis in entry["axes"]}


def _measure_delays(
    cell: str,
    cell_def: Mapping[str, Any],
    inputs: Mapping[str, Any],
    nominal_deck: str,
    entry: Mapping[str, Any],
    drawn: Sequence[Mapping[str, int]],
    version: str,
    cache: dict[str, dict[str, Any]],
    cache_path: Path,
    workers: int,
) -> list[dict[str, int] | None]:
    pending: list[dict[str, Any]] = []
    rows: list[dict[str, Any] | None] = [None] * len(drawn)
    for index, units in enumerate(drawn):
        params: dict[str, Any] = {}
        for axis in entry["axes"]:
            name = axis["name"]
            if name == "bias_frac":
                params[name] = Decimal(units[name]) / Decimal(1_000_000)
            else:
                params[name] = int(units[name])
        if "dt_prev_fs" not in params:
            params["dt_prev_fs"] = SETTLED_DT_PREV_FS
        stimulus = schedule_inputs(inputs, params)
        deck = patch_deck(nominal_deck, params["bias_frac"], stimulus, stop_fs(stimulus))
        key = cache_key(deck, params, version)
        hit = cache.get(key)
        if hit is not None:
            rows[index] = hit
            continue
        pending.append(
            {
                "index": index,
                "key": key,
                "cell": cell,
                "cell_def": dict(cell_def),
                "stimulus": stimulus,
                "deck": deck,
                "params": {name: _param_text(value) for name, value in params.items()},
                "josim_version": version,
            }
        )

    def save(row: dict[str, Any]) -> None:
        stored = {name: value for name, value in row.items() if name != "index"}
        if stored["key"] not in cache:
            _append_jsonl(cache_path, stored)
            cache[stored["key"]] = stored
        rows[int(row["index"])] = stored

    if pending:
        if workers == 1:
            for job in pending:
                save(_eval_with(job, simulate_deck))
        else:
            pool_map(_eval_job, pending, workers, on_result=save)
    return [_median_arcs(row) for row in rows]


def _median_arcs(row: Mapping[str, Any] | None) -> dict[str, int] | None:
    if row is None or row.get("outcome") != "expected":
        return None
    found: dict[str, list[int]] = {}
    for arc in _arcs(row):
        found.setdefault(_arc_name(arc), []).append(int(arc["delay_fs"]))
    if not found:
        return None
    return {name: _integer_median(values) for name, values in found.items()}


def _param_text(value: Any) -> Any:
    if isinstance(value, Decimal):
        return format(value, "f")
    return value


def _offset_statuses(
    nominal_deck: str,
    window: Mapping[str, Any],
    anchor: int,
    lead: int,
    pairs: Sequence[tuple[Decimal, int]],
    version: str,
    cache: dict[str, dict[str, Any]],
    cache_path: Path,
    workers: int,
) -> list[str]:
    statuses = [""] * len(pairs)
    pending: list[dict[str, Any]] = []
    for index, (bias, offset) in enumerate(pairs):
        try:
            _deck, _params, key = _probe_request(
                nominal_deck, bias, window, anchor, lead, offset, version
            )
        except WindowError:
            statuses[index] = "illegal"
            continue
        hit = cache.get(key)
        if hit is not None:
            statuses[index] = _probe_label(hit)
            continue
        pending.append(
            {
                "index": index,
                "key": key,
                "nominal": nominal_deck,
                "bias": bias,
                "window": window,
                "anchor": anchor,
                "lead": lead,
                "offset": offset,
                "version": version,
            }
        )
    if pending:
        if workers == 1:
            for job in pending:
                statuses[int(job["index"])] = _run_probe(job, cache, cache_path)
        else:
            # Each worker simulates. The parent writes the shared cache.
            for job in pending:
                job["cache"] = {}
            found = pool_map(_probe_job, pending, workers)
            for row in found:
                hit = row["hit"]
                if hit["key"] not in cache:
                    _append_jsonl(cache_path, hit)
                    cache[hit["key"]] = hit
                statuses[int(row["index"])] = _probe_label(hit)
    return statuses


def _probe_job(job: Mapping[str, Any]) -> dict[str, Any]:
    """One uncached window offset. Picklable for the process pool."""
    fresh: list[dict[str, Any]] = []
    _probe(
        str(job["nominal"]),
        job["bias"],
        job["window"],
        int(job["anchor"]),
        int(job["lead"]),
        int(job["offset"]),
        str(job["version"]),
        {},
        fresh,
    )
    if not fresh:
        raise LutError("window probe was already cached in a worker")
    return {"index": int(job["index"]), "hit": fresh[0]}


def _run_probe(job: Mapping[str, Any], cache: dict[str, dict[str, Any]], cache_path: Path) -> str:
    fresh: list[dict[str, Any]] = []
    _probe(
        str(job["nominal"]),
        job["bias"],
        job["window"],
        int(job["anchor"]),
        int(job["lead"]),
        int(job["offset"]),
        str(job["version"]),
        cache,
        fresh,
    )
    if fresh:
        _append_jsonl(cache_path, fresh[0])
    return _probe_label(cache[str(job["key"])])


def _probe_label(hit: Mapping[str, Any]) -> str:
    status = str(hit["outcome"])
    if status in {"pass", "fail"}:
        return status
    return "undelivered"


def _bisect_many(
    cell: str,
    window: Mapping[str, Any],
    anchor: int,
    lead: int,
    resolution: int,
    biases: Sequence[Decimal],
    nominal_deck: str,
    version: str,
    cache: dict[str, dict[str, Any]],
    cache_path: Path,
    workers: int,
) -> list[int | None]:
    if not biases:
        return []
    jobs = [
        {
            "index": index,
            "cell": cell,
            "window": window,
            "anchor_fs": anchor,
            "lead_fs": lead,
            "resolution_fs": resolution,
            "bias": bias,
            "nominal": nominal_deck,
            "josim_version": version,
            "cache": dict(cache),
        }
        for index, bias in enumerate(biases)
    ]
    if workers == 1 or len(jobs) == 1:
        found = [bisect_job(job) for job in jobs]
    else:
        found = pool_map(bisect_job, jobs, workers)
    found.sort(key=lambda row: int(row["index"]))
    values: list[int | None] = []
    for row in found:
        for probe in row.pop("probes", []):
            if probe["key"] not in cache:
                cache[probe["key"]] = probe
                _append_jsonl(cache_path, probe)
        if row.get("bracket") != "bracketed" or row.get("window_fs") is None:
            values.append(None)
        else:
            values.append(int(row["window_fs"]))
    return values


def _fixture_bundle() -> dict[str, Any]:
    return {
        "cell": "SAMPLE",
        "entries": {
            "t_a_q": {
                "axes": [
                    {"name": "bias_frac", "points": ["0.80", "1.00", "1.20"]},
                    {"name": "dt_prev_fs", "points": ["6000", "200000"]},
                ],
                "values": [1000, 3000, 2000, 4000, 5000, 7000],
                "interpolation": "multilinear",
                "valid": {
                    "bias_frac": {"lo": "0.80", "hi": "1.20"},
                    "dt_prev_fs": {"lo": "6000", "hi": "200000"},
                },
                "sigma_fs": 0,
            }
        },
        "source": {
            "josim_version": "fixture",
            "netlist_sha256": "ab" * 32,
            "sweep_sha256": "cd" * 32,
            "timestep_fs": 25,
        },
    }


def _fixture_points(bundle: Mapping[str, Any]) -> list[dict[str, Any]]:
    seeds = [
        {"bias_frac": "0.80", "dt_prev_fs": "6000"},
        {"bias_frac": "1.00", "dt_prev_fs": "6000"},
        {"bias_frac": "1.20", "dt_prev_fs": "200000"},
        {"bias_frac": "0.90", "dt_prev_fs": "103000"},
        {"bias_frac": "1.00", "dt_prev_fs": "103000"},
        {"bias_frac": "0.50", "dt_prev_fs": "6000"},
        {"bias_frac": "1.50", "dt_prev_fs": "200000"},
        {"bias_frac": "1.00", "dt_prev_fs": "300000"},
        {"bias_frac": "0.40", "dt_prev_fs": "1"},
        {"bias_frac": "0.80", "dt_prev_fs": "0"},
    ]
    rng = random.Random(0)
    while len(seeds) < 100:
        bias = rng.randrange(400_000, 1_600_001)
        dt = rng.randrange(0, 400_001)
        seeds.append({"bias_frac": format_units("bias_frac", bias), "dt_prev_fs": str(dt)})
    points = []
    for point in seeds[:100]:
        fs, valid = lookup(bundle, "t_a_q", point)
        points.append({"entry": "t_a_q", "point": point, "fs": fs, "in_valid": valid})
    return points


def _cli() -> None:
    """Re-import so process-pool workers resolve jobs on this module."""
    import multiprocessing

    multiprocessing.freeze_support()
    parent = str(Path(__file__).resolve().parent)
    if parent not in sys.path:
        sys.path.insert(0, parent)
    import lut as lut_module

    lut_module.main()


if __name__ == "__main__":
    _cli()
