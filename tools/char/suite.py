"""G-cells and G-comp cases for the golden suite.

SPEC §8. ``build_cases`` is deterministic. Golden files are written only by
``golden.generate_entry``, and only when ``FLUXEL_GOLDEN_UPDATE=1``.
"""

from __future__ import annotations

import json
import os
import random
import subprocess
import tempfile
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from decimal import Decimal
from functools import lru_cache
from pathlib import Path
from typing import Any

import yaml

from bench import BENCHES, LIB, _design
from golden import GoldenUpdateError, generate_entry
from lut import CHARACTERIZED, _is_window, _nominal_delay, load_cbor, lookup
from sweep import PULSE_SPAN_FS
from windows import schedule

ROOT = Path(__file__).resolve().parents[2]
SUITE = Path(__file__).resolve().parent / "suite"
EXAMPLES = ROOT / "examples"

BIASES = (0.85, 1.0, 1.15)
CYCLES = 16
PERIOD_FS = 200_000
EDGE_FS = 20
ANCHOR_FS = 200_000
LEAD_FS = 50_000
CELL_TIMEOUT_S = 60
COMP_TIMEOUT_S = 600

WindowFn = Callable[[str, str, str], int | None]


@dataclass(frozen=True)
class Case:
    name: str
    group: str
    design: dict[str, Any]
    stimulus: dict[str, list[int]]
    bias: float
    kind: str
    window: str | None
    edge: str | None
    cell: str | None
    timeout_s: int
    note: str


def bias_text(bias: float) -> str:
    """``0.85``, ``1``, ``1.15``."""
    text = format(Decimal(str(bias)), "f")
    if "." in text:
        text = text.rstrip("0").rstrip(".")
    return text


def bias_tag(bias: float) -> str:
    """``b085``, ``b100``, ``b115``."""
    return f"b{int(round(bias * 100)):03d}"


def logic_depth(design: Mapping[str, Any]) -> dict[str, int]:
    """``1 + max(driver depths)``. A primary input has depth 0."""
    drivers: dict[str, list[str | None]] = {}
    for net in design["nets"]:
        sink = net["sink"]
        if "instance" not in sink:
            continue
        driver = net["driver"]
        drivers.setdefault(sink["instance"], []).append(driver.get("instance"))
    memo: dict[str, int] = {}

    def depth(inst: str) -> int:
        if inst not in memo:
            memo[inst] = 0
            srcs = drivers.get(inst, [None])
            memo[inst] = 1 + max(0 if src is None else depth(src) for src in srcs)
        return memo[inst]

    return {inst: depth(inst) for inst in design["instances"]}


def build_cases(window_fs: WindowFn | None = None) -> list[Case]:
    """Every G-cells and G-comp entry, sorted by name."""
    windows = _lut_windows if window_fs is None else window_fs
    cases = _cell_cases(windows)
    cases.extend(_composite_cases(windows))
    cases.sort(key=lambda case: case.name)
    return cases


def write_suite(cases: Sequence[Case], dest: Path) -> None:
    """Write ``manifest.json`` plus one design and stimulus per case."""
    dest = Path(dest)
    dest.mkdir(parents=True, exist_ok=True)
    rows = []
    for case in cases:
        folder = dest / case.name
        folder.mkdir(parents=True, exist_ok=True)
        _dump(folder / "design.json", case.design)
        _dump(folder / "stimulus.json", case.stimulus)
        rows.append(
            {
                "name": case.name,
                "group": case.group,
                "design": f"{case.name}/design.json",
                "stimulus": f"{case.name}/stimulus.json",
                "bias": bias_text(case.bias),
                "kind": case.kind,
                "window": case.window,
                "edge": case.edge,
                "cell": case.cell,
                "timeout_s": case.timeout_s,
                "note": case.note,
            }
        )
    _dump(dest / "manifest.json", rows)


