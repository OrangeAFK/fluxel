#!/usr/bin/env python3
"""Run the exploratory JTL chain in JoSIM and report phase crossings.

Writes only under scratch/step6/. Does not touch lib/rsfqlib, golden, or docs.
"""

import csv
import subprocess
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np

ROOT = Path(__file__).resolve().parents[2]
STEP = Path(__file__).resolve().parent
LIB = ROOT / "lib" / "rsfqlib" / "RSFQlib"
DCSFQ_SRC = LIB / "mitll_DCSFQ" / "THmitll_DCSFQ_v3p0_extracted.cir"
JTL_SRC = LIB / "mitll_JTL" / "THmitll_JTL_v3p0_extracted.cir"

PI = np.pi
STEP_WINDOW_S = 10e-12
JUNCTIONS = (
    "B1.XIN",
    "B2.XIN",
    "B1.XMID",
    "B2.XMID",
    "B1.XLOAD",
    "B2.XLOAD",
)
N_PULSES = 3


def scaled_cell(src: Path, factor: float) -> str:
    text = src.read_text(encoding="utf-8")
    old_bias = ".param BiasCoef=0.7"
    new_bias = f".param BiasCoef={0.7 * factor:.2f}"
    if text.count(old_bias) != 1:
        raise SystemExit(f"{src.name}: expected one {old_bias}")
    text = text.replace(old_bias, new_bias, 1)
    old_ib1 = ".param IB1=275u"
    count = text.count(old_ib1)
    if count == 1:
        text = text.replace(old_ib1, f".param IB1={275 * factor:.0f}u", 1)
    elif count != 0:
        raise SystemExit(f"{src.name}: unexpected IB1 count {count}")
    return text


def top_deck(include_dcsfq: str, include_jtl: str, note: str) -> str:
    return (
        f"* scratch/step6 chain, {note}\n"
        f".include {include_dcsfq}\n"
        f".include {include_jtl}\n"
        "\n"
        "Iin 0 a pwl(0 0 20p 0 23p 600u 26p 0 70p 0 73p 600u 76p 0 "
        "120p 0 123p 600u 126p 0)\n"
        "XSRC  THmitll_DCSFQ a  n1\n"
        "XIN   THmitll_JTL   n1 n2\n"
        "XMID  THmitll_JTL   n2 n3\n"
        "XLOAD THmitll_JTL   n3 n4\n"
        "Rsink n4 0 2\n"
        "\n"
        ".tran 0.025p 300p 0\n"
        ".print p(B1.XIN) p(B2.XIN) p(B1.XMID) p(B2.XMID) "
        "p(B1.XLOAD) p(B2.XLOAD)\n"
        ".end\n"
    )


def write_bias_decks() -> None:
    for factor, tag in ((0.8, "0p8"), (1.2, "1p2")):
        (STEP / f"dcsfq_{tag}.cir").write_text(
            scaled_cell(DCSFQ_SRC, factor), encoding="utf-8"
        )
        (STEP / f"jtl_{tag}.cir").write_text(
            scaled_cell(JTL_SRC, factor), encoding="utf-8"
        )
        (STEP / f"chain_{tag}.cir").write_text(
            top_deck(f"dcsfq_{tag}.cir", f"jtl_{tag}.cir", f"bias {factor}× nominal"),
            encoding="utf-8",
        )


def quiet_log(log: str) -> str:
    """Drop JoSIM's progress bar. Keep the banner and any warning text."""
    kept = []
    for line in log.splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and "%" in stripped:
            continue
        kept.append(line.rstrip())
    return "\n".join(kept).strip()


