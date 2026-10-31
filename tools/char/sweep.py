"""Parallel JoSIM sweeps over the characterization benches.

SPEC §4.2. A sweep YAML names the axes. Each point is one patched deck: the
nominal bench is rendered once, then bias, stimulus, and stop time are
rewritten. Workers run JoSIM. A point that fails to simulate is stored with
the others.

The PWL edges match ``pwl`` in ``crates/fx-flow/src/export.rs``:
settle 20 ps, edge 3 ps, span 6 ps, 600 µA. ``min`` is that span, so two
pulses on one port are legal. ``settled`` is 200 ps, the JTL bench gap.
"""

from __future__ import annotations

import hashlib
import itertools
import json
import multiprocessing
import os
import re
import statistics
import sys
import tempfile
import time
from collections.abc import Callable, Mapping, Sequence
from concurrent.futures import ProcessPoolExecutor, as_completed
from decimal import Decimal
from pathlib import Path
from typing import Any

import yaml

from bench import BENCHES, LIB, BenchError, _bias_token, delays, render
from extract import extract_crossings
from josim import (
    JosimFailed,
    JosimOutputError,
    JosimTimeout,
    run_with_log,
    version,
    warning_lines,
)

# export.rs: SETTLE_FS, EDGE_FS, PULSE_SPAN_FS, TAIL_FS.
SETTLE_FS = 20_000
EDGE_FS = 3_000
PULSE_SPAN_FS = 6_000
TAIL_FS = 500_000
MIN_DT_PREV_FS = PULSE_SPAN_FS
SETTLED_DT_PREV_FS = 200_000

SWEEPS = Path(__file__).resolve().parent / "sweeps"
OUT = Path(__file__).resolve().parent / "out"

_OPTIONAL = ("data_clock_offset_fs", "in_in_offset_fs")


class SweepError(RuntimeError):
    """A sweep config or schedule cannot be run."""


def worker_count() -> int:
    """Leave one core free for the parent process."""
    return max(1, (os.cpu_count() or 2) - 1)


def load_sweep(path: Path | str) -> dict[str, Any]:
    """Read one cell's sweep YAML."""
    loaded = yaml.safe_load(Path(path).read_text(encoding="utf-8"))
    if not isinstance(loaded, dict) or "cell" not in loaded or "axes" not in loaded:
        raise SweepError(f"{path} is not a sweep config")
    return loaded


def expand_axis(spec: Mapping[str, Any]) -> list[Any]:
    """Expand one axis spec into grid values.

    ``start: min`` and ``stop: settled`` are the pulse span and 200 ps.
    ``spacing: log`` rounds every interior point to 1 fs and keeps both ends.
    A ``step`` axis uses Decimal when any bound is fractional.
    """
    start = _resolve_token(spec["start"])
    stop = _resolve_token(spec["stop"])
    if spec.get("spacing") == "log":
        return _expand_log(int(start), int(stop), int(spec["points"]))
    step = spec["step"]
    if _fractional(start, stop, step):
        return _expand_decimal(start, stop, step)
    return _expand_int(int(start), int(stop), int(step))


def grid_points(sweep: Mapping[str, Any]) -> list[dict[str, Any]]:
    """Cartesian product of the axes, in YAML order. The first axis changes slowest."""
    names = list(sweep["axes"])
    columns = [expand_axis(sweep["axes"][name]) for name in names]
    return [dict(zip(names, combo, strict=True)) for combo in itertools.product(*columns)]


