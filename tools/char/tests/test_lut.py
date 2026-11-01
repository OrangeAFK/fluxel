"""LUT assembly, multilinear interpolation, and the held-out report."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from lut import (
    LutError,
    assemble,
    bracket_ok,
    bracket_plan,
    format_heldout,
    gap_within_margin,
    load_cbor,
    lookup,
    margin_fs,
    round_fs,
    validate,
    write_cbor,
)

ROOT = Path(__file__).resolve().parents[3]
FIXTURE = ROOT / "tests" / "fixtures"
SOURCE = {
    "josim_version": "josim-test",
    "netlist_sha256": "ab" * 32,
    "sweep_sha256": "cd" * 32,
    "timestep_fs": 25,
}


def _entry_2d() -> dict:
    return {
        "axes": [
            {"name": "bias_frac", "points": ["0.80", "1.00", "1.20"]},
            {"name": "dt_prev_fs", "points": ["6000", "200000"]},
        ],
        "values": [1000, 3000, 2000, 4000, 5000, 7000],
        "interpolation": "multilinear",
        "valid": {
            "bias_frac": {"lo": "0.80", "hi": "1.20"},
            "dt_prev_fs": {"lo": "6000", "hi": "200000"},
        },
        "sigma_fs": 0,
    }


def _bundle() -> dict:
    return {"cell": "SAMPLE", "entries": {"t_a_q": _entry_2d()}, "source": dict(SOURCE)}


def test_round_fs_half_away_from_zero() -> None:
    assert round_fs(1.5) == 2
    assert round_fs(-1.5) == -2
    assert round_fs(1.4) == 1
    assert round_fs(-0.4) == 0
    assert round_fs(0.0) == 0


def test_grid_node_is_exact() -> None:
    fs, valid = lookup(_bundle(), "t_a_q", {"bias_frac": "1.00", "dt_prev_fs": "6000"})
    assert fs == 2000
    assert valid is True


def test_bilinear_midpoint() -> None:
    fs, valid = lookup(_bundle(), "t_a_q", {"bias_frac": "0.90", "dt_prev_fs": "103000"})
    assert fs == 2500
    assert valid is True


def test_one_axis_midpoint() -> None:
    bundle = {
        "cell": "SAMPLE",
        "entries": {
            "t_a_q": {
                "axes": [{"name": "bias_frac", "points": ["0.80", "1.20"]}],
                "values": [10, 30],
                "interpolation": "multilinear",
                "valid": {"bias_frac": {"lo": "0.80", "hi": "1.20"}},
                "sigma_fs": 0,
            }
        },
        "source": dict(SOURCE),
    }
    fs, valid = lookup(bundle, "t_a_q", {"bias_frac": "1.00"})
    assert (fs, valid) == (20, True)


def test_half_femtosecond_rounds_away_from_zero() -> None:
    bundle = {
        "cell": "SAMPLE",
        "entries": {
            "up": {
                "axes": [{"name": "dt_prev_fs", "points": ["0", "2"]}],
                "values": [0, 1],
                "interpolation": "multilinear",
                "valid": {"dt_prev_fs": {"lo": "0", "hi": "2"}},
                "sigma_fs": 0,
            },
            "down": {
                "axes": [{"name": "dt_prev_fs", "points": ["0", "2"]}],
                "values": [0, -1],
                "interpolation": "multilinear",
                "valid": {"dt_prev_fs": {"lo": "0", "hi": "2"}},
                "sigma_fs": 0,
            },
        },
        "source": dict(SOURCE),
    }
    assert lookup(bundle, "up", {"dt_prev_fs": "1"}) == (1, True)
    assert lookup(bundle, "down", {"dt_prev_fs": "1"}) == (-1, True)


def test_outside_box_clamps() -> None:
    clamped, valid = lookup(_bundle(), "t_a_q", {"bias_frac": "0.50", "dt_prev_fs": "6000"})
    on_edge, edge_valid = lookup(_bundle(), "t_a_q", {"bias_frac": "0.80", "dt_prev_fs": "6000"})
    assert valid is False
    assert edge_valid is True
    assert clamped == on_edge == 1000


def test_schema_rejects_other_interpolation() -> None:
    bundle = _bundle()
    bundle["entries"]["t_a_q"]["interpolation"] = "linear"
    with pytest.raises(LutError):
        validate(bundle)


def test_assemble_rectangle_median_and_window_hole() -> None:
    delays = [
        _delay("0.70", 6_000, "expected", [10]),
        _delay("0.70", 200_000, "failed", []),
        _delay("1.00", 6_000, "expected", [20]),
        _delay("1.00", 200_000, "expected", [10, 12]),
        _delay("1.00", 200_000, "expected", [1, 2], src="a", dst="q", tag="off"),
        _delay("1.30", 6_000, "expected", [40]),
        _delay("1.30", 200_000, "expected", [50]),
    ]
    # The second nominal arc is a different entry. The 1 and 2 delays are on t_a_q_off.
    windows = [
        _window("0.70", "bracketed", 500),
        _window("1.00", "bracketed", 1000),
        _window("1.15", "unbracketed", None),
        _window("1.30", "bracketed", 1500),
    ]
    bundle = assemble("JTL", delays, windows, source=SOURCE)
    validate(bundle)
    delay = bundle["entries"]["t_a_q"]
    assert delay["axes"][0]["points"] == ["1.00", "1.30"]
    assert delay["axes"][1]["points"] == ["6000", "200000"]
    assert delay["values"] == [20, 11, 40, 50]
    assert delay["sigma_fs"] == 0
    assert delay["valid"]["bias_frac"] == {"lo": "1.00", "hi": "1.30"}
    off = bundle["entries"]["t_a_q_off"]
    assert off["values"] == [2]
    window = bundle["entries"]["t_hold"]
    assert window["axes"] == [{"name": "bias_frac", "points": ["0.70", "1.00"]}]
    assert window["values"] == [500, 1000]
    assert bundle["source"] == SOURCE


def test_nominal_failure_is_an_error() -> None:
    delays = [
        _delay("0.70", 200_000, "expected", [10]),
        _delay("1.30", 200_000, "expected", [10]),
    ]
    with pytest.raises(LutError):
        assemble("JTL", delays, [], source=SOURCE)


def test_arc_absent_at_nominal_is_omitted() -> None:
    delays = [
        _delay("1.00", 200_000, "expected", [10], src="clk", dst="q"),
        _delay("0.70", 6_000, "expected", [4], src="a", dst="q"),
        _delay("1.30", 200_000, "expected", [12], src="clk", dst="q"),
    ]
    bundle = assemble("DFF", delays, [], source=SOURCE)
    assert set(bundle["entries"]) == {"t_clk_q"}
    assert bundle["entries"]["t_clk_q"]["values"] == [10, 12]


def test_window_with_no_bracket_is_omitted() -> None:
    delays = [
        _delay("1.00", 200_000, "expected", [10]),
        _delay("1.30", 200_000, "expected", [12]),
    ]
    bundle = assemble("JTL", delays, [_window("1.00", "unbracketed", None)], source=SOURCE)
    assert set(bundle["entries"]) == {"t_a_q"}


def test_bracket_sides() -> None:
    assert bracket_ok(low="fail", high="pass") is True
    assert bracket_ok(low="pass", high="pass") is False
    assert bracket_ok(low="fail", high="fail") is False
    assert bracket_ok(low="illegal", high="pass") is False
    assert bracket_ok(low="undelivered", high="pass") is False


def test_measured_window_is_checked_as_a_gap() -> None:
    assert bracket_plan({}, 1_000, 20) == ("offset", 980, 1_020)
    assert bracket_plan({"measure": {"kind": "gap"}}, 1_000, 20) == ("gap",)
    assert gap_within_margin(1_000, 1_015, 20) is True
    assert gap_within_margin(1_000, 1_030, 20) is False
    assert gap_within_margin(1_000, None, 20) is False


def test_margin_is_at_least_20_fs() -> None:
    assert margin_fs(100) == 20
    assert margin_fs(10_000) == 200
    assert margin_fs(1_819) == 36


def test_heldout_table_notes_window_resolution() -> None:
    text = format_heldout(
        [
            {
                "cell": "JTL",
                "entry": "t_a_q",
                "kind": "delay",
                "samples": 50,
                "max_err_fs": 4,
                "mean_err_fs": 1.5,
                "max_pct_nominal": 0.22,
                "mean_pct_nominal": 0.08,
                "pass_rate": "",
                "note": "",
            },
            {
                "cell": "DFF",
                "entry": "t_setup",
                "kind": "window",
                "samples": 12,
                "max_err_fs": 10,
                "mean_err_fs": 5,
                "max_pct_nominal": 1.0,
                "mean_pct_nominal": 0.5,
                "pass_rate": "48/50",
                "note": "window error resolution 10 fs",
            },
        ]
    )
    lines = text.splitlines()
    assert lines[0].split("\t") == [
        "cell",
        "entry",
        "kind",
        "samples",
        "max_err_fs",
        "mean_err_fs",
        "max_pct_nominal",
        "mean_pct_nominal",
        "pass_rate",
        "note",
    ]
    delay = lines[1].split("\t")
    assert delay[0] == "JTL"
    assert delay[8] == ""
    assert "10 fs" in lines[2]


def test_cbor_roundtrip_matches_lookup(tmp_path: Path) -> None:
    path = tmp_path / "sample.lut.cbor"
    write_cbor(_bundle(), path)
    loaded = load_cbor(path)
    fs, valid = lookup(loaded, "t_a_q", {"bias_frac": "0.90", "dt_prev_fs": "103000"})
    assert (fs, valid) == (2500, True)


def test_committed_fixture_matches_interpolator() -> None:
    bundle = load_cbor(FIXTURE / "sample.lut.cbor")
    points = json.loads((FIXTURE / "sample.lut.points.json").read_text(encoding="utf-8"))
    assert len(points) == 100
    for row in points:
        fs, valid = lookup(bundle, row["entry"], row["point"])
        assert fs == row["fs"]
        assert valid is row["in_valid"]


def _delay(
    bias: str,
    dt: int,
    outcome: str,
    delays: list[int],
    *,
    src: str = "a",
    dst: str = "q",
    tag: str = "",
) -> dict:
    return {
        "bias_frac": bias,
        "dt_prev_fs": dt,
        "data_clock_offset_fs": None,
        "in_in_offset_fs": None,
        "outcome": outcome,
        "arcs": [
            {"src": src, "dst": dst, "cause_fs": 0, "delay_fs": delay, "tag": tag} for delay in delays
        ],
        "josim_version": "josim-test",
    }


def _window(bias: str, bracket: str, window_fs: int | None) -> dict:
    return {
        "cell": "JTL",
        "window": "t_hold",
        "bias_frac": bias,
        "window_fs": window_fs,
        "bracket": bracket,
        "how": "",
    }
