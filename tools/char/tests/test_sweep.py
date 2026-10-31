"""Parallel sweep runner: grids, schedules, cache, and bias curves."""

import json
import os
import shutil
from decimal import Decimal
from pathlib import Path

import pytest
import yaml
from josim import JosimFailed, JosimTimeout

from bench import render
from sweep import (
    SETTLED_DT_PREV_FS,
    bias_delay_series,
    cache_key,
    classify,
    collect,
    expand_axis,
    format_progress,
    grid_points,
    load_sweep,
    patch_deck,
    pool_map,
    probe_job,
    schedule_inputs,
    stop_fs,
    worker_count,
    write_delay_plot,
)

ROOT = Path(__file__).resolve().parents[3]
BENCHES = ROOT / "tools" / "char" / "benches"
LIB = ROOT / "lib" / "families" / "rsfq"
SWEEPS = ROOT / "tools" / "char" / "sweeps"


def _cell(name: str) -> dict:
    return json.loads((LIB / f"{name}.cell.json").read_text(encoding="utf-8"))


def _inputs(cell: str) -> dict:
    loaded = yaml.safe_load((BENCHES / f"{cell}.yaml").read_text(encoding="utf-8"))
    return loaded["inputs"]


def _arc(delay_fs: int, cause_fs: int = 1) -> dict:
    return {"src": "a", "dst": "q", "cause_fs": cause_fs, "delay_fs": delay_fs, "tag": ""}


def test_bias_grid_is_exact_decimal() -> None:
    values = expand_axis({"start": "0.70", "stop": "1.30", "step": "0.05"})

    assert values == [Decimal("0.70") + Decimal("0.05") * i for i in range(13)]
    assert values[0] == Decimal("0.70")
    assert values[-1] == Decimal("1.30")


def test_log_grid_ends_at_min_and_settled() -> None:
    values = expand_axis({"start": "min", "stop": "settled", "spacing": "log", "points": 8})

    assert values[0] == 6_000
    assert values[-1] == SETTLED_DT_PREV_FS == 200_000
    assert len(values) == 8
    assert all(isinstance(item, int) for item in values)
    assert values == sorted(set(values))


def test_sweep_configs_match_the_benches() -> None:
    for path in sorted(BENCHES.glob("*.yaml")):
        bench = yaml.safe_load(path.read_text(encoding="utf-8"))
        cell = bench["cell"]
        sweep = load_sweep(SWEEPS / f"{cell}.yaml")
        ports = _cell(cell)["ports"]["in"]
        axes = sweep["axes"]
        assert sweep["cell"] == cell
        assert set(axes) >= {"bias_frac", "dt_prev_fs"}
        assert ("data_clock_offset_fs" in axes) == ("clk" in ports)
        assert ("in_in_offset_fs" in axes) == (cell == "MERGE")


def test_jtl_and_dff_grids() -> None:
    jtl = grid_points(load_sweep(SWEEPS / "JTL.yaml"))
    dff = grid_points(load_sweep(SWEEPS / "DFF.yaml"))

    assert len(jtl) == 13 * 8
    assert len(dff) == 13 * 8 * 11
    assert min(point["dt_prev_fs"] for point in jtl) == 6_000
    assert max(point["dt_prev_fs"] for point in jtl) == 200_000
    assert {point["data_clock_offset_fs"] for point in dff} == set(range(-50_000, 50_001, 10_000))


def test_jtl_dt_prev_moves_only_the_later_pulse() -> None:
    close = schedule_inputs(_inputs("JTL"), {"dt_prev_fs": 6_000})
    settled = schedule_inputs(_inputs("JTL"), {"dt_prev_fs": 200_000})

    assert close == {"a": [100_000, 106_000]}
    assert settled == {"a": [100_000, 300_000]}


def test_dff_offset_shifts_clocks_after_dt_prev() -> None:
    nominal = schedule_inputs(
        _inputs("DFF"), {"dt_prev_fs": 200_000, "data_clock_offset_fs": 0}
    )
    early = schedule_inputs(
        _inputs("DFF"), {"dt_prev_fs": 200_000, "data_clock_offset_fs": -50_000}
    )

    assert nominal == {
        "a": [100_000, 300_000],
        "clk": [200_000, 400_000, 600_000],
    }
    assert early["a"] == nominal["a"]
    assert early["clk"] == [150_000, 350_000, 550_000]


