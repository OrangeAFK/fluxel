"""Window bisection: explicit pass clauses, 10 fs brackets, review flags."""

from decimal import Decimal
from pathlib import Path

import pytest
import yaml

from windows import (
    WindowError,
    apply_measurement,
    bisect,
    inputs_arrived,
    judge,
    junction_gap,
    legal_lo,
    review,
    schedule,
    write_setup_hold_plot,
    write_windows,
)

ROOT = Path(__file__).resolve().parents[3]
BENCHES = ROOT / "tools" / "char" / "benches"

HEADER = {"anchor_fs": 200_000, "lead_fs": 50_000, "resolution_fs": 10}


def _window(
    name: str,
    lo: int,
    hi: int,
    stimulus: dict,
    passes: list,
    arrive: list | None = None,
    measure: dict | None = None,
) -> dict:
    row = {
        "name": name,
        "lo_fs": lo,
        "hi_fs": hi,
        "stimulus": stimulus,
        "pass": passes,
    }
    if arrive is not None:
        row["arrive"] = arrive
    if measure is not None:
        row["measure"] = measure
    return row


def _arrive(*ports: str) -> list:
    return [{"port": port, "n": 1} for port in ports]


def _gap(
    later: str,
    earlier: str,
    later_index: int | None = None,
    earlier_index: int | None = None,
) -> dict:
    row = {"kind": "gap", "later": later, "earlier": earlier}
    if later_index is not None:
        row["later_index"] = later_index
    if earlier_index is not None:
        row["earlier_index"] = earlier_index
    return row


def _count(port: str, n: int) -> dict:
    return {"kind": "count", "port": port, "n": n}


def _after(port: str, ref: str, index: int | None = None, ref_index: int | None = None) -> dict:
    clause = {"kind": "after", "port": port, "ref": ref}
    if index is not None:
        clause["index"] = index
    if ref_index is not None:
        clause["ref_index"] = ref_index
    return clause


def _before(port: str, index: int, ref: str, ref_index: int) -> dict:
    return {"kind": "before", "port": port, "index": index, "ref": ref, "ref_index": ref_index}


# Pass clauses below are the functional definitions. They are compared to the
# bench YAML as literals. windows.judge does not read the cell state machine.

_SETUP = (-100_000, 200_000)
_HOLD = (-100_000, 200_000)
_MERGE = (0, 200_000)
_DT = (6_000, 400_000)
_CYCLE = (56_000, 400_000)

