"""Diff two pulse logs by matching each port in time order.

Pairs as many pulses as the shorter side has. Equal counts pair index with
index. Unequal counts leave the extras unpaired and, among those alignments,
pick the smallest sum of absolute errors. A tie unpairs the earliest
reference pulse, then the earliest candidate pulse. Signed error is
candidate minus reference.
"""

from collections import Counter
from collections.abc import Mapping, Sequence
from dataclasses import dataclass


@dataclass(frozen=True)
class PulseDiff:
    missing: tuple[tuple[str, int], ...]
    extra: tuple[tuple[str, int], ...]
    mean_fs: float
    max_fs: int
    histogram_fs: tuple[tuple[int, int], ...]
    mean_pct: float
    max_pct: float
    histogram_pct: tuple[tuple[float, int], ...]


def diff(
    reference: Mapping[str, Sequence[int]],
    candidate: Mapping[str, Sequence[int]],
    *,
    nominal_fs: int,
) -> PulseDiff:
    if isinstance(nominal_fs, bool) or not isinstance(nominal_fs, int) or nominal_fs <= 0:
        raise ValueError("nominal_fs must be a positive integer")

    missing: list[tuple[str, int]] = []
    extra: list[tuple[str, int]] = []
    errors: list[int] = []
    ports = set(reference) | set(candidate)
    for port in sorted(ports):
        ref = sorted(reference.get(port, ()))
        cand = sorted(candidate.get(port, ()))
        pairs, missed, added = _match_port(ref, cand)
        missing.extend((port, time) for time in missed)
        extra.extend((port, time) for time in added)
        errors.extend(cand_t - ref_t for ref_t, cand_t in pairs)

    if errors:
        mean_fs = sum(errors) / len(errors)
        max_fs = max(errors)
    else:
        mean_fs = 0.0
        max_fs = 0
    histogram_fs = tuple(sorted(Counter(errors).items()))
    histogram_pct = tuple(
        (100 * error / nominal_fs, count) for error, count in histogram_fs
    )
    return PulseDiff(
        missing=tuple(sorted(missing)),
        extra=tuple(sorted(extra)),
        mean_fs=mean_fs,
        max_fs=max_fs,
        histogram_fs=histogram_fs,
        mean_pct=100 * mean_fs / nominal_fs,
        max_pct=100 * max_fs / nominal_fs,
        histogram_pct=histogram_pct,
    )


def _match_port(
    ref: list[int], cand: list[int]
) -> tuple[list[tuple[int, int]], list[int], list[int]]:
    if len(ref) >= len(cand):
        pairs, dropped = _match_drop_longer(ref, cand)
        return pairs, dropped, []
    swapped, dropped = _match_drop_longer(cand, ref)
    return [(ref_t, cand_t) for cand_t, ref_t in swapped], [], dropped


def _match_drop_longer(longer: list[int], shorter: list[int]) -> tuple[list[tuple[int, int]], list[int]]:
    """Pair every shorter pulse with a subsequence of longer. Drop earliest on a tie."""
    n = len(longer)
    m = len(shorter)
    if n == m:
        return list(zip(longer, shorter)), []
    pref = _prefix(longer, shorter)
    suff = _suffix(longer, shorter)
    optimal = pref[m][n]
    pairs: list[tuple[int, int]] = []
    dropped: list[int] = []
    j = 0
    i = 0
    while i < n:
        drop_cost = suff[j][i + 1]
        drop_ok = (
            drop_cost is not None
            and pref[j][i] is not None
            and pref[j][i] + drop_cost == optimal
        )
        pair_ok = False
        if j < m:
            pair_cost = suff[j + 1][i + 1]
            pair_ok = (
                pair_cost is not None
                and pref[j][i] is not None
                and pref[j][i] + abs(shorter[j] - longer[i]) + pair_cost == optimal
            )
        if drop_ok:
            dropped.append(longer[i])
            i += 1
        elif pair_ok:
            pairs.append((longer[i], shorter[j]))
            i += 1
            j += 1
        else:
            raise RuntimeError("pulse alignment could not be reconstructed")
    return pairs, dropped


def _prefix(longer: list[int], shorter: list[int]) -> list[list[int | None]]:
    n = len(longer)
    m = len(shorter)
    dp: list[list[int | None]] = [[None] * (n + 1) for _ in range(m + 1)]
    for i in range(n + 1):
        dp[0][i] = 0
    for j in range(1, m + 1):
        for i in range(1, n + 1):
            best = dp[j][i - 1]
            paired = dp[j - 1][i - 1]
            if paired is not None:
                cost = paired + abs(shorter[j - 1] - longer[i - 1])
                if best is None or cost < best:
                    best = cost
            dp[j][i] = best
    return dp


def _suffix(longer: list[int], shorter: list[int]) -> list[list[int | None]]:
    n = len(longer)
    m = len(shorter)
    dp: list[list[int | None]] = [[None] * (n + 1) for _ in range(m + 1)]
    for i in range(n + 1):
        dp[m][i] = 0
    for j in range(m - 1, -1, -1):
        for i in range(n - 1, -1, -1):
            best = dp[j][i + 1]
            paired = dp[j + 1][i + 1]
            if paired is not None:
                cost = abs(shorter[j] - longer[i]) + paired
                if best is None or cost < best:
                    best = cost
            dp[j][i] = best
    return dp
