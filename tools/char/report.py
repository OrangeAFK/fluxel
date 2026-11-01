"""Tier-1 versus JoSIM report for the M2.6 golden suite.

Reads committed golden pulse logs. Does not run JoSIM and does not change a
LUT or a kernel delay. A missed §8 threshold is recorded in the HTML; the
process still exits 0 when the report is written.
"""

from __future__ import annotations

import json
import math
import subprocess
import tempfile
from collections import Counter
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from bench import LIB
from diff import diff
from pulselog import load
from suite import _fx_command, logic_depth, nominal_delay

ROOT = Path(__file__).resolve().parents[2]

# Two-sided 95% Student-t critical values. df above the table uses 1.96.
_T_975 = {
    1: 12.7062047364,
    2: 4.3026527299,
    3: 3.1824463053,
    4: 2.7764451052,
    5: 2.5705818366,
    6: 2.4469118511,
    7: 2.3646242516,
    8: 2.3060041353,
    9: 2.2621571628,
    10: 2.2281388519,
    11: 2.2009851601,
    12: 2.1788128297,
    13: 2.1603686565,
    14: 2.1447866879,
    15: 2.1314495456,
    16: 2.1199052992,
    17: 2.1098155778,
    18: 2.1009220402,
    19: 2.0930240544,
    20: 2.0859634473,
    21: 2.0796138447,
    22: 2.0738730679,
    23: 2.0686576104,
    24: 2.0638985616,
    25: 2.0595385528,
    26: 2.0555294386,
    27: 2.0518305165,
    28: 2.0484071418,
    29: 2.0452296421,
    30: 2.0422724563,
    40: 2.021075383,
    60: 2.000297822,
    120: 1.979930405,
}


class ReportError(RuntimeError):
    """The comparison could not be run."""


@dataclass(frozen=True)
class Fit:
    slope: float
    lo: float
    hi: float


@dataclass(frozen=True)
class TimestampStats:
    mean_fs: float
    max_fs: int
    mean_pct: float
    max_pct: float
    histogram_fs: tuple[tuple[int, int], ...]
    errors_fs: tuple[int, ...]
    errors_pct: tuple[float, ...]


def split_mismatches(
    rows: Sequence[tuple[str, int, str]],
    uncharacterized: set[str],
) -> tuple[tuple[tuple[str, int, str], ...], tuple[tuple[str, int, str], ...]]:
    """Split ``(port, time, kind)`` rows by whether that instance left its LUT box."""
    inside: list[tuple[str, int, str]] = []
    outside: list[tuple[str, int, str]] = []
    for row in rows:
        inst, _, _port = row[0].rpartition(".")
        if inst in uncharacterized:
            outside.append(row)
        else:
            inside.append(row)
    return tuple(inside), tuple(outside)


def timestamp_stats(
    pairs: Sequence[tuple[str, int, int]],
    cells: Mapping[str, str],
    nominal: Mapping[str, int],
) -> TimestampStats:
    """Signed errors. ``max_*`` is the maximum absolute error.

    Each pulse is a percent of the magnitude of its firing cell's nominal delay.
    """
    errors: list[int] = []
    pcts: list[float] = []
    for port, ref_t, cand_t in pairs:
        inst, _, _name = port.rpartition(".")
        err = cand_t - ref_t
        errors.append(err)
        pcts.append(100.0 * err / abs(nominal[cells[inst]]))
    if not errors:
        return TimestampStats(0.0, 0, 0.0, 0.0, (), (), ())
    histogram = tuple(sorted(Counter(errors).items()))
    return TimestampStats(
        mean_fs=sum(errors) / len(errors),
        max_fs=max(abs(err) for err in errors),
        mean_pct=sum(pcts) / len(pcts),
        max_pct=max(abs(pct) for pct in pcts),
        histogram_fs=histogram,
        errors_fs=tuple(errors),
        errors_pct=tuple(pcts),
    )


def _anchor_fs(nominal: Mapping[str, int]) -> int:
    """Largest delay magnitude. Pulse alignment ignores this scale."""
    return max(abs(value) for value in nominal.values())


def linreg_slope_ci(xs: Sequence[float], ys: Sequence[float]) -> Fit | None:
    """OLS slope of ``ys`` on ``xs`` with a 95% CI.

    Undefined when there are fewer than 3 points or ``xs`` does not vary.
    A perfect line has a zero-width interval.
    """
    xs = [float(value) for value in xs]
    ys = [float(value) for value in ys]
    n = len(xs)
    if n < 3 or n != len(ys):
        return None
    xmean = sum(xs) / n
    ymean = sum(ys) / n
    sxx = sum((x - xmean) ** 2 for x in xs)
    if sxx == 0:
        return None
    slope = sum((x - xmean) * (y - ymean) for x, y in zip(xs, ys, strict=True)) / sxx
    intercept = ymean - slope * xmean
    sse = sum((y - (intercept + slope * x)) ** 2 for x, y in zip(xs, ys, strict=True))
    if sse == 0:
        return Fit(slope, slope, slope)
    se = math.sqrt(sse / (n - 2) / sxx)
    delta = _t_975(n - 2) * se
    return Fit(slope, slope - delta, slope + delta)


