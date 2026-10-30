"""Write golden entries. Golden data is produced only by this script.

Set FLUXEL_GOLDEN_UPDATE=1 to write an entry. Each entry is
``golden/<name>/{netlist.cir, stimulus.yaml, pulses.json, meta.json}``.
``pulses.json`` is a pulse log of upward (2k+1)π crossings on the ports named
in the stimulus. ``meta.json`` records the JoSIM version, the ``.tran``
timestep in femtoseconds, and the SHA-256 of the other three files.
``MANIFEST.sha256`` then hashes every golden file except itself and the README.
"""

import hashlib
import json
import os
import re
from decimal import Decimal
from pathlib import Path

from extract import extract_crossings
from josim import run as run_josim
from josim import version as josim_version
from pulselog import dump

REPO_ROOT = Path(__file__).resolve().parents[2]
TIMEOUT_S = 60

README = """\
# Golden suite

This directory holds JoSIM reference runs.

Entries are generated only by `tools/char/golden.py`. Do not edit them by hand.

`MANIFEST.sha256` stores SHA-256 hashes of every other file here, one `sha256sum` line per file (`<64 lowercase hex><two spaces><path>`). This README and `MANIFEST.sha256` are not listed. `scripts/check_manifest.py` fails if any hash differs.
"""

_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]*\Z")
_TRAN = re.compile(r"(?im)^\.tran\s+(\S+)")
_TIME = re.compile(r"([+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?)([A-Za-z]*)")
_FS = {
    "": Decimal("1e15"),
    "s": Decimal("1e15"),
    "m": Decimal("1e12"),
    "u": Decimal("1e9"),
    "n": Decimal("1e6"),
    "p": Decimal(1000),
    "f": Decimal(1),
}
_MANIFEST_SKIP = frozenset({"README.md", "MANIFEST.sha256"})


class GoldenUpdateError(RuntimeError):
    """Raised when a golden write is attempted without FLUXEL_GOLDEN_UPDATE=1."""


def write_golden_index(root: Path) -> None:
    golden = root / "golden"
    golden.mkdir(parents=True, exist_ok=True)
    (golden / "README.md").write_text(README, encoding="utf-8", newline="\n")
    manifest = golden / "MANIFEST.sha256"
    if not manifest.exists():
        manifest.write_bytes(b"")


def generate_entry(
    name: str,
    netlist: Path,
    stimulus: Path,
    *,
    root: Path | None = None,
) -> None:
    """Simulate ``netlist`` and write ``golden/<name>`` under ``root``."""
    if os.environ.get("FLUXEL_GOLDEN_UPDATE") != "1":
        raise GoldenUpdateError(
            "set FLUXEL_GOLDEN_UPDATE=1 to write golden entries"
        )
    if _NAME.fullmatch(name) is None:
        raise ValueError(f"invalid entry name {name!r}")
    netlist = Path(netlist)
    stimulus = Path(stimulus)
    root = REPO_ROOT if root is None else Path(root)

    netlist_text = netlist.read_text(encoding="utf-8")
    stimulus_text = stimulus.read_text(encoding="utf-8")
    ports = _ports(stimulus_text)
    columns = tuple(ports.values())
    traces = run_josim(netlist, timeout=TIMEOUT_S, columns=columns)
    pulses: dict[str, list[int]] = {}
    for port, column in ports.items():
        time, phase = traces[column]
        crossings = extract_crossings(time, phase)
        pulses[port] = [_seconds_to_fs(item.t) for item in crossings.pulses]

    write_golden_index(root)
    entry = root / "golden" / name
    entry.mkdir(parents=True, exist_ok=True)
    files = {
        "netlist.cir": _lf(netlist.read_bytes()),
        "stimulus.yaml": _lf(stimulus.read_bytes()),
        "pulses.json": (dump(pulses) + "\n").encode("utf-8"),
    }
    for filename, data in files.items():
        (entry / filename).write_bytes(data)
    meta = {
        "josim_version": josim_version(),
        "sha256": {filename: hashlib.sha256(data).hexdigest() for filename, data in files.items()},
        "timestep_fs": _timestep_fs(netlist_text),
    }
    (entry / "meta.json").write_text(
        json.dumps(meta, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    _write_manifest(root / "golden")


def main() -> None:
    import sys

    args = sys.argv[1:]
    if not args:
        write_golden_index(REPO_ROOT)
        return
    if len(args) != 3:
        raise SystemExit("usage: golden.py <name> <netlist.cir> <stimulus.yaml>")
    try:
        generate_entry(args[0], Path(args[1]), Path(args[2]))
    except GoldenUpdateError as exc:
        raise SystemExit(str(exc)) from exc


def _ports(text: str) -> dict[str, str]:
    """Read the ``ports:`` map. Values are JoSIM column names."""
    ports: dict[str, str] = {}
    in_ports = False
    for line in text.splitlines():
        if line.startswith("ports:"):
            in_ports = True
            continue
        if not in_ports:
            continue
        if line and not line.startswith((" ", "\t")):
            break
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        key, sep, value = stripped.partition(":")
        if sep == "" or key == "" or value.strip() == "":
            raise ValueError(f"stimulus.yaml has a bad port line {stripped!r}")
        ports[key.strip()] = value.strip().strip("\"'")
    if not ports:
        raise ValueError("stimulus.yaml has no ports")
    return ports


def _timestep_fs(netlist_text: str) -> int:
    match = _TRAN.search(netlist_text)
    if match is None:
        raise ValueError("netlist has no .tran statement")
    token = match.group(1)
    parsed = _TIME.fullmatch(token)
    if parsed is None:
        raise ValueError(f"netlist has an unreadable timestep {token!r}")
    scale = _FS.get(parsed.group(2).lower())
    if scale is None:
        raise ValueError(f"netlist has an unreadable timestep {token!r}")
    femtoseconds = Decimal(parsed.group(1)) * scale
    if femtoseconds != femtoseconds.to_integral_value():
        raise ValueError(f"timestep {token} is not an integer number of femtoseconds")
    return int(femtoseconds)


def _seconds_to_fs(seconds: float) -> int:
    return int(round(seconds * 1e15))


def _lf(data: bytes) -> bytes:
    return data.replace(b"\r\n", b"\n").replace(b"\r", b"\n")


def _write_manifest(golden: Path) -> None:
    lines: list[str] = []
    for path in sorted(golden.rglob("*")):
        if not path.is_file():
            continue
        rel = path.relative_to(golden).as_posix()
        if rel in _MANIFEST_SKIP:
            continue
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        lines.append(f"{digest}  {rel}")
    text = ("\n".join(lines) + "\n") if lines else ""
    (golden / "MANIFEST.sha256").write_text(text, encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main()
