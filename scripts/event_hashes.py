"""Print `example sha256` for every examples/ design and compare examples/HASHES.txt.

The digest is fx-cli's SHA-256 of the CBOR event log. Line endings in HASHES.txt
are ignored. `--write` replaces HASHES.txt with the hashes just printed.
"""

import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXAMPLES = ROOT / "examples"
HASHES = EXAMPLES / "HASHES.txt"
LIB = ROOT / "lib" / "families" / "rsfq"
_HEX = frozenset("0123456789abcdef")


def fx_cli() -> Path:
    binary = ROOT / "target" / "debug" / "fx-cli"
    windows = binary.with_suffix(".exe")
    if not binary.exists() and not windows.exists():
        subprocess.run(["cargo", "build", "-p", "fx-cli"], cwd=ROOT, check=True)
    if windows.exists() and not binary.exists():
        return windows
    return binary


def example_names() -> list[str]:
    names = []
    for directory in sorted(EXAMPLES.iterdir()):
        if (directory / "design.json").is_file() and (directory / "stimulus.json").is_file():
            names.append(directory.name)
    return names


def hash_example(fx: Path, name: str) -> str:
    directory = EXAMPLES / name
    with tempfile.TemporaryDirectory() as tmp:
        log = Path(tmp) / "events.cbor"
        proc = subprocess.run(
            [
                str(fx),
                "sim",
                "--design",
                str(directory / "design.json"),
                "--stimulus",
                str(directory / "stimulus.json"),
                "--lib",
                str(LIB),
                "--log",
                str(log),
            ],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
        if proc.returncode != 0:
            detail = (proc.stderr or proc.stdout or "fx-cli sim failed").strip()
            raise SystemExit(f"{name}: sim exit {proc.returncode}: {detail}")
        hashed = subprocess.run(
            [str(fx), "hash", "--log", str(log)],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
    if hashed.returncode != 0:
        detail = (hashed.stderr or hashed.stdout or "fx-cli hash failed").strip()
        raise SystemExit(f"{name}: hash exit {hashed.returncode}: {detail}")
    digest = hashed.stdout.strip()
    if len(digest) != 64 or any(char not in _HEX for char in digest):
        raise SystemExit(f"{name}: bad hash {digest!r}")
    return digest


def parse_hashes(text: str) -> list[tuple[str, str]]:
    rows: list[tuple[str, str]] = []
    for lineno, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if line == "":
            continue
        parts = line.split()
        if (
            len(parts) != 2
            or len(parts[1]) != 64
            or any(char not in _HEX for char in parts[1])
        ):
            raise SystemExit(f"HASHES.txt:{lineno}: expected 'example sha256'")
        rows.append((parts[0], parts[1]))
    return rows


def main() -> int:
    write = "--write" in sys.argv
    fx = fx_cli()
    rows = [(name, hash_example(fx, name)) for name in example_names()]
    for name, digest in rows:
        print(f"{name} {digest}", flush=True)
    body = "".join(f"{name} {digest}\n" for name, digest in rows)
    if write:
        HASHES.write_text(body, encoding="utf-8", newline="\n")
        return 0
    if not HASHES.is_file():
        print("examples/HASHES.txt: missing", file=sys.stderr)
        return 1
    expected = parse_hashes(HASHES.read_text(encoding="utf-8"))
    if expected != rows:
        print("event-log hash mismatch", file=sys.stderr)
        print("expected:", file=sys.stderr)
        for name, digest in expected:
            print(f"  {name} {digest}", file=sys.stderr)
        print("actual:", file=sys.stderr)
        for name, digest in rows:
            print(f"  {name} {digest}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
