from compare import cycle_counts, functional_view, unclocked_groups


def test_cycle_counts_use_half_open_clock_windows() -> None:
    clocks = [1_000_000, 2_000_000]
    times = [1_500_000, 2_000_000, 2_500_000]
    assert cycle_counts(times, clocks) == [2, 1]


def test_unclocked_groups_collapse_equal_timestamps() -> None:
    pulses = {"out0": [10, 20], "out1": [10]}
    assert unclocked_groups(pulses) == [frozenset({"out0", "out1"}), frozenset({"out0"})]


def test_clocked_view_is_per_output() -> None:
    design = {
        "instances": {
            "ff": {"family": "rsfq", "cell": "DFF", "bias_domain": "b"},
        },
        "nets": [
            {
                "driver": {"instance": "ff", "port": "q"},
                "sink": {"port": "q"},
                "wire": {"kind": "direct"},
            }
        ],
        "ports": {"in": ["d", "clk"], "out": ["q"]},
    }
    stimulus = {"clk": [1_000_000, 2_000_000, 3_000_000]}
    pulses = {"ff.q": [1_200_000, 3_100_000]}
    kind, view = functional_view(design, stimulus, pulses)
    assert kind == "clocked"
    assert view == {"q": [1, 0, 1]}


def test_unclocked_view_counts_and_order() -> None:
    design = {
        "instances": {
            "s": {"family": "rsfq", "cell": "SPLIT", "bias_domain": "b"},
        },
        "nets": [
            {
                "driver": {"instance": "s", "port": "q0"},
                "sink": {"port": "out0"},
                "wire": {"kind": "direct"},
            },
            {
                "driver": {"instance": "s", "port": "q1"},
                "sink": {"port": "out1"},
                "wire": {"kind": "direct"},
            },
        ],
        "ports": {"in": ["in"], "out": ["out0", "out1"]},
    }
    pulses = {"s.q0": [5, 15], "s.q1": [5, 15]}
    kind, view = functional_view(design, {"in": [0, 10]}, pulses)
    assert kind == "unclocked"
    assert view["counts"] == {"out0": 2, "out1": 2}
    assert view["order"] == [frozenset({"out0", "out1"}), frozenset({"out0", "out1"})]