def test_merger_offset_places_b_on_a() -> None:
    together = schedule_inputs(
        _inputs("MERGE"), {"dt_prev_fs": 200_000, "in_in_offset_fs": 0}
    )
    apart = schedule_inputs(
        _inputs("MERGE"), {"dt_prev_fs": 200_000, "in_in_offset_fs": 10_000}
    )

    assert together == {"a": [100_000, 300_000], "b": [100_000]}
    assert apart["b"] == [110_000]


def test_stop_includes_settle_and_tail() -> None:
    assert stop_fs({"a": [100_000, 106_000]}) == 626_000


def test_patched_deck_matches_render() -> None:
    if shutil.which("cargo") is None:
        pytest.skip("cargo is not on PATH")
    nominal = render(BENCHES / "JTL.yaml")
    for bias, dt, pulses in (
        (Decimal("0.8"), 6_000, [100, 106]),
        (Decimal("1.3"), 200_000, [100, 300]),
    ):
        stimulus = schedule_inputs({"a": [100, 300]}, {"dt_prev_fs": dt})
        assert stimulus == {"a": [pulse * 1_000 for pulse in pulses]}
        length = stop_fs(stimulus)
        patched = patch_deck(nominal, bias, stimulus, length)
        direct = render(
            BENCHES / "JTL.yaml",
            {
                "bias_frac": bias,
                "inputs": {"a": pulses},
                "length_ps": length // 1_000,
            },
        )
        assert patched == direct


def test_classify_jtl_counts() -> None:
    cell = _cell("JTL")
    stimulus = {"a": [100_000, 300_000]}
    good = {"a": (110_000, 310_000), "q": (112_000, 312_000)}
    short = {"a": (110_000, 310_000), "q": (112_000,)}

    assert classify(cell, stimulus, good) == ("expected", "")
    assert classify(cell, stimulus, short) == ("failed", "q: expected 2 got 1")


def test_classify_dff_clock_before_data_fires_nothing() -> None:
    cell = _cell("DFF")
    stimulus = {"a": [100_000], "clk": [50_000]}
    pulses = {"a": (120_000,), "clk": (70_000,)}

    assert classify(cell, stimulus, pulses) == ("expected", "")


def test_classify_sfqdc_level() -> None:
    cell = _cell("SFQDC")
    stimulus = {"a": [100_000, 300_000]}
    toggled = {"a": (110_000, 310_000), "q": (120_000, 140_000, 320_000)}
    stuck = {"a": (110_000, 310_000), "q": (120_000,)}

    assert classify(cell, stimulus, toggled) == ("expected", "")
    assert classify(cell, stimulus, stuck) == ("failed", "q: level did not toggle")


def _job(deck: str, bias: str) -> dict:
    return {
        "params": {"bias_frac": Decimal(bias), "dt_prev_fs": 200_000},
        "deck": deck,
        "stimulus": {"a": [100_000, 300_000]},
    }


def _good_pulses():
    return {"a": (100_000, 300_000), "q": (110_000, 310_000)}, ()


def test_josim_failures_are_kept(tmp_path: Path) -> None:
    jobs = [
        _job("ok", "1.0"),
        _job("bad", "0.70"),
        _job("timeout", "0.75"),
        _job("later", "1.30"),
    ]

    def simulate(deck: str):
        if deck == "bad":
            raise JosimFailed(2, "solver blew up")
        if deck == "timeout":
            raise JosimTimeout(180)
        return _good_pulses()

    rows = collect(
        "JTL",
        _cell("JTL"),
        jobs,
        simulate,
        out_dir=tmp_path,
        josim_version="josim-test",
    )

    assert [row["outcome"] for row in rows] == ["expected", "failed", "failed", "expected"]
    assert rows[1]["how"] == "josim exited 2"
    assert rows[2]["how"] == "josim timed out after 180 s"
    assert rows[0]["arcs"]
    assert rows[1]["arcs"] == []
    assert rows[3]["params"]["bias_frac"] == "1.30"
    assert {arc["delay_fs"] for arc in rows[0]["arcs"]} == {10_000}

    import pyarrow.parquet as pq

    table = pq.read_table(tmp_path / "JTL" / "results.parquet")
    assert table.num_rows == 4
    assert set(table.column("outcome").to_pylist()) == {"expected", "failed"}


