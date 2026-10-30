"""Pulse log: JSON object mapping a port to sorted integer femtoseconds."""

import json
from collections.abc import Mapping, Sequence


def dump(log: Mapping[str, Sequence[int]]) -> str:
    """Serialize a pulse log. Port names and times are sorted."""
    payload = {port: sorted(times) for port, times in log.items()}
    return json.dumps(payload, sort_keys=True, separators=(",", ":"))


def load(text: str) -> dict[str, tuple[int, ...]]:
    """Parse a pulse log. Times are integers, sorted non-decreasing."""
    data = json.loads(text)
    if not isinstance(data, dict):
        raise ValueError("pulse log must be a JSON object")
    parsed: dict[str, tuple[int, ...]] = {}
    for port, times in data.items():
        if not isinstance(times, list):
            raise ValueError(f"{port}: times must be a list of integers")
        values: list[int] = []
        for time in times:
            if isinstance(time, bool) or not isinstance(time, int):
                raise ValueError(f"{port}: times must be integers")
            values.append(time)
        for earlier, later in zip(values, values[1:]):
            if later < earlier:
                raise ValueError(f"{port}: times must be sorted")
        parsed[port] = tuple(values)
    return parsed
