import json
from pathlib import Path

from hypothesis import given
from hypothesis import strategies as st

from pulselog import dump, load

PORT = st.text(alphabet="abcxyz", min_size=1, max_size=3)
TIMES = st.lists(st.integers(min_value=-2000, max_value=2000), max_size=5)
LOG = st.dictionaries(PORT, TIMES, max_size=3)


def _sorted_log(log: dict[str, list[int]]) -> dict[str, tuple[int, ...]]:
    return {port: tuple(sorted(times)) for port, times in log.items()}


@given(LOG)
def test_round_trip(log: dict[str, list[int]]) -> None:
    text = dump(log)

    assert load(text) == _sorted_log(log)
    assert dump(load(text)) == text
    json.loads(text)


def test_kernel_export_parses() -> None:
    text = (Path(__file__).parent / "fixtures" / "kernel_pulses.json").read_text(encoding="utf-8")
    assert load(text) == {"u0.q": (10,)}


def test_reject_non_integer_boolean_and_decreasing() -> None:
    for text in ('{"q": [1.5]}', '{"q": [true]}', '{"q": [2, 1]}'):
        try:
            load(text)
        except ValueError as exc:
            assert "q" in str(exc)
        else:
            raise AssertionError(text)