def schedule_inputs(inputs: Mapping[str, Sequence[Any]], params: Mapping[str, Any]) -> dict[str, list[int]]:
    """Place bench pulses, in femtoseconds.

    The first pulse on a port stays at its bench time. Each later pulse on
    that port is ``anchor + i * dt_prev_fs``. Clock times then move by
    ``data_clock_offset_fs``. Merger ``b[i]`` becomes ``a[i] + in_in_offset_fs``.
    """
    if "dt_prev_fs" not in params:
        raise SweepError("dt_prev_fs is required")
    dt = int(params["dt_prev_fs"])
    if dt < MIN_DT_PREV_FS:
        raise SweepError(f"dt_prev_fs {dt} is under the {MIN_DT_PREV_FS} fs pulse span")
    placed: dict[str, list[int]] = {}
    for port, times in inputs.items():
        femto = _ps_list_to_fs(times)
        if not femto:
            placed[str(port)] = []
            continue
        anchor = femto[0]
        placed[str(port)] = [anchor + i * dt for i in range(len(femto))]
    if "data_clock_offset_fs" in params and "clk" in placed:
        shift = int(params["data_clock_offset_fs"])
        placed["clk"] = [time + shift for time in placed["clk"]]
    if "in_in_offset_fs" in params and "b" in placed:
        if "a" not in placed:
            raise SweepError("in_in_offset_fs needs port a")
        offset = int(params["in_in_offset_fs"])
        if len(placed["b"]) > len(placed["a"]):
            raise SweepError("in_in_offset_fs needs an a pulse for every b pulse")
        placed["b"] = [placed["a"][i] + offset for i in range(len(placed["b"]))]
    for port, times in placed.items():
        if any(time < 0 for time in times):
            raise SweepError(f"{port} pulse time is negative")
        ordered = sorted(times)
        for left, right in zip(ordered, ordered[1:], strict=False):
            if right - left < PULSE_SPAN_FS:
                raise SweepError(f"{port} pulses are closer than {PULSE_SPAN_FS} fs")
        placed[port] = ordered
    return placed


def stop_fs(inputs: Mapping[str, Sequence[int]]) -> int:
    """Exporter stop: last stimulus + 20 ps settle + 500 ps tail."""
    last = max(int(time) for times in inputs.values() for time in times)
    return last + SETTLE_FS + TAIL_FS


def patch_deck(
    deck: str,
    bias: Any,
    stimulus: Mapping[str, Sequence[int]],
    length_fs: int,
) -> str:
    """Rewrite bias, each ``Ifxsrc`` PWL, and the ``.tran`` stop of a nominal deck."""
    token = _bias_token(bias)
    lines = deck.splitlines(keepends=True)
    saw_bias = False
    saw_tran = False
    seen: set[str] = set()
    for index, line in enumerate(lines):
        if line.startswith(".param bias_mul="):
            lines[index] = f".param bias_mul={token}\n"
            saw_bias = True
            continue
        if line.startswith("Ifxsrc_"):
            port = line.split()[0].removeprefix("Ifxsrc_")
            if port not in stimulus:
                raise SweepError(f"deck has Ifxsrc_{port} but the schedule does not")
            updated, count = re.subn(r"pwl\([^)]*\)", _format_pwl(stimulus[port]), line, count=1)
            if count != 1:
                raise SweepError(f"Ifxsrc_{port} has no pwl()")
            if not updated.endswith("\n"):
                updated += "\n"
            lines[index] = updated
            seen.add(port)
            continue
        if line.startswith(".tran "):
            step = line.split()[1]
            lines[index] = f".tran {step} {format_fs(length_fs)} 0\n"
            saw_tran = True
    missing = set(stimulus) - seen
    if missing:
        raise SweepError(f"schedule ports missing from the deck: {sorted(missing)}")
    if not saw_bias or not saw_tran:
        raise SweepError("deck is missing bias_mul or .tran")
    return "".join(lines)


def cache_key(netlist: str, params: Mapping[str, Any], josim_version: str) -> str:
    """sha256 of the netlist, canonical params, and the JoSIM version string."""
    payload = netlist.encode() + b"\0" + _canonical(params) + b"\0" + josim_version.encode()
    return hashlib.sha256(payload).hexdigest()


def classify(
    cell: Mapping[str, Any],
    stimulus: Mapping[str, Sequence[int]],
    pulses: Mapping[str, Sequence[int]],
) -> tuple[str, str]:
    """Compare measured crossings with the cell state machine.

    One input crossing is expected per stimulus pulse. Output counts follow
    the transitions. SFQDC ``q`` is a DC level: it must toggle across the
    high interval instead of firing once per edge. ``how`` is empty when the
    counts match.
    """
    mismatches: list[str] = []
    for port in cell["ports"]["in"]:
        expected = len(stimulus.get(port, ()))
        got = len(pulses.get(port, ()))
        if got != expected:
            mismatches.append(f"{port}: expected {expected} got {got}")
    if cell["name"] == "SFQDC":
        level = cell["ports"]["out"][0]
        if not _level_ok(pulses, level):
            mismatches.append(f"{level}: level did not toggle")
    else:
        fires = _predict(cell, stimulus)
        for port in cell["ports"]["out"]:
            expected = fires.get(port, 0)
            got = len(pulses.get(port, ()))
            if got != expected:
                mismatches.append(f"{port}: expected {expected} got {got}")
    if not mismatches:
        return "expected", ""
    mismatches.sort()
    return "failed", "; ".join(mismatches)