def output_stimulus_yaml(deck: str, design: Mapping[str, Any]) -> str:
    """Golden ``ports:`` map for design output junctions.

    Input junctions and exporter wrappers (``fxsrc_``, ``fxload_``) are omitted.
    The kernel pulse log uses the same ``instance.port`` keys.
    """
    wanted = _output_keys(design)
    lines = ["ports:"]
    for line in deck.splitlines():
        parts = line.split()
        if parts[:2] != ["*", "fluxel-map"] or len(parts) != 4:
            continue
        key, column = parts[2], parts[3]
        inst, _, _port = key.rpartition(".")
        if key not in wanted or inst.startswith(("fxsrc_", "fxload_")):
            continue
        lines.append(f"  {key}: {column}")
    return "\n".join(lines) + "\n"


def export_deck(case: Case, dest: Path) -> None:
    """Write the JoSIM deck for ``case`` with ``fx-cli export-josim``."""
    dest = Path(dest)
    dest.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        directory = Path(tmp)
        design_path = directory / "design.json"
        stimulus_path = directory / "stimulus.json"
        _dump(design_path, case.design)
        _dump(stimulus_path, case.stimulus)
        command = [
            *_fx_command(),
            "export-josim",
            "--design",
            str(design_path),
            "--stimulus",
            str(stimulus_path),
            "--lib",
            str(LIB),
            "--out",
            str(dest),
            "--bias",
            bias_text(case.bias),
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
            raise RuntimeError(detail or f"export-josim exited {proc.returncode}")


def update_golden(cases: Sequence[Case], *, root: Path) -> None:
    """Export each case and write its golden entry. Requires the env flag."""
    if os.environ.get("FLUXEL_GOLDEN_UPDATE") != "1":
        raise GoldenUpdateError("set FLUXEL_GOLDEN_UPDATE=1 to write golden entries")
    root = Path(root)
    with tempfile.TemporaryDirectory() as tmp:
        directory = Path(tmp)
        for case in cases:
            deck = directory / f"{case.name}.cir"
            stimulus = directory / f"{case.name}.yaml"
            export_deck(case, deck)
            stimulus.write_text(
                output_stimulus_yaml(deck.read_text(encoding="utf-8"), case.design),
                encoding="utf-8",
                newline="\n",
            )
            generate_entry(case.name, deck, stimulus, root=root, timeout=case.timeout_s)


def nominal_delay(cell: str) -> int:
    """Largest |delay| at the bundle's nominal point, in femtoseconds."""
    return _nominal_delay(cell, _bundle(cell))


def main(argv: Sequence[str] | None = None) -> None:
    import sys

    args = list(sys.argv[1:] if argv is None else argv)
    cases = build_cases()
    if not args:
        write_suite(cases, SUITE)
        print(SUITE, flush=True)
        return
    if args == ["--update-golden"]:
        try:
            update_golden(cases, root=ROOT)
        except GoldenUpdateError as exc:
            raise SystemExit(str(exc)) from exc
        return
    if len(args) == 2 and args[0] == "--write":
        write_suite(cases, Path(args[1]))
        print(args[1], flush=True)
        return
    raise SystemExit("usage: suite.py [--write <dir> | --update-golden]")


def _cell_cases(window_fs: WindowFn) -> list[Case]:
    cases: list[Case] = []
    for cell in CHARACTERIZED:
        bench = _bench(cell)
        design = _cell_design(cell)
        ports = list(_cell_ports(cell)[0])
        clocked = "clk" in ports
        anchor = int(bench["anchor_fs"])
        lead = int(bench["lead_fs"])
        for bias in BIASES:
            tag = bias_tag(bias)
            name = f"gcell_{cell}_random_{tag}"
            cases.append(
                _case(
                    name=name,
                    group="g-cells",
                    design=design,
                    stimulus=_random_stream(
                        ports,
                        clock="clk" if clocked else None,
                        anchor=anchor,
                        lead=lead,
                        seed=f"fluxel-m26-{name}",
                    ),
                    bias=bias,
                    kind="random",
                    window=None,
                    edge=None,
                    cell=cell,
                    timeout_s=CELL_TIMEOUT_S,
                    note="",
                )
            )
        for window in bench["windows"]:
            for edge, sign in (("m20", -EDGE_FS), ("p20", EDGE_FS)):
                for bias in BIASES:
                    tag = bias_tag(bias)
                    base = window_fs(cell, str(window["name"]), bias_text(bias))
                    if base is None:
                        continue
                    offset = base + sign
                    name = f"gcell_{cell}_{window['name']}_{edge}_{tag}"
                    cases.append(
                        _case(
                            name=name,
                            group="g-cells",
                            design=design,
                            stimulus=_window_stimulus(
                                cell,
                                str(window["name"]),
                                offset,
                                {},
                                ports,
                            ),
                            bias=bias,
                            kind="adversarial",
                            window=str(window["name"]),
                            edge=edge,
                            cell=cell,
                            timeout_s=CELL_TIMEOUT_S,
                            note="",
                        )
                    )
    return cases


def _composite_cases(window_fs: WindowFn) -> list[Case]:
    cases: list[Case] = []
    for spec in _composites():
        design = spec["design"]
        ports = list(spec["ports"])
        clock = spec["clock"]
        for bias in BIASES:
            tag = bias_tag(bias)
            name = f"gcomp_{spec['slug']}_random_{tag}"
            cases.append(
                _case(
                    name=name,
                    group="g-comp",
                    design=design,
                    stimulus=_random_stream(
                        ports,
                        clock=clock,
                        anchor=ANCHOR_FS,
                        lead=LEAD_FS,
                        seed=f"fluxel-m26-{name}",
                    ),
                    bias=bias,
                    kind="random",
                    window=None,
                    edge=None,
                    cell=None,
                    timeout_s=COMP_TIMEOUT_S,
                    note="",
                )
            )
        for cell, window, port_map in spec["windows"]:
            qualify = bool(spec["qualify"])
            for edge, sign in (("m20", -EDGE_FS), ("p20", EDGE_FS)):
                for bias in BIASES:
                    tag = bias_tag(bias)
                    middle = f"{cell}_{window}" if qualify else window
                    name = f"gcomp_{spec['slug']}_{middle}_{edge}_{tag}"
                    base = window_fs(cell, window, bias_text(bias))
                    if base is None:
                        continue
                    offset = base + sign
                    cases.append(
                        _case(
                            name=name,
                            group="g-comp",
                            design=design,
                            stimulus=_window_stimulus(cell, window, offset, port_map, ports),
                            bias=bias,
                            kind="adversarial",
                            window=window,
                            edge=edge,
                            cell=cell,
                            timeout_s=COMP_TIMEOUT_S,
                            note=str(spec["note"]),
                        )
                    )
    return cases


def _composites() -> list[dict[str, Any]]:
    identity: dict[str, str] = {}
    shift_map = {"a": "d", "clk": "clk"}
    split_map = {"a": "in"}
    return [
        {
            "slug": "shift8",
            "design": _shift8(),
            "ports": ["d", "clk"],
            "clock": "clk",
            "qualify": False,
            "note": "Primary d/clk offset ignores clock-tree skew.",
            "windows": (
                ("DFF", "t_setup", shift_map),
                ("DFF", "t_hold", shift_map),
            ),
        },
        {
            "slug": "adder",
            "design": _adder(),
            "ports": ["a", "b", "cin", "clk"],
            "clock": "clk",
            "qualify": True,
            "note": "",
            "windows": (
                ("AND", "t_setup_a", identity),
                ("XOR", "t_setup_a", identity),
                ("OR", "t_setup_a", identity),
                ("DFF", "t_setup", identity),
            ),
        },
        {
            "slug": "split4",
            "design": _split4(),
            "ports": ["in"],
            "clock": None,
            "qualify": False,
            "note": "",
            "windows": (("SPLIT", "dt_min", split_map),),
        },
        {
            "slug": "merge",
            "design": _cell_design("MERGE"),
            "ports": ["a", "b"],
            "clock": None,
            "qualify": False,
            "note": "",
            "windows": (
                ("MERGE", "t_merge_ab", identity),
                ("MERGE", "t_merge_ba", identity),
            ),
        },
        {
            "slug": "dff_edge",
            "design": _cell_design("DFF"),
            "ports": ["a", "clk"],
            "clock": "clk",
            "qualify": False,
            "note": "",
            "windows": (
                ("DFF", "t_setup", identity),
                ("DFF", "t_hold", identity),
            ),
        },
    ]


def _case(
    *,
    name: str,
    group: str,
    design: Mapping[str, Any],
    stimulus: Mapping[str, list[int]],
    bias: float,
    kind: str,
    window: str | None,
    edge: str | None,
    cell: str | None,
    timeout_s: int,
    note: str,
) -> Case:
    return Case(
        name=name,
        group=group,
        design=_copy(design),
        stimulus={port: list(times) for port, times in stimulus.items()},
        bias=bias,
        kind=kind,
        window=window,
        edge=edge,
        cell=cell,
        timeout_s=timeout_s,
        note=note,
    )


def _random_stream(
    ports: Sequence[str],
    *,
    clock: str | None,
    anchor: int,
    lead: int,
    seed: str,
) -> dict[str, list[int]]:
    rng = random.Random(seed)
    stimulus = {port: [] for port in ports}
    if clock is not None:
        stimulus[clock] = [anchor + i * PERIOD_FS for i in range(CYCLES)]
        for port in ports:
            if port == clock:
                continue
            for tick in stimulus[clock]:
                if rng.random() < 0.5:
                    stimulus[port].append(tick - lead)
        return stimulus
    for port in ports:
        for i in range(CYCLES):
            if rng.random() < 0.5:
                stimulus[port].append(anchor + i * PERIOD_FS)
    return stimulus


def _window_stimulus(
    cell: str,
    window: str,
    offset: int,
    port_map: Mapping[str, str],
    ports: Sequence[str],
) -> dict[str, list[int]]:
    bench = _bench(cell)
    spec = next(item for item in bench["windows"] if item["name"] == window)
    placed = schedule(
        spec["stimulus"],
        anchor=int(bench["anchor_fs"]),
        lead=int(bench["lead_fs"]),
        offset=offset,
    )
    stimulus = {port: [] for port in ports}
    for src, times in placed.items():
        dest = port_map.get(src, src)
        stimulus[dest] = list(times)
    return stimulus


def _lut_windows(cell: str, window: str, bias: str) -> int | None:
    """LUT window at ``bias``, or None when that bench window was not stored."""
    bundle = _bundle(cell)
    entry = bundle["entries"].get(window)
    if entry is None or not _is_window(entry):
        return None
    value, _valid = lookup(bundle, window, {"bias_frac": bias})
    return value


@lru_cache(maxsize=None)
def _bundle(cell: str) -> dict[str, Any]:
    return load_cbor(LIB / f"{cell}.lut.cbor")


@lru_cache(maxsize=None)
def _bench(cell: str) -> dict[str, Any]:
    loaded = yaml.safe_load((BENCHES / f"{cell}.yaml").read_text(encoding="utf-8"))
    if not isinstance(loaded, dict):
        raise ValueError(f"{cell} bench is not a mapping")
    return loaded


@lru_cache(maxsize=None)
def _cell_design(cell: str) -> dict[str, Any]:
    spec = dict(_bench(cell))
    spec["inputs"] = {port: [] for port in spec["inputs"]}
    design, _stimulus = _design(spec)
    return design


@lru_cache(maxsize=None)
def _cell_ports(cell: str) -> tuple[tuple[str, ...], tuple[str, ...]]:
    data = json.loads((LIB / f"{cell}.cell.json").read_text(encoding="utf-8"))
    return tuple(data["ports"]["in"]), tuple(data["ports"]["out"])


def _output_keys(design: Mapping[str, Any]) -> set[str]:
    keys: set[str] = set()
    for inst, spec in design["instances"].items():
        if str(inst).startswith(("fxsrc_", "fxload_")):
            continue
        _inputs, outputs = _cell_ports(str(spec["cell"]))
        for port in outputs:
            keys.add(f"{inst}.{port}")
    return keys


def _shift8() -> dict[str, Any]:
    instances: dict[str, dict[str, str]] = {}
    nets: list[dict[str, Any]] = []

    def inst(name: str, cell: str) -> None:
        instances[name] = {"family": "rsfq", "cell": cell, "bias_domain": "b"}

    def connect(driver: dict[str, str], sink: dict[str, str]) -> None:
        nets.append({"driver": driver, "sink": sink, "wire": {"kind": "direct"}})

    for index in range(8):
        inst(f"dff{index}", "DFF")
    for index in range(7):
        inst(f"c{index}", "SPLIT")
    connect({"port": "d"}, {"instance": "dff0", "port": "a"})
    connect({"port": "clk"}, {"instance": "c0", "port": "a"})
    for parent, left, right in ((0, 1, 2), (1, 3, 4), (2, 5, 6)):
        connect(
            {"instance": f"c{parent}", "port": "q0"},
            {"instance": f"c{left}", "port": "a"},
        )
        connect(
            {"instance": f"c{parent}", "port": "q1"},
            {"instance": f"c{right}", "port": "a"},
        )
    for index, parent in enumerate((3, 4, 5, 6)):
        connect(
            {"instance": f"c{parent}", "port": "q0"},
            {"instance": f"dff{2 * index}", "port": "clk"},
        )
        connect(
            {"instance": f"c{parent}", "port": "q1"},
            {"instance": f"dff{2 * index + 1}", "port": "clk"},
        )
    for index in range(7):
        connect(
            {"instance": f"dff{index}", "port": "q"},
            {"instance": f"dff{index + 1}", "port": "a"},
        )
    connect({"instance": "dff7", "port": "q"}, {"port": "q"})
    return {
        "instances": instances,
        "nets": nets,
        "ports": {"in": ["d", "clk"], "out": ["q"]},
        "clock_domains": {"clk": [f"dff{index}" for index in range(8)]},
        "bias_domains": ["b"],
    }


def _split4() -> dict[str, Any]:
    instances: dict[str, dict[str, str]] = {}
    nets: list[dict[str, Any]] = []

    def connect(driver: dict[str, str], sink: dict[str, str]) -> None:
        nets.append({"driver": driver, "sink": sink, "wire": {"kind": "direct"}})

    for index in range(15):
        instances[f"s{index}"] = {"family": "rsfq", "cell": "SPLIT", "bias_domain": "b"}
    connect({"port": "in"}, {"instance": "s0", "port": "a"})
    for index in range(7):
        connect(
            {"instance": f"s{index}", "port": "q0"},
            {"instance": f"s{2 * index + 1}", "port": "a"},
        )
        connect(
            {"instance": f"s{index}", "port": "q1"},
            {"instance": f"s{2 * index + 2}", "port": "a"},
        )
    outputs: list[str] = []
    leaf = 0
    for index in range(7, 15):
        for port in ("q0", "q1"):
            name = f"out{leaf}"
            leaf += 1
            outputs.append(name)
            connect({"instance": f"s{index}", "port": port}, {"port": name})
    return {
        "instances": instances,
        "nets": nets,
        "ports": {"in": ["in"], "out": outputs},
        "clock_domains": {},
        "bias_domains": ["b"],
    }


def _adder() -> dict[str, Any]:
    return json.loads((EXAMPLES / "full_adder" / "design.json").read_text(encoding="utf-8"))


def _copy(value: Mapping[str, Any]) -> dict[str, Any]:
    return json.loads(json.dumps(value))


def _dump(path: Path, value: Any) -> None:
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )


def _fx_command() -> list[str]:
    binary = ROOT / "target" / "debug" / ("fx-cli.exe" if os.name == "nt" else "fx-cli")
    if binary.is_file():
        return [str(binary)]
    return ["cargo", "run", "-p", "fx-cli", "--quiet", "--"]


if __name__ == "__main__":
    main()
