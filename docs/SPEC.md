# Fluxel — SFQ Voxel Workbench: Spec & Implementation Method

Oct 25, 2026 · @V S

## 1. Purpose, scope, claims

Fluxel is an interactive, multi-fidelity simulator for superconducting digital logic with a voxel editor: build or import a circuit, run it, and see pulses, timing slack and bias margins on the blocks themselves. Working name; rename freely.

**Goals**

- **G1** RSFQ pulse/timing simulation consistent with JoSIM, using RSFQlib v3.0 cell data.
- **G2** Build → run → inspect loop: place blocks, simulate, scrub time, see each violation on the block that caused it.
- **G3** Verilog in → legalized, placed, simulated design → JoSIM netlist out.
- **G4** Cross-family comparison: one design mapped to RSFQ, ERSFQ, xSFQ, xeSFQ, AQFP and HFQ, compared on JJ count, latency, power, margins and legalization overhead.
- **G5** Analog “X-ray” of any cell, driven by its recorded input pulse times.

**Non-goals**

- Layout-accurate parasitics. Voxel geometry is not physical layout.
- Flux trapping, stray magnetic fields, ground-return crosstalk.
- Replacing JoSIM or WRspice for signoff.
- Any claim of silicon accuracy.

**Claims policy (enforced in the UI)**

- Every displayed number carries a provenance tag: `characterized` (RSFQlib + JoSIM), `self-characterized` (cells transcribed from papers), or `behavioral` (illustrative timing).
- Accuracy is stated only as “consistent with JoSIM within the measured error of the golden suite”, per family.
- Operation outside a cell’s characterized range emits an `Uncharacterized` event. The kernel never extrapolates silently.

**License:** GPLv3 for the whole project, because the bundled RSFQlib netlists and tables derived from them are GPLv3. Not legal advice.

## 2. Architecture

The stack has three parts: a Rust core compiled both natively and to WASM, a TypeScript + three.js front end, and Python tooling that drives JoSIM for characterization and golden data. Every simulation tier lives in the Rust crates, so the browser and the CLI give identical results.

&#91;embedded content: data flow · offline characterization feeds one shared Rust core\]

Characterization runs offline. The browser and the CLI load the same LUT bundles and run the same crates, so a result reproduced in CI is the result the UI shows.

```text
fluxel/
  Cargo.toml              # workspace
  crates/
    fx-time/              # Femtos time type, ordering
    fx-ir/                # netlist IR, cell defs, LUT bundle types, serde schemas
    fx-kernel/            # discrete-event engine, event log, snapshots, replay
    fx-families/          # plugins: rsfq, ersfq, xsfq, xesfq, aqfp, hfq, rql_pcl
    fx-sta/               # static timing: clock arrival, slack, Fmax
    fx-stat/              # Monte Carlo, spatial correlation
    fx-analog/            # phase-based MNA solver (RCSJ) + netlist parser subset
    fx-flow/              # Yosys import, legalizers, placer, router, exporters
    fx-world/             # voxel world, chunks, world -> Design compiler
    fx-cli/               # headless: sim, golden, bench, char-check
    fx-wasm/              # wasm-bindgen API for the web app
  web/                    # Vite + TypeScript + three.js
    src/render  src/editor  src/overlays  src/panels  src/worker
  tools/char/             # Python: JoSIM harness, sweeps, extraction, LUT fitting
  lib/
    rsfqlib/              # vendored RSFQlib v3.0 (GPLv3), unmodified
    families/<family>/    # cell defs (JSON) + LUT bundles (CBOR)
  golden/                 # JoSIM reference runs + stimuli; read-only to agents
  docs/  SPEC.md  MILESTONES.md  DECISIONS.md
  .cursor/rules/
```

**Crate dependency rule:** `fx-time` ← `fx-ir` ← {`fx-kernel`, `fx-analog`} ← `fx-families` ← {`fx-sta`, `fx-stat`, `fx-flow`, `fx-world`} ← {`fx-cli`, `fx-wasm`}. No upward dependencies. `fx-analog` must not depend on `fx-kernel`.

| Tool | Notes | Used for |
| --- | --- | --- |
| Rust stable | + `wasm32-unknown-unknown` target | core |
| wasm-bindgen / wasm-pack | single-threaded by default | WASM API |
| Node LTS, Vite, TypeScript (strict) | — | front end |
| three.js | instanced meshes, greedy-meshed chunks | voxel rendering |
| Python 3.11+ | numpy, scipy, pyyaml, pytest, hypothesis, matplotlib, pyarrow | characterization, golden data |
| JoSIM | built from source (CMake); invoked as CLI from Python | reference analog sim |
| Yosys | OSS CAD Suite | synthesis and import |
| PyLSE | installed from GitHub | functional oracle for the kernel |

