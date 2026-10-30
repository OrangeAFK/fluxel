"""Run fx-cli against PyLSE and print a functional match report."""

import json
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

from compare import format_view, functional_view, ps_to_fs
from design_to_pylse import UnknownCell, to_pylse_source
from generate import SEED, generate_cases

MARKER = "FX_ORACLE_JSON"


@dataclass
class Row:
    match: bool
    report: str
    traces: str


def ensure_fx_cli(root: Path) -> Path:
    subprocess.run(
        ["cargo", "build", "-p", "fx-cli", "--offline"],
        cwd=root,
        check=True,
    )
    binary = root / "target" / "debug" / "fx-cli"
    windows = binary.with_suffix(".exe")
    if windows.exists() and not binary.exists():
        return windows
    return binary


def evaluate(
    name: str,
    design: dict,
    stimulus: dict,
    fx_bin: Path,
    lib_dir: Path,
) -> Row:
    fx_pulses, fx_error = _run_fx(design, stimulus, fx_bin, lib_dir)
    try:
        source = to_pylse_source(design, stimulus)
    except UnknownCell as err:
        fx_text = fx_error if fx_error is not None else _view_text(design, stimulus, fx_pulses)
        report = f"{name} | {fx_text} | no built-in for {err.cell} | no"
        traces = _traces(fx_pulses, {}, f"no built-in for {err.cell}")
        return Row(False, report, traces)

    if fx_error is not None:
        report = f"{name} | {fx_error} | | no"
        return Row(False, report, _traces(fx_pulses, {}, fx_error))

    pylse_pulses, pylse_error = _run_pylse(source)
    if pylse_error is not None:
        fx_text = _view_text(design, stimulus, fx_pulses)
        report = f"{name} | {fx_text} | {pylse_error} | no"
        return Row(False, report, _traces(fx_pulses, pylse_pulses, pylse_error))

    fx_view = functional_view(design, stimulus, fx_pulses)
    pylse_view = functional_view(design, stimulus, pylse_pulses)
    matched = fx_view == pylse_view
    fx_text = format_view(*fx_view)
    pylse_text = format_view(*pylse_view)
    flag = "yes" if matched else "no"
    report = f"{name} | {fx_text} | {pylse_text} | {flag}"
    traces = "" if matched else _traces(fx_pulses, pylse_pulses, None)
    return Row(matched, report, traces)


def main() -> int:
    root = Path(__file__).resolve().parents[2]
    fx_bin = ensure_fx_cli(root)
    lib = root / "lib" / "families" / "rsfq"
    cases = list(generate_cases(200, SEED))
    for directory in sorted((root / "examples").iterdir()):
        design_path = directory / "design.json"
        if not design_path.is_file():
            continue
        design = json.loads(design_path.read_text(encoding="utf-8"))
        stimulus = json.loads((directory / "stimulus.json").read_text(encoding="utf-8"))
        cases.append((directory.name, design, stimulus))
    for name, design, stimulus in cases:
        row = evaluate(name, design, stimulus, fx_bin, lib)
        print(row.report, flush=True)
        if not row.match:
            print(row.traces, flush=True)
            return 1
    return 0


def _run_fx(
    design: dict, stimulus: dict, fx_bin: Path, lib_dir: Path
) -> tuple[dict[str, list[int]], str | None]:
    with tempfile.TemporaryDirectory() as tmp:
        folder = Path(tmp)
        design_path = folder / "design.json"
        stimulus_path = folder / "stimulus.json"
        out_path = folder / "pulses.json"
        design_path.write_text(json.dumps(design), encoding="utf-8")
        stimulus_path.write_text(json.dumps(stimulus), encoding="utf-8")
        proc = subprocess.run(
            [
                str(fx_bin),
                "sim",
                "--design",
                str(design_path),
                "--stimulus",
                str(stimulus_path),
                "--lib",
                str(lib_dir),
                "--out",
                str(out_path),
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        if proc.returncode not in (0, 2) or not out_path.is_file():
            detail = (proc.stderr or proc.stdout or "fx-cli failed").strip()
            return {}, detail
        pulses = json.loads(out_path.read_text(encoding="utf-8"))
        return {key: [int(time) for time in times] for key, times in pulses.items()}, None


def _run_pylse(source: str) -> tuple[dict[str, list[int]], str | None]:
    with tempfile.TemporaryDirectory() as tmp:
        script = Path(tmp) / "oracle.py"
        script.write_text(source, encoding="utf-8")
        try:
            proc = subprocess.run(
                [sys.executable, str(script)],
                capture_output=True,
                text=True,
                check=False,
                timeout=30,
            )
        except subprocess.TimeoutExpired:
            return {}, "PyLSE timed out"
    if proc.returncode != 0:
        detail = (proc.stderr or proc.stdout or "PyLSE failed").strip()
        return {}, detail
    marker = proc.stdout.rfind(MARKER)
    if marker < 0:
        return {}, (proc.stderr or proc.stdout or "PyLSE produced no pulse JSON").strip()
    payload = proc.stdout[marker + len(MARKER) :].strip()
    try:
        events = json.loads(payload)
    except json.JSONDecodeError:
        return {}, payload
    return {key: ps_to_fs(times) for key, times in events.items()}, None


def _view_text(design: dict, stimulus: dict, pulses: dict[str, list[int]]) -> str:
    return format_view(*functional_view(design, stimulus, pulses))


def _traces(
    fx_pulses: dict[str, list[int]],
    pylse_pulses: dict[str, list[int]],
    note: str | None,
) -> str:
    lines = ["fx trace:", _dump(fx_pulses), "PyLSE trace:"]
    if note is not None and not pylse_pulses:
        lines.append(note)
    else:
        lines.append(_dump(pylse_pulses))
        if note:
            lines.append(note)
    return "\n".join(lines)


def _dump(pulses: dict[str, list[int]]) -> str:
    if not pulses:
        return "  (none)"
    return "\n".join(f"  {key}: {pulses[key]}" for key in sorted(pulses))


if __name__ == "__main__":
    sys.exit(main())