def count_uncharacterized(data: bytes) -> int:
    """Count ``Uncharacterized`` events in a kernel CBOR log."""
    return _count_variant(cbor_loads(data), "Uncharacterized")


def render_html(entries: Sequence[Mapping[str, Any]]) -> str:
    """One self-contained page. The DCSFQ wrapper offset is stated and not removed."""
    parts = [
        "<!DOCTYPE html>",
        "<html><head><meta charset=\"utf-8\"><title>M2.6 tier 1 vs JoSIM</title></head><body>",
        "<h1>M2.6 tier 1 vs JoSIM</h1>",
        "<p>The JoSIM deck inserts a DCSFQ before every input and an SFQDC after every output. "
        "Those wrappers are not in the kernel design, so absolute timestamps include that "
        "converter delay. The report does not subtract it.</p>",
        "<p>§8 max timestamp error: 10% of nominal cell delay. "
        "Drift: the 95% CI of the error-vs-depth slope includes 0. "
        "Functional: mismatched pulses inside the characterized region = 0.</p>",
    ]
    for entry in entries:
        parts.append(_entry_html(entry))
    parts.append("</body></html>")
    return "\n".join(parts)


def run_report(manifest: Path | str, golden: Path | str, out: Path | str) -> None:
    """Simulate each manifest row and write ``out/index.html``.

    Raises ``ReportError`` when a golden entry is missing or ``fx-cli`` fails.
    """
    manifest_path = Path(manifest)
    golden_dir = Path(golden)
    out_dir = Path(out)
    rows = json.loads(manifest_path.read_text(encoding="utf-8"))
    missing = [
        row["name"]
        for row in rows
        if not (golden_dir / row["name"] / "pulses.json").is_file()
    ]
    if missing:
        raise ReportError("missing golden entry " + ", ".join(missing))
    suite_dir = manifest_path.parent
    sections = []
    for row in rows:
        design = json.loads((suite_dir / row["design"]).read_text(encoding="utf-8"))
        reference = load((golden_dir / row["name"] / "pulses.json").read_text(encoding="utf-8"))
        candidate, log = simulate_case(
            suite_dir / row["design"],
            suite_dir / row["stimulus"],
            str(row["bias"]),
        )
        sections.append(_measure(row, design, reference, candidate, log))
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "index.html").write_text(render_html(sections), encoding="utf-8", newline="\n")


def simulate_case(design: Path, stimulus: Path, bias: str) -> tuple[dict[str, tuple[int, ...]], bytes]:
    """Run ``fx-cli sim`` at ``bias``. Exit 2 is a recorded violation, not a failure."""
    with tempfile.TemporaryDirectory() as tmp:
        directory = Path(tmp)
        pulses = directory / "pulses.json"
        log = directory / "log.cbor"
        command = [
            *_fx_command(),
            "sim",
            "--design",
            str(design),
            "--stimulus",
            str(stimulus),
            "--lib",
            str(LIB),
            "--out",
            str(pulses),
            "--log",
            str(log),
            "--bias",
            bias,
        ]
        proc = subprocess.run(
            command,
            cwd=ROOT,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            check=False,
        )
        if proc.returncode not in (0, 2):
            detail = proc.stderr.strip() or proc.stdout.strip()
            raise ReportError(detail or f"fx-cli sim exited {proc.returncode}")
        return load(pulses.read_text(encoding="utf-8")), log.read_bytes()


def main(argv: Sequence[str] | None = None) -> int:
    import sys

    args = list(sys.argv[1:] if argv is None else argv)
    suite = ROOT / "tools" / "char" / "suite" / "manifest.json"
    golden = ROOT / "golden"
    out = ROOT / "reports" / "m2"
    index = 0
    while index < len(args):
        flag = args[index]
        if flag not in {"--suite", "--golden", "--out"} or index + 1 >= len(args):
            print(
                "usage: report.py --suite <manifest> --out <dir> [--golden <dir>]",
                file=sys.stderr,
            )
            return 2
        value = Path(args[index + 1])
        if flag == "--suite":
            suite = value
        elif flag == "--golden":
            golden = value
        else:
            out = value
        index += 2
    try:
        run_report(suite, golden, out)
    except ReportError as exc:
        print(exc, file=sys.stderr)
        return 1
    print(out / "index.html", flush=True)
    return 0


