import json
from pathlib import Path

from generate import SEED, generate_cases
from run import evaluate, ensure_fx_cli

ROOT = Path(__file__).resolve().parents[3]
EXAMPLES = ("jtl_chain", "splitter_tree", "shift_register", "full_adder")


def load_example(name: str) -> tuple[dict, dict]:
    directory = ROOT / "examples" / name
    design = json.loads((directory / "design.json").read_text(encoding="utf-8"))
    stimulus = json.loads((directory / "stimulus.json").read_text(encoding="utf-8"))
    return design, stimulus


def test_examples_and_random_designs_match() -> None:
    fx_bin = ensure_fx_cli(ROOT)
    lib = ROOT / "lib" / "families" / "rsfq"
    cases = [(name, *load_example(name)) for name in EXAMPLES]
    cases.extend(generate_cases(200, SEED))
    for name, design, stimulus in cases:
        row = evaluate(name, design, stimulus, fx_bin, lib)
        assert row.match, f"{row.report}\n{row.traces}"
