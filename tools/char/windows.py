"""Timing windows by bisection on offset.

SPEC §4.2. A functional failure is whatever the bench template's ``pass``
clauses reject: a missing pulse, an extra pulse, or a misplaced one. The cell
state machine is not consulted.

The published window is the last offset that still passes. The next sample,
10 fs toward the failure, is the first functional failure. Offsets are
integer femtoseconds.
"""

from __future__ import annotations

import multiprocessing
import sys
import time
from collections.abc import Callable, Mapping, Sequence
from decimal import Decimal
from pathlib import Path
from typing import Any

import yaml

from bench import BENCHES, BenchError, render
from josim import JosimFailed, JosimOutputError, JosimTimeout, version
from sweep import (
    OUT,
    PULSE_SPAN_FS,
    SWEEPS,
    _append_jsonl,
    _load_cache,
    cache_key,
    expand_axis,
    failure_how,
    format_progress,
    load_sweep,
    patch_deck,
    pool_map,
    simulate_deck,
    stop_fs,
    worker_count,
)

# Review flag only. Not a SPEC §8 threshold.
NOISE_FS = 1_000


class WindowError(RuntimeError):
    """A window template or search bound cannot be used."""


def schedule(
    stimulus: Mapping[str, Sequence[Any]],
    *,
    anchor: int,
    lead: int,
    offset: int,
) -> dict[str, list[int]]:
    """Place one probe. Expressions use ``anchor``, ``lead``, ``offset``, and ``+``.

    A negative time is rejected. Two pulses on one port closer than the
    exporter's 6 ps span are rejected.
    """
    placed: dict[str, list[int]] = {}
    for port, exprs in stimulus.items():
        times = [_eval_time(expr, anchor, lead, offset) for expr in exprs]
        if any(item < 0 for item in times):
            raise WindowError(f"{port} pulse time is negative")
        times.sort()
        for left, right in zip(times, times[1:], strict=False):
            if right - left < PULSE_SPAN_FS:
                raise WindowError(f"{port} pulses are closer than {PULSE_SPAN_FS} fs")
        placed[str(port)] = times
    return placed


def judge(
    clauses: Sequence[Mapping[str, Any]],
    pulses: Mapping[str, Sequence[int]],
) -> tuple[str, str]:
    """Apply the template clauses. ``how`` is empty when every clause passes.

    ``between`` compares the sole output with the first pulse of each reference.
    ``after`` and ``before`` use a written index when the clause has one, and
    otherwise require exactly one pulse on that port.
    """
    measured = {str(port): tuple(int(item) for item in times) for port, times in pulses.items()}
    for clause in clauses:
        how = _clause(clause, measured)
        if how:
            return "fail", how
    return "pass", ""