def collect(
    cell: str,
    cell_def: Mapping[str, Any],
    jobs: Sequence[Mapping[str, Any]],
    simulate: Callable[[str], tuple[dict[str, tuple[int, ...]], tuple[str, ...]]],
    *,
    out_dir: Path,
    josim_version: str,
    workers: int = 1,
    progress: bool = False,
) -> list[dict[str, Any]]:
    """Run jobs, skip keys already in the jsonl cache, and write parquet.

    ``workers`` above 1 uses a process pool and always calls ``simulate_deck``.
    An injected simulator runs in this process, so tests can pass a closure.
    """
    if workers < 1:
        raise SweepError("workers must be positive")
    directory = Path(out_dir) / cell
    directory.mkdir(parents=True, exist_ok=True)
    cache_path = directory / "cache.jsonl"
    cached = _load_cache(cache_path)
    by_index: dict[int, dict[str, Any]] = {}
    pending: list[dict[str, Any]] = []
    for index, job in enumerate(jobs):
        params = _stored_params(job["params"])
        key = cache_key(str(job["deck"]), job["params"], josim_version)
        hit = cached.get(key)
        if hit is not None:
            by_index[index] = hit
            continue
        pending.append(
            {
                "index": index,
                "cell": cell,
                "cell_def": dict(cell_def),
                "stimulus": dict(job["stimulus"]),
                "deck": str(job["deck"]),
                "params": params,
                "key": key,
                "josim_version": josim_version,
            }
        )

    started = time.perf_counter()
    done = len(by_index)
    total = len(jobs)

    def save(row: dict[str, Any]) -> None:
        nonlocal done
        stored = {key: value for key, value in row.items() if key != "index"}
        _append_jsonl(cache_path, stored)
        by_index[int(row["index"])] = stored
        done += 1
        if progress:
            _emit_progress(done, total, started)

    if progress:
        _emit_progress(done, total, started)
    if pending:
        if workers == 1:
            for item in pending:
                save(_eval_with(item, simulate))
        else:
            if simulate is not simulate_deck:
                raise SweepError("a worker pool can only run simulate_deck")
            pool_map(_eval_job, pending, workers, on_result=save)

    ordered = [by_index[index] for index in range(total)]
    _write_parquet(ordered, directory / "results.parquet")
    return ordered


def pool_map(
    fn: Callable[[Any], Any],
    jobs: Sequence[Any],
    workers: int,
    on_result: Callable[[Any], None] | None = None,
) -> list[Any]:
    """Run ``fn`` over ``jobs``. A JoSIM or bench failure becomes a failed row."""
    if workers < 1:
        raise SweepError("workers must be positive")
    ordered: list[Any] = [None] * len(jobs)
    context = multiprocessing.get_context("spawn")
    with ProcessPoolExecutor(max_workers=workers, mp_context=context) as pool:
        futures = {
            pool.submit(_guard, (fn, job)): index for index, job in enumerate(jobs)
        }
        for future in as_completed(futures):
            index = futures[future]
            row = future.result()
            ordered[index] = row
            if on_result is not None:
                on_result(row)
    return ordered


def probe_job(job: Mapping[str, Any]) -> dict[str, Any]:
    """Picklable stand-in that fails one point. Proves the pool keeps it."""
    if job["fail"]:
        raise JosimFailed(1, "probe")
    return {"outcome": "expected", "how": "", "arcs": []}


def format_progress(done: int, total: int, elapsed: float) -> str:
    """One status line: bar, count, rate, and ETA."""
    width = 20
    filled = 0 if total <= 0 else min(width, int(width * done / total))
    bar = "#" * filled + "-" * (width - filled)
    if done <= 0 or elapsed <= 0:
        return f"[{bar}] {done}/{total}  0.0/s  ETA unknown"
    rate = done / elapsed
    remaining = (total - done) / rate
    return f"[{bar}] {done}/{total}  {rate:.1f}/s  ETA {_format_eta(remaining)}"


