"""Fail if any golden file's SHA-256 differs from golden/MANIFEST.sha256."""

import hashlib
import sys
from pathlib import Path

EXCLUDED = frozenset({"README.md", "MANIFEST.sha256"})
_HEX = frozenset("0123456789abcdef")


def _parse(text: str) -> dict[str, str]:
    entries: dict[str, str] = {}
    for lineno, line in enumerate(text.splitlines(), 1):
        if line == "":
            continue
        digest, sep, path = line[:64], line[64:66], line[66:]
        if (
            len(line) < 66
            or sep != "  "
            or any(char not in _HEX for char in digest)
            or path == ""
            or "\\" in path
            or path.startswith("/")
            or ".." in Path(path).parts
        ):
            raise ValueError(f"MANIFEST.sha256:{lineno}: expected a sha256sum line")
        if path in entries:
            raise ValueError(f"MANIFEST.sha256:{lineno}: duplicate path {path}")
        entries[path] = digest
    return entries


def check_manifest(golden: Path) -> list[str]:
    if not golden.is_dir():
        return [f"{golden}: not a directory"]
    manifest_path = golden / "MANIFEST.sha256"
    if not manifest_path.is_file():
        return ["MANIFEST.sha256: missing"]
    try:
        expected = _parse(manifest_path.read_text(encoding="utf-8"))
    except ValueError as exc:
        return [str(exc)]

    actual: dict[str, str] = {}
    for path in golden.rglob("*"):
        if not path.is_file():
            continue
        rel = path.relative_to(golden).as_posix()
        if rel in EXCLUDED:
            continue
        actual[rel] = hashlib.sha256(path.read_bytes()).hexdigest()

    errors: list[str] = []
    for rel in sorted(set(actual) | set(expected)):
        if rel not in expected:
            errors.append(f"{rel}: not listed in MANIFEST.sha256")
        elif rel not in actual:
            errors.append(f"{rel}: listed in MANIFEST.sha256 but missing")
        elif actual[rel] != expected[rel]:
            errors.append(
                f"{rel}: hash {actual[rel]} differs from manifest {expected[rel]}"
            )
    return errors


def main() -> int:
    golden = Path(__file__).resolve().parents[1] / "golden"
    errors = check_manifest(golden)
    for error in errors:
        print(error, file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