**Rust crates to standardize on:** serde + ciborium (CBOR), thiserror, proptest, criterion, rayon (native only), faer or sprs for sparse LU.

**WASM threading:** multithreaded WASM requires cross-origin isolation (COOP/COEP headers). Default to single-threaded WASM, with parallelism through a pool of Web Workers, one simulation per worker.

## 3. Core data model

All simulation time is held as integer femtoseconds, and every design-level structure enforces fanout = 1. Those two invariants make results deterministic, rewind exact, and illegal SFQ wiring impossible to represent.

**Time and events**

- `Femtos(i64)`: all kernel time. The range is about ±2.5 hours of simulated time. LUT outputs are rounded to 1 fs. No `f64` time inside the kernel.
- `Event { t: Femtos, seq: u64, target: CellId, port: PortId, kind: EventKind }`, where `EventKind = Pulse | AntiPulse | PhaseTick(u8)`.
- Queue order is `(t, target, port, seq)`. `seq` is a global insertion counter, so ties break deterministically.
- Event log: an append-only record of fired events and violations, with a snapshot of all cell state every N events. Scrubbing restores the nearest snapshot and replays forward.

**Cell machine (one JSON file per cell, PyLSE-style)**

```json
{
  "name": "DFF", "family": "rsfq", "provenance": "characterized",
  "ports": { "in": ["d", "clk"], "out": ["q"] },
  "faces": { "d": "W", "clk": "S", "q": "E" },
  "states": ["empty", "stored"], "initial": "empty",
  "transitions": [
    { "from": "empty",  "on": "d",   "to": "stored", "fire": [] },
    { "from": "stored", "on": "d",   "to": "stored", "fire": [], "note": "second pulse escapes" },
    { "from": "stored", "on": "clk", "to": "empty",  "fire": [{ "port": "q", "delay": "t_clk_q" }] },
    { "from": "empty",  "on": "clk", "to": "empty",  "fire": [] }
  ],
  "constraints": [
    { "kind": "setup", "first": "d",   "then": "clk", "window": "t_setup" },
    { "kind": "hold",  "first": "clk", "then": "d",   "window": "t_hold" }
  ],
  "jj_count": "<from RSFQlib>", "bias_ua": "<from RSFQlib>",
  "netlist": "lib/rsfqlib/<cell>.cir", "lut": "lib/families/rsfq/DFF.lut.cbor"
}
```

Delay and window fields name LUT entries. They never hold literal numbers, except in `behavioral` cells.

**LUT bundle (CBOR, one per cell)**

- One entry per delay or window name: `axes` (for example `bias_frac`, `dt_prev_fs`, `offset_fs`), grid points, values in fs, interpolation (multilinear), `valid` box, `sigma_fs` for Monte Carlo.
- `source`: JoSIM version, SHA-256 of the netlist, sweep-config hash, timestep.
- JSON export for inspection (`fx-cli lut dump`).

**Netlist IR (`fx-ir::Design`)**

- `Design { instances, nets, ports, clock_domains, bias_domains }`.
- `Net { driver: PinRef, sink: PinRef, wire: Wire }`. Exactly one sink. The constructor rejects fanout > 1, and the legalizer inserts splitters.
- `Wire = Direct | Jtl { stages: u32 } | Ptl { length_units: u32 }`. Delays come from the library’s interconnect characterization.
- `Instance { id, family, cell, bias_domain, macro_parent: Option<MacroId> }`.

**Voxel world (`fx-world`)**

- Sparse map of 16×16×16 chunks.
- `Block { kind, facing: Dir6, layer: u8 }`, where `kind = Cell(CellRef) | JtlStage | PtlSegment | Source | Probe | ClockSource | BiasTap | MacroPort`.
- One cell occupies one voxel (abstract scale). Ports sit on faces according to the cell’s `faces` map, rotated by `facing`.
- Connectivity: an output face touching a compatible input face forms a net. Chained JTL/PTL voxels collapse into one `Wire`.
- The `world → Design` compiler is pure and incremental: only dirty chunks recompile.
- Macro: a saved sub-world with declared ports, placed as a single block and flattened at compile time.
- Save file: CBOR `{ schema_version, world, stimulus, metadata }`, with versioned migrations.

## 4. Simulation tiers

