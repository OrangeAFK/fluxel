from pathlib import Path

from golden import write_golden_index


def test_write_golden_index_creates_readme_and_empty_manifest(tmp_path: Path) -> None:
    write_golden_index(tmp_path)

    readme = (tmp_path / "golden" / "README.md").read_text(encoding="utf-8")
    assert "tools/char/golden.py" in readme
    assert "MANIFEST.sha256" in readme
    assert "Do not edit" in readme
    assert (tmp_path / "golden" / "MANIFEST.sha256").read_bytes() == b""


def test_write_golden_index_leaves_existing_manifest(tmp_path: Path) -> None:
    golden = tmp_path / "golden"
    golden.mkdir()
    manifest = golden / "MANIFEST.sha256"
    manifest.write_bytes(b"already-there\n")

    write_golden_index(tmp_path)

    assert manifest.read_bytes() == b"already-there\n"
    assert (golden / "README.md").is_file()
