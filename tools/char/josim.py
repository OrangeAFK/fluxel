"""Run josim-cli and read its CSV trace output.

SPEC §4.2: characterization starts from a JoSIM netlist. Column names follow
the deck in DECISIONS.md: quoted ``P(device|instance)`` becomes ``P(B1|XIN)``.
The ``time`` column is already in seconds.
"""

import csv
import subprocess
import tempfile
from collections.abc import Sequence
from pathlib import Path


class JosimError(RuntimeError):
    """Base error for a JoSIM run or its CSV."""


class JosimFailed(JosimError):
    """josim-cli exited nonzero."""

    def __init__(self, returncode: int, log: str) -> None:
        self.returncode = returncode
        self.log = log
        super().__init__(f"josim-cli exited {returncode}\n{log}")


class JosimTimeout(JosimError):
    """josim-cli did not finish before the timeout. The child is killed."""

    def __init__(self, timeout: float) -> None:
        self.timeout = timeout
        super().__init__(f"josim-cli timed out after {timeout} s")


class JosimOutputError(JosimError):
    """The CSV has no time column, or a requested signal column is absent."""

    def __init__(self, missing: tuple[str, ...]) -> None:
        self.missing = missing
        super().__init__("JoSIM CSV is missing " + ", ".join(missing))


def version() -> str:
    """Return the ``josim-cli -v`` banner."""
    proc = subprocess.run(
        ["josim-cli", "-v"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )
    text = ((proc.stdout or "") + (proc.stderr or "")).strip()
    if proc.returncode != 0:
        raise JosimFailed(proc.returncode, text)
    return text


def run(
    netlist: Path,
    *,
    timeout: float,
    columns: Sequence[str] | None = None,
) -> dict[str, tuple[tuple[float, ...], tuple[float, ...]]]:
    """Run ``josim-cli`` on ``netlist`` and return ``{name: (t_seconds, values)}``.

    The CSV is written in a temporary directory, which is removed before this
    returns. The working directory is the netlist's parent so relative
    ``.include`` paths resolve. ``columns`` selects signal names in that order;
    omitted, every signal column is returned.
    """
    netlist = Path(netlist).resolve()
    with tempfile.TemporaryDirectory() as tmp:
        csv_path = Path(tmp) / "output.csv"
        try:
            proc = subprocess.run(
                ["josim-cli", "-o", str(csv_path), str(netlist)],
                cwd=netlist.parent,
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=timeout,
                check=False,
            )
        except subprocess.TimeoutExpired as exc:
            raise JosimTimeout(timeout) from exc
        log = (proc.stdout or "") + (proc.stderr or "")
        if proc.returncode != 0:
            raise JosimFailed(proc.returncode, log)
        if not csv_path.is_file():
            raise JosimOutputError(("time",))
        text = csv_path.read_text(encoding="utf-8", errors="replace")
    return _parse_csv(text, columns)


def _parse_csv(
    text: str,
    columns: Sequence[str] | None,
) -> dict[str, tuple[tuple[float, ...], tuple[float, ...]]]:
    header, rows = _table(text)
    if not header or header[0].strip().lower() != "time":
        raise JosimOutputError(("time",))
    names = [cell.strip() for cell in header]
    wanted = list(columns) if columns is not None else names[1:]
    missing = tuple(name for name in wanted if name not in names)
    if missing:
        raise JosimOutputError(missing)
    index = {name: i for i, name in enumerate(names)}
    time: list[float] = []
    series = {name: [] for name in wanted}
    for row in rows:
        if not row or not any(cell.strip() for cell in row):
            continue
        time.append(float(row[0]))
        for name in wanted:
            series[name].append(float(row[index[name]]))
    t = tuple(time)
    return {name: (t, tuple(series[name])) for name in wanted}


def _table(text: str) -> tuple[list[str] | None, list[list[str]]]:
    lines = text.splitlines()
    start = None
    for i, line in enumerate(lines):
        stripped = line.strip().lstrip("\ufeff").lower()
        if stripped.startswith("time") or stripped.startswith('"time"'):
            start = i
            break
    if start is None:
        return None, []
    rows = list(csv.reader(lines[start:]))
    if not rows:
        return None, []
    return rows[0], rows[1:]