There are four tiers, and they share one pulse-timestamp definition: **a pulse occurs when the reference JJ’s phase crosses (2k+1)π**, found by linear interpolation between samples. Python extraction, the analog tier and the golden suite all use this definition, so differences between tiers reflect the models, not how timestamps were taken.

### 4.1 Event kernel (tier 1, `fx-kernel`)

For each input event at time t on cell c, port p:

1. Find the transition for (state, p). If none exists, record an `IllegalInput` violation and leave the state unchanged.
2. Check every constraint that involves p against the per-port last-arrival times. A breach records `SetupViolation` or `HoldViolation` with its signed slack.
3. For each fired output, delay = LUT(name; `bias_frac` of c’s bias domain, `dt_prev` = t − c’s last input time, plus any other axes). If an axis is outside the valid box, record an `Uncharacterized` event and use the clamped value.
4. Schedule each output at t + delay + wire delay of its net.
5. Update state and last-arrival times.

Further rules:

- **Merger collision:** a second input inside `t_merge_window` follows the characterized behavior (one output pulse) and logs a `Collision` info event.
- **Violation policy:** `record` (default, keep simulating) or `halt`.
- **Determinism:** identical design and stimulus must give a byte-identical event log on every platform. CI checks this.
- **Performance target:** ≥10⁷ events/s native, single-threaded, using a binary heap. Move to a calendar queue only if profiling demands it.

### 4.2 Characterization pipeline (`tools/char`)

1. **Testbench template per cell:** source cells → DUT → load cells, so input pulse shape and output loading are realistic.
2. **Sweeps** (YAML config, run in parallel processes):
   - bias\_frac across the rated margin
   - dt\_prev from minimum to settled
   - data–clock offset (clocked cells)
   - input–input offset (merger)
3. **Timestep:** 0.025 ps by default, matching published RSFQlib simulations.
4. **Extraction:** pulse timestamps on the reference JJ of each port, then delays per timing arc.
5. **Timing windows:** bisection on offset to the first functional failure (setup, hold, collision window).
6. **Storage:** write the LUT grid and metadata, then validate on held-out random points and report interpolation error.

### 4.3 Analog tier (tier 3, `fx-analog`)

Phase-based modified nodal analysis. The unknowns are node phases φ (plus branch currents for voltage sources), with Φ₀ = 2.067833848 × 10⁻¹⁵ Wb.

```latex
I_{JJ} = I_c \sin\phi + \frac{\Phi_0}{2\pi R}\dot{\phi} + \frac{\Phi_0 C}{2\pi}\ddot{\phi}, \qquad I_L = \frac{\Phi_0}{2\pi L}(\phi_a - \phi_b), \qquad V = \frac{\Phi_0}{2\pi}\dot{\phi}
```

- **Integration:** trapezoidal, fixed step h (default 0.025 ps).
- **Mode B (reference, build first):** Newton iteration on the sin φ term each step.
- **Mode A (fast):** sin φ extrapolated from previous steps and moved to the right-hand side, so the matrix stays constant. Factor once and back-substitute each step. Accepted only where it matches Mode B within threshold.
- **Elements:** JJ (RCSJ, parameters from RSFQlib `.model` cards), L, R, C, K (mutual inductance).
- **Sources:** DC, PWL, PULSE, SIN (for AQFP excitation), and Johnson noise current sources with spectral density 4kT/R.
- **Netlist parser:** the JoSIM/WRspice syntax subset needed for RSFQlib `.cir` files (`.subckt`, `.model`, `.param`).
- **Sanity tests:**
  - ∫V dt over one pulse equals Φ₀.
  - A single JJ: average V ≈ 0 below Ic, and V = Φ₀ × f above it.
  - A JTL chain matches JoSIM.
- **HFQ:** use the junction model from the 2φ library paper, which validated its cells in JoSIM.

### 4.4 Statistical tier (tier 2, `fx-stat`)

- **Per-instance delay:** d = d\_nom × (1 + ε\_global + ε\_spatial(x, y) + ε\_local).
  - The σ values come from the LUT `sigma_fs` entries.
  - ε\_spatial is a Gaussian random field with correlation length λ, a user parameter. This follows qSSTA’s use of spatially correlated, LUT-based gate models.
- **Outputs:** functional yield, Fmax distribution, per-path slack distributions.
- **Execution:** rayon over seeds natively; in the browser, a Web Worker pool with one seed per worker.
- **Calibration:** compare the simulated delay spread with JoSIM Monte Carlo (component-value spread) on golden cells.

### 4.5 Cross-check (tier 1 vs tier 3)

1. Select a region.
2. Extract its subnetlist, using the recorded boundary input times as sources.
3. Run the analog tier on it.
4. Diff that pulse log against the tier-1 log restricted to the region.

