import math
import sys
from decimal import Decimal, ROUND_HALF_UP
from pathlib import Path

import pytest

from golden import GoldenUpdateError, generate_entry, write_golden_index

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts"))

from check_manifest import check_manifest  # noqa: E402
from pulselog import load  # noqa: E402


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


def test_generate_entry_refuses_without_flag(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("FLUXEL_GOLDEN_UPDATE", raising=False)
    netlist = tmp_path / "chain.cir"
    stimulus = tmp_path / "stimulus.yaml"
    netlist.write_text(".tran 0.025p 1p\n.end\n", encoding="utf-8")
    stimulus.write_text("ports:\n  a: P(B1|XMID)\n", encoding="utf-8")

    with pytest.raises(GoldenUpdateError):
        generate_entry("demo", netlist, stimulus, root=tmp_path)

    assert not (tmp_path / "golden").exists()


def test_generate_entry_writes_pulses_and_manifest(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("FLUXEL_GOLDEN_UPDATE", "1")
    netlist = tmp_path / "chain.cir"
    stimulus = tmp_path / "stimulus.yaml"
    netlist.write_text(".tran 0.025p 1p\n.end\n", encoding="utf-8", newline="\n")
    stimulus.write_text(
        "ports:\n  a: P(B1|XMID)\n  q: P(B2|XMID)\n",
        encoding="utf-8",
        newline="\n",
    )

    def fake_run(path: Path, *, timeout: float, columns: tuple[str, ...]) -> dict:
        assert path == netlist
        assert timeout > 0
        assert columns == ("P(B1|XMID)", "P(B2|XMID)")
        time = (0.0, 1e-12)
        phase = (0.0, 2 * math.pi)
        return {name: (time, phase) for name in columns}

    monkeypatch.setattr("golden.run_josim", fake_run)
    monkeypatch.setattr("golden.josim_version", lambda: "vtest")

    generate_entry("demo", netlist, stimulus, root=tmp_path)

    entry = tmp_path / "golden" / "demo"
    pulses = load((entry / "pulses.json").read_text(encoding="utf-8"))
    assert pulses == {"a": (500,), "q": (500,)}
    meta = __import__("json").loads((entry / "meta.json").read_text(encoding="utf-8"))
    assert meta["josim_version"] == "vtest"
    assert meta["timestep_fs"] == 25
    assert set(meta["sha256"]) == {"netlist.cir", "stimulus.yaml", "pulses.json"}
    for name, digest in meta["sha256"].items():
        assert __import__("hashlib").sha256((entry / name).read_bytes()).hexdigest() == digest
    assert check_manifest(tmp_path / "golden") == []


# report.txt crossings for XMID, rounded half up to 1 fs. DECISIONS.md lags were
# subtracted before that rounding, so the stored lag is within 1 fs of each one.
_B1_PS = ("30.921673", "80.921410", "130.921410")
_B2_PS = ("32.740889", "82.740637", "132.740638")
_LAG_PS = ("1.819217", "1.819227", "1.819228")


def _ps_to_fs(picoseconds: str) -> int:
    return int((Decimal(picoseconds) * 1000).to_integral_value(ROUND_HALF_UP))


def test_jtl_chain_matches_manual_run() -> None:
    entry = ROOT / "golden" / "jtl_chain"
    pulses = load((entry / "pulses.json").read_text(encoding="utf-8"))

    assert pulses["a"] == tuple(_ps_to_fs(text) for text in _B1_PS)
    assert pulses["q"] == tuple(_ps_to_fs(text) for text in _B2_PS)
    for lag_fs, recorded_ps in zip(
        (q - a for a, q in zip(pulses["a"], pulses["q"])),
        _LAG_PS,
        strict=True,
    ):
        assert abs(Decimal(lag_fs) - Decimal(recorded_ps) * 1000) < 1

    meta = __import__("json").loads((entry / "meta.json").read_text(encoding="utf-8"))
    assert "v2.7.02a34ee" in meta["josim_version"]
    assert meta["timestep_fs"] == 25
    assert check_manifest(ROOT / "golden") == []