EXPECTED = {
    "JTL": [
        _window(
            "dt_min",
            *_DT,
            {"a": ["anchor", "anchor + offset"]},
            [_count("a", 2), _count("q", 2), {"kind": "paired", "port": "q", "ref": "a"}],
        )
    ],
    "SPLIT": [
        _window(
            "dt_min",
            *_DT,
            {"a": ["anchor", "anchor + offset"]},
            [
                _count("a", 2),
                _count("q0", 2),
                _count("q1", 2),
                {"kind": "paired", "port": "q0", "ref": "a"},
                {"kind": "paired", "port": "q1", "ref": "a"},
            ],
        )
    ],
    "MERGE": [
        _window(
            "t_merge_ab",
            *_MERGE,
            {"a": ["anchor"], "b": ["anchor + offset"]},
            [
                _count("q", 2),
                _after("q", "a", 0, 0),
                _before("q", 0, "b", 0),
                _after("q", "b", 1, 0),
            ],
            _arrive("a", "b"),
            _gap("b", "a"),
        ),
        _window(
            "t_merge_ba",
            *_MERGE,
            {"b": ["anchor"], "a": ["anchor + offset"]},
            [
                _count("q", 2),
                _after("q", "b", 0, 0),
                _before("q", 0, "a", 0),
                _after("q", "a", 1, 0),
            ],
            _arrive("b", "a"),
            _gap("a", "b"),
        ),
        _window(
            "dt_min",
            *_DT,
            {"a": ["anchor", "anchor + offset"], "b": []},
            [
                _count("a", 2),
                _count("b", 0),
                _count("q", 2),
                {"kind": "paired", "port": "q", "ref": "a"},
            ],
        ),
    ],
    "DFF": [
        _window(
            "t_setup",
            *_SETUP,
            {"a": ["anchor"], "clk": ["anchor + offset"]},
            [_count("q", 1), _after("q", "clk", ref_index=0)],
            _arrive("a", "clk"),
            _gap("clk", "a"),
        ),
        _window(
            "t_hold",
            *_HOLD,
            {
                "a": ["anchor", "anchor + lead + offset"],
                "clk": ["anchor + lead", "anchor + lead + offset + lead"],
            },
            [
                _count("q", 2),
                _before("q", 0, "a", 1),
                _after("q", "a", 1, 1),
            ],
            _arrive("a", "clk"),
            _gap("a", "clk", 1, 0),
        ),
        _window(
            "dt_min",
            *_CYCLE,
            {
                "a": ["anchor", "anchor + offset"],
                "clk": ["anchor + lead", "anchor + offset + lead"],
            },
            [
                _count("a", 2),
                _count("clk", 2),
                _count("q", 2),
                {"kind": "paired", "port": "q", "ref": "clk"},
                _before("q", 0, "a", 1),
            ],
        ),
    ],
    "NDRO": [
        _window(
            "t_setup_a",
            *_SETUP,
            {"a": ["anchor"], "b": [], "clk": ["anchor + offset"]},
            [_count("q", 1), _after("q", "clk", ref_index=0)],
            _arrive("a", "clk"),
            _gap("clk", "a"),
        ),
        _window(
            "t_setup_b",
            *_SETUP,
            {"a": ["anchor"], "b": ["anchor + lead"], "clk": ["anchor + lead + offset"]},
            [_count("q", 0)],
            _arrive("a", "b", "clk"),
            _gap("clk", "b"),
        ),
        _window(
            "t_hold_a",
            *_HOLD,
            {
                "a": ["anchor", "anchor + lead + offset"],
                "b": [],
                "clk": ["anchor + lead", "anchor + lead + offset + lead"],
            },
            [
                _count("q", 2),
                _before("q", 0, "a", 1),
                _after("q", "a", 1, 1),
            ],
            _arrive("a", "clk"),
            _gap("a", "clk", 1, 0),
        ),
        _window(
            "t_hold_b",
            *_HOLD,
            {
                "a": ["anchor"],
                "b": ["anchor + lead + offset"],
                "clk": ["anchor + lead", "anchor + lead + offset + lead"],
            },
            [
                _count("q", 1),
                {"kind": "between", "port": "q", "earlier": "clk", "later": "b"},
            ],
            _arrive("a", "b", "clk"),
            _gap("b", "clk", earlier_index=0),
        ),
        _window(
            "dt_min",
            *_DT,
            {
                "a": ["anchor"],
                "b": [],
                "clk": ["anchor + lead", "anchor + lead + offset"],
            },
            [
                _count("a", 1),
                _count("b", 0),
                _count("clk", 2),
                _count("q", 2),
                {"kind": "paired", "port": "q", "ref": "clk"},
            ],
        ),
    ],
    "AND": [
        _window(
            "t_setup_a",
            *_SETUP,
            {"a": ["anchor + lead"], "b": ["anchor"], "clk": ["anchor + lead + offset"]},
            [_count("q", 1), _after("q", "clk", ref_index=0)],
            _arrive("a", "b", "clk"),
            _gap("clk", "a"),
        ),
        _window(
            "t_setup_b",
            *_SETUP,
            {"a": ["anchor"], "b": ["anchor + lead"], "clk": ["anchor + lead + offset"]},
            [_count("q", 1), _after("q", "clk", ref_index=0)],
            _arrive("a", "b", "clk"),
            _gap("clk", "b"),
        ),
        _window(
            "t_hold_a",
            *_HOLD,
            {
                "a": ["anchor + offset"],
                "b": ["anchor + offset + lead"],
                "clk": ["anchor", "anchor + offset + lead + lead"],
            },
            [_count("q", 1), _after("q", "b", ref_index=0)],
            _arrive("a", "b", "clk"),
            _gap("a", "clk", earlier_index=0),
        ),
        _window(
            "t_hold_b",
            *_HOLD,
            {
                "b": ["anchor + offset"],
                "a": ["anchor + offset + lead"],
                "clk": ["anchor", "anchor + offset + lead + lead"],
            },
            [_count("q", 1), _after("q", "a", ref_index=0)],
            _arrive("a", "b", "clk"),
            _gap("b", "clk", earlier_index=0),
        ),
        _window(
            "dt_min",
            *_CYCLE,
            {
                "a": ["anchor", "anchor + offset"],
                "b": ["anchor", "anchor + offset"],
                "clk": ["anchor + lead", "anchor + offset + lead"],
            },
            [
                _count("a", 2),
                _count("b", 2),
                _count("clk", 2),
                _count("q", 2),
                {"kind": "paired", "port": "q", "ref": "clk"},
                _before("q", 0, "a", 1),
            ],
        ),
    ],
    "NOT": [
        _window(
            "t_setup",
            *_SETUP,
            {"a": ["anchor"], "clk": ["anchor + offset"]},
            [_count("q", 0)],
            _arrive("a", "clk"),
            _gap("clk", "a"),
        ),
        _window(
            "t_hold",
            *_HOLD,
            {"clk": ["anchor"], "a": ["anchor + offset"]},
            [
                _count("q", 1),
                {"kind": "between", "port": "q", "earlier": "clk", "later": "a"},
            ],
            _arrive("a", "clk"),
            _gap("a", "clk"),
        ),
        _window(
            "dt_min",
            *_DT,
            {"a": [], "clk": ["anchor", "anchor + offset"]},
            [
                _count("a", 0),
                _count("clk", 2),
                _count("q", 2),
                {"kind": "paired", "port": "q", "ref": "clk"},
            ],
        ),
    ],
    "DCSFQ": [
        _window(
            "dt_min",
            *_DT,
            {"a": ["anchor", "anchor + offset"]},
            [
                _count("a", 2),
                _count("q", 2),
                {"kind": "nearer", "port": "q", "ref": "a"},
            ],
        )
    ],
    "SFQDC": [
        _window(
            "dt_min",
            *_DT,
            {"a": ["anchor", "anchor + offset"]},
            [_count("a", 2), {"kind": "level", "port": "q", "ref": "a"}],
        )
    ],
}