This runs in CI on golden circuits and on demand from the UI.

## 5. Family plugins

A family is a plugin that supplies event kinds, a clock model, a bias model, a cell library, checkers, a legalizer and metrics. The kernel, flow and UI never branch on a family name.

```rust
pub trait Family {
    fn id(&self) -> FamilyId;
    fn event_kinds(&self) -> &[EventKind];            // Pulse | AntiPulse | PhaseTick
    fn clock_model(&self) -> ClockModel;               // DcTree | Alternating2 | MultiphaseAc { phases } | None
    fn bias_model(&self) -> BiasModel;                 // Resistive | Ersfq | Ac
    fn library(&self) -> &CellLibrary;
    fn checkers(&self) -> Vec<Box<dyn Checker>>;       // run on the design and the event stream
    fn legalize(&self, d: &mut Design) -> Result<LegalizeReport, FlowError>;
    fn metrics(&self, d: &Design, log: &EventLog) -> Metrics; // JJ count, static/dynamic power, latency
}
```

| Family | Events | Clock | Bias | Family-specific checkers | Legalizer | Provenance |
| --- | --- | --- | --- | --- | --- | --- |
| RSFQ | Pulse | DC clock tree | Resistive | setup/hold, collision | splitters, path-balancing DFFs, clock tree | characterized (RSFQlib) |
| ERSFQ | Pulse | DC clock tree | ERSFQ | + bias-domain phase imbalance | as RSFQ | self-characterized |
| xSFQ | Pulse, dual-rail | Alternating excite/relax | Resistive | alternation, init pattern | dual-rail AIG mapping, retiming | self-characterized |
| xeSFQ | Pulse, dual-rail | Alternating excite/relax | ERSFQ | + one pulse per line per cycle | as xSFQ | self-characterized |
| AQFP | PhaseTick | Multiphase AC | AC | row/phase alignment | buffer + splitter insertion | self-characterized |
| HFQ (2φ) | Pulse | DC clock tree | Resistive | as RSFQ | as RSFQ | self-characterized |
| RQL / PCL | Pulse + AntiPulse | Multiphase AC | AC | pulse/antipulse pairing | maps to its gate set | behavioral |

**Per-family rules**

- **ERSFQ:** In ERSFQ, bias JJs still switch to correct phase imbalances across the circuit, and this can cost as much as dynamic power. Model it per bias domain as net pulses accumulated (in − out) over a window, converted to estimated bias-JJ switching events. Label it an approximation and validate it against JoSIM on small domains.
- **xSFQ:**
  - AND is a Last Arrival cell (Muller C-element); OR is a First Arrival cell (its inverse).
  - Each logical cycle has an excite phase carrying values and a relax phase carrying complements; the relax phase resets every cell.
  - The checker flags lines that miss or duplicate a pulse within a logical cycle, and DRO pairs that start with the same initial value.
- **xeSFQ:** xSFQ encoding with ERSFQ biasing, which gives one pulse per line per logical cycle. The checker enforces that property.
- **AQFP:**
  - The AC excitation is both power and clock. Logic is majority-3 based, and the cell set is buffers, NOT, constants and branch cells.
  - The evaluator steps by phase: each cell latches the majority polarity of its inputs on its phase.
  - The legalizer inserts buffers so every edge spans exactly one phase. Each logic level sits in one row.
- **RQL / PCL:** A positive pulse encodes a rising edge and a negative pulse a falling edge. PCL’s three-input, three-output gate produces or3, maj3 and and3. Timing is illustrative, and the UI shows a `behavioral` badge on every cell.

## 6. Design flow (`fx-flow`)

The flow takes Verilog through import, mapping, legalization, placement and routing, then back-annotates routed wire delays into the simulator. Without back-annotation the voxel layout would be decoration rather than design.

1. **Import:** Yosys runs `read_verilog`, then `synth -top <top>`, then a family-specific `abc` gate set (AND/OR/XOR/NOT for RSFQ, AIG for xSFQ, majority form for AQFP), then `write_json`. The JSON is parsed into IR with generic gates.
2. **Tech map:** generic gates are mapped to family cells using a table per family.
3. **Legalize (family plugin):**
   - fanout > 1 → balanced splitter trees
   - path balancing: compute each gate’s level and insert DFFs (RSFQ) or buffers (AQFP) so every input of a clocked gate comes from the same stage
   - clock-tree synthesis: a splitter tree from the clock source to every clocked cell; concurrent clocking by default, counter-flow optional
