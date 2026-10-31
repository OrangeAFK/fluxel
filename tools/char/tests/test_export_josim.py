"""Exported JoSIM decks become instance.port pulse logs."""

import math
import shutil
import subprocess
from pathlib import Path

import pytest

from extract import pulse_log
from josim import run_with_log, warning_lines

ROOT = Path(__file__).resolve().parents[3]
PI = math.pi
EXAMPLES = ("jtl_chain", "splitter_tree", "shift_register", "full_adder")


def test_map_comments_become_instance_ports() -> None:
    deck = "\n".join(
        [
            "* fluxel-map j0.a P(B1|j0)",
            "* fluxel-map j0.q P(B2|j0)",
            ".print p(B1.j0) p(B2.j0)",
        ]
    )
    # 0 → 2π between 0 and 2 ps crosses π at 1 ps = 1000 fs. A flat trace is no pulse.
    # A falling 2π → 0 step is an antipulse and is not logged.
    rise = (0.0, 2e-12)
    traces = {
        "P(B2|j0)": (rise, (0.0, 2 * PI)),
        "P(B1|j0)": ((0.0, 2e-12, 4e-12), (2 * PI, 0.0, 0.0)),
    }

    assert pulse_log(deck, traces) == {"j0.q": (1000,)}


def test_banner_and_progress_are_clean() -> None:
    log = "JoSIM v2.7\npath/to/file.cir\n[==========] 50%\n"

    assert warning_lines(log) == []


def test_warning_line_is_reported() -> None:
    log = "JoSIM v2.7\n[==] 10%\nWarning: timestep\n"

    assert warning_lines(log) == ["Warning: timestep"]


def test_josim_w_prefix_is_a_warning() -> None:
    log = "JoSIM v2.7\nW: Controls\nUnknown device/node B1|J0\n"

    assert warning_lines(log) == ["W: Controls"]


def _run_fx(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["cargo", "run", "-p", "fx-cli", "--quiet", "--", *args],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )


@pytest.mark.skipif(shutil.which("josim-cli") is None, reason="josim-cli not on PATH")
@pytest.mark.parametrize("name", EXAMPLES)
def test_example_pulse_counts(name: str, tmp_path: Path) -> None:
    design = ROOT / "examples" / name / "design.json"
    stimulus = ROOT / "examples" / name / "stimulus.json"
    lib = ROOT / "lib" / "families" / "rsfq"
    pulses = tmp_path / "pulses.json"
    deck_path = tmp_path / "deck.cir"

    sim = _run_fx(
        "sim",
        "--design",
        str(design),
        "--stimulus",
        str(stimulus),
        "--lib",
        str(lib),
        "--out",
        str(pulses),
    )
    assert sim.returncode == 0, sim.stderr
    exported = _run_fx(
        "export-josim",
        "--design",
        str(design),
        "--stimulus",
        str(stimulus),
        "--lib",
        str(lib),
        "--out",
        str(deck_path),
    )
    assert exported.returncode == 0, exported.stderr

    deck = deck_path.read_text(encoding="utf-8")
    traces, log = run_with_log(deck_path, timeout=180)
    assert warning_lines(log) == [], log

    from pulselog import load

    kernel = load(pulses.read_text(encoding="utf-8"))
    got = pulse_log(deck, traces)
    mismatches = []
    lines = [name]
    for key, times in kernel.items():
        josim = got.get(key, ())
        deltas = [j - k for j, k in zip(josim, times)]
        lines.append(f"  {key} kernel={list(times)} josim={list(josim)} delta_fs={deltas}")
        if len(josim) != len(times):
            mismatches.append(f"{key}: kernel {len(times)} josim {len(josim)}")
    report = "\n".join(lines)
    print(report)
    assert not mismatches, report + "\n" + log
