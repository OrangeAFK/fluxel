import json
from pathlib import Path

from compare import load_cells
from generate import SEED, generate_cases
from run import Status, evaluate, ensure_fx_cli

ROOT = Path(__file__).resolve().parents[3]
EXAMPLES = ("jtl_chain", "splitter_tree", "shift_register", "full_adder")


def load_example(name: str) -> tuple[dict, dict]:
    directory = ROOT / "examples" / name
    design = json.loads((directory / "design.json").read_text(encoding="utf-8"))
    stimulus = json.loads((directory / "stimulus.json").read_text(encoding="utf-8"))
    return design, stimulus


def test_tff_divider_skipped() -> None:
    fx_bin = ensure_fx_cli(ROOT)
    lib = ROOT / "lib" / "families" / "rsfq"
    design, stimulus = load_example("tff_divider")
    row = evaluate("tff_divider", design, stimulus, fx_bin, lib, load_cells(lib))
    assert row.status is Status.SKIPPED
    assert row.report == "tff_divider | skip | skip | unsupported cell TFF"


def test_unknown_cell_is_mismatch() -> None:
    fx_bin = ensure_fx_cli(ROOT)
    lib = ROOT / "lib" / "families" / "rsfq"
    design = {
        "instances": {
            "u": {"family": "rsfq", "cell": "NOPE", "bias_domain": "b"},
        },
        "nets": [
            {
                "driver": {"port": "in"},
                "sink": {"instance": "u", "port": "a"},
                "wire": {"kind": "direct"},
            },
            {
                "driver": {"instance": "u", "port": "q"},
                "sink": {"port": "out"},
                "wire": {"kind": "direct"},
            },
        ],
        "ports": {"in": ["in"], "out": ["out"]},
        "clock_domains": {},
        "bias_domains": ["b"],
    }
    row = evaluate("nope", design, {"in": [0]}, fx_bin, lib, load_cells(lib))
    assert row.status is Status.MISMATCH
    assert row.report == "nope | unclocked | no | no built-in for NOPE"


def test_examples_and_random_designs_match() -> None:
    fx_bin = ensure_fx_cli(ROOT)
    lib = ROOT / "lib" / "families" / "rsfq"
    cells = load_cells(lib)
    cases = [(name, *load_example(name)) for name in EXAMPLES]
    cases.extend(generate_cases(200, SEED))
    rows = [
        evaluate(name, design, stimulus, fx_bin, lib, cells)
        for name, design, stimulus in cases
    ]
    table = "\n".join(row.report for row in rows)
    print(table)
    bad = [row for row in rows if row.status not in (Status.MATCH, Status.SKIPPED)]
    traces = "\n".join(row.traces for row in bad if row.traces)
    assert not bad, f"{table}\n{traces}"
