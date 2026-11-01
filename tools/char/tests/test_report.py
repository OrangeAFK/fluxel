"""Tier-1 versus JoSIM report. Fixtures only; no golden writes."""

import cbor2
from pathlib import Path

import pytest

from report import (
    _anchor_fs,
    count_uncharacterized,
    linreg_slope_ci,
    render_html,
    run_report,
    split_mismatches,
    timestamp_stats,
)

ROOT = Path(__file__).resolve().parents[3]


def test_mismatches_split_on_uncharacterized_instances() -> None:
    rows = (("dut.q", 10, "missing"), ("src_a.q", 20, "extra"))

    inside, outside = split_mismatches(rows, {"dut"})

    assert inside == (("src_a.q", 20, "extra"),)
    assert outside == (("dut.q", 10, "missing"),)


def test_timestamp_stats_use_each_cells_nominal_delay() -> None:
    stats = timestamp_stats(
        (("j0.q", 0, 1000), ("j0.q", 5000, 5000)),
        {"j0": "JTL"},
        {"JTL": 1000},
    )
    early = timestamp_stats((("j0.q", 1000, 0),), {"j0": "JTL"}, {"JTL": 1000})

    assert stats.mean_fs == 500
    assert stats.max_fs == 1000
    assert stats.mean_pct == 50
    assert stats.max_pct == 100
    assert stats.histogram_fs == ((0, 1), (1000, 1))
    assert early.mean_fs == -1000
    assert early.max_fs == 1000
    assert early.max_pct == 100


def test_timestamp_percent_uses_the_delay_magnitude() -> None:
    stats = timestamp_stats((("d.q", 0, 273),), {"d": "DCSFQ"}, {"DCSFQ": -2734})

    assert stats.mean_fs == 273
    assert stats.mean_pct == pytest.approx(100 * 273 / 2734)
    assert stats.max_pct == pytest.approx(100 * 273 / 2734)


def test_anchor_uses_the_delay_magnitude() -> None:
    assert _anchor_fs({"DCSFQ": -2734, "JTL": 1814}) == 2734


def test_flat_slope_interval_includes_zero() -> None:
    fit = linreg_slope_ci((1, 2, 3, 4, 5), (0, 0, 0, 0, 0))

    assert fit is not None
    assert fit.slope == 0
    assert fit.lo <= 0 <= fit.hi


def test_steep_slope_interval_excludes_zero() -> None:
    fit = linreg_slope_ci((1, 2, 3, 4, 5), (5, 10, 15, 20, 25))

    assert fit is not None
    assert fit.slope == 5
    assert fit.lo > 0


def test_slope_is_undefined_without_depth_spread() -> None:
    assert linreg_slope_ci((1, 2), (1, 2)) is None
    assert linreg_slope_ci((1, 1, 1), (1, 2, 3)) is None


def test_uncharacterized_count_reads_the_variant() -> None:
    payload = cbor2.dumps(
        [
            {"Fired": {"t": 1, "inst": "j0", "port": "q"}},
            {
                "Uncharacterized": {
                    "t": 0,
                    "inst": "j0",
                    "entry": "t_a_q",
                    "point": {"bias_frac": 500_000},
                }
            },
            {
                "Uncharacterized": {
                    "t": 5,
                    "inst": "j0",
                    "entry": "t_a_q",
                    "point": {},
                }
            },
        ]
    )

    assert count_uncharacterized(payload) == 2
    assert count_uncharacterized(cbor2.dumps([{"Fired": {"t": 1}}])) == 0


def test_html_has_the_report_sections() -> None:
    html = render_html(
        [
            {
                "name": "gcell_JTL_random_b100",
                "inside": (("dut.q", 10, "missing"),),
                "outside": (("src_a.q", 20, "extra"),),
                "mean_fs": 1.5,
                "max_fs": 4,
                "mean_pct": 0.1,
                "max_pct": 0.2,
                "errors_fs": (1, 2),
                "depths": (1, 2),
                "slope": 0.0,
                "lo": -0.1,
                "hi": 0.1,
                "uncharacterized": 3,
            }
        ]
    )

    assert "dut.q" in html
    assert "src_a.q" in html
    assert "inside" in html
    assert "outside" in html
    assert "Uncharacterized: 3" in html
    assert html.count("<svg") >= 2
    assert "10%" in html
    assert "DCSFQ" in html


def test_render_does_not_write_golden(tmp_path: Path) -> None:
    before = sorted((ROOT / "golden").rglob("*"))

    render_html([])
    (tmp_path / "index.html").write_text(render_html([]), encoding="utf-8")

    assert sorted((ROOT / "golden").rglob("*")) == before


def test_missing_golden_is_an_error(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    suite = tmp_path / "suite"
    case = suite / "gcell_JTL_random_b100"
    case.mkdir(parents=True)
    (case / "design.json").write_text("{}", encoding="utf-8")
    (case / "stimulus.json").write_text("{}", encoding="utf-8")
    (suite / "manifest.json").write_text(
        json_manifest(),
        encoding="utf-8",
    )
    monkeypatch.setattr(
        "report.simulate_case",
        lambda *args, **kwargs: pytest.fail("fx-cli should not run"),
    )

    with pytest.raises(Exception, match="gcell_JTL_random_b100"):
        run_report(suite / "manifest.json", tmp_path / "golden", tmp_path / "out")


def json_manifest() -> str:
    return """[
      {
        "name": "gcell_JTL_random_b100",
        "group": "g-cells",
        "design": "gcell_JTL_random_b100/design.json",
        "stimulus": "gcell_JTL_random_b100/stimulus.json",
        "bias": "1",
        "kind": "random",
        "window": null,
        "edge": null,
        "cell": "JTL",
        "timeout_s": 60,
        "note": ""
      }
    ]
    """