def _gate(setup_port: str) -> list:
    """OR and XOR: one data port against the clock, the other quiet."""
    other = "b" if setup_port == "a" else "a"
    return [
        _window(
            f"t_setup_{setup_port}",
            *_SETUP,
            {setup_port: ["anchor"], other: [], "clk": ["anchor + offset"]},
            [_count("q", 1), _after("q", "clk", ref_index=0)],
            _arrive(setup_port, "clk"),
            _gap("clk", setup_port),
        )
    ]


def _gate_hold(port: str) -> dict:
    other = "b" if port == "a" else "a"
    return _window(
        f"t_hold_{port}",
        *_HOLD,
        {port: ["anchor + offset"], other: [], "clk": ["anchor", "anchor + offset + lead"]},
        [_count("q", 1), _after("q", port, ref_index=0)],
        _arrive(port, "clk"),
        _gap(port, "clk", earlier_index=0),
    )


def _gate_dt() -> dict:
    return _window(
        "dt_min",
        *_CYCLE,
        {
            "a": ["anchor", "anchor + offset"],
            "b": [],
            "clk": ["anchor + lead", "anchor + offset + lead"],
        },
        [
            _count("a", 2),
            _count("b", 0),
            _count("clk", 2),
            _count("q", 2),
            {"kind": "paired", "port": "q", "ref": "clk"},
            _before("q", 0, "a", 1),
        ],
    )


