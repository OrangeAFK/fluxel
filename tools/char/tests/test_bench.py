"""Per-cell characterization benches render through the JoSIM exporter."""

import shutil
from pathlib import Path

import pytest
import yaml

from bench import Arc, delays, format_table, measure_nominal, render

ROOT = Path(__file__).resolve().parents[3]
BENCHES = ROOT / "tools" / "char" / "benches"

# Nominal schedules. Counts are pulses at the DUT reference junction.
SCHEDULES = {
    "JTL": {
        "source": "JTL",
        "load": "JTL",
        "inputs": {"a": [100, 300]},
        "counts": {"a": 2, "q": 2},
    },
    "SPLIT": {
        "source": "JTL",
        "load": "JTL",
        "inputs": {"a": [100, 300]},
        "counts": {"a": 2, "q0": 2, "q1": 2},
    },
    "MERGE": {
        "source": "JTL",
        "load": "JTL",
        "inputs": {"a": [100, 400], "b": [250]},
        "counts": {"a": 2, "b": 1, "q": 3},
    },
    "DFF": {
        "source": "JTL",
        "load": "JTL",
        "inputs": {"a": [100, 500], "clk": [200, 350, 650]},
        "counts": {"a": 2, "clk": 3, "q": 2},
    },
    "AND": {
        "source": "JTL",
        "load": "JTL",
        "inputs": {"a": [100, 400], "b": [450], "clk": [200, 550, 800]},
        "counts": {"a": 2, "b": 1, "clk": 3, "q": 1},
    },
    "OR": {
        "source": "JTL",
        "load": "JTL",
        "inputs": {"a": [200], "b": [500], "clk": [100, 350, 650]},
        "counts": {"a": 1, "b": 1, "clk": 3, "q": 2},
    },
    "XOR": {
        "source": "JTL",
        "load": "JTL",
        "inputs": {"a": [100, 400], "b": [450, 700], "clk": [200, 550, 850]},
        "counts": {"a": 2, "b": 2, "clk": 3, "q": 2},
    },
    "NOT": {
        "source": "JTL",
        "load": "JTL",
        "inputs": {"a": [250], "clk": [100, 400, 600]},
        "counts": {"a": 1, "clk": 3, "q": 2},
    },
    "NDRO": {
        "source": "JTL",
        "load": "JTL",
        "inputs": {"a": [200], "b": [550], "clk": [100, 350, 480, 700]},
        "counts": {"a": 1, "b": 1, "clk": 4, "q": 2},
    },
    "DCSFQ": {
        "source": "current",
        "load": "JTL",
        "inputs": {"a": [100, 300]},
        "counts": {"a": 2, "q": 2},
    },
    "SFQDC": {
        "source": "JTL",
        "load": "resistor",
        "inputs": {"a": [100, 300]},
        "counts": {"a": 2},
        # B8 phase-slips for the whole high interval. That is the DC level.
        "level": "q",
    },
}


def _subckt_count(deck: str, name: str) -> int:
    return sum(1 for line in deck.splitlines() if line.split()[:2] == [".subckt", name])


def _xline(deck: str, label: str) -> str:
    prefix = f"X{label} "
    for line in deck.splitlines():
        if line.startswith(prefix):
            return line
    raise AssertionError(prefix)


def _pins(line: str) -> list[str]:
    return line.split()[2:]


def test_delay_pairs_with_the_latest_earlier_input() -> None:
    pulses = {"a": (100_000,), "b": (250_000,), "clk": (400_000,), "q": (410_000,)}

    assert delays(pulses, ("a", "b", "clk")) == (Arc("clk", "q", 400_000, 10_000),)


def test_delay_can_be_negative_when_the_output_junction_leads() -> None:
    # DCSFQ B3 crosses upward before B1 crosses downward.
    pulses = {"a": (127_930, 327_928), "q": (125_197, 325_194)}

    assert delays(pulses, ("a",)) == (
        Arc("a", "q", 127_930, 125_197 - 127_930),
        Arc("a", "q", 327_928, 325_194 - 327_928),
    )


def test_level_output_reports_on_and_off() -> None:
    pulses = {"a": (100_000, 300_000), "q": (120_000, 140_000, 302_000)}

    assert delays(pulses, ("a",)) == (
        Arc("a", "q", 100_000, 20_000, "on"),
        Arc("a", "q", 300_000, 2_000, "off"),
    )


