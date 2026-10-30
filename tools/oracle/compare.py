"""Functional comparison of fx and PyLSE pulse logs.

Timestamps are not compared. Clocked designs contribute one count per clock
window. Unclocked designs contribute a pulse count per output and the order of
outputs, with same-time pulses grouped together.
"""

from design_to_pylse import CLOCKED


def output_drivers(design: dict) -> dict[str, str]:
    """Map each primary output port to the ``instance.port`` that drives it."""
    wanted = set(design["ports"]["out"])
    found: dict[str, str] = {}
    for net in design["nets"]:
        sink = net["sink"]
        if "instance" in sink or sink["port"] not in wanted:
            continue
        driver = net["driver"]
        if "instance" not in driver:
            raise ValueError(f"output {sink['port']} is driven by a port")
        found[sink["port"]] = f"{driver['instance']}.{driver['port']}"
    missing = wanted - set(found)
    if missing:
        raise ValueError(f"outputs without drivers: {sorted(missing)}")
    return found


def is_clocked(design: dict) -> bool:
    return any(instance["cell"] in CLOCKED for instance in design["instances"].values())


def cycle_counts(times_fs: list[int], clocks_fs: list[int]) -> list[int]:
    """Count pulses in ``(clk_k, clk_{k+1}]`` and one period after the last edge."""
    clocks = sorted(clocks_fs)
    if not clocks:
        raise ValueError("clocked design has no clk pulses")
    period = clocks[-1] - clocks[-2] if len(clocks) > 1 else 1_000_000
    edges = [*clocks, clocks[-1] + period]
    counts = []
    for index in range(len(edges) - 1):
        start, stop = edges[index], edges[index + 1]
        counts.append(sum(1 for time in times_fs if start < time <= stop))
    return counts


def unclocked_groups(pulses_by_port: dict[str, list[int]]) -> list[frozenset[str]]:
    """Time-ordered sets of ports. Equal timestamps form one set."""
    grouped: dict[int, set[str]] = {}
    for port, times in pulses_by_port.items():
        for time in times:
            grouped.setdefault(time, set()).add(port)
    return [frozenset(grouped[time]) for time in sorted(grouped)]


def functional_view(
    design: dict, stimulus: dict, pulses_by_driver: dict[str, list[int]]
) -> tuple:
    """Return ``("clocked", {port: counts})`` or an unclocked counts-and-order dict."""
    by_port = {
        port: list(pulses_by_driver.get(driver, []))
        for port, driver in output_drivers(design).items()
    }
    if is_clocked(design):
        clocks = list(stimulus["clk"])
        cycles = {
            port: cycle_counts(times, clocks) for port, times in sorted(by_port.items())
        }
        return ("clocked", cycles)
    counts = {port: len(times) for port, times in sorted(by_port.items())}
    return (
        "unclocked",
        {"counts": counts, "order": unclocked_groups(by_port)},
    )


def format_view(kind: str, view: dict) -> str:
    if kind == "clocked":
        return " ".join(f"{port}:{cycles}" for port, cycles in view.items())
    counts = " ".join(f"{port}:{count}" for port, count in view["counts"].items())
    order = ",".join("+".join(sorted(group)) for group in view["order"])
    return f"{counts} order=[{order}]"


def ps_to_fs(times_ps: list[float]) -> list[int]:
    return [int(round(float(time) * 1000)) for time in times_ps]
