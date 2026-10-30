"""Seeded clocked designs for the PyLSE oracle.

Stimuli sit 100 ps off every clock edge. That is more than three times the
largest PyLSE setup/hold among the mapped cells (OR ``_setup_time`` = 5.8 ps).
"""

import random

from design_to_pylse import CELLS

SEED = 1515
MAX_WINDOW_PS = 5.8
MIN_MARGIN_PS = 3 * MAX_WINDOW_PS
PERIOD_FS = 1_000_000
MARGIN_FS = 100_000
CYCLES = 4
LOGIC = ("AND", "DFF", "NOT", "OR", "XOR")


def generate_cases(n: int = 200, seed: int = SEED) -> list[tuple[str, dict, dict]]:
    rng = random.Random(seed)
    return [(f"random-{index:03d}", *generate_one(rng)) for index in range(n)]


def generate_one(rng: random.Random) -> tuple[dict, dict]:
    kind_count = rng.randint(1, 5)
    kinds = [rng.choice(LOGIC) for _ in range(kind_count)]
    share = rng.random() < 0.7
    instances: dict[str, dict] = {}
    data_nets: list[dict] = []
    producers: list[dict] = []
    data_ports: list[str] = []

    def new_primary() -> dict:
        name = f"d{len(data_ports)}"
        data_ports.append(name)
        return {"port": name}

    def take_source() -> dict:
        if not producers:
            return new_primary()
        if share and rng.random() < 0.5:
            return dict(rng.choice(producers))
        if not share and rng.random() < 0.5:
            return producers.pop(rng.randrange(len(producers)))
        return new_primary()

    logic_ids = []
    for index, kind in enumerate(kinds):
        name = f"g{index}"
        logic_ids.append(name)
        instances[name] = _cell(kind)
        width = 2 if kind in ("AND", "OR", "XOR") else 1
        for port in ("a", "b")[:width]:
            data_nets.append(_net(take_source(), {"instance": name, "port": port}))
        producers.append({"instance": name, "port": "q"})

    used = {_pin_key(net["driver"]) for net in data_nets}
    ports_out: list[str] = []
    for pin in producers:
        if _pin_key(pin) in used:
            continue
        port = f"y{len(ports_out)}"
        ports_out.append(port)
        data_nets.append(_net(dict(pin), {"port": port}))

    data_nets = _legalize(instances, data_nets, "s")
    data_nets = _splice_jtls(rng, instances, data_nets)

    leaves = [{"instance": name, "port": "clk"} for name in logic_ids]
    clock_nets: list[dict] = []
    root = _build_tree(leaves, instances, clock_nets, "c", [0])
    clock_nets.append(_net({"port": "clk"}, root))

    clocks = [(cycle + 1) * PERIOD_FS for cycle in range(CYCLES)]
    stimulus: dict[str, list[int]] = {"clk": clocks}
    for port in data_ports:
        stimulus[port] = [
            clock - MARGIN_FS for clock in clocks if rng.random() < 0.5
        ]

    design = {
        "instances": instances,
        "nets": data_nets + clock_nets,
        "ports": {"in": [*data_ports, "clk"], "out": ports_out},
        "clock_domains": {"clk": logic_ids},
        "bias_domains": ["b"],
    }
    if len(instances) > 20:
        raise RuntimeError(f"generated {len(instances)} instances")
    if any(instance["cell"] not in CELLS for instance in instances.values()):
        raise RuntimeError("generated a cell outside the PyLSE table")
    return design, stimulus


def _cell(name: str) -> dict:
    return {"family": "rsfq", "cell": name, "bias_domain": "b"}


def _net(driver: dict, sink: dict) -> dict:
    return {"driver": driver, "sink": sink, "wire": {"kind": "direct"}}


def _pin_key(pin: dict) -> tuple:
    if "instance" in pin:
        return ("instance", pin["instance"], pin["port"])
    return ("port", pin["port"])


def _legalize(instances: dict, nets: list[dict], prefix: str) -> list[dict]:
    groups: dict[tuple, list[dict]] = {}
    order: list[tuple] = []
    for net in nets:
        key = _pin_key(net["driver"])
        if key not in groups:
            groups[key] = []
            order.append(key)
        groups[key].append(net)
    legal: list[dict] = []
    counter = [0]
    for key in order:
        group = groups[key]
        if len(group) == 1:
            legal.append(group[0])
            continue
        extra: list[dict] = []
        root = _build_tree(
            [net["sink"] for net in group], instances, extra, prefix, counter
        )
        legal.append(_net(group[0]["driver"], root))
        legal.extend(extra)
    return legal


def _build_tree(
    leaves: list[dict],
    instances: dict,
    nets: list[dict],
    prefix: str,
    counter: list[int],
) -> dict:
    """Return the pin that drives every leaf through equal-depth splitters."""
    level: list[dict | None] = list(leaves)
    if len(level) == 1:
        return level[0]
    size = 1
    while size < len(level):
        size *= 2
    level.extend([None] * (size - len(level)))
    while len(level) > 1:
        nxt: list[dict | None] = []
        for index in range(0, len(level), 2):
            left, right = level[index], level[index + 1]
            if left is None and right is None:
                nxt.append(None)
                continue
            name = f"{prefix}{counter[0]}"
            counter[0] += 1
            instances[name] = _cell("SPLIT")
            if left is not None:
                nets.append(_net({"instance": name, "port": "q0"}, left))
            if right is not None:
                nets.append(_net({"instance": name, "port": "q1"}, right))
            nxt.append({"instance": name, "port": "a"})
        level = nxt
    if level[0] is None:
        raise RuntimeError("splitter tree produced no root")
    return level[0]


def _splice_jtls(rng: random.Random, instances: dict, nets: list[dict]) -> list[dict]:
    rewritten: list[dict] = []
    serial = 0
    for net in nets:
        sink = net["sink"]
        insert = (
            "instance" in sink
            and sink["port"] in ("a", "b")
            and len(instances) < 20
            and rng.random() < 0.3
        )
        if not insert:
            rewritten.append(net)
            continue
        name = f"j{serial}"
        serial += 1
        instances[name] = _cell("JTL")
        rewritten.append(_net(net["driver"], {"instance": name, "port": "a"}))
        rewritten.append(_net({"instance": name, "port": "q"}, sink))
    return rewritten
