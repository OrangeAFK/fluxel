"""Write the golden suite index. Golden entries are produced only by this script."""

from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]

README = """\
# Golden suite

This directory is empty. It will hold JoSIM reference runs.

Entries are generated only by `tools/char/golden.py`. Do not edit them by hand.

`MANIFEST.sha256` stores SHA-256 hashes of every other file here, one `sha256sum` line per file (`<64 lowercase hex><two spaces><path>`). This README and `MANIFEST.sha256` are not listed. `scripts/check_manifest.py` fails if any hash differs.
"""


def write_golden_index(root: Path) -> None:
    golden = root / "golden"
    golden.mkdir(parents=True, exist_ok=True)
    (golden / "README.md").write_text(README, encoding="utf-8", newline="\n")
    manifest = golden / "MANIFEST.sha256"
    if not manifest.exists():
        manifest.write_bytes(b"")


def main() -> None:
    write_golden_index(REPO_ROOT)


if __name__ == "__main__":
    main()
