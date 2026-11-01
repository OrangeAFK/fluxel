"""G-cells and G-comp configs for the golden suite."""

import json
import re
from pathlib import Path

import pytest

from golden import GoldenUpdateError
from lut import CHARACTERIZED, LIB, load_cbor, lookup
from suite import (
    BIASES,
    PULSE_SPAN_FS,
    bias_tag,
    bias_text,
    build_cases,
    logic_depth,
    output_stimulus_yaml,
    update_golden,
    write_suite,
)

ROOT = Path(__file__).resolve().parents[3]
SUITE = Path(__file__).resolve().parents[1] / "suite"
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]*\Z")


def _fake_window(cell: str, window: str, bias: str) -> int:
    del cell, window, bias
    return 80_000


def _by_name(cases, name: str):
    found = [case for case in cases if case.name == name]
    assert len(found) == 1
    return found[0]


def test_builds_match() -> None:
    first = build_cases(window_fs=_fake_window)
    second = build_cases(window_fs=_fake_window)

    assert [case.name for case in first] == [case.name for case in second]
    assert [case.stimulus for case in first] == [case.stimulus for case in second]
    assert [case.design for case in first] == [case.design for case in second]


def test_every_cell_has_random_streams_at_each_bias() -> None:
    names = {case.name for case in build_cases(window_fs=_fake_window)}

    for cell in CHARACTERIZED:
        for bias in BIASES:
            assert f"gcell_{cell}_random_{bias_tag(bias)}" in names


def test_adversarial_offsets_use_the_window_table() -> None:
    cases = build_cases(window_fs=_fake_window)
    jtl = _by_name(cases, "gcell_JTL_dt_min_m20_b100")
    jtl_hi = _by_name(cases, "gcell_JTL_dt_min_p20_b100")
    dff = _by_name(cases, "gcell_DFF_t_setup_m20_b085")

    assert jtl.stimulus["a"] == [200_000, 200_000 + 80_000 - 20]
    assert jtl_hi.stimulus["a"] == [200_000, 200_000 + 80_000 + 20]
    assert dff.stimulus["a"] == [200_000]
    assert dff.stimulus["clk"] == [200_000 + 80_000 - 20]


def test_real_jtl_window_matches_the_lut() -> None:
    bundle = load_cbor(LIB / "JTL.lut.cbor")
    value, valid = lookup(bundle, "dt_min", {"bias_frac": "1"})
    assert valid
    cases = build_cases()
    case = _by_name(cases, "gcell_JTL_dt_min_m20_b100")
    names = {item.name for item in cases}

    assert case.stimulus["a"][1] - case.stimulus["a"][0] == value - 20
    assert not any(name.startswith("gcell_AND_dt_min_") for name in names)


def test_shift8_has_eight_dffs_and_split4_leaves_are_depth_4() -> None:
    cases = build_cases(window_fs=_fake_window)
    shift = _by_name(cases, "gcomp_shift8_random_b100")
    tree = _by_name(cases, "gcomp_split4_random_b100")
    depths = logic_depth(tree.design)

    assert sum(inst["cell"] == "DFF" for inst in shift.design["instances"].values()) == 8
    assert "skew" in _by_name(cases, "gcomp_shift8_t_setup_m20_b100").note.lower()
    assert sum(inst["cell"] == "SPLIT" for inst in tree.design["instances"].values()) == 15
    assert max(depths.values()) == 4
    assert sum(depth == 4 for depth in depths.values()) == 8
    assert len(tree.design["ports"]["out"]) == 16


def test_names_biases_gaps_and_timeouts() -> None:
    cases = build_cases(window_fs=_fake_window)
    names = [case.name for case in cases]

    assert names == sorted(names)
    assert len(names) == len(set(names))
    for case in cases:
        assert NAME.fullmatch(case.name)
        assert case.bias in BIASES
        assert bias_tag(case.bias) in case.name
        assert bias_text(case.bias) in {"0.85", "1", "1.15"}
        for times in case.stimulus.values():
            ordered = sorted(times)
            assert times == ordered
            for left, right in zip(ordered, ordered[1:]):
                assert right - left >= PULSE_SPAN_FS
        if case.group == "g-cells":
            assert case.timeout_s == 60
        else:
            assert case.timeout_s == 600


