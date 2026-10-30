import math

from extract import Crossing, PhaseCrossings, extract_crossings

PI = math.pi


def _assert_crossings(
    got: PhaseCrossings,
    pulses: list[tuple[float, int]],
    antipulses: list[tuple[float, int]],
) -> None:
    assert list(got.pulses) == [
        Crossing(t, k) for t, k in pulses
    ]
    assert list(got.antipulses) == [
        Crossing(t, k) for t, k in antipulses
    ]


def test_single_2pi_step() -> None:
    # φ: 0, 0, 2π, 2π. The only odd level in the rising interval is π, halfway in phase.
    got = extract_crossings([0.0, 1.0, 2.0, 3.0], [0.0, 0.0, 2 * PI, 2 * PI])

    _assert_crossings(got, [(1.5, 0)], [])


def test_pulse_train() -> None:
    phase = [0.0, 2 * PI, 2 * PI, 4 * PI, 4 * PI, 6 * PI]
    got = extract_crossings([0.0, 1.0, 2.0, 3.0, 4.0, 5.0], phase)

    _assert_crossings(got, [(0.5, 0), (2.5, 1), (4.5, 2)], [])


def test_added_noise_shifts_timestamp_without_extra_crossings() -> None:
    # Samples stay in the same slabs as a clean 0 → 2π step: below π, then in (π, 3π).
    phase = [PI / 10, -PI / 5, 2 * PI + PI / 10, 2 * PI + PI / 5]
    got = extract_crossings([0.0, 1.0, 2.0, 3.0], phase)

    _assert_crossings(got, [(1 + 12 / 23, 0)], [])


def test_pulse_at_trace_start_or_end() -> None:
    start = extract_crossings([0.0, 1.0, 2.0], [0.0, 2 * PI, 2 * PI])
    _assert_crossings(start, [(0.5, 0)], [])

    # Last interval has Δt = 20, so the halfway phase is not a unit-step midpoint.
    end = extract_crossings([0.0, 10.0, 30.0], [0.0, 0.0, 2 * PI])
    _assert_crossings(end, [(20.0, 0)], [])


def test_glitch_that_crosses_pi_and_returns() -> None:
    # Rises through π to 3π/2, then falls back through π. 3π is never reached.
    got = extract_crossings([0.0, 1.0, 2.0], [0.0, 3 * PI / 2, 0.0])

    _assert_crossings(got, [(2 / 3, 0)], [(4 / 3, 0)])


def test_nonzero_phase_offset_at_t0() -> None:
    # Starts at 5π/2, already past π, and the +2π step crosses 3π a quarter of the way.
    got = extract_crossings([0.0, 4.0], [5 * PI / 2, 9 * PI / 2])

    _assert_crossings(got, [(1.0, 1)], [])


def test_exact_sample_on_a_level_counts_once() -> None:
    upward = extract_crossings([0.0, 2.0, 3.0], [0.0, PI, 2 * PI])
    _assert_crossings(upward, [(2.0, 0)], [])

    downward = extract_crossings([0.0, 2.0, 3.0], [2 * PI, PI, 0.0])
    _assert_crossings(downward, [], [(2.0, 0)])

    starts_on_level = extract_crossings([0.0, 1.0], [PI, 2 * PI])
    _assert_crossings(starts_on_level, [], [])