EXPECTED["OR"] = _gate("a") + _gate("b") + [_gate_hold("a"), _gate_hold("b"), _gate_dt()]
EXPECTED["XOR"] = _gate("a") + _gate("b") + [_gate_hold("a"), _gate_hold("b"), _gate_dt()]


def _row(bias: str, window_fs: int | None, bracket: str = "bracketed") -> dict:
    return {
        "cell": "DFF",
        "window": "t_setup",
        "bias_frac": Decimal(bias),
        "window_fs": window_fs,
        "fail_fs": None if window_fs is None else window_fs - 10,
        "bracket": bracket,
        "how": "" if bracket == "bracketed" else "unbracketed: low passes",
    }


def test_bisect_stops_on_a_10_fs_bracket() -> None:
    def fails(offset: int) -> bool:
        return offset < 12_350

    row = bisect(-100_000, 200_000, 10, fails)

    assert row["bracket"] == "bracketed"
    assert row["window_fs"] - row["fail_fs"] == 10
    assert row["window_fs"] == 12_350
    assert row["fail_fs"] == 12_340
    assert fails(row["fail_fs"])
    assert not fails(row["window_fs"])


def test_bisect_reports_an_unbracketed_low_pass() -> None:
    row = bisect(0, 200_000, 10, lambda _offset: False)

    assert row["bracket"] == "unbracketed"
    assert row["window_fs"] is None
    assert row["fail_fs"] is None
    assert "low passes" in row["how"]


def test_bisect_reports_an_unbracketed_high_fail() -> None:
    row = bisect(0, 200_000, 10, lambda _offset: True)

    assert row["bracket"] == "unbracketed"
    assert row["window_fs"] is None
    assert "high fails" in row["how"]


def test_count_missing_and_extra() -> None:
    clause = [_count("q", 1)]

    assert judge(clause, {"q": (1,)}) == ("pass", "")
    assert "missing" in judge(clause, {"q": ()})[1]
    assert "extra" in judge(clause, {"q": (1, 2)})[1]


def test_after_flags_a_misplaced_pulse() -> None:
    clause = [_after("q", "clk")]

    assert judge(clause, {"q": (10,), "clk": (5,)}) == ("pass", "")
    assert "misplaced" in judge(clause, {"q": (5,), "clk": (10,)})[1]
    assert "missing" in judge(clause, {"q": (), "clk": (5,)})[1]

    indexed = [_after("q", "b", 1, 0)]
    assert judge(indexed, {"q": (10, 30), "b": (20,)}) == ("pass", "")
    assert "misplaced" in judge(indexed, {"q": (10, 15), "b": (20,)})[1]


def test_before_flags_a_misplaced_pulse() -> None:
    clause = [_before("q", 0, "a", 1)]

    assert judge(clause, {"q": (110, 300), "a": (100, 200)}) == ("pass", "")
    assert "misplaced" in judge(clause, {"q": (250, 300), "a": (100, 200)})[1]


def test_between_uses_the_first_reference_pulse() -> None:
    clause = [{"kind": "between", "port": "q", "earlier": "clk", "later": "a"}]

    assert judge(clause, {"q": (15,), "clk": (10,), "a": (20,)}) == ("pass", "")
    assert "misplaced" in judge(clause, {"q": (25,), "clk": (10,), "a": (20,)})[1]
    assert judge(clause, {"q": (15,), "clk": (10, 40), "a": (20,)}) == ("pass", "")
    assert "extra" in judge(clause, {"q": (15, 16), "clk": (10,), "a": (20,)})[1]


def test_paired_requires_each_output_in_its_input_gap() -> None:
    clause = [{"kind": "paired", "port": "q", "ref": "a"}]

    assert judge(clause, {"q": (110, 210), "a": (100, 200)}) == ("pass", "")
    assert "misplaced" in judge(clause, {"q": (200, 250), "a": (100, 200)})[1]
    assert "missing" in judge(clause, {"q": (110,), "a": (100, 200)})[1]