def test_delay_merger_uses_the_firing_input() -> None:
    pulses = {"a": (100_000, 400_000), "b": (250_000,), "q": (110_000, 260_000, 410_000)}

    assert delays(pulses, ("a", "b")) == (
        Arc("a", "q", 100_000, 10_000),
        Arc("b", "q", 250_000, 10_000),
        Arc("a", "q", 400_000, 10_000),
    )


def test_nominal_schedules() -> None:
    for cell, spec in SCHEDULES.items():
        loaded = yaml.safe_load((BENCHES / f"{cell}.yaml").read_text(encoding="utf-8"))
        assert loaded["cell"] == cell
        assert loaded["source"] == spec["source"]
        assert loaded["load"] == spec["load"]
        assert loaded["bias_frac"] == 1.0
        assert loaded["timestep_ps"] == 0.025
        assert loaded["length_ps"] == 1200
        assert loaded["inputs"] == spec["inputs"]


def test_render_jtl_chain() -> None:
    deck = render(BENCHES / "JTL.yaml")

    assert _subckt_count(deck, "THmitll_JTL") == 1
    for label in ("dut", "src_a", "load_q", "fxsrc_a", "fxload_q"):
        assert _xline(deck, label)
    assert "* fluxel-map dut.a " in deck
    assert "* fluxel-map dut.q " in deck
    assert ".tran 0.025p 1200p 0\n" in deck


def test_bias_frac_is_the_global_multiplier() -> None:
    deck = render(BENCHES / "JTL.yaml", {"bias_frac": 0.8})

    assert ".param bias_mul=0.8\n" in deck
    assert ".param bias_mul=1\n" not in deck


def test_omitted_timestep_defaults_to_25_fs() -> None:
    template = {
        "cell": "JTL",
        "source": "JTL",
        "load": "JTL",
        "bias_frac": 1.0,
        "length_ps": 400,
        "inputs": {"a": [100]},
    }

    deck = render(template)

    assert ".tran 0.025p 400p 0\n" in deck


def test_explicit_length_sets_the_tran_stop() -> None:
    deck = render(BENCHES / "JTL.yaml", {"length_ps": 900})

    assert ".tran 0.025p 900p 0\n" in deck


def test_dcsfq_is_current_driven() -> None:
    deck = render(BENCHES / "DCSFQ.yaml")

    assert "Xfxsrc_" not in deck
    node = _pins(_xline(deck, "dut"))[0]
    assert f"Ifxsrc_a 0 {node} " in deck
    assert _xline(deck, "load_q")
    assert _xline(deck, "fxload_q")


def test_sfqdc_ends_in_a_resistor() -> None:
    deck = render(BENCHES / "SFQDC.yaml")

    assert "Xfxload_" not in deck
    node = _pins(_xline(deck, "dut"))[1]
    assert f"Rfxload_q {node} 0 2\n" in deck
    assert _xline(deck, "src_a")
    assert _xline(deck, "fxsrc_a")


@pytest.mark.skipif(shutil.which("josim-cli") is None, reason="josim-cli not on PATH")
def test_nominal_function() -> None:
    rows = measure_nominal()
    table = format_table(rows)
    print(table)
    by_cell = {row.cell: row for row in rows}
    mismatches: list[str] = []
    for cell, spec in SCHEDULES.items():
        row = by_cell[cell]
        for port, count in spec["counts"].items():
            got = len(row.pulses.get(port, ()))
            if got != count:
                mismatches.append(f"{cell}.{port}: expected {count} crossings, got {got}")
        level = spec.get("level")
        if level:
            times = row.pulses.get(level, ())
            ins = row.pulses.get("a", ())
            spacing = ins[1] - ins[0] if len(ins) == 2 else 0
            if (
                len(ins) != 2
                or len(times) < 2
                or times[0] <= ins[0]
                or times[-1] <= ins[1]
                or times[-1] - ins[1] >= spacing
            ):
                mismatches.append(f"{cell} {level} did not toggle: in={list(ins)} out={list(times)}")
        input_times = sorted(t for port in row.inputs for t in row.pulses.get(port, ()))
        for arc in row.arcs:
            if arc.delay_fs == 0:
                mismatches.append(f"{cell} {arc.src}→{arc.dst} delay is 0")
                continue
            others = [t for t in input_times if t != arc.cause_fs]
            if not others:
                continue
            gap = min(abs(arc.cause_fs - t) for t in others)
            if abs(arc.delay_fs) >= gap:
                mismatches.append(
                    f"{cell} {arc.src}→{arc.dst} delay {arc.delay_fs} fs "
                    f"is not inside the {gap} fs input gap"
                )
    assert not mismatches, table + "\n" + "\n".join(mismatches)