4. **Place:**
   - x = stage (pipeline level, or AQFP phase row) × pitch
   - y = order within the stage, using barycenter crossing minimization (Sugiyama, 4–8 sweeps)
   - z = layer: data 0, clock 1, bias 2
   - Sequential loops: cut cycles at DFFs and route the back-edges on a dedicated layer.
5. **Route:**
   - A\* maze routing on the voxel grid, nets ordered by criticality (worst slack first)
   - costs for bends, layer changes and congestion; rip-up and reroute on failure
   - routes become PTL segments, or JTL stages if selected
6. **Back-annotate:** routed lengths become wire delays. Re-run tier 1 and STA. If path balance broke, insert delay JTLs and repeat.
7. **Export:** JoSIM `.cir` (RSFQlib subcircuits + PTL models + stimulus sources), structural Verilog, a PyLSE script (oracle), and a metrics CSV.

**Placement acceptance metrics:** crossings, total wirelength, bounding volume, runtime, and a readability check: can someone find the carry chain of an 8-bit Kogge-Stone adder within 30 s? If not, imported designs fall back to a schematic view, and voxels are kept for hand-built designs.

## 7. Front end (`web/`)

The primary view is a first-person voxel world. Each SFQ part is modeled as the physics it stands for, the way redstone parts teach CMOS. The reference prototype is [Fluxel Playground](https://claude.ai/artifact/7mjuzwaaQ1NS4rDQ3UG8yT); its look and part models are the baseline, and its timing is behavioral only. The simulator runs in a Web Worker on `fx-wasm`; the main thread renders and handles input.

**Part visual language**

| Part | Model | Concept it must make visible |
| --- | --- | --- |
| Josephson junction | dark wheel with a notch, turns exactly once per switching event | a pulse is a 2π phase slip; flux comes in whole quanta |
| Bias | copper post on every biased JJ; dims or glows orange with bias | every active JJ needs bias; margins |
| JTL | two wheels joined by an inductor coil, two posts | active, regenerating, has delay, costs bias |
| PTL | flat strip over a ground plane, no wheels or posts | passive and fast; driver and receiver wheels drawn automatically where it meets active parts |
| Splitter | Y plate, three wheels, extra bias | fanout = 1; branching costs hardware |
| Merger | inputs with escape wheels (orange notch) | close pulses are dumped; a loop cannot hold 2Φ₀ |
| Storage loop (DFF, gate inputs) | glass ring; an orbiting bead while it holds Φ₀; bright while settling | a bit is a persistent current; destructive readout; setup = clocking an unsettled loop |
| Clocked gate | input rings plus a yellow clock pad | every SFQ gate is a clocked memory |
| Clock | yellow flywheel, one turn per pulse; clock pulses drawn yellow | clock is a pulse train with delay and skew |
| DC→SFQ / SFQ→DC | push-button / lamp that toggles per pulse | one step makes one pulse; readout toggles a DC level |

**Rendering contract**

- Visuals are driven only by the kernel event log, never by animation timers. A pulse event at a cell maps to a spin schedule over that cell's junctions in their real switching order, using the cell's characterized delay.
- Failure states map to visible events: a failed switch is a half-turn that falls back; spontaneous switching spins a wheel with no input; `Uncharacterized` tints the part.
- No visual state without kernel state: a bead exists only while the cell state machine is in a stored state.
- Fixed color meanings: cyan = flux pulse or stored quantum, orange = violation or loss, yellow = clock, copper = bias. Never reused for decoration.
- Far parts collapse to plain colored blocks (LOD); per-part models are instanced.

**Views**

- **World (primary):** first-person, fly mode, hotbar of parts, crosshair targeting, placement facing away from the player.
- **Map (secondary):** top-down or isometric overview for large or imported designs; same world data.
- **Inspector line:** part name, JJ count, bias posts, delay at current bias, accumulated phase (slips × 2π), plus a one-sentence concept explanation.
- **Waveforms, X-ray, metrics bar, compare view:** as before; X-ray opens from the targeted part.

**Controls**

- WASD + Space/Shift fly, mouse look (pointer lock, drag-look fallback), right/left click place/break, 1–9 or wheel to pick, R rotate, E use (buttons, clocks).
- Bias slider drives the kernel's bias domains, not a visual effect. Pause, time scale, scrub.
- Touch: on-screen pad and action buttons; desktop is the primary target.

**Targets:** 60 fps with 10⁴ parts in view on a mid-range laptop GPU; simulation and rendering decoupled.

## 8. Validation

The golden suite is the project’s source of truth. JoSIM outputs are committed, so CI checks every tier against them without running JoSIM; a nightly job regenerates them to catch JoSIM version drift.

**Golden suite (`golden/`)**

- Each entry holds a JoSIM netlist, a stimulus, the JoSIM pulse log, and metadata (JoSIM version, timestep, SHA-256 hashes).
- Entries are generated only by `tools/char/golden.py` and are never edited by hand or by an agent.
- **G-cells:** each characterized cell in its standard testbench.
- **G-comp:** 8-stage shift register, clocked full adder, depth-4 splitter clock tree, merger collision test, DFF setup/hold edge test.
- **G-flow:** imported 8-bit Kogge-Stone adder, 4-bit multiplier, 4-bit counter.
- **G-family:** equivalent circuits for each family once its cells are characterized.

| Check | Metric | Initial threshold |
| --- | --- | --- |
| Functional | mismatched pulses inside the characterized region | 0 |
| Timing | max timestamp error, tier 1 vs JoSIM | ≤ 10% of nominal cell delay |
| Drift | slope of error vs logic depth | 95% CI of the slope includes 0 |
| Determinism | event-log hash across runs and platforms | identical |
| Analog Mode B | timestamps vs JoSIM, same netlist and step | ≤ 1% of nominal cell delay |
| Analog Mode A | timestamps vs Mode B | ≤ 2% of nominal cell delay |
| Flow | KSA8 routed and legal | 100% of nets |
| Kernel speed | events/s, native, single thread | ≥ 10⁷ |

Thresholds are revisited once, after M2, and the change is logged in `DECISIONS.md`. After that, they move only with a logged reason.

**CI (GitHub Actions)**

- every push: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`, WASM build, `pytest tools/char`, golden comparisons, golden hash-manifest check
- nightly: build JoSIM, regenerate golden data, diff against the committed copy, open an issue on any difference
- weekly: criterion benchmarks, with history kept as artifacts

## 9. Implementation method in Cursor

Work in small, test-first tasks tied to sections of this spec. Machine-enforced guardrails keep the agent away from golden data, tolerances and physics shortcuts.

**Repo documents**

- `docs/SPEC.md`: this doc, exported as Markdown. Reference sections with `@docs/SPEC.md` in prompts.
- `docs/MILESTONES.md`: section 10 as checklists. Tick an item only when its acceptance test passes.
- `docs/DECISIONS.md`: one line per deviation: date · decision · reason · evidence (test or report).

**Project rules (`.cursor/rules/*.mdc`)**

| File | Applies | Content |
| --- | --- | --- |
| `00-core.mdc` | always | time is `Femtos(i64)`, no `f64` time in the kernel; fanout = 1 enforced in IR; deterministic ordering; no `unwrap`/`expect` in library crates (use thiserror); respect the crate dependency rule; ask before adding a dependency |
| `10-golden.mdc` | always | never create, edit or delete files in `golden/` or `lib/rsfqlib/`; never loosen a threshold or tolerance to pass a test; if a golden test fails, stop and report the diff |
| `20-physics.mdc` | `crates/fx-analog/**`, `tools/char/**` | equations from §4.3 with units; SI internally, fs/ps only at boundaries; the §4 timestamp definition; every new element ships with an analytic sanity test; cite the equation in a comment beside the code |
| `30-web.mdc` | `web/**` | simulation only in the worker; never block the main thread; instanced rendering; TypeScript strict; no new UI framework without asking |
| `40-testing.mdc` | always | every change adds or updates tests; proptest for kernel ordering and determinism; criterion benches for hot paths |

**Per-task loop**

1. **Plan (Plan or Ask mode):** reference the spec section and milestone item, and ask for files touched, tests to add and risks. Read the plan before anything is written.
2. **Tests first (Agent mode):** write the tests, run them, and confirm they fail for the expected reason.
3. **Implement:** iterate until `cargo test -p <crate>` or `pytest` passes.
4. **Review the diff yourself:** physics, determinism, crate boundaries. Then ask the agent to self-review against the rules.
5. **Commit** as `M<n>.<k>: <what>`, and tick the checklist.
6. **New chat per task**, to keep context small.

**Prompt template**

```text
@docs/SPEC.md §<section>  @docs/MILESTONES.md M<n>.<k>
Goal: <one testable behavior>.
Constraints: follow .cursor/rules; touch only <crates/paths>.
First: list the tests that prove this works and the files you will change. Wait for my OK.
Then: write the tests, run them, show the failure, implement, run again, summarize the diff.
```

**Hard guardrails**

- **Pre-commit hook:** reject changes under `golden/` or `lib/rsfqlib/` unless `FLUXEL_GOLDEN_UPDATE=1` is set. Set it only when you regenerate golden data yourself.
- **CI hash manifest:** committed SHA-256 hashes of golden files are checked on every push.
- **No agent physics:** analog solver, timestamp extraction and characterization code are written with the agent but reviewed line by line by you, against the analytic tests.
- **Background agents:** only for tasks that are fully specified and test-gated (for example parser coverage, UI panels). Never for physics or golden data.
- **Reject on sight:** any diff that loosens a tolerance, deletes a failing test, or adds `#[ignore]` without a `DECISIONS.md` entry.

## 10. Milestones

Build in ten milestones with four gates. Gates A–D are the earlier spikes #1, #6, #5 and #4: each either passes or forces a logged scope change before work continues.

&#91;embedded content: milestone dependencies · gated milestones highlighted\]

The analog solver (M3) is built beside the main chain and joins at M5. The statistical tier and the extra families both build on the design flow from M6.

### M0 — Foundation

- [ ] 0.1 Cargo workspace with empty crates, `web/` scaffold, CI skeleton
- [ ] 0.2 Vendor RSFQlib v3.0 unmodified into `lib/rsfqlib/`; project LICENSE GPLv3 + NOTICE
- [ ] 0.3 Build JoSIM; `tools/char/josim.py` runs a netlist and parses its output
- [ ] 0.4 `extract.py`: (2k+1)π crossings with interpolation; tests on synthetic phase traces
- [ ] 0.5 Pulse-log format (port, t\_fs) + `diff.py` (per-port matching, mismatches, error statistics)
- [ ] 0.6 `golden.py` + hash manifest + pre-commit guard

**Accept:** an RSFQlib JTL chain is simulated, its timestamps extracted and stored as golden data, and CI is green.

### M1 — Event kernel, constant delays

- [ ] 1.1 `fx-time::Femtos`; `fx-ir` cell schema with serde + validation
- [ ] 1.2 Kernel: heap, ordering, transitions, constraints, violations, event log, snapshots, replay
- [ ] 1.3 RSFQ cell definitions with constant delays: JTL, SPLIT, MERGE, DFF, NDRO, TFF, AND, OR, XOR, NOT, DC/SFQ, SFQ/DC
- [ ] 1.4 `fx-cli sim`: design + stimulus → pulse log
- [ ] 1.5 PyLSE oracle: the same circuits in PyLSE, compared functionally
- [ ] 1.6 Property tests: determinism, causality, fanout rejection

**Accept:** functional match with PyLSE on at least 5 circuits; the determinism test passes.

### M2 — Characterization + table timing → Gate A

- [x] 2.1 Testbench templates per cell (source/load)
- [x] 2.2 Parallel sweep runner with YAML configs
- [ ] 2.3 Window extraction by bisection (setup, hold, merger collision)
- [ ] 2.4 CBOR LUT writer; Rust reader with multilinear interpolation and valid-box checks
- [ ] 2.5 Kernel uses LUT delays and windows; bias domains
- [ ] 2.6 G-cells + G-comp comparison, with a report (error histograms, error vs depth)
- [ ] 2.7 If failing: add the dt\_prev axis, then internal state
- [ ] 2.8 JTL-interconnect characterization

**Gate A:** the §8 timing, functional and drift thresholds are met on G-comp. Otherwise restrict scope to PTL-only interconnect at lower rates and log it.

### M3 — Analog solver → Gate B

- [ ] 3.1 Netlist parser for the RSFQlib `.cir` subset, with tests
- [ ] 3.2 MNA assembly + trapezoidal integration; Mode B (Newton) first
- [ ] 3.3 Mode A (explicit sin φ, constant matrix)
- [ ] 3.4 Sources: DC, PWL, PULSE, SIN; Johnson noise
- [ ] 3.5 Analytic sanity tests (Φ₀ area, I–V, JTL)
- [ ] 3.6 Comparison with JoSIM on G-cells; benchmarks native vs WASM on 10/100/1000-JJ JTL chains

**Gate B:** Mode B meets the §8 threshold, and the single-cell X-ray (≤100 JJs) runs at ≥1 simulated ns per wall-clock second in WASM. Otherwise the analog tier is native-only (logged).

### M4 — Web front end v0

- [ ] 4.1 `fx-wasm` API: load world, step, fetch events
- [ ] 4.2 Worker architecture + message protocol
- [ ] 4.3 First-person controller, targeting, place/break/rotate, undo/redo
- [ ] 4.4 Part models per §7 table, instanced, with LOD
- [ ] 4.5 Event-log → animation mapper (junction spin schedules, beads, lamps, flywheels, failure states)
- [ ] 4.6 Incremental world → Design compiler (PTL driver/receiver rendering from connectivity)
- [ ] 4.7 Bias slider wired to kernel bias domains; pause, time scale
- [ ] 4.8 Waveform panel, inspector line with concept text
- [ ] 4.9 Save/load

**Accept:** a hand-built shift register and full adder give waveforms identical to `fx-cli`, and every junction spin in the world matches a switching event in the kernel log (checked by an automated test).

### M5 — Engineering overlays

- [ ] 5.1 `fx-sta`: clock arrival, setup/hold slack, Fmax
- [ ] 5.2 Slack heatmap, violation markers, skew overlay
- [ ] 5.3 Bias sliders + margin sweep
- [ ] 5.4 Scrubbing via snapshots
- [ ] 5.5 Macros
- [ ] 5.6 Inspector + provenance badges
- [ ] 5.7 X-ray (WASM if Gate B passed, otherwise a native helper)
- [ ] 5.8 Metrics bar

**Accept:** on each G-comp circuit, the margin sweep names the same first-failing cell, and the same failure direction (low or high bias), as a JoSIM bias sweep.

### M6 — Design flow → Gate C

- [ ] 6.1 Yosys driver + JSON import
- [ ] 6.2 Tech-mapping tables
- [ ] 6.3 Splitter insertion, path balancing, clock-tree synthesis
- [ ] 6.4 Layered placer + feedback handling
- [ ] 6.5 A\* router + rip-up and reroute
- [ ] 6.6 Back-annotation + re-legalization
- [ ] 6.7 Exporters: JoSIM, Verilog, PyLSE

**Gate C:** imported KSA8 gives the same results in the sandbox and in the exported JoSIM run (functional, and timing within §8), and passes the readability check. Otherwise fall back to the schematic view for imports (logged).

### M7 — Statistical tier

- [ ] 7.1 Variation model + spatial random field
- [ ] 7.2 Native batch runs (rayon) + Web Worker pool
- [ ] 7.3 Yield and Fmax distributions in the UI
- [ ] 7.4 Calibration against JoSIM Monte Carlo on golden cells

### M8 — More families → Gate D (per family)

Order: xSFQ → xeSFQ → AQFP → ERSFQ → HFQ → RQL/PCL (behavioral). For each family:

- [ ] transcribe cells from the papers into JoSIM netlists
- [ ] reproduce the published waveforms
- [ ] run margin analysis
- [ ] characterize with the M2 pipeline
- [ ] write cell definitions, checkers, legalizer and overlay
- [ ] add golden entries

**Gate D:** published behavior is reproduced, with margins within about 20% of reported values. Otherwise the family ships with `behavioral` badges.

### M9 — Comparison mode, curriculum, release

- [ ] 9.1 Compare view: one Verilog design → every family, with metrics and provenance
- [ ] 9.2 Tutorial levels: JTL → DFF → adder → pipelined ALU → tiny CPU, plus cross-family challenges
- [ ] 9.3 User guide + a per-family accuracy report generated from golden data
- [ ] 9.4 Release: static web build, native CLI binaries, GPLv3 source

## Sources

- [RSFQlib v3.0 extracted files and GPLv3 license (rust\_sfq)](https://docs.rs/crate/rust_sfq/latest/source/lib/rsfqlib/LICENSE)
- [JoSIM — superconductor SPICE simulator](https://github.com/ayala-io/JoSIM)
- [PyLSE: A Pulse-Transfer Level Language for Superconductor Electronics](https://par.nsf.gov/servlets/purl/10486905)
- [qSSTA: statistical static timing analysis for SFQ](https://par.nsf.gov/servlets/purl/10232621)
- [Synthesis of Resource-Efficient Superconducting Circuits with Clock-Free Alternating Logic (xSFQ)](https://arxiv.org/html/2407.20942v2)
- [xeSFQ: Clockless SFQ Logic with Zero Static Power](https://web3.arxiv.org/abs/2411.03052)
- [Superconducting Pulse Conserving Logic and Josephson-SRAM](https://arxiv.org/abs/2303.16801v1)
- [Layout-Aware Buffer and Splitter Insertion for AQFP](https://arxiv.org/pdf/2607.23761)
- [All-JJ Inductor-Free (2φ) Cell Library](https://arxiv.org/pdf/2310.13857)
- [Low-power Rapid Planar Superconducting Logic Devices (JoSIM + ColdFlux setup, 0.025 ps step)](https://arxiv.org/pdf/2405.18309)
