from hypothesis import given
from hypothesis import strategies as st

from diff import diff

PORT = st.text(alphabet="abcxyz", min_size=1, max_size=3)
TIME = st.integers(min_value=-2000, max_value=2000)
TIMES = st.lists(TIME, max_size=5)
LOG = st.dictionaries(PORT, TIMES, max_size=3)
NOMINAL = st.integers(min_value=1, max_value=5000)


def test_concrete_errors() -> None:
    got = diff({"q": [0, 5000]}, {"q": [1000, 5000]}, nominal_fs=1000)

    assert got.missing == ()
    assert got.extra == ()
    assert got.mean_fs == 500
    assert got.max_fs == 1000
    assert got.mean_pct == 50
    assert got.max_pct == 100
    assert got.histogram_fs == ((0, 1), (1000, 1))
    assert got.histogram_pct == ((0.0, 1), (100.0, 1))


@given(LOG, NOMINAL)
def test_self_diff_has_zero_error(log: dict[str, list[int]], nominal_fs: int) -> None:
    got = diff(log, log, nominal_fs=nominal_fs)
    matched = sum(len(times) for times in log.values())

    assert got.missing == ()
    assert got.extra == ()
    assert got.mean_fs == 0
    assert got.max_fs == 0
    assert got.mean_pct == 0
    assert got.max_pct == 0
    if matched == 0:
        assert got.histogram_fs == ()
        assert got.histogram_pct == ()
    else:
        assert got.histogram_fs == ((0, matched),)
        assert got.histogram_pct == ((0.0, matched),)


@given(LOG.filter(lambda log: any(log.values())), TIME, NOMINAL)
def test_shift_error_is_exactly_d(
    log: dict[str, list[int]], d: int, nominal_fs: int
) -> None:
    shifted = {port: [t + d for t in times] for port, times in log.items()}
    got = diff(log, shifted, nominal_fs=nominal_fs)
    matched = sum(len(times) for times in log.values())
    pct = 100 * d / nominal_fs

    assert got.missing == ()
    assert got.extra == ()
    assert got.mean_fs == d
    assert got.max_fs == d
    assert got.mean_pct == pct
    assert got.max_pct == pct
    assert got.histogram_fs == ((d, matched),)
    assert got.histogram_pct == ((pct, matched),)


@given(LOG, PORT, TIME, NOMINAL)
def test_dropping_a_pulse_is_missing(
    log: dict[str, list[int]], port: str, t_fs: int, nominal_fs: int
) -> None:
    reference = {name: list(times) for name, times in log.items()}
    reference.setdefault(port, []).append(t_fs)
    candidate = {name: list(times) for name, times in reference.items()}
    candidate[port].remove(t_fs)

    got = diff(reference, candidate, nominal_fs=nominal_fs)

    assert got.missing == ((port, t_fs),)
    assert got.extra == ()
    assert got.mean_fs == 0
    assert got.max_fs == 0


@given(LOG, PORT, TIME, NOMINAL)
def test_inserting_a_pulse_is_extra(
    log: dict[str, list[int]], port: str, t_fs: int, nominal_fs: int
) -> None:
    candidate = {name: list(times) for name, times in log.items()}
    candidate.setdefault(port, []).append(t_fs)

    got = diff(log, candidate, nominal_fs=nominal_fs)

    assert got.extra == ((port, t_fs),)
    assert got.missing == ()
    assert got.mean_fs == 0
    assert got.max_fs == 0
