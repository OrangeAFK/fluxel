import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts"))

from check_manifest import check_manifest  # noqa: E402


def _golden(tmp_path: Path) -> Path:
    golden = tmp_path / "golden"
    golden.mkdir()
    (golden / "README.md").write_text("index\n", encoding="utf-8")
    return golden


def test_empty_manifest_passes(tmp_path: Path) -> None:
    golden = _golden(tmp_path)
    (golden / "MANIFEST.sha256").write_bytes(b"")

    assert check_manifest(golden) == []


def test_wrong_hash_fails(tmp_path: Path) -> None:
    golden = _golden(tmp_path)
    (golden / "entry.txt").write_bytes(b"data")
    (golden / "MANIFEST.sha256").write_text(
        f"{'0' * 64}  entry.txt\n",
        encoding="utf-8",
    )

    errors = check_manifest(golden)
    assert len(errors) == 1
    assert "entry.txt" in errors[0]


def test_unlisted_file_fails(tmp_path: Path) -> None:
    golden = _golden(tmp_path)
    (golden / "MANIFEST.sha256").write_bytes(b"")
    (golden / "extra.txt").write_bytes(b"x")

    errors = check_manifest(golden)
    assert len(errors) == 1
    assert "extra.txt" in errors[0]


def test_missing_file_fails(tmp_path: Path) -> None:
    golden = _golden(tmp_path)
    (golden / "MANIFEST.sha256").write_text(
        f"{'ab' * 32}  cells/jtl.txt\n",
        encoding="utf-8",
    )

    errors = check_manifest(golden)
    assert len(errors) == 1
    assert "cells/jtl.txt" in errors[0]