def bias_delay_series(records: Sequence[Mapping[str, Any]]) -> list[tuple[Decimal, float]]:
    """Median output-arc delay, in picoseconds, at settled dt and zero offset.

    Failed points and other slices are omitted. Bias stays a Decimal.
    """
    series: list[tuple[Decimal, float]] = []
    for row in records:
        if row.get("outcome") != "expected":
            continue
        if _as_int(_field(row, "dt_prev_fs")) != SETTLED_DT_PREV_FS:
            continue
        if _field(row, "data_clock_offset_fs") not in (None, 0):
            continue
        if _field(row, "in_in_offset_fs") not in (None, 0):
            continue
        arcs = _arcs(row)
        if not arcs:
            continue
        median_fs = statistics.median(int(arc["delay_fs"]) for arc in arcs)
        bias = _field(row, "bias_frac")
        if not isinstance(bias, Decimal):
            bias = Decimal(str(bias))
        series.append((bias, float(median_fs) / 1_000))
    series.sort(key=lambda item: item[0])
    return series


def write_delay_plot(records: Sequence[Mapping[str, Any]], path: Path | str, title: str = "") -> None:
    """Write the settled bias-to-delay curve as a PNG."""
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    series = bias_delay_series(records)
    fig, ax = plt.subplots()
    if series:
        ax.plot(
            [float(bias) for bias, _delay in series],
            [delay for _bias, delay in series],
            marker="o",
        )
    ax.set_xlabel("bias_frac")
    ax.set_ylabel("delay (ps)")
    if title:
        ax.set_title(title)
    ax.grid(True)
    destination = Path(path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    fig.savefig(destination, dpi=120)
    plt.close(fig)


def run_cell(
    cell: str,
    *,
    out_dir: Path | None = None,
    workers: int | None = None,
    josim_version: str | None = None,
    progress: bool = True,
) -> list[dict[str, Any]]:
    """Sweep one cell and write its parquet and bias curve."""
    sweep = load_sweep(SWEEPS / f"{cell}.yaml")
    if sweep["cell"] != cell:
        raise SweepError(f"{cell} sweep file is for {sweep['cell']}")
    bench = yaml.safe_load((BENCHES / f"{cell}.yaml").read_text(encoding="utf-8"))
    cell_def = json.loads((LIB / f"{cell}.cell.json").read_text(encoding="utf-8"))
    nominal = render(BENCHES / f"{cell}.yaml")
    jobs = []
    for params in grid_points(sweep):
        stimulus = schedule_inputs(bench["inputs"], params)
        jobs.append(
            {
                "params": params,
                "deck": patch_deck(nominal, params["bias_frac"], stimulus, stop_fs(stimulus)),
                "stimulus": stimulus,
            }
        )
    rows = collect(
        cell,
        cell_def,
        jobs,
        simulate_deck,
        out_dir=OUT if out_dir is None else out_dir,
        josim_version=version() if josim_version is None else josim_version,
        workers=worker_count() if workers is None else workers,
        progress=progress,
    )
    destination = (OUT if out_dir is None else out_dir) / cell / "delay_vs_bias.png"
    write_delay_plot(rows, destination, title=f"{cell} delay vs bias")
    return rows


def simulate_deck(
    deck: str,
    timeout: float = 180,
) -> tuple[dict[str, tuple[int, ...]], tuple[str, ...]]:
    """Run one deck. Warnings are returned with the pulses, not raised."""
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "bench.cir"
        path.write_text(deck, encoding="utf-8")
        traces, log = run_with_log(path, timeout=timeout)
    return _pulses(deck, traces), tuple(warning_lines(log))


def main(argv: Sequence[str] | None = None) -> None:
    """Sweep the named cells. Prints each settled bias curve."""
    args = list(sys.argv[1:] if argv is None else argv)
    if not args:
        raise SystemExit("usage: sweep.py CELL [CELL ...]")
    josim_version = version()
    count = worker_count()
    for cell in args:
        print(f"{cell}: {count} workers, {josim_version}", flush=True)
        rows = run_cell(cell, workers=count, josim_version=josim_version)
        failed = sum(row["outcome"] == "failed" for row in rows)
        print(f"{cell}: {len(rows)} points, {failed} failed", flush=True)
        print(format_series(bias_delay_series(rows)), flush=True)
        print(OUT / cell / "delay_vs_bias.png", flush=True)


def format_series(series: Sequence[tuple[Decimal, float]]) -> str:
    """Text form of the bias curve, delays in picoseconds."""
    lines = ["bias_frac delay_ps"]
    for bias, delay in series:
        lines.append(f"{bias} {delay:.3f}")
    return "\n".join(lines)


def _eval_job(job: Mapping[str, Any]) -> dict[str, Any]:
    return _eval_with(job, simulate_deck)


def _eval_with(
    job: Mapping[str, Any],
    simulate: Callable[[str], tuple[dict[str, tuple[int, ...]], tuple[str, ...]]],
) -> dict[str, Any]:
    outcome, how, arcs = evaluate(job["cell_def"], job["stimulus"], str(job["deck"]), simulate)
    return {
        "index": job["index"],
        "key": job["key"],
        "cell": job["cell"],
        "params": dict(job["params"]),
        "outcome": outcome,
        "how": how,
        "arcs": arcs,
        "josim_version": job["josim_version"],
    }


def evaluate(
    cell_def: Mapping[str, Any],
    stimulus: Mapping[str, Sequence[int]],
    deck: str,
    simulate: Callable[[str], tuple[dict[str, tuple[int, ...]], tuple[str, ...]]],
) -> tuple[str, str, list[dict[str, Any]]]:
    """One point: outcome, how, and every measured arc. JoSIM failures have no arcs."""
    try:
        pulses, warnings = simulate(deck)
    except (JosimFailed, JosimTimeout, JosimOutputError, BenchError) as exc:
        return "failed", failure_how(exc), []
    outcome, how = classify(cell_def, stimulus, pulses)
    arcs = _measured_arcs(pulses, stimulus)
    if warnings:
        outcome = "failed"
        text = "josim warning: " + "; ".join(warnings)
        how = text if not how else f"{how}; {text}"
    return outcome, how, arcs


def failure_how(exc: BaseException) -> str:
    """Short reason stored on a failed JoSIM or bench row."""
    if isinstance(exc, JosimFailed):
        return f"josim exited {exc.returncode}"
    if isinstance(exc, JosimTimeout):
        return f"josim timed out after {exc.timeout:g} s"
    if isinstance(exc, JosimOutputError):
        return f"josim output: {exc}"
    return str(exc)


def _guard(payload: tuple[Callable[[Any], Any], Any]) -> Any:
    fn, job = payload
    try:
        return fn(job)
    except (JosimFailed, JosimTimeout, JosimOutputError, BenchError) as exc:
        return {"outcome": "failed", "how": failure_how(exc), "arcs": []}


def _pulses(
    deck: str,
    traces: Mapping[str, tuple[tuple[float, ...], tuple[float, ...]]],
) -> dict[str, tuple[int, ...]]:
    """DUT port times in femtoseconds. Same crossing rule as ``bench.run``."""
    found: dict[str, tuple[int, ...]] = {}
    for line in deck.splitlines():
        parts = line.split()
        if parts[:2] != ["*", "fluxel-map"] or len(parts) != 4:
            continue
        key, column = parts[2], parts[3]
        if not key.startswith("dut."):
            continue
        series = traces.get(column)
        if series is None:
            raise KeyError(column)
        crossings = extract_crossings(series[0], series[1])
        # SPEC §4: a pulse is an upward (2k+1)π crossing. A junction that only
        # slips downward, as DCSFQ B1 does, uses those times instead.
        chosen = crossings.pulses or crossings.antipulses
        times = tuple(int(round(item.t * 1e15)) for item in chosen)
        if times:
            found[key.removeprefix("dut.")] = times
    return found


def _measured_arcs(
    pulses: Mapping[str, Sequence[int]],
    stimulus: Mapping[str, Sequence[int]],
) -> list[dict[str, Any]]:
    try:
        found = delays(pulses, stimulus.keys())
    except BenchError:
        return []
    return [
        {
            "src": arc.src,
            "dst": arc.dst,
            "cause_fs": arc.cause_fs,
            "delay_fs": arc.delay_fs,
            "tag": arc.tag,
        }
        for arc in found
    ]


def _predict(cell: Mapping[str, Any], stimulus: Mapping[str, Sequence[int]]) -> dict[str, int]:
    table = {(item["from"], item["on"]): item for item in cell["transitions"]}
    state = cell["initial"]
    fires = {port: 0 for port in cell["ports"]["out"]}
    events = sorted(
        (int(time), str(port)) for port, times in stimulus.items() for time in times
    )
    for _time, port in events:
        trans = table.get((state, port))
        if trans is None:
            continue
        state = trans["to"]
        for fire in trans.get("fire") or []:
            fires[fire["port"]] += 1
    return fires


def _level_ok(pulses: Mapping[str, Sequence[int]], level: str) -> bool:
    """SFQDC q slips for the whole high interval. Same bounds as the nominal bench."""
    times = tuple(pulses.get(level, ()))
    ins = tuple(pulses.get("a", ()))
    if len(ins) != 2 or len(times) < 2:
        return False
    spacing = ins[1] - ins[0]
    return times[0] > ins[0] and times[-1] > ins[1] and times[-1] - ins[1] < spacing


def _resolve_token(value: Any) -> Any:
    if value == "min":
        return MIN_DT_PREV_FS
    if value == "settled":
        return SETTLED_DT_PREV_FS
    return value


def _fractional(*values: Any) -> bool:
    for value in values:
        if isinstance(value, float):
            return True
        if isinstance(value, Decimal) and value != value.to_integral_value():
            return True
        if isinstance(value, str) and value not in {"min", "settled"} and "." in value:
            return True
    return False


def _expand_decimal(start: Any, stop: Any, step: Any) -> list[Decimal]:
    start_d, stop_d, step_d = Decimal(str(start)), Decimal(str(stop)), Decimal(str(step))
    if step_d <= 0:
        raise SweepError(f"step {step_d} is not positive")
    span = (stop_d - start_d) / step_d
    if span != span.to_integral_value():
        raise SweepError(f"{start_d}..{stop_d} step {step_d} does not land on the stop")
    count = int(span) + 1
    return [start_d + step_d * i for i in range(count)]


def _expand_int(start: int, stop: int, step: int) -> list[int]:
    if step <= 0 or (stop - start) % step != 0:
        raise SweepError(f"{start}..{stop} step {step} does not land on the stop")
    return list(range(start, stop + 1, step))


def _expand_log(start: int, stop: int, points: int) -> list[int]:
    if points < 2 or start <= 0 or stop <= start:
        raise SweepError(f"log grid {start}..{stop} x {points} is empty")
    ratio = (stop / start) ** (1 / (points - 1))
    values = [start]
    for index in range(1, points - 1):
        values.append(int(round(start * ratio**index)))
    values.append(stop)
    for index in range(1, len(values)):
        if values[index] <= values[index - 1]:
            values[index] = values[index - 1] + 1
    values[-1] = stop
    if any(values[index] <= values[index - 1] for index in range(1, len(values))):
        raise SweepError("log grid is not strictly increasing")
    return values


def _ps_list_to_fs(times: Sequence[Any]) -> list[int]:
    found = []
    for time in times:
        femto = Decimal(str(time)) * Decimal(1000)
        if femto != femto.to_integral_value():
            raise SweepError(f"{time} ps is not an integer number of femtoseconds")
        found.append(int(femto))
    return found


def format_fs(fs: int) -> str:
    """Match ``format_fs`` in ``crates/fx-flow/src/export.rs``."""
    neg = "-" if fs < 0 else ""
    magnitude = abs(fs)
    picoseconds, rem = divmod(magnitude, 1000)
    if rem == 0:
        return f"{neg}{picoseconds}p"
    digits = f"{rem:03d}".rstrip("0")
    return f"{neg}{picoseconds}.{digits}p"


def _format_pwl(times: Sequence[int]) -> str:
    """Match ``pwl`` in ``crates/fx-flow/src/export.rs``."""
    parts = ["0", "0"]
    for time in times:
        start = int(time) + SETTLE_FS
        parts.extend(
            [
                format_fs(start),
                "0",
                format_fs(start + EDGE_FS),
                "600u",
                format_fs(start + PULSE_SPAN_FS),
                "0",
            ]
        )
    return "pwl(" + " ".join(parts) + ")"


def _canonical(params: Mapping[str, Any]) -> bytes:
    return json.dumps(
        {key: _param_value(value) for key, value in params.items()},
        sort_keys=True,
        separators=(",", ":"),
    ).encode()


def _param_value(value: Any) -> Any:
    if isinstance(value, Decimal):
        return format(value, "f")
    if isinstance(value, float):
        return format(Decimal(str(value)), "f")
    return value


def _stored_params(params: Mapping[str, Any]) -> dict[str, Any]:
    return {key: _param_value(value) for key, value in params.items()}


def _field(row: Mapping[str, Any], name: str) -> Any:
    if name in row:
        return row[name]
    params = row.get("params") or {}
    if isinstance(params, str):
        params = json.loads(params)
    return params.get(name)


def _arcs(row: Mapping[str, Any]) -> list[dict[str, Any]]:
    arcs = row.get("arcs") or []
    if isinstance(arcs, str):
        arcs = json.loads(arcs)
    return list(arcs)


def _as_int(value: Any) -> int | None:
    if value is None:
        return None
    return int(value)


def _load_cache(path: Path) -> dict[str, dict[str, Any]]:
    found: dict[str, dict[str, Any]] = {}
    if not path.is_file():
        return found
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        found[row["key"]] = row
    return found


def _append_jsonl(path: Path, row: Mapping[str, Any]) -> None:
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(row, separators=(",", ":")) + "\n")
        handle.flush()
        os.fsync(handle.fileno())


