# Fluxel

Interactive simulator for superconducting digital logic.

## Setup

Use Ubuntu under WSL. From the repository root:

```bash
bash scripts/bootstrap.sh
```

The script is idempotent and does not use sudo. It installs anything still missing:

- Rust stable, the `wasm32-unknown-unknown` target, and wasm-pack
- Node.js 26 LTS under `~/.local`
- CMake 4.4.5 under `~/.local` (JoSIM's build)
- a Python 3.11+ virtualenv at `.venv` with numpy, scipy, pyyaml, pytest, hypothesis, and matplotlib
- OSS CAD Suite (Yosys) at `third_party/oss-cad-suite`
- JoSIM `v2.7` built from source, with `josim-cli` copied to `~/.local/bin`
- PyLSE tag `pldi-2022` and its vendored pyuppaal, both editable installs in `.venv`
- `web/` npm packages

It finishes by running `cargo test --workspace`, `pytest tools/char`, and `wasm-pack build crates/fx-wasm`.

`git`, `curl`, `gcc`, `g++`, `make`, and Python 3.11+ must already be on the system.

Open a new shell after the first run. rustup adds Cargo to `~/.bashrc`, and Ubuntu login shells already include `~/.local/bin`.

Yosys is not on the default PATH. From the repository root:

```bash
source third_party/oss-cad-suite/environment
```

Python tools:

```bash
source .venv/bin/activate
```

Web app:

```bash
cd web
npm run dev
```