def _measure(
    row: Mapping[str, Any],
    design: Mapping[str, Any],
    reference: Mapping[str, Sequence[int]],
    candidate: Mapping[str, Sequence[int]],
    log: bytes,
) -> dict[str, Any]:
    cells = {name: inst["cell"] for name, inst in design["instances"].items()}
    nominal = {cell: nominal_delay(cell) for cell in set(cells.values())}
    result = diff(reference, candidate, nominal_fs=_anchor_fs(nominal))
    mismatches = tuple((port, time, "missing") for port, time in result.missing)
    mismatches += tuple((port, time, "extra") for port, time in result.extra)
    inside, outside = split_mismatches(mismatches, _uncharacterized_instances(log))
    stats = timestamp_stats(result.pairs, cells, nominal)
    depths = logic_depth(design)
    xs = []
    for port, _ref, _cand in result.pairs:
        inst, _, _name = port.rpartition(".")
        xs.append(depths[inst])
    fit = linreg_slope_ci(xs, stats.errors_fs)
    return {
        "name": row["name"],
        "note": row.get("note") or "",
        "inside": inside,
        "outside": outside,
        "mean_fs": stats.mean_fs,
        "max_fs": stats.max_fs,
        "mean_pct": stats.mean_pct,
        "max_pct": stats.max_pct,
        "errors_fs": stats.errors_fs,
        "errors_pct": stats.errors_pct,
        "depths": tuple(xs),
        "slope": None if fit is None else fit.slope,
        "lo": None if fit is None else fit.lo,
        "hi": None if fit is None else fit.hi,
        "uncharacterized": count_uncharacterized(log),
    }


def _entry_html(entry: Mapping[str, Any]) -> str:
    inside = tuple(entry.get("inside") or ())
    outside = tuple(entry.get("outside") or ())
    errors = tuple(entry.get("errors_fs") or ())
    pcts = tuple(entry.get("errors_pct") or ())
    depths = tuple(entry.get("depths") or ())
    lines = [
        f"<h2>{_esc(entry['name'])}</h2>",
        f"<p>{_esc(entry.get('note') or '')}</p>",
        f"<p>inside ({len(inside)})</p>",
        _list(inside),
        f"<p>outside ({len(outside)})</p>",
        _list(outside),
        (
            f"<p>mean {entry['mean_fs']} fs ({entry['mean_pct']}%), "
            f"max {entry['max_fs']} fs ({entry['max_pct']}%). "
            "§8 line: 10% of nominal cell delay.</p>"
        ),
        f"<p>Uncharacterized: {entry['uncharacterized']}</p>",
        f"<p>{_esc(_slope_text(entry))}</p>",
        _hist_svg(pcts if pcts else errors, percent=bool(pcts)),
        _scatter_svg(depths, errors),
    ]
    return "\n".join(lines)


def _slope_text(entry: Mapping[str, Any]) -> str:
    if entry.get("lo") is None:
        return "error vs depth: undefined"
    return (
        f"error vs depth slope {entry['slope']} "
        f"95% CI [{entry['lo']}, {entry['hi']}]"
    )


def _list(rows: Sequence[tuple[str, int, str]]) -> str:
    if not rows:
        return "<ul></ul>"
    items = "".join(
        f"<li>{_esc(kind)} {_esc(port)} {time}</li>" for port, time, kind in rows
    )
    return f"<ul>{items}</ul>"


def _hist_svg(values: Sequence[float], *, percent: bool) -> str:
    def draw(ax: Any) -> None:
        if values:
            ax.hist(list(values), bins=min(21, max(1, len(set(values)))))
        if percent:
            ax.axvline(10, color="crimson", linewidth=1)
            ax.axvline(-10, color="crimson", linewidth=1)
            ax.set_xlabel("% of nominal cell delay")
        else:
            ax.set_xlabel("timestamp error (fs)")
        ax.set_title("10% of nominal cell delay")

    return _svg(draw)


def _scatter_svg(depths: Sequence[int], errors: Sequence[int]) -> str:
    def draw(ax: Any) -> None:
        points = list(zip(depths, errors, strict=False))
        if points:
            ax.scatter([item[0] for item in points], [item[1] for item in points])
        ax.set_xlabel("logic depth")
        ax.set_ylabel("timestamp error (fs)")

    return _svg(draw)


def _svg(draw: Any) -> str:
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    fig, ax = plt.subplots(figsize=(4.5, 3))
    draw(ax)
    import io

    buf = io.StringIO()
    fig.savefig(buf, format="svg")
    plt.close(fig)
    text = buf.getvalue()
    return text[text.find("<svg") :]


def _t_975(df: int) -> float:
    if df in _T_975:
        return _T_975[df]
    if df > 120:
        return 1.9599639845
    lower = max(key for key in _T_975 if key <= df)
    return _T_975[lower]


def _count_variant(node: Any, name: str) -> int:
    if isinstance(node, list):
        return sum(_count_variant(item, name) for item in node)
    if isinstance(node, dict):
        found = sum(1 for key in node if key == name)
        return found + sum(_count_variant(value, name) for value in node.values())
    return 0


def _uncharacterized_instances(data: bytes) -> set[str]:
    found: set[str] = set()

    def walk(node: Any) -> None:
        if isinstance(node, list):
            for item in node:
                walk(item)
            return
        if not isinstance(node, dict):
            return
        event = node.get("Uncharacterized")
        if isinstance(event, dict) and isinstance(event.get("inst"), str):
            found.add(event["inst"])
        for value in node.values():
            walk(value)

    walk(cbor_loads(data))
    return found


def cbor_loads(data: bytes) -> Any:
    import cbor2

    return cbor2.loads(data)


def _esc(value: object) -> str:
    return (
        str(value)
        .replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
    )


if __name__ == "__main__":
    import sys

    sys.exit(main())