def test_build_does_not_create_golden(tmp_path: Path) -> None:
    before = sorted(path.relative_to(ROOT / "golden") for path in (ROOT / "golden").rglob("*"))

    cases = build_cases(window_fs=_fake_window)
    write_suite(cases, tmp_path / "suite")

    after = sorted(path.relative_to(ROOT / "golden") for path in (ROOT / "golden").rglob("*"))
    assert before == after
    assert not (tmp_path / "golden").exists()
    manifest = json.loads((tmp_path / "suite" / "manifest.json").read_text(encoding="utf-8"))
    assert [row["name"] for row in manifest] == [case.name for case in cases]
    sample = cases[0]
    folder = tmp_path / "suite" / sample.name
    assert json.loads((folder / "design.json").read_text(encoding="utf-8")) == sample.design
    assert json.loads((folder / "stimulus.json").read_text(encoding="utf-8")) == sample.stimulus


def test_committed_suite_matches_the_builder() -> None:
    cases = build_cases()
    manifest = json.loads((SUITE / "manifest.json").read_text(encoding="utf-8"))

    assert [row["name"] for row in manifest] == [case.name for case in cases]
    by_name = {case.name: case for case in cases}
    for row in manifest:
        case = by_name[row["name"]]
        folder = SUITE / case.name
        assert json.loads((folder / "design.json").read_text(encoding="utf-8")) == case.design
        assert json.loads((folder / "stimulus.json").read_text(encoding="utf-8")) == case.stimulus
        assert row["bias"] == bias_text(case.bias)
        assert row["timeout_s"] == case.timeout_s


def test_stimulus_yaml_keeps_design_outputs_only() -> None:
    design = {
        "instances": {
            "dut": {"cell": "JTL"},
            "fxsrc_a": {"cell": "DCSFQ"},
        }
    }
    deck = "\n".join(
        [
            "* fluxel-map dut.a P(B1|XDUT)",
            "* fluxel-map dut.q P(B2|XDUT)",
            "* fluxel-map fxsrc_a.q P(B3|XFXSRC_A)",
        ]
    )

    text = output_stimulus_yaml(deck, design)

    assert "dut.q:" in text
    assert "dut.a:" not in text
    assert "fxsrc_a.q:" not in text


def test_update_golden_refuses_without_the_flag(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("FLUXEL_GOLDEN_UPDATE", raising=False)
    before = sorted((ROOT / "golden").rglob("*"))

    with pytest.raises(GoldenUpdateError):
        update_golden(build_cases(window_fs=_fake_window)[:1], root=tmp_path)

    assert sorted((ROOT / "golden").rglob("*")) == before
    assert not (tmp_path / "golden").exists()


def test_update_golden_exports_and_forwards_timeout(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("FLUXEL_GOLDEN_UPDATE", "1")
    calls: list[tuple[str, float, str]] = []

    def fake_export(case, dest: Path) -> None:
        del case
        dest.write_text("* fluxel-map dut.q P(B2|XDUT)\n", encoding="utf-8")

    def fake_generate(name, netlist, stimulus, *, root, timeout):
        del netlist, root
        calls.append((name, timeout, stimulus.read_text(encoding="utf-8")))

    monkeypatch.setattr("suite.export_deck", fake_export)
    monkeypatch.setattr("suite.generate_entry", fake_generate)
    cases = build_cases(window_fs=_fake_window)
    chosen = [
        _by_name(cases, "gcell_JTL_random_b100"),
        _by_name(cases, "gcomp_shift8_random_b100"),
    ]

    update_golden(chosen, root=tmp_path)

    assert [name for name, _, _ in calls] == [case.name for case in chosen]
    assert calls[0][1] == 60
    assert calls[1][1] == 600
    assert "dut.q:" in calls[0][2]