def test_nearer_rejects_a_tie() -> None:
    clause = [{"kind": "nearer", "port": "q", "ref": "a"}]

    assert judge(clause, {"q": (-10, 110), "a": (0, 100)}) == ("pass", "")
    assert "misplaced" in judge(clause, {"q": (50, 110), "a": (0, 100)})[1]
    assert "extra" in judge(clause, {"q": (-10, 110, 200), "a": (0, 100)})[1]


def test_level_is_the_written_sfqdc_rule() -> None:
    clause = [{"kind": "level", "port": "q", "ref": "a"}]

    assert judge(clause, {"q": (120, 180, 310), "a": (100, 300)}) == ("pass", "")
    assert "misplaced" in judge(clause, {"q": (120, 600), "a": (100, 300)})[1]
    assert "missing" in judge(clause, {"q": (120,), "a": (100, 300)})[1]


def test_templates_write_every_pass_rule() -> None:
    for cell, windows in EXPECTED.items():
        loaded = yaml.safe_load((BENCHES / f"{cell}.yaml").read_text(encoding="utf-8"))
        assert loaded["anchor_fs"] == HEADER["anchor_fs"]
        assert loaded["lead_fs"] == HEADER["lead_fs"]
        assert loaded["resolution_fs"] == HEADER["resolution_fs"]
        assert loaded["windows"] == windows
        for window in windows:
            assert window["lo_fs"] < window["hi_fs"]
            assert window["lo_fs"] % HEADER["resolution_fs"] == 0
            assert window["hi_fs"] % HEADER["resolution_fs"] == 0


def test_extra_input_pulse_with_a_correct_output_passes() -> None:
    clauses = [_count("q", 1), _after("q", "clk", ref_index=0)]

    assert judge(clauses, {"a": (1, 2), "clk": (5, 9), "q": (12,)}) == ("pass", "")


def test_a_missing_input_is_undelivered() -> None:
    arrived, how = inputs_arrived([{"port": "clk", "n": 1}], {"clk": (1, 2)})
    missing, detail = inputs_arrived([{"port": "clk", "n": 1}], {"clk": ()})

    assert arrived
    assert how == ""
    assert not missing
    assert "missing" in detail


def test_junction_gap_uses_the_named_pulses() -> None:
    gap = junction_gap(
        {"kind": "gap", "later": "clk", "earlier": "a"},
        {"a": (1000, 5000), "clk": (1800, 9000)},
    )
    hold = junction_gap(
        {"kind": "gap", "later": "a", "earlier": "clk", "later_index": 1, "earlier_index": 0},
        {"a": (1000, 4000), "clk": (1800, 9000)},
    )

    assert gap == 800
    assert hold == 2200


def test_bracket_stores_the_junction_gap() -> None:
    window = {"measure": {"kind": "gap", "later": "clk", "earlier": "a"}}
    row = apply_measurement(
        window,
        {"bracket": "bracketed", "window_fs": 50_000, "fail_fs": 49_990, "how": ""},
        pass_pulses={"a": (1000,), "clk": (1800,), "q": (3000,)},
        fail_status="fail",
        fail_how="q: missing, expected 1 got 0",
        fail_pulses={"a": (1000,), "clk": (1790,)},
    )

    assert row["window_fs"] == 800
    assert row["fail_fs"] == 790
    assert row["bracket"] == "bracketed"
    assert "q:" in row["how"]


def test_missing_input_is_not_a_timing_edge() -> None:
    row = apply_measurement(
        {"measure": {"kind": "gap", "later": "clk", "earlier": "a"}},
        {"bracket": "bracketed", "window_fs": 50_000, "fail_fs": 49_990, "how": ""},
        pass_pulses={"a": (1000,), "clk": (1800,)},
        fail_status="undelivered",
        fail_how="a: missing, expected 1 got 0",
        fail_pulses={"clk": (1790,)},
    )

    assert row["bracket"] == "unbracketed"
    assert row["window_fs"] is None
    assert row["fail_fs"] is None
    assert "missing" in row["how"]


