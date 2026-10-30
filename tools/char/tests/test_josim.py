import math
import shutil
from pathlib import Path

import pytest

from josim import JosimFailed, JosimOutputError, JosimTimeout, run

pytestmark = pytest.mark.skipif(
    shutil.which("josim-cli") is None,
    reason="josim-cli not on PATH",
)

ROOT = Path(__file__).resolve().parents[3]
CHAIN = ROOT / "scratch" / "step6" / "chain.cir"
COLUMNS = (
    "P(B1|XIN)",
    "P(B2|XIN)",
    "P(B1|XMID)",
    "P(B2|XMID)",
    "P(B1|XLOAD)",
    "P(B2|XLOAD)",
)


def test_chain_returns_decision_columns() -> None:
    result = run(CHAIN, timeout=60, columns=COLUMNS)

    assert list(result) == list(COLUMNS)
    time, first = result[COLUMNS[0]]
    assert time[0] == 0.0
    assert time[1] == 2.5e-14
    assert time[-1] == 2.99975e-10
    assert len(time) == 11999
    for name in COLUMNS:
        t, samples = result[name]
        assert t == time
        assert len(samples) == len(time)
        assert samples[0] == 0.0
        assert samples[-1] > 6 * math.pi


def test_nonzero_exit(tmp_path: Path) -> None:
    deck = tmp_path / "broken.cir"
    deck.write_text("R1 1\n.end\n", encoding="utf-8")

    with pytest.raises(JosimFailed) as exc:
        run(deck, timeout=30)

    assert exc.value.returncode != 0
    assert isinstance(exc.value.log, str)


def test_timeout() -> None:
    with pytest.raises(JosimTimeout) as exc:
        run(CHAIN, timeout=0.05)

    assert exc.value.timeout == 0.05


def test_missing_column() -> None:
    with pytest.raises(JosimOutputError) as exc:
        run(CHAIN, timeout=60, columns=("P(B1|XIN)", "P(B9|NOPE)"))

    assert exc.value.missing == ("P(B9|NOPE)",)
