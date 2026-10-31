"""Characterization benches: source cells, one DUT, load cells.

SPEC §4.2. ``render`` builds a design of source, DUT, and load, then calls
``fx-cli export-josim``, the same deck a composite uses. Picoseconds are
converted to femtoseconds only at this boundary.
"""

from __future__ import annotations

import json
import subprocess
import tempfile
from collections.abc import Iterable, Mapping
from decimal import Decimal
from pathlib import Path
from typing import Any, NamedTuple

import yaml

from extract import extract_crossings
from josim import run_with_log, warning_lines

ROOT = Path(__file__).resolve().parents[2]
LIB = ROOT / "lib" / "families" / "rsfq"
BENCHES = Path(__file__).resolve().parent / "benches"
_DEFAULT_STEP_PS = Decimal("0.025")


class BenchError(RuntimeError):
    """A bench could not be rendered or its JoSIM log was not clean."""


class Arc(NamedTuple):
    src: str
    dst: str
    cause_fs: int
    delay_fs: int
    tag: str = ""


class Nominal(NamedTuple):
    cell: str
    inputs: tuple[str, ...]
    outputs: tuple[str, ...]
    pulses: dict[str, tuple[int, ...]]
    arcs: tuple[Arc, ...]


def render(template: Mapping[str, Any] | Path | str, params: Mapping[str, Any] | None = None) -> str:
    """Turn a bench template into a JoSIM deck."""
    spec = _load(template)
    if params:
        spec.update(params)
    design, stimulus = _design(spec)
    step = spec.get("timestep_ps", _DEFAULT_STEP_PS)
    if step is None:
        step = _DEFAULT_STEP_PS
    extra = ["--bias", _bias_token(spec.get("bias_frac", 1)), "--step", str(_ps_to_fs(step))]
    if spec.get("length_ps") is not None:
        extra.extend(["--stop", str(_ps_to_fs(spec["length_ps"]))])
    if spec["source"] == "current":
        extra.append("--current-in")
    if spec["load"] == "resistor":
        extra.append("--resistor-out")
    with tempfile.TemporaryDirectory() as tmp:
        directory = Path(tmp)
        design_path = directory / "design.json"
        stimulus_path = directory / "stimulus.json"
        out_path = directory / "bench.cir"
        design_path.write_text(json.dumps(design), encoding="utf-8")
        stimulus_path.write_text(json.dumps(stimulus), encoding="utf-8")
        command = [
            "cargo",
            "run",
            "-p",
            "fx-cli",
            "--quiet",
            "--",
            "export-josim",
            "--design",
            str(design_path),
            "--stimulus",
            str(stimulus_path),
            "--lib",
            str(LIB),
            "--out",
            str(out_path),
            *extra,
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
        if proc.returncode != 0:
            detail = proc.stderr.strip() or proc.stdout.strip()
            raise BenchError(detail or f"export-josim exited {proc.returncode}")
        return out_path.read_text(encoding="utf-8")


def run(deck: str, *, timeout: float = 180) -> dict[str, tuple[int, ...]]:
    """Crossing times, in femtoseconds, at the DUT reference junction of each port.

    An upward (2k+1)π crossing is the port time. A junction that only slips
    downward, as DCSFQ ``B1`` does, uses those crossing times instead.
    """
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "bench.cir"
        path.write_text(deck, encoding="utf-8")
        traces, log = run_with_log(path, timeout=timeout)
    warnings = warning_lines(log)
    if warnings:
        raise BenchError("\n".join(warnings))
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
        chosen = crossings.pulses or crossings.antipulses
        times = tuple(int(round(item.t * 1e15)) for item in chosen)
        if times:
            found[key.removeprefix("dut.")] = times
    return found


def delays(pulses: Mapping[str, tuple[int, ...]], inputs: Iterable[str]) -> tuple[Arc, ...]:
    """Pair each output crossing with the nearest input crossing.

    The delay is signed, so an output junction that switches first is negative.
    An output that keeps crossing while a level is high, as SFQDC ``B8`` does,
    reports the delay to the first crossing and from the last input to the last
    crossing.
    """
    input_ports = set(inputs)
    events = sorted(
        (time, port) for port, times in pulses.items() if port in input_ports for time in times
    )
    if not events:
        raise BenchError("bench has no input crossings")
    outputs = {
        port: times for port, times in pulses.items() if port not in input_ports and times
    }
    if any(len(times) > len(events) for times in outputs.values()):
        return _level_arcs(events, outputs)
    found: list[Arc] = []
    for port, times in outputs.items():
        for arrival in times:
            cause_fs, src = min(events, key=lambda event: (abs(event[0] - arrival), event[0]))
            found.append(Arc(src, port, cause_fs, arrival - cause_fs))
    found.sort(key=lambda arc: (arc.cause_fs + arc.delay_fs, arc.dst, arc.src))
    return tuple(found)


def _level_arcs(
    events: list[tuple[int, str]],
    outputs: Mapping[str, tuple[int, ...]],
) -> tuple[Arc, ...]:
    arcs: list[Arc] = []
    first_fs, first_port = events[0]
    last_fs, last_port = events[-1]
    for port, times in outputs.items():
        arcs.append(Arc(first_port, port, first_fs, times[0] - first_fs, "on"))
        arcs.append(Arc(last_port, port, last_fs, times[-1] - last_fs, "off"))
    return tuple(arcs)


def measure_nominal(*, timeout: float = 180) -> list[Nominal]:
    """Run every bench at its nominal parameters."""
    rows: list[Nominal] = []
    for path in sorted(BENCHES.glob("*.yaml")):
        spec = yaml.safe_load(path.read_text(encoding="utf-8"))
        cell = spec["cell"]
        inputs, outputs = _ports(cell)
        pulses = run(render(path), timeout=timeout)
        rows.append(Nominal(cell, tuple(inputs), tuple(outputs), pulses, delays(pulses, inputs)))
    return rows


def format_table(rows: Iterable[Nominal]) -> str:
    """``cell | inputs | outputs | measured delay(s)``, times in picoseconds."""
    lines = ["cell | inputs | outputs | measured delay(s)"]
    for row in rows:
        lines.append(
            f"{row.cell} | {_port_times(row.pulses, row.inputs)} | "
            f"{_port_times(row.pulses, row.outputs)} | {_delay_text(row.arcs)}"
        )
    return "\n".join(lines)


def nominal_table(*, timeout: float = 180) -> str:
    """Print and return the nominal bench table."""
    text = format_table(measure_nominal(timeout=timeout))
    print(text, flush=True)
    return text


def _load(template: Mapping[str, Any] | Path | str) -> dict[str, Any]:
    if isinstance(template, Mapping):
        return dict(template)
    loaded = yaml.safe_load(Path(template).read_text(encoding="utf-8"))
    if not isinstance(loaded, dict):
        raise BenchError(f"{template} is not a bench mapping")
    return loaded


def _design(spec: Mapping[str, Any]) -> tuple[dict[str, Any], dict[str, list[int]]]:
    cell = str(spec["cell"])
    source = spec["source"]
    load = spec["load"]
    inputs, outputs = _ports(cell)
    raw = spec.get("inputs") or {}
    instances: dict[str, dict[str, str]] = {
        "dut": {"family": "rsfq", "cell": cell, "bias_domain": "b"}
    }
    nets: list[dict[str, Any]] = []

    def connect(driver: dict[str, str], sink: dict[str, str]) -> None:
        nets.append({"driver": driver, "sink": sink, "wire": {"kind": "direct"}})

    if source == "JTL":
        for name in inputs:
            src = f"src_{name}"
            instances[src] = {"family": "rsfq", "cell": "JTL", "bias_domain": "b"}
            connect({"port": name}, {"instance": src, "port": "a"})
            connect({"instance": src, "port": "q"}, {"instance": "dut", "port": name})
    elif source == "current":
        for name in inputs:
            connect({"port": name}, {"instance": "dut", "port": name})
    else:
        raise BenchError(f"unknown source {source}")

    if load == "JTL":
        for name in outputs:
            ld = f"load_{name}"
            instances[ld] = {"family": "rsfq", "cell": "JTL", "bias_domain": "b"}
            connect({"instance": "dut", "port": name}, {"instance": ld, "port": "a"})
            connect({"instance": ld, "port": "q"}, {"port": name})
    elif load == "resistor":
        for name in outputs:
            connect({"instance": "dut", "port": name}, {"port": name})
    else:
        raise BenchError(f"unknown load {load}")

    design = {
        "instances": instances,
        "nets": nets,
        "ports": {"in": inputs, "out": outputs},
        "clock_domains": {},
        "bias_domains": ["b"],
    }
    stimulus = {name: [_ps_to_fs(time) for time in raw.get(name, [])] for name in inputs}
    return design, stimulus


def _ports(cell: str) -> tuple[list[str], list[str]]:
    data = json.loads((LIB / f"{cell}.cell.json").read_text(encoding="utf-8"))
    return list(data["ports"]["in"]), list(data["ports"]["out"])


def _ps_to_fs(value: int | float | str) -> int:
    femtoseconds = Decimal(str(value)) * Decimal(1000)
    if femtoseconds != femtoseconds.to_integral_value():
        raise BenchError(f"{value} ps is not an integer number of femtoseconds")
    return int(femtoseconds)


def _bias_token(value: int | float) -> str:
    text = format(Decimal(str(value)), "f")
    if "." in text:
        text = text.rstrip("0").rstrip(".")
    return text


def _port_times(pulses: Mapping[str, tuple[int, ...]], ports: Iterable[str]) -> str:
    return " ".join(
        f"{port}={','.join(f'{time / 1000:.3f}' for time in pulses.get(port, ()))}" for port in ports
    )


def _delay_text(arcs: Iterable[Arc]) -> str:
    arcs = tuple(arcs)
    if any(arc.tag for arc in arcs):
        head = f"{arcs[0].src}→{arcs[0].dst}"
        body = ", ".join(f"{arc.tag} {arc.delay_fs / 1000:.3f} ps" for arc in arcs)
        return f"{head} {body}"
    grouped: dict[str, list[str]] = {}
    for arc in arcs:
        grouped.setdefault(f"{arc.src}→{arc.dst}", []).append(f"{arc.delay_fs / 1000:.3f}")
    return " ".join(f"{name} {', '.join(values)} ps" for name, values in grouped.items())


if __name__ == "__main__":
    nominal_table()
