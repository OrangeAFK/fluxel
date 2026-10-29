# Fluxel design rules

Reference prototype: Fluxel Playground (https://claude.ai/artifact/7mjuzwaaQ1NS4rDQ3UG8yT).
Its look and part models are the baseline. Its timing is behavioral and must not be copied into the kernel.

## Principle

Every part is modeled as the physics it stands for, the way redstone parts teach CMOS.
If a part's model does not make its concept visible, the part is not done.

## Part visual language

| Part | Model | Concept it must show |
|---|---|---|
| Josephson junction | dark wheel with a notch; turns exactly once per switching event | pulse = 2π phase slip; whole flux quanta |
| Bias | copper post on every biased JJ; dims below margin, glows orange above | active JJs need bias; operating margins |
| JTL | two wheels joined by an inductor coil, two posts | active, regenerating, delay, bias cost |
| PTL | flat strip over dark ground plane; no wheels, no posts | passive, fast; driver/receiver wheels auto-drawn where it meets active parts |
| Splitter | Y plate, 3 wheels, extra bias | fanout = 1; branching costs hardware |
| Merger | inputs with escape wheels (orange notch), output wheel | close pulses dumped; a loop cannot hold 2Φ₀ |
| Storage loop | glass ring; bead orbits while holding Φ₀; bright while settling | bit = persistent current; destructive readout; setup window |
| Clocked gate | input rings + yellow clock pad | every SFQ gate is a clocked memory |
| Clock | yellow flywheel, one turn per pulse | clock = pulse train with delay and skew |
| DC→SFQ | push-button + output wheel | one step → one pulse |
| SFQ→DC | lamp toggling per pulse | readout toggles a DC level |

Port marks: input = dark pad, output = cyan pad, clock input = yellow pad.
Parts face away from the player when placed. Output is local +X.

## Rendering contract

- Animation is driven only by kernel events. No timers, no decorative motion.
- A pulse event at a cell spins that cell's junctions in their real switching order across its characterized delay.
- Failed switch = half-turn that falls back. Spontaneous switch = wheel spins with no input. `Uncharacterized` = tinted part.
- A bead exists only while the cell's state machine is in a stored state.
- Far parts collapse to plain blocks in their category color (LOD). Part models are instanced.

## Color (fixed meanings, never decorative)

| Token | Hex | Meaning |
|---|---|---|
| flux | #5ff0ff | pulse in flight, stored quantum, output port |
| violation | #ff8a1f | violation, lost or dumped pulse, escape junction |
| clock | #d8b43a | clock source, clock pads, clock pulses (#ffd24a in flight) |
| bias | #c27a45 | bias posts |
| niobium | #8e96ab | part base plates |
| dielectric | #1f2633 | PTL ground/dielectric |
| night | #142036 | sky / world background |
| substrate | #28303f | floor |
| cryo | #141826 | dark data panels |
| stone | #b8bcc4 | light control panels |
| frost | #d8dde8 | text on dark |
| ink | #1b1d22 | text on light, outlines |

Slack overlay: orange (<0) → light orange (0–1 ps) → gray (n/a) → light blue (1–3 ps) → blue (>3 ps). Colorblind-safe; orange always means trouble.

## UI

- Fonts: Pixelify Sans (UI words), VT323 (all numbers, logs, waveforms). Min 12 px UI, 16 px VT323.
- Panels: 2 px black outline + 3 px bevel (light top-left, dark bottom-right). No rounded corners, no gradients, no shadows beyond the outline.
- Stone panels hold controls; cryo panels hold data.
- Touch targets ≥ 44 px. Hotbar slots 56 px.
- Every targeted part shows: name, JJ count, bias posts, delay at current bias, phase (slips × 2π), and a one-sentence concept line.

## Process

- Agents use tokens from `web/src/theme/tokens.ts` only; no literal colors or fonts in components.
- A new part, overlay, panel region or interaction pattern: stop, show 2–3 options (screenshot or HTML mock), wait for a pick, log it in `DECISIONS.md`.
- Visual review at M4, M5 and M9 via the `/gallery` route and its Playwright screenshots.
