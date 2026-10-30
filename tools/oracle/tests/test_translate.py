import json
from pathlib import Path

import pytest

from design_to_pylse import UnknownCell, to_pylse_source

ROOT = Path(__file__).resolve().parents[3]


def load_example(name: str) -> tuple[dict, dict]:
    directory = ROOT / "examples" / name
    design = json.loads((directory / "design.json").read_text(encoding="utf-8"))
    stimulus = json.loads((directory / "stimulus.json").read_text(encoding="utf-8"))
    return design, stimulus


def test_jtl_script_calls_builtins() -> None:
    design, stimulus = load_example("jtl_chain")
    source = to_pylse_source(design, stimulus)
    assert "pylse.jtl" in source
    assert "pylse.inp_at" in source
    assert "t_a_q" not in source
    assert ".cell.json" not in source


def test_splitter_maps_q0_q1_to_left_right() -> None:
    design, stimulus = load_example("splitter_tree")
    source = to_pylse_source(design, stimulus)
    assert 'left_name="s0.q0"' in source
    assert 'right_name="s0.q1"' in source
    assert "pylse.s(" in source


def test_tff_has_no_builtin() -> None:
    design, _stimulus = load_example("tff_divider")
    with pytest.raises(UnknownCell) as caught:
        to_pylse_source(design, {"in": [0]})
    assert caught.value.cell == "TFF"