def test_dff_hold_keeps_a_negative_offset() -> None:
    window = next(item for item in EXPECTED["DFF"] if item["name"] == "t_hold")

    lo = legal_lo(window["stimulus"], 200_000, 50_000, window["lo_fs"], window["hi_fs"], 10)

    assert window["lo_fs"] == -100_000
    assert lo == -100_000


def test_and_hold_clamps_above_an_illegal_offset() -> None:
    window = next(item for item in EXPECTED["AND"] if item["name"] == "t_hold_a")

    lo = legal_lo(window["stimulus"], 200_000, 50_000, window["lo_fs"], window["hi_fs"], 10)

    assert window["lo_fs"] == -100_000
    assert lo == -94_000
    with pytest.raises(WindowError):
        schedule(window["stimulus"], anchor=200_000, lead=50_000, offset=window["lo_fs"])


def test_dff_setup_places_the_clock_after_the_data() -> None:
    stimulus = EXPECTED["DFF"][0]["stimulus"]

    placed = schedule(stimulus, anchor=200_000, lead=50_000, offset=50_000)

    assert placed["a"] == [200_000]
    assert placed["clk"] == [250_000]


def test_schedule_rejects_a_negative_time() -> None:
    stimulus = EXPECTED["DFF"][0]["stimulus"]

    with pytest.raises(WindowError):
        schedule(stimulus, anchor=200_000, lead=50_000, offset=-250_000)


def test_review_stays_quiet_for_a_flat_or_10_fs_step() -> None:
    flags = review([_row("0.70", 1_000), _row("0.75", 1_000), _row("0.80", 1_010)])

    assert flags == []


def test_review_flags_a_reversal_and_a_2_ps_jump() -> None:
    reversal = review([_row("0.70", 1_000), _row("0.75", 2_000), _row("0.80", 1_500)])
    jump = review([_row("0.70", 1_000), _row("0.75", 3_000)])

    assert any("non-monotonic" in item for item in reversal)
    assert any("noisy" in item for item in jump)


def test_review_flags_a_hole_and_ignores_an_edge_gap() -> None:
    hole = review(
        [
            _row("0.70", 1_000),
            _row("0.75", None, "unbracketed"),
            _row("0.80", 1_000),
        ]
    )
    edge = review(
        [
            _row("0.70", None, "unbracketed"),
            _row("0.75", 1_000),
            _row("0.80", 1_010),
        ]
    )

    assert any("hole" in item for item in hole)
    assert edge == []


def test_setup_hold_plot_is_a_png(tmp_path: Path) -> None:
    rows = [
        _row("0.70", None, "unbracketed"),
        _row("1.00", 12_000),
        {
            "cell": "DFF",
            "window": "t_hold",
            "bias_frac": Decimal("1.00"),
            "window_fs": 8_000,
            "fail_fs": 7_990,
            "bracket": "bracketed",
            "how": "",
        },
    ]
    path = tmp_path / "setup_hold_vs_bias.png"

    write_setup_hold_plot(rows, path)

    data = path.read_bytes()
    assert data.startswith(b"\x89PNG")
    assert len(data) > 1_000


def test_windows_parquet_roundtrip(tmp_path: Path) -> None:
    import pyarrow.parquet as pq

    rows = [_row("0.70", None, "unbracketed"), _row("1.00", 12_350)]
    path = tmp_path / "windows.parquet"

    write_windows(rows, path)
    table = pq.read_table(path)

    assert table.column_names == [
        "cell",
        "window",
        "bias_frac",
        "window_fs",
        "fail_fs",
        "bracket",
        "how",
    ]
    assert table.column("window_fs").to_pylist() == [None, 12_350]
    assert table.column("bias_frac").to_pylist() == ["0.70", "1.00"]
