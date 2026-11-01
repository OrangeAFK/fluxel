"""Functional comparison of fx and PyLSE pulse logs.

Timestamps are not compared. Clocked designs contribute one count per clock
window. Unclocked designs contribute one count per output per stimulus epoch.
An epoch is the half-open span from one stimulus time to the next. The last
epoch is every pulse after the last stimulus. ``L`` is the longest
combinational path. A stimulus gap shorter than ``2L`` cannot separate those
epochs, so that case is a stimulus error. A later arrival still counts in the
epoch it falls in: PyLSE's constant delays are not bounded by ``L``.
"""

from __future__ import annotations

import json
from decimal import ROUND_HALF_UP, Decimal
from pathlib import Path

from design_to_pylse import CLOCKED

NOMINAL_BIAS = 1_000_000


class StimulusSpacingError(ValueError):
    """Input pulses are closer than twice the longest path, or a pulse has no epoch."""


class PathError(ValueError):
    """The combinational graph has a cycle or a missing arc bound."""


def output_drivers(design: dict) -> dict[str, str]:
    """Map each primary output port to the ``instance.port`` that drives it."""
    wanted = set(design["ports"]["out"])
    found: dict[str, str] = {}
    for net in design["nets"]:
        sink = net["sink"]
        if "instance" in sink or sink["port"] not in wanted:
            continue
        driver = net["driver"]
        if "instance" not in driver:
            raise ValueError(f"output {sink['port']} is driven by a port")
        found[sink["port"]] = f"{driver['instance']}.{driver['port']}"
    missing = wanted - set(found)
    if missing:
        raise ValueError(f"outputs without drivers: {sorted(missing)}")
    return found


def is_clocked(design: dict) -> bool:
    return any(instance["cell"] in CLOCKED for instance in design["instances"].values())


def cycle_counts(times_fs: list[int], clocks_fs: list[int]) -> list[int]:
    """Count pulses in ``(clk_k, clk_{k+1}]`` and one period after the last edge."""
    clocks = sorted(clocks_fs)
    if not clocks:
        raise ValueError("clocked design has no clk pulses")
    period = clocks[-1] - clocks[-2] if len(clocks) > 1 else 1_000_000
    edges = [*clocks, clocks[-1] + period]
    counts = []
    for index in range(len(edges) - 1):
        start, stop = edges[index], edges[index + 1]
        counts.append(sum(1 for time in times_fs if start < time <= stop))
    return counts


def load_cells(lib_dir: Path) -> dict[str, dict]:
    """Cell definitions with ``arc_fs`` bounds for every fired delay."""
    import cbor2

    cells: dict[str, dict] = {}
    for path in sorted(lib_dir.glob("*.cell.json")):
        cell = json.loads(path.read_text(encoding="utf-8"))
        names = _fire_names(cell)
        arc_fs: dict[str, int] = {}
        if cell.get("provenance") == "behavioral":
            delays = cell.get("delays") or {}
            for name in names:
                if name not in delays:
                    raise PathError(f"{cell['name']} has no constant delay {name}")
                arc_fs[name] = int(delays[name])
        else:
            bundle = cbor2.loads((lib_dir / f"{cell['name']}.lut.cbor").read_bytes())
            entries = bundle["entries"]
            for name in names:
                if name not in entries:
                    raise PathError(f"{cell['name']} LUT has no entry {name}")
                arc_fs[name] = _nominal_row_max(entries[name])
        cell["arc_fs"] = arc_fs
        cells[cell["name"]] = cell
    return cells


def longest_path_fs(design: dict, cells: dict[str, dict]) -> int:
    """Longest combinational path from a primary input to a primary output."""
    adj: dict[tuple, list[tuple[tuple, int]]] = {}

    def add(src: tuple, dst: tuple, weight: int) -> None:
        adj.setdefault(src, []).append((dst, weight))

    for net in design["nets"]:
        add(_endpoint(net["driver"], primary="in"), _endpoint(net["sink"], primary="out"), 0)
    for inst, instance in design["instances"].items():
        cell = cells[instance["cell"]]
        for src_port, dst_port, delay in _cell_arcs(cell):
            add(("pin", inst, src_port), ("pin", inst, dst_port), delay)

    visiting: set[tuple] = set()
    memo: dict[tuple, int | None] = {}

    def walk(node: tuple) -> int | None:
        if node in memo:
            return memo[node]
        if node in visiting:
            raise PathError(f"combinational cycle at {node}")
        visiting.add(node)
        best: int | None = 0 if node[0] == "out" else None
        for nxt, weight in adj.get(node, []):
            down = walk(nxt)
            if down is None:
                continue
            total = weight + down
            if best is None or total > best:
                best = total
        visiting.remove(node)
        memo[node] = best
        return best

    longest = 0
    for port in design["ports"]["in"]:
        got = walk(("in", port))
        if got is not None and got > longest:
            longest = got
    return longest


