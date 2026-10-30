"""Translate an fx design into a PyLSE script.

The cell table is PyLSE's built-in cells at tag pldi-2022. This module does not
read ``*.cell.json`` and does not emit a PyLSE state machine.
"""

import json

# pylse function, input ports, output ports.
# SPLIT outputs are PyLSE splitter ``l`` then ``r`` (``out1``, ``out2``).
CELLS: dict[str, tuple[str, tuple[str, ...], tuple[str, ...]]] = {
    "JTL": ("jtl", ("a",), ("q",)),
    "SPLIT": ("s", ("a",), ("q0", "q1")),
    "MERGE": ("m", ("a", "b"), ("q",)),
    "DFF": ("dro", ("a", "clk"), ("q",)),
    "AND": ("and_s", ("a", "b", "clk"), ("q",)),
    "OR": ("or_s", ("a", "b", "clk"), ("q",)),
    "XOR": ("xor_s", ("a", "b", "clk"), ("q",)),
    "NOT": ("inv", ("a", "clk"), ("q",)),
}

CLOCKED = frozenset({"DFF", "AND", "OR", "XOR", "NOT"})


class UnknownCell(Exception):
    """The design uses a cell PyLSE does not provide as a built-in."""

    def __init__(self, cell: str) -> None:
        self.cell = cell
        super().__init__(f"no PyLSE built-in for {cell}")


def to_pylse_source(design: dict, stimulus: dict) -> str:
    """Return a Python script that simulates ``design`` with PyLSE built-ins."""
    for instance in design["instances"].values():
        if instance["cell"] not in CELLS:
            raise UnknownCell(instance["cell"])

    incoming = {name: 0 for name in design["instances"]}
    children: dict[str, list[str]] = {name: [] for name in design["instances"]}
    for net in design["nets"]:
        source = _instance(net["driver"])
        sink = _instance(net["sink"])
        if source is not None and sink is not None:
            children[source].append(sink)
            incoming[sink] += 1
    ready = sorted(name for name, count in incoming.items() if count == 0)
    order: list[str] = []
    while ready:
        name = ready.pop(0)
        order.append(name)
        for child in children[name]:
            incoming[child] -= 1
            if incoming[child] == 0:
                ready.append(child)
        ready.sort()
    if len(order) != len(design["instances"]):
        raise ValueError("design has a cycle")

    driven: dict[tuple[str, str], dict] = {}
    for net in design["nets"]:
        sink = net["sink"]
        if "instance" in sink:
            driven[(sink["instance"], sink["port"])] = net["driver"]

    lines = [
        "import json",
        "import pylse",
        "",
        "pylse.working_circuit().reset()",
        "w = {}",
    ]
    for port in design["ports"]["in"]:
        times = sorted(stimulus.get(port, []))
        picoseconds = ", ".join(repr(float(time) / 1000) for time in times)
        if picoseconds:
            call = f"pylse.inp_at({picoseconds}, name={json.dumps(port)})"
        else:
            call = f"pylse.inp_at(name={json.dumps(port)})"
        lines.append(f"w[{json.dumps(port)}] = {call}")

    for name in order:
        function, inputs, outputs = CELLS[design["instances"][name]["cell"]]
        wires = []
        for port in inputs:
            key = json.dumps(_wire(driven[(name, port)]))
            wires.append(f"w[{key}]")
        arguments = ", ".join(wires)
        if function == "s":
            left = f"{name}.q0"
            right = f"{name}.q1"
            lines.append(
                f"outs = pylse.s({arguments}, left_name={json.dumps(left)}, "
                f"right_name={json.dumps(right)})"
            )
            lines.append(f"w[{json.dumps(left)}] = outs.out1")
            lines.append(f"w[{json.dumps(right)}] = outs.out2")
            continue
        wire = f"{name}.{outputs[0]}"
        lines.append(
            f"w[{json.dumps(wire)}] = pylse.{function}({arguments}, name={json.dumps(wire)})"
        )

    lines.extend(
        [
            "events = pylse.Simulation().simulate()",
            "print('FX_ORACLE_JSON')",
            "print(json.dumps(events))",
        ]
    )
    return "\n".join(lines) + "\n"


def _instance(pin: dict) -> str | None:
    if "instance" in pin:
        return str(pin["instance"])
    return None


def _wire(pin: dict) -> str:
    if "instance" in pin:
        return f"{pin['instance']}.{pin['port']}"
    return str(pin["port"])