def bisect(
    lo: int,
    hi: int,
    resolution: int,
    fails: Callable[[int], bool],
) -> dict[str, Any]:
    """Find the last passing offset on a fixed grid.

    ``fails(lo)`` and ``not fails(hi)`` are required. Otherwise the row is
    unbracketed and no window is invented. The search assumes one transition.
    """
    if resolution <= 0 or lo % resolution or hi % resolution or lo >= hi:
        raise WindowError(f"search {lo}..{hi} step {resolution} is not a grid")
    if not fails(lo):
        return _unbracketed("unbracketed: low passes")
    if fails(hi):
        return _unbracketed("unbracketed: high fails")
    while hi - lo > resolution:
        mid = lo + ((hi - lo) // resolution // 2) * resolution
        if mid == lo or mid == hi:
            break
        if fails(mid):
            lo = mid
        else:
            hi = mid
    if hi - lo != resolution:
        raise WindowError(f"bracket {lo}..{hi} is wider than {resolution} fs")
    return {"bracket": "bracketed", "window_fs": hi, "fail_fs": lo, "how": ""}


def review(rows: Sequence[Mapping[str, Any]]) -> list[str]:
    """Flag non-monotonic, noisy, or holed window-versus-bias series.

    Bracketed points are compared in bias order. A zero step is not a
    reversal. A step larger than ``NOISE_FS`` is noisy. An unbracketed point
    with bracketed points on both sides is a hole.
    """
    groups: dict[tuple[str, str], list[Mapping[str, Any]]] = {}
    for row in rows:
        groups.setdefault((str(row["cell"]), str(row["window"])), []).append(row)
    flags: list[str] = []
    for cell, window in sorted(groups):
        ordered = sorted(groups[(cell, window)], key=lambda item: Decimal(str(item["bias_frac"])))
        flags.extend(_series_flags(cell, window, ordered))
    return flags


def write_setup_hold_plot(rows: Sequence[Mapping[str, Any]], path: Path | str) -> None:
    """DFF ``t_setup`` and ``t_hold`` versus bias. Unbracketed biases are marks."""
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    fig, ax = plt.subplots()
    labeled = False
    for name in ("t_setup", "t_hold"):
        series: list[tuple[float, float]] = []
        marks: list[float] = []
        for row in rows:
            if row.get("cell") != "DFF" or row.get("window") != name:
                continue
            bias = float(Decimal(str(row["bias_frac"])))
            if row.get("bracket") != "bracketed" or row.get("window_fs") is None:
                marks.append(bias)
                continue
            series.append((bias, int(row["window_fs"]) / 1_000))
        series.sort()
        if series:
            ax.plot(
                [bias for bias, _delay in series],
                [delay for _bias, delay in series],
                marker="o",
                label=name,
            )
            labeled = True
        for bias in marks:
            ax.axvline(bias, linestyle=":", linewidth=0.8)
    ax.set_xlabel("bias_frac")
    ax.set_ylabel("window (ps)")
    ax.set_title("DFF setup/hold vs bias")
    ax.grid(True)
    if labeled:
        ax.legend()
    destination = Path(path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    fig.savefig(destination, dpi=120)
    plt.close(fig)


def write_windows(rows: Sequence[Mapping[str, Any]], path: Path | str) -> None:
    """Write one parquet of bracket results. Times are femtoseconds."""
    import pyarrow as pa
    import pyarrow.parquet as pq

    def optional(row: Mapping[str, Any], name: str) -> int | None:
        value = row[name]
        if value is None:
            return None
        return int(value)

    schema = pa.schema(
        [
            ("cell", pa.string()),
            ("window", pa.string()),
            ("bias_frac", pa.string()),
            ("window_fs", pa.int64()),
            ("fail_fs", pa.int64()),
            ("bracket", pa.string()),
            ("how", pa.string()),
        ]
    )
    columns = [
        pa.array([str(row["cell"]) for row in rows], type=pa.string()),
        pa.array([str(row["window"]) for row in rows], type=pa.string()),
        pa.array([_bias_text(row["bias_frac"]) for row in rows], type=pa.string()),
        pa.array([optional(row, "window_fs") for row in rows], type=pa.int64()),
        pa.array([optional(row, "fail_fs") for row in rows], type=pa.int64()),
        pa.array([str(row["bracket"]) for row in rows], type=pa.string()),
        pa.array([str(row["how"]) for row in rows], type=pa.string()),
    ]
    destination = Path(path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    pq.write_table(pa.Table.from_arrays(columns, schema=schema), destination)


def run_cells(
    cells: Sequence[str],
    *,
    out_dir: Path | None = None,
    workers: int | None = None,
    josim_version: str | None = None,
    progress: bool = True,
) -> list[dict[str, Any]]:
    """Bisect every template window at every bias point. Resume skips cached decks."""
    destination = OUT if out_dir is None else Path(out_dir)
    josim = version() if josim_version is None else josim_version
    jobs: list[dict[str, Any]] = []
    for cell in cells:
        bench = _load_bench(cell)
        print(f"{cell}: rendering", flush=True)
        nominal = render(BENCHES / f"{cell}.yaml")
        directory = destination / cell
        directory.mkdir(parents=True, exist_ok=True)
        cache_path = directory / "window_cache.jsonl"
        cached = _load_cache(cache_path)
        for window in bench["windows"]:
            for bias in expand_axis(load_sweep(SWEEPS / f"{cell}.yaml")["axes"]["bias_frac"]):
                jobs.append(
                    {
                        "index": len(jobs),
                        "cell": cell,
                        "window": window,
                        "anchor_fs": int(bench["anchor_fs"]),
                        "lead_fs": int(bench["lead_fs"]),
                        "resolution_fs": int(bench["resolution_fs"]),
                        "bias": bias,
                        "nominal": nominal,
                        "josim_version": josim,
                        "cache": cached,
                    }
                )
    count = worker_count() if workers is None else workers
    started = time.perf_counter()
    done = 0
    total = len(jobs)

    def save(row: dict[str, Any]) -> None:
        nonlocal done
        cache_path = destination / row["cell"] / "window_cache.jsonl"
        for probe in row.pop("probes", []):
            _append_jsonl(cache_path, probe)
        done += 1
        if progress:
            print(format_progress(done, total, time.perf_counter() - started), flush=True)

    if workers == 1:
        rows = []
        for job in jobs:
            row = bisect_job(job)
            save(row)
            rows.append(row)
    else:
        rows = pool_map(bisect_job, jobs, count, on_result=save)
    by_cell: dict[str, list[dict[str, Any]]] = {}
    for row in rows:
        by_cell.setdefault(row["cell"], []).append(row)
    for cell, cell_rows in by_cell.items():
        ordered = sorted(cell_rows, key=lambda item: int(item["index"]))
        write_windows(ordered, destination / cell / "windows.parquet")
    return rows


def bisect_job(job: Mapping[str, Any]) -> dict[str, Any]:
    """One (cell, window, bias) search. JoSIM failures count as functional failures."""
    window = job["window"]
    anchor = int(job["anchor_fs"])
    lead = int(job["lead_fs"])
    resolution = int(job["resolution_fs"])
    bias = job["bias"]
    nominal = str(job["nominal"])
    josim_version = str(job["josim_version"])
    cache: dict[str, dict[str, Any]] = dict(job.get("cache") or {})
    fresh: list[dict[str, Any]] = []
    reasons: dict[int, str] = {}

    def fails(offset: int) -> bool:
        failed, how = _probe(
            nominal,
            bias,
            window,
            anchor,
            lead,
            offset,
            josim_version,
            cache,
            fresh,
        )
        reasons[offset] = how
        return failed

    found = bisect(int(window["lo_fs"]), int(window["hi_fs"]), resolution, fails)
    how = str(found["how"])
    if found["bracket"] == "bracketed":
        how = reasons.get(int(found["fail_fs"]), "")
    else:
        end = int(window["hi_fs"]) if "high fails" in how else int(window["lo_fs"])
        detail = reasons.get(end, "")
        if detail:
            how = f"{how}; {detail}"
    return {
        "index": int(job["index"]),
        "cell": str(job["cell"]),
        "window": str(window["name"]),
        "bias_frac": _bias_text(bias),
        "window_fs": found["window_fs"],
        "fail_fs": found["fail_fs"],
        "bracket": found["bracket"],
        "how": how,
        "probes": fresh,
    }


def _entry() -> None:
    """Re-import so process-pool workers resolve ``bisect_job`` on this module."""
    multiprocessing.freeze_support()
    parent = str(Path(__file__).resolve().parent)
    if parent not in sys.path:
        sys.path.insert(0, parent)
    import windows as windows_module

    windows_module.main()


def main(argv: Sequence[str] | None = None) -> None:
    """Bisect every characterized bench, then print the review and the DFF plot."""
    args = list(sys.argv[1:] if argv is None else argv)
    if not args:
        args = sorted(path.stem for path in BENCHES.glob("*.yaml"))
    josim_version = version()
    count = worker_count()
    print(f"{len(args)} cells, {count} workers, {josim_version}", flush=True)
    rows = run_cells(args, workers=count, josim_version=josim_version)
    flags = review(rows)
    text = "\n".join(flags) if flags else "no non-monotonic or noisy windows"
    print(text, flush=True)
    review_path = OUT / "windows_review.txt"
    review_path.write_text(text + "\n", encoding="utf-8")
    print(review_path, flush=True)
    if "DFF" in args:
        plot = OUT / "DFF" / "setup_hold_vs_bias.png"
        write_setup_hold_plot(rows, plot)
        print(plot, flush=True)
    for row in rows:
        window_fs = "" if row["window_fs"] is None else str(row["window_fs"])
        print(
            f"{row['cell']} {row['window']} {row['bias_frac']} {row['bracket']} {window_fs} {row['how']}",
            flush=True,
        )


def _probe(
    nominal: str,
    bias: Any,
    window: Mapping[str, Any],
    anchor: int,
    lead: int,
    offset: int,
    josim_version: str,
    cache: dict[str, dict[str, Any]],
    fresh: list[dict[str, Any]],
) -> tuple[bool, str]:
    stimulus = schedule(window["stimulus"], anchor=anchor, lead=lead, offset=offset)
    deck = patch_deck(nominal, bias, stimulus, stop_fs(stimulus))
    params = {
        "bias_frac": _bias_text(bias),
        "offset_fs": offset,
        "window": window["name"],
        "pass": list(window["pass"]),
    }
    key = cache_key(deck, params, josim_version)
    hit = cache.get(key)
    if hit is None:
        hit = _simulate(deck, window["pass"])
        hit["key"] = key
        cache[key] = hit
        fresh.append(hit)
    return hit["outcome"] != "pass", str(hit.get("how") or "")


def _simulate(deck: str, clauses: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    try:
        pulses, warnings = simulate_deck(deck)
    except (JosimFailed, JosimTimeout, JosimOutputError, BenchError, KeyError) as exc:
        how = str(exc) if isinstance(exc, KeyError) else failure_how(exc)
        return {"outcome": "fail", "how": how, "pulses": {}}
    stored = {port: list(times) for port, times in pulses.items()}
    if warnings:
        return {
            "outcome": "fail",
            "how": "josim warning: " + "; ".join(warnings),
            "pulses": stored,
        }
    outcome, how = judge(clauses, pulses)
    return {"outcome": outcome, "how": how, "pulses": stored}


def _eval_time(expr: Any, anchor: int, lead: int, offset: int) -> int:
    if isinstance(expr, int) and not isinstance(expr, bool):
        return expr
    names = {"anchor": anchor, "lead": lead, "offset": offset}
    total = 0
    text = str(expr)
    if not text.strip():
        raise WindowError(f"bad time {expr!r}")
    for part in text.split("+"):
        token = part.strip()
        if token in names:
            total += names[token]
            continue
        try:
            total += int(token)
        except ValueError as exc:
            raise WindowError(f"bad time {expr!r}") from exc
    return total


def _clause(clause: Mapping[str, Any], pulses: Mapping[str, tuple[int, ...]]) -> str | None:
    kind = clause["kind"]
    if kind == "count":
        return _count_how(str(clause["port"]), int(clause["n"]), len(_times(pulses, clause["port"])))
    if kind == "after":
        return _order(clause, pulses, before=False)
    if kind == "before":
        return _order(clause, pulses, before=True)
    if kind == "between":
        return _between(clause, pulses)
    if kind == "paired":
        return _paired(clause, pulses)
    if kind == "nearer":
        return _nearer(clause, pulses)
    if kind == "level":
        return _level(clause, pulses)
    raise WindowError(f"unknown clause {kind}")


def _order(clause: Mapping[str, Any], pulses: Mapping[str, tuple[int, ...]], *, before: bool) -> str | None:
    port = str(clause["port"])
    ref = str(clause["ref"])
    err, left = _at(pulses, port, clause.get("index"), sole="index" not in clause)
    if err:
        return err
    err, right = _at(pulses, ref, clause.get("ref_index"), sole="ref_index" not in clause)
    if err:
        return err
    if left is None or right is None:
        raise WindowError(f"{port} order clause has no pulse")
    misplaced = left >= right if before else left <= right
    if misplaced:
        return f"{port}: misplaced"
    return None


def _between(clause: Mapping[str, Any], pulses: Mapping[str, tuple[int, ...]]) -> str | None:
    port = str(clause["port"])
    times = _times(pulses, port)
    how = _count_how(port, 1, len(times))
    if how:
        return how
    earlier_name = str(clause["earlier"])
    later_name = str(clause["later"])
    earlier = _times(pulses, earlier_name)
    later = _times(pulses, later_name)
    if not earlier:
        return f"{earlier_name}: missing, expected 1 got 0"
    if not later:
        return f"{later_name}: missing, expected 1 got 0"
    if not (times[0] > earlier[0] and times[0] < later[0]):
        return f"{port}: misplaced"
    return None


def _paired(clause: Mapping[str, Any], pulses: Mapping[str, tuple[int, ...]]) -> str | None:
    port = str(clause["port"])
    out = _times(pulses, port)
    ref = _times(pulses, clause["ref"])
    how = _count_how(port, len(ref), len(out))
    if how:
        return how
    for index, time in enumerate(out):
        if time <= ref[index]:
            return f"{port}: misplaced"
        if index + 1 < len(ref) and time >= ref[index + 1]:
            return f"{port}: misplaced"
    return None


def _nearer(clause: Mapping[str, Any], pulses: Mapping[str, tuple[int, ...]]) -> str | None:
    port = str(clause["port"])
    out = _times(pulses, port)
    ref = _times(pulses, clause["ref"])
    how = _count_how(port, len(ref), len(out))
    if how:
        return how
    for index, time in enumerate(out):
        own = abs(time - ref[index])
        for other_index, other in enumerate(ref):
            if other_index != index and own >= abs(time - other):
                return f"{port}: misplaced"
    return None


def _level(clause: Mapping[str, Any], pulses: Mapping[str, tuple[int, ...]]) -> str | None:
    """SFQDC: q[0] > a[0], q[-1] > a[1], and q[-1] - a[1] < a[1] - a[0]."""
    port = str(clause["port"])
    ref_name = str(clause["ref"])
    out = _times(pulses, port)
    ref = _times(pulses, ref_name)
    if len(ref) < 2:
        return _count_how(ref_name, 2, len(ref))
    if len(out) < 2:
        return _count_how(port, 2, len(out))
    if not (out[0] > ref[0] and out[-1] > ref[1] and out[-1] - ref[1] < ref[1] - ref[0]):
        return f"{port}: misplaced"
    return None


def _at(
    pulses: Mapping[str, tuple[int, ...]],
    port: str,
    index: Any,
    *,
    sole: bool,
) -> tuple[str | None, int | None]:
    times = _times(pulses, port)
    if sole:
        how = _count_how(port, 1, len(times))
        if how:
            return how, None
        return None, times[0]
    chosen = 0 if index is None else int(index)
    if chosen < 0 or chosen >= len(times):
        return f"{port}: missing, expected {chosen + 1} got {len(times)}", None
    return None, times[chosen]


def _times(pulses: Mapping[str, tuple[int, ...]], port: Any) -> tuple[int, ...]:
    return pulses.get(str(port), ())


def _count_how(port: str, expected: int, got: int) -> str | None:
    if got == expected:
        return None
    word = "missing" if got < expected else "extra"
    return f"{port}: {word}, expected {expected} got {got}"


def _unbracketed(how: str) -> dict[str, Any]:
    return {"bracket": "unbracketed", "window_fs": None, "fail_fs": None, "how": how}


def _series_flags(cell: str, window: str, ordered: Sequence[Mapping[str, Any]]) -> list[str]:
    values: list[int] = []
    seen = False
    gap = False
    hole = False
    for row in ordered:
        bracketed = row.get("bracket") == "bracketed" and row.get("window_fs") is not None
        if not bracketed:
            if seen:
                gap = True
            continue
        if seen and gap:
            hole = True
        gap = False
        seen = True
        values.append(int(row["window_fs"]))
    signed: list[int] = []
    noisy = False
    for left, right in zip(values, values[1:], strict=False):
        delta = right - left
        if abs(delta) > NOISE_FS:
            noisy = True
        if delta != 0:
            signed.append(delta)
    flags: list[str] = []
    if any(left * right < 0 for left, right in zip(signed, signed[1:], strict=False)):
        flags.append(f"{cell} {window} non-monotonic")
    if noisy:
        flags.append(f"{cell} {window} noisy")
    if hole:
        flags.append(f"{cell} {window} hole")
    return flags


def _bias_text(value: Any) -> str:
    return format(Decimal(str(value)), "f")


def _load_bench(cell: str) -> dict[str, Any]:
    path = BENCHES / f"{cell}.yaml"
    loaded = yaml.safe_load(path.read_text(encoding="utf-8"))
    if not isinstance(loaded, dict) or "windows" not in loaded:
        raise WindowError(f"{cell} has no windows")
    return loaded


if __name__ == "__main__":
    _entry()
