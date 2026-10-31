"""Pulse timestamps from a JJ phase trace.

SPEC §4: a pulse occurs when the phase crosses (2k+1)π, found by linear
interpolation between samples. An upward crossing is a pulse. A downward
crossing is an antipulse.
"""

import math
from collections.abc import Sequence
from dataclasses import dataclass

# Odd multiples of π. Same constant the interpolation below uses for (2k+1)π.
PI = math.pi


@dataclass(frozen=True)
class Crossing:
    t: float
    k: int


@dataclass(frozen=True)
class PhaseCrossings:
    pulses: tuple[Crossing, ...]
    antipulses: tuple[Crossing, ...]


def extract_crossings(time: Sequence[float], phase: Sequence[float]) -> PhaseCrossings:
    """Return upward and downward (2k+1)π crossings of a phase trace.

    ``time`` and ``phase`` are equal-length samples. ``phase`` is in radians.
    Times are returned in the same units as ``time``. A level that lands on a
    sample belongs to the interval that arrives there.
    """
    if len(time) != len(phase):
        raise ValueError("time and phase must have the same length")
    if len(time) < 2:
        return PhaseCrossings((), ())

    pulses: list[Crossing] = []
    antipulses: list[Crossing] = []
    for i in range(len(time) - 1):
        t0 = float(time[i])
        t1 = float(time[i + 1])
        p0 = float(phase[i])
        p1 = float(phase[i + 1])
        if p1 == p0:
            continue
        if p1 > p0:
            for k in _ks_up(p0, p1):
                pulses.append(Crossing(_crossing_time(t0, t1, p0, p1, k), k))
        else:
            for k in _ks_down(p0, p1):
                antipulses.append(Crossing(_crossing_time(t0, t1, p0, p1, k), k))
    return PhaseCrossings(tuple(pulses), tuple(antipulses))


def _crossing_time(t0: float, t1: float, p0: float, p1: float, k: int) -> float:
    # (2k+1)π crossing, linear in time between samples (SPEC §4).
    level = (2 * k + 1) * PI
    return t0 + (t1 - t0) * (level - p0) / (p1 - p0)


def _ks_up(p0: float, p1: float) -> list[int]:
    """Odd levels with p0 < (2k+1)π <= p1, low k first."""
    k0 = math.floor((p0 / PI - 1.0) / 2.0)
    k1 = math.floor((p1 / PI - 1.0) / 2.0) + 1
    return [k for k in range(k0, k1 + 1) if p0 < (2 * k + 1) * PI <= p1]


def pulse_log(
    deck: str,
    traces: dict[str, tuple[tuple[float, ...], tuple[float, ...]]],
) -> dict[str, tuple[int, ...]]:
    """Upward crossings keyed by ``instance.port``.

    ``deck`` carries one ``* fluxel-map <instance.port> <column>`` comment per
    printed junction. Times are femtoseconds, rounded from the trace's seconds.
    A port with no upward crossing is omitted.
    """
    log: dict[str, tuple[int, ...]] = {}
    mapped: list[tuple[str, str]] = []
    for line in deck.splitlines():
        parts = line.split()
        if parts[:2] == ["*", "fluxel-map"] and len(parts) == 4:
            mapped.append((parts[2], parts[3]))
    for key, column in mapped:
        series = traces.get(column)
        if series is None:
            raise KeyError(column)
        time, phase = series
        crossings = extract_crossings(time, phase)
        times = tuple(int(round(crossing.t * 1e15)) for crossing in crossings.pulses)
        if times:
            log[key] = times
    return log


def _ks_down(p0: float, p1: float) -> list[int]:
    """Odd levels with p1 <= (2k+1)π < p0, high k first."""
    k_hi = math.ceil((p0 / PI - 1.0) / 2.0)
    k_lo = math.floor((p1 / PI - 1.0) / 2.0) - 1
    return [k for k in range(k_hi, k_lo - 1, -1) if p1 <= (2 * k + 1) * PI < p0]