def _write_parquet(records: Sequence[Mapping[str, Any]], path: Path) -> None:
    import pyarrow as pa
    import pyarrow.parquet as pq

    def params_of(row: Mapping[str, Any]) -> dict[str, Any]:
        raw = row["params"]
        if isinstance(raw, str):
            return json.loads(raw)
        return dict(raw)

    def optional(row: Mapping[str, Any], name: str) -> int | None:
        value = params_of(row).get(name)
        if value is None:
            return None
        return int(value)

    def arcs_json(row: Mapping[str, Any]) -> str:
        arcs = row["arcs"]
        if isinstance(arcs, str):
            return arcs
        return json.dumps(arcs, separators=(",", ":"))

    schema = pa.schema(
        [
            ("key", pa.string()),
            ("cell", pa.string()),
            ("bias_frac", pa.string()),
            ("dt_prev_fs", pa.int64()),
            ("data_clock_offset_fs", pa.int64()),
            ("in_in_offset_fs", pa.int64()),
            ("outcome", pa.string()),
            ("how", pa.string()),
            ("arcs", pa.string()),
            ("josim_version", pa.string()),
        ]
    )
    columns = [
        pa.array([row["key"] for row in records], type=pa.string()),
        pa.array([row["cell"] for row in records], type=pa.string()),
        pa.array([str(params_of(row)["bias_frac"]) for row in records], type=pa.string()),
        pa.array([int(params_of(row)["dt_prev_fs"]) for row in records], type=pa.int64()),
        pa.array([optional(row, "data_clock_offset_fs") for row in records], type=pa.int64()),
        pa.array([optional(row, "in_in_offset_fs") for row in records], type=pa.int64()),
        pa.array([row["outcome"] for row in records], type=pa.string()),
        pa.array([row["how"] for row in records], type=pa.string()),
        pa.array([arcs_json(row) for row in records], type=pa.string()),
        pa.array([row["josim_version"] for row in records], type=pa.string()),
    ]
    pq.write_table(pa.Table.from_arrays(columns, schema=schema), path)


def _emit_progress(done: int, total: int, started: float) -> None:
    elapsed = time.perf_counter() - started
    end = "\n" if done == total else "\r"
    print(format_progress(done, total, elapsed), file=sys.stderr, end=end, flush=True)


def _format_eta(seconds: float) -> str:
    whole = int(round(seconds))
    minutes, secs = divmod(whole, 60)
    hours, minutes = divmod(minutes, 60)
    if hours:
        return f"{hours}:{minutes:02d}:{secs:02d}"
    return f"{minutes}:{secs:02d}"


if __name__ == "__main__":
    multiprocessing.freeze_support()
    parent = str(Path(__file__).resolve().parent)
    if parent not in sys.path:
        sys.path.insert(0, parent)
    import sweep as sweep_module

    sweep_module.main()