def epoch_counts(
    stimulus: dict, pulses_by_port: dict[str, list[int]], longest_fs: int
) -> dict[str, list[int]]:
    """Per-port counts in each stimulus epoch.

    The epoch of a pulse is the latest stimulus strictly before it, and the
    pulse must fall before the next stimulus. The last stimulus has no next
    bound. ``longest_fs`` rejects a stimulus whose gaps are below ``2L``.
    """
    times = sorted({int(time) for port_times in stimulus.values() for time in port_times})
    _check_spacing(times, longest_fs)
    counts: dict[str, list[int]] = {}
    for port, pulses in sorted(pulses_by_port.items()):
        buckets = [0] * len(times)
        for time in pulses:
            cause = _causing_input(times, int(time))
            if cause is None:
                raise StimulusSpacingError(
                    f"pulse on {port} at {int(time)} fs is outside the epoch of its causing input"
                )
            buckets[times.index(cause)] += 1
        counts[port] = buckets
    return counts


def functional_view(
    design: dict,
    stimulus: dict,
    pulses_by_driver: dict[str, list[int]],
    cells: dict[str, dict] | None = None,
) -> tuple:
    """Return ``("clocked", counts)`` or ``("unclocked", epoch counts)``."""
    by_port = {
        port: list(pulses_by_driver.get(driver, []))
        for port, driver in output_drivers(design).items()
    }
    if is_clocked(design):
        clocks = list(stimulus["clk"])
        cycles = {
            port: cycle_counts(times, clocks) for port, times in sorted(by_port.items())
        }
        return ("clocked", cycles)
    if cells is None:
        raise PathError("unclocked comparison needs cell arc bounds")
    longest = longest_path_fs(design, cells)
    return ("unclocked", epoch_counts(stimulus, by_port, longest))


def format_view(kind: str, view: dict) -> str:
    return " ".join(f"{port}:{counts}" for port, counts in view.items())


def first_difference(left: dict, right: dict) -> str | None:
    """The first port and epoch whose counts differ, or None when they match."""
    for port in sorted(set(left) | set(right)):
        a = list(left.get(port, []))
        b = list(right.get(port, []))
        width = max(len(a), len(b))
        a.extend([0] * (width - len(a)))
        b.extend([0] * (width - len(b)))
        for index, (got, expected) in enumerate(zip(a, b)):
            if got != expected:
                return f"{port} epoch {index}: {got} vs {expected}"
    return None


def ps_to_fs(times_ps: list[float]) -> list[int]:
    return [int(round(float(time) * 1000)) for time in times_ps]


def _fire_names(cell: dict) -> list[str]:
    names: list[str] = []
    for transition in cell.get("transitions", []):
        for fire in transition.get("fire", []):
            name = fire["delay"]
            if name not in names:
                names.append(name)
    return names


def _cell_arcs(cell: dict) -> list[tuple[str, str, int]]:
    bounds = cell["arc_fs"]
    best: dict[tuple[str, str], int] = {}
    for transition in cell["transitions"]:
        for fire in transition["fire"]:
            name = fire["delay"]
            if name not in bounds:
                raise PathError(f"missing arc bound {name}")
            key = (transition["on"], fire["port"])
            delay = int(bounds[name])
            if key not in best or delay > best[key]:
                best[key] = delay
    return [(src, dst, delay) for (src, dst), delay in best.items()]


def _endpoint(pin: dict, *, primary: str) -> tuple:
    if "instance" in pin:
        return ("pin", pin["instance"], pin["port"])
    return (primary, pin["port"])


def _check_spacing(times: list[int], longest_fs: int) -> None:
    limit = 2 * longest_fs
    for earlier, later in zip(times, times[1:]):
        gap = later - earlier
        if gap < limit:
            raise StimulusSpacingError(
                f"input spacing {gap} fs is below 2× longest path {longest_fs} fs"
            )


def _causing_input(times: list[int], pulse: int) -> int | None:
    cause = None
    for time in times:
        if time < pulse:
            cause = time
        else:
            break
    return cause


def _nominal_row_max(entry: dict) -> int:
    axes = entry["axes"]
    values = entry["values"]
    coords = [_axis_coords(axis) for axis in axes]
    try:
        bias_index = next(index for index, axis in enumerate(axes) if axis["name"] == "bias_frac")
    except StopIteration as err:
        raise PathError("LUT entry has no bias_frac axis") from err
    if NOMINAL_BIAS not in coords[bias_index]:
        raise PathError("LUT entry has no bias_frac = 1 row")
    sizes = [len(points) for points in coords]
    strides = [1] * len(sizes)
    for index in range(len(sizes) - 2, -1, -1):
        strides[index] = strides[index + 1] * sizes[index + 1]
    best: int | None = None
    for flat in range(len(values)):
        bias_coord = (flat // strides[bias_index]) % sizes[bias_index]
        if coords[bias_index][bias_coord] != NOMINAL_BIAS:
            continue
        sample = int(values[flat])
        if best is None or sample > best:
            best = sample
    if best is None:
        raise PathError("LUT bias_frac = 1 row is empty")
    return best


def _axis_coords(axis: dict) -> list[int]:
    name = axis["name"]
    coords = []
    for text in axis["points"]:
        if name == "bias_frac":
            coords.append(int((Decimal(str(text)) * Decimal(NOMINAL_BIAS)).to_integral_value(rounding=ROUND_HALF_UP)))
        else:
            coords.append(int(text))
    return coords
