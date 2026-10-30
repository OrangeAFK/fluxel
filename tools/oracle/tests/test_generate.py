from design_to_pylse import CELLS
from generate import MIN_MARGIN_PS, SEED, generate_cases


def _driver_key(pin: dict) -> tuple:
    if "instance" in pin:
        return ("instance", pin["instance"], pin["port"])
    return ("port", pin["port"])


def _sink_key(pin: dict) -> tuple:
    return _driver_key(pin)


def test_two_hundred_designs_stay_inside_the_margin() -> None:
    cases = generate_cases(200, SEED)
    assert len(cases) == 200
    assert generate_cases(200, SEED)[0] == cases[0]
    for name, design, stimulus in cases:
        assert name.startswith("random-")
        assert len(design["instances"]) <= 20
        assert design["ports"]["out"]
        for instance in design["instances"].values():
            assert instance["cell"] in CELLS
        clocks = stimulus["clk"]
        assert len(clocks) >= 2
        for port, times in stimulus.items():
            if port == "clk":
                continue
            for time in times:
                nearest = min(abs(time - clock) for clock in clocks) / 1000
                assert nearest >= MIN_MARGIN_PS
        drivers: dict[tuple, int] = {}
        sinks: dict[tuple, int] = {}
        for net in design["nets"]:
            drivers[_driver_key(net["driver"])] = drivers.get(_driver_key(net["driver"]), 0) + 1
            sinks[_sink_key(net["sink"])] = sinks.get(_sink_key(net["sink"]), 0) + 1
        assert all(count == 1 for count in drivers.values())
        assert all(count == 1 for count in sinks.values())
