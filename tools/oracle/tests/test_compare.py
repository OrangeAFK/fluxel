import pytest

from compare import StimulusSpacingError, cycle_counts, functional_view


def test_cycle_counts_use_half_open_clock_windows() -> None:
    clocks = [1_000_000, 2_000_000]
    times = [1_500_000, 2_000_000, 2_500_000]
    assert cycle_counts(times, clocks) == [2, 1]


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
        "clock_domains": {},
        "bias_domains": ["b"],
    }
    stimulus = {"clk": [1_000_000, 2_000_000, 3_000_000]}
    pulses = {"ff.q": [1_200_000, 3_100_000]}
    kind, view = functional_view(design, stimulus, pulses)
    assert kind == "clocked"
    assert view == {"q": [1, 0, 1]}


def _splitter() -> tuple[dict, dict]:
    design = {
        "instances": {
            "s": {"family": "rsfq", "cell": "SPLIT", "bias_domain": "b"},
        },
        "nets": [
            {
                "driver": {"port": "in"},
                "sink": {"instance": "s", "port": "a"},
                "wire": {"kind": "direct"},
            },
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
        "clock_domains": {},
        "bias_domains": ["b"],
    }
    cells = {
        "SPLIT": {
            "transitions": [
                {
                    "on": "a",
                    "fire": [
                        {"port": "q0", "delay": "t_a_q0"},
                        {"port": "q1", "delay": "t_a_q1"},
                    ],
                }
            ],
            "arc_fs": {"t_a_q0": 1_000, "t_a_q1": 2_000},
        }
    }
    return design, cells


def _view(pulses: dict[str, list[int]], stimulus: dict[str, list[int]]) -> dict:
    design, cells = _splitter()
    kind, view = functional_view(design, stimulus, pulses, cells)
    assert kind == "unclocked"
    return view


def test_skewed_splitter_matches() -> None:
    stimulus = {"in": [0, 100_000]}
    left = {"s.q0": [1_000, 101_000], "s.q1": [2_000, 102_000]}
    right = {"s.q0": [800, 100_800], "s.q1": [1_900, 101_900]}
    assert _view(left, stimulus) == _view(right, stimulus)
    assert _view(left, stimulus) == {"out0": [1, 1], "out1": [1, 1]}


def test_wrong_output_in_epoch_fails() -> None:
    stimulus = {"in": [0, 100_000]}
    left = {"s.q0": [1_000], "s.q1": []}
    right = {"s.q0": [], "s.q1": [1_500]}
    assert _view(left, stimulus) != _view(right, stimulus)


def test_missing_pulse_fails() -> None:
    stimulus = {"in": [0, 100_000]}
    left = {"s.q0": [1_000, 101_000], "s.q1": [2_000, 102_000]}
    right = {"s.q0": [1_000], "s.q1": [2_000, 102_000]}
    assert _view(left, stimulus) != _view(right, stimulus)


def test_extra_pulse_fails() -> None:
    stimulus = {"in": [0, 100_000]}
    left = {"s.q0": [1_000], "s.q1": [2_000]}
    right = {"s.q0": [1_000, 1_500], "s.q1": [2_000]}
    assert _view(left, stimulus) != _view(right, stimulus)


def test_pulse_later_than_path_stays_in_its_epoch() -> None:
    stimulus = {"in": [0, 100_000]}
    # L is 2000 fs. 50_000 fs is far past the path and still before the next input.
    left = {"s.q0": [50_000, 150_000], "s.q1": [50_000, 150_000]}
    right = {"s.q0": [1_000, 101_000], "s.q1": [2_000, 102_000]}
    assert _view(left, stimulus) == _view(right, stimulus)


def test_close_stimulus_raises_spacing_error() -> None:
    design, cells = _splitter()
    with pytest.raises(StimulusSpacingError):
        functional_view(design, {"in": [0, 1_000]}, {"s.q0": [500]}, cells)