def test_warning_keeps_the_arcs(tmp_path: Path) -> None:
    def simulate(_deck: str):
        return _good_pulses()[0], ("phase slipped",)

    rows = collect(
        "JTL",
        _cell("JTL"),
        [_job("warn", "1.0")],
        simulate,
        out_dir=tmp_path,
        josim_version="josim-test",
    )

    assert rows[0]["outcome"] == "failed"
    assert rows[0]["how"] == "josim warning: phase slipped"
    assert rows[0]["arcs"]


def test_cache_key_covers_netlist_params_and_version() -> None:
    params = {"bias_frac": Decimal("1.0"), "dt_prev_fs": 6_000}
    flipped = {"dt_prev_fs": 6_000, "bias_frac": Decimal("1.0")}
    other = {"bias_frac": Decimal("0.8"), "dt_prev_fs": 6_000}

    assert cache_key("deck", params, "v1") == cache_key("deck", flipped, "v1")
    assert cache_key("deck", params, "v1") != cache_key("deck", other, "v1")
    assert cache_key("deck", params, "v1") != cache_key("deck\n", params, "v1")
    assert cache_key("deck", params, "v1") != cache_key("deck", params, "v2")


def test_resume_skips_cached_points(tmp_path: Path) -> None:
    calls = {"n": 0}

    def simulate(_deck: str):
        calls["n"] += 1
        return _good_pulses()

    jobs = [_job("one", "1.0"), _job("two", "1.05")]
    kwargs = {
        "out_dir": tmp_path,
        "josim_version": "josim-test",
    }
    first = collect("JTL", _cell("JTL"), jobs, simulate, **kwargs)
    second = collect("JTL", _cell("JTL"), jobs, simulate, **kwargs)

    assert calls["n"] == 2
    assert [row["key"] for row in second] == [row["key"] for row in first]
    assert all(row["outcome"] == "expected" for row in second)


def test_worker_count_is_cores_minus_one() -> None:
    assert worker_count() == max(1, (os.cpu_count() or 2) - 1)


def test_pool_keeps_a_failed_job() -> None:
    jobs = [{"fail": False}, {"fail": True}, {"fail": False}, {"fail": False}]

    rows = pool_map(probe_job, jobs, workers=2)

    assert len(rows) == 4
    failed = [row for row in rows if row["outcome"] == "failed"]
    assert len(failed) == 1
    assert failed[0]["how"] == "josim exited 1"
    assert sum(row["outcome"] == "expected" for row in rows) == 3


def test_bias_delay_series_is_the_settled_median() -> None:
    records = [
        {
            "bias_frac": "0.70",
            "dt_prev_fs": 200_000,
            "outcome": "expected",
            "arcs": [_arc(10_000), _arc(12_000)],
        },
        {
            "bias_frac": "0.75",
            "dt_prev_fs": 200_000,
            "outcome": "failed",
            "how": "q: expected 2 got 0",
            "arcs": [_arc(9_000)],
        },
        {
            "bias_frac": "0.80",
            "dt_prev_fs": 6_000,
            "outcome": "expected",
            "arcs": [_arc(15_000)],
        },
        {
            "bias_frac": "0.90",
            "dt_prev_fs": 200_000,
            "data_clock_offset_fs": 10_000,
            "outcome": "expected",
            "arcs": [_arc(7_000)],
        },
        {
            "bias_frac": "1.00",
            "dt_prev_fs": 200_000,
            "data_clock_offset_fs": 0,
            "outcome": "expected",
            "arcs": [_arc(8_000)],
        },
        {
            "bias_frac": "1.05",
            "dt_prev_fs": 200_000,
            "outcome": "expected",
            "arcs": [_arc(9_000)],
        },
    ]

    assert bias_delay_series(records) == [
        (Decimal("0.70"), 11.0),
        (Decimal("1.00"), 8.0),
        (Decimal("1.05"), 9.0),
    ]


def test_progress_reports_count_and_eta() -> None:
    text = format_progress(4, 10, 8.0)

    assert "4/10" in text
    assert "ETA" in text
    assert "0:12" in text


def test_plot_is_a_png(tmp_path: Path) -> None:
    records = [
        {
            "bias_frac": "0.70",
            "dt_prev_fs": 200_000,
            "outcome": "expected",
            "arcs": [_arc(10_000)],
        },
        {
            "bias_frac": "1.30",
            "dt_prev_fs": 200_000,
            "outcome": "expected",
            "arcs": [_arc(12_000)],
        },
    ]
    path = tmp_path / "delay_vs_bias.png"

    write_delay_plot(records, path, title="JTL")

    data = path.read_bytes()
    assert data.startswith(b"\x89PNG")
    assert len(data) > 1_000