def run_josim(cir: Path, csv_path: Path) -> str:
    proc = subprocess.run(
        ["josim-cli", "-o", str(csv_path), str(cir)],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    log = quiet_log((proc.stdout or "") + (proc.stderr or ""))
    (csv_path.with_suffix(".log")).write_text(log + "\n", encoding="utf-8")
    if proc.returncode != 0:
        raise SystemExit(f"josim-cli failed on {cir} (exit {proc.returncode})\n{log}")
    return log


def josim_version() -> str:
    proc = subprocess.run(["josim-cli", "-v"], capture_output=True, text=True)
    return ((proc.stdout or "") + (proc.stderr or "")).strip()


def read_traces(path: Path):
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    header_i = None
    for i, line in enumerate(lines):
        stripped = line.strip().lstrip("\ufeff").lower()
        if stripped.startswith("time") or stripped.startswith('"time"'):
            header_i = i
            break
    if header_i is None:
        preview = "\n".join(lines[:40])
        raise SystemExit(f"no CSV header in {path}\n{preview}")
    rows = list(csv.reader(lines[header_i:]))
    rows = [row for row in rows if row and any(cell.strip() for cell in row)]
    header_line = lines[header_i]
    columns = [cell.strip() for cell in rows[0]]
    data = np.array([[float(cell) for cell in row] for row in rows[1:]], dtype=float)
    return header_line, columns, data


def column_index(columns, junction: str) -> int:
    device, instance = junction.split(".")
    hits = [
        i
        for i, name in enumerate(columns)
        if device.upper() in name.upper() and instance.upper() in name.upper()
    ]
    if len(hits) != 1:
        raise SystemExit(f"{junction} matched {hits} in {columns}")
    return hits[0]


def upward_crossings(time_s, phase):
    """Times when phase rises through (2k+1)π, linearly interpolated."""
    times = []
    for i in range(len(phase) - 1):
        y0 = phase[i]
        y1 = phase[i + 1]
        if y1 <= y0:
            continue
        n_first = int(np.floor(y0 / PI)) + 1
        n_last = int(np.floor(y1 / PI))
        for n in range(n_first, n_last + 1):
            if n % 2 == 0:
                continue
            level = n * PI
            frac = (level - y0) / (y1 - y0)
            times.append(time_s[i] + frac * (time_s[i + 1] - time_s[i]))
    return times


def phase_step(time_s, phase, crossing_s):
    after = np.interp(crossing_s + STEP_WINDOW_S, time_s, phase)
    before = np.interp(crossing_s - STEP_WINDOW_S, time_s, phase)
    return float(after - before)


def analyze(header, columns, data):
    time_s = data[:, 0]
    per_junction = {}
    for junction in JUNCTIONS:
        phase = data[:, column_index(columns, junction)]
        crossings = upward_crossings(time_s, phase)
        steps = [phase_step(time_s, phase, tc) for tc in crossings]
        per_junction[junction] = {
            "phase": phase,
            "crossings_ps": [tc * 1e12 for tc in crossings],
            "steps_rad": steps,
        }
    b1 = per_junction["B1.XMID"]["crossings_ps"]
    b2 = per_junction["B2.XMID"]["crossings_ps"]
    paired = min(len(b1), len(b2))
    lags = [b2[i] - b1[i] for i in range(paired)]
    return {
        "header": header,
        "time_ps": time_s * 1e12,
        "junctions": per_junction,
        "lags_ps": lags,
        "unpaired_b1": len(b1) - paired,
        "unpaired_b2": len(b2) - paired,
    }


def plot_phases(result, path: Path, title: str) -> None:
    fig, axes = plt.subplots(len(JUNCTIONS), 1, sharex=True, figsize=(8, 11))
    time_ps = result["time_ps"]
    for ax, junction in zip(axes, JUNCTIONS):
        phase = result["junctions"][junction]["phase"]
        ax.plot(time_ps, phase, color="black", lw=0.8)
        lo = float(phase.min()) - PI
        hi = float(phase.max()) + PI
        n_first = int(np.floor(lo / PI))
        n_last = int(np.ceil(hi / PI))
        for n in range(n_first, n_last + 1):
            if n % 2 == 0:
                continue
            level = n * PI
            if lo <= level <= hi:
                ax.axhline(level, color="0.45", ls="--", lw=0.6)
        ax.set_ylim(lo, hi)
        ax.set_ylabel(junction)
    axes[-1].set_xlabel("time (ps)")
    fig.suptitle(title)
    fig.tight_layout()
    fig.savefig(path, dpi=120)
    plt.close(fig)


def format_result(name, log, result) -> str:
    lines = [f"== {name} ==", "JoSIM log:", log.rstrip(), "", f"CSV header: {result['header']}"]
    for junction in JUNCTIONS:
        info = result["junctions"][junction]
        steps = ", ".join(f"{step:.6f}" for step in info["steps_rad"])
        times = ", ".join(f"{tc:.6f}" for tc in info["crossings_ps"])
        lines.append(
            f"{junction}: crossings {len(info['crossings_ps'])} vs {N_PULSES} input pulses"
        )
        lines.append(f"  t_ps: {times}")
        lines.append(f"  step_rad: {steps}")
    lags = ", ".join(f"{lag:.6f}" for lag in result["lags_ps"])
    lines.append(f"XMID B2-B1 lag_ps: {lags}")
    lines.append(
        f"unpaired crossings: B1 {result['unpaired_b1']}, B2 {result['unpaired_b2']}"
    )
    lines.append("")
    return "\n".join(lines)


def delay_change(nominal, scaled) -> str:
    lines = []
    n = min(len(nominal["lags_ps"]), len(scaled["lags_ps"]))
    for i in range(n):
        delta = scaled["lags_ps"][i] - nominal["lags_ps"][i]
        lines.append(
            f"  pulse {i + 1}: lag {scaled['lags_ps'][i]:.6f} ps, "
            f"change {delta:.6f} ps"
        )
    if len(scaled["lags_ps"]) != len(nominal["lags_ps"]):
        lines.append(
            f"  paired lags {len(scaled['lags_ps'])} vs nominal {len(nominal['lags_ps'])}"
        )
    for junction in JUNCTIONS:
        count = len(scaled["junctions"][junction]["crossings_ps"])
        if count != N_PULSES:
            lines.append(f"  {junction} crossings {count} (input pulses {N_PULSES})")
    if not any("crossings" in line for line in lines):
        lines.append(f"  every plotted junction has {N_PULSES} crossings")
    return "\n".join(lines)


def main() -> None:
    write_bias_decks()
    version = josim_version()
    runs = []
    for name, cir, csv_name, png, title in (
        ("nominal", STEP / "chain.cir", "nominal.csv", "phases.png", "nominal bias"),
        ("0.8x", STEP / "chain_0p8.cir", "bias0p8.csv", "phases_0p8.png", "0.8× bias"),
        ("1.2x", STEP / "chain_1p2.cir", "bias1p2.csv", "phases_1p2.png", "1.2× bias"),
    ):
        print(f"running {name}", flush=True)
        log = run_josim(cir, STEP / csv_name)
        header, columns, data = read_traces(STEP / csv_name)
        result = analyze(header, columns, data)
        plot_phases(result, STEP / png, title)
        runs.append((name, log, result))

    nominal = runs[0][2]
    parts = [f"JoSIM version:\n{version}\n"]
    for name, log, result in runs:
        parts.append(format_result(name, log, result))
    parts.append("== delay change vs nominal, XMID B2 minus B1 ==")
    for name, _log, result in runs[1:]:
        parts.append(name)
        parts.append(delay_change(nominal, result))
    report = "\n".join(parts) + "\n"
    (STEP / "report.txt").write_text(report, encoding="utf-8")
    print(report)


if __name__ == "__main__":
    main()
