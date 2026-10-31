#!/usr/bin/env bash
# Install the Fluxel toolchain and third-party tools, then run the skeleton checks.
# Idempotent: existing installs are left in place. Does not use sudo.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

NODE_VERSION=26.13.0
CMAKE_VERSION=4.4.5
OSS_CAD_URL=https://github.com/YosysHQ/oss-cad-suite-build/releases/download/2026-10-14/oss-cad-suite-linux-x64-20261014.tgz
JOSIM_REF=v2.7
PYLSE_REF=pldi-2022

export PATH="${HOME}/.local/bin:${PATH}"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "bootstrap: missing '$1'. Install it, then re-run. This script does not use sudo." >&2
    exit 1
  fi
}

require_cmd git
require_cmd curl
require_cmd gcc
require_cmd g++
require_cmd make
require_cmd python3

python3 - <<'PY'
import sys
if sys.version_info < (3, 11):
    raise SystemExit("bootstrap: Python 3.11+ is required")
PY

install_rust() {
  if [[ ! -x "${HOME}/.cargo/bin/rustup" ]]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
  fi
  # shellcheck disable=SC1091
  source "${HOME}/.cargo/env"
  rustup toolchain install stable
  rustup default stable
  rustup target add wasm32-unknown-unknown
  if ! command -v wasm-pack >/dev/null 2>&1; then
    curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh
  fi
}

install_node() {
  if command -v node >/dev/null 2>&1; then
    return
  fi
  local prefix="${HOME}/.local/node-v${NODE_VERSION}-linux-x64"
  local tmp
  tmp=$(mktemp -d)
  curl -fL --retry 3 -o "${tmp}/node.tar.xz" \
    "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-x64.tar.xz"
  mkdir -p "${HOME}/.local"
  tar -xJf "${tmp}/node.tar.xz" -C "${HOME}/.local"
  rm -rf "${tmp}"
  mkdir -p "${HOME}/.local/bin"
  ln -sfn "${prefix}/bin/node" "${HOME}/.local/bin/node"
  ln -sfn "${prefix}/bin/npm" "${HOME}/.local/bin/npm"
  ln -sfn "${prefix}/bin/npx" "${HOME}/.local/bin/npx"
  ln -sfn "${prefix}/bin/corepack" "${HOME}/.local/bin/corepack"
  hash -r
}

cmake_is_new_enough() {
  command -v cmake >/dev/null 2>&1 || return 1
  local ver
  ver=$(cmake --version | awk 'NR==1 { print $3 }')
  python3 - "${ver}" <<'PY'
import sys
parts = [int(x) for x in sys.argv[1].split(".")[:2]]
sys.exit(0 if parts >= [3, 14] else 1)
PY
}

install_cmake() {
  if cmake_is_new_enough; then
    return
  fi
  local prefix="${HOME}/.local/cmake-${CMAKE_VERSION}-linux-x86_64"
  local tmp
  tmp=$(mktemp -d)
  curl -fL --retry 3 -o "${tmp}/cmake.tgz" \
    "https://github.com/Kitware/CMake/releases/download/v${CMAKE_VERSION}/cmake-${CMAKE_VERSION}-linux-x86_64.tar.gz"
  mkdir -p "${HOME}/.local"
  tar -xzf "${tmp}/cmake.tgz" -C "${HOME}/.local"
  rm -rf "${tmp}"
  mkdir -p "${HOME}/.local/bin"
  ln -sfn "${prefix}/bin/cmake" "${HOME}/.local/bin/cmake"
  ln -sfn "${prefix}/bin/ctest" "${HOME}/.local/bin/ctest"
  ln -sfn "${prefix}/bin/cpack" "${HOME}/.local/bin/cpack"
  hash -r
}

install_python() {
  local py="${ROOT}/.venv/bin/python"
  if [[ ! -x "${py}" ]]; then
    if ! python3 -m venv "${ROOT}/.venv"; then
      rm -rf "${ROOT}/.venv"
      python3 -m venv --without-pip "${ROOT}/.venv"
      curl -fsSL https://bootstrap.pypa.io/get-pip.py | "${py}"
    fi
  fi
  if ! "${py}" -m pip --version >/dev/null 2>&1; then
    curl -fsSL https://bootstrap.pypa.io/get-pip.py | "${py}"
  fi
  "${py}" -m pip install numpy scipy pyyaml pytest hypothesis matplotlib pyarrow
}

install_yosys() {
  local dest="${ROOT}/third_party/oss-cad-suite"
  if [[ ! -x "${dest}/bin/yosys" ]]; then
    local tmp
    tmp=$(mktemp)
    curl -fL --retry 3 -o "${tmp}" "${OSS_CAD_URL}"
    mkdir -p "${ROOT}/third_party"
    tar -xzf "${tmp}" -C "${ROOT}/third_party"
    rm -f "${tmp}"
  fi
  export PATH="${dest}/bin:${PATH}"
}

install_josim() {
  local src="${ROOT}/third_party/josim"
  if [[ ! -d "${src}/.git" ]]; then
    git clone --branch "${JOSIM_REF}" https://github.com/JoeyDelp/JoSIM.git "${src}"
  fi
  if [[ ! -x "${src}/build/josim-cli" ]]; then
    cmake -S "${src}" -B "${src}/build" -DCMAKE_BUILD_TYPE=Release
    cmake --build "${src}/build" -j "$(nproc)"
  fi
  mkdir -p "${HOME}/.local/bin"
  if [[ ! -x "${HOME}/.local/bin/josim-cli" ]]; then
    cp "${src}/build/josim-cli" "${HOME}/.local/bin/josim-cli"
  fi
  hash -r
}

install_pylse() {
  local src="${ROOT}/third_party/pylse"
  local py="${ROOT}/.venv/bin/python"
  if [[ ! -d "${src}/.git" ]]; then
    git clone --branch "${PYLSE_REF}" https://github.com/UCSBarchlab/PyLSE.git "${src}"
  fi
  "${py}" -m pip install -e "${src}/third-party/pyuppaal"
  "${py}" -m pip install -e "${src}"
}

install_web() {
  (cd "${ROOT}/web" && npm install)
}

install_hooks() {
  chmod +x "${ROOT}/scripts/hooks/pre-commit"
  git config --local core.hooksPath scripts/hooks
}

echo "bootstrap: hooks"
install_hooks
echo "bootstrap: rust"
install_rust
echo "bootstrap: node"
install_node
echo "bootstrap: cmake"
install_cmake
echo "bootstrap: python"
install_python
echo "bootstrap: yosys"
install_yosys
echo "bootstrap: josim"
install_josim
echo "bootstrap: pylse"
install_pylse
echo "bootstrap: web"
install_web

echo "bootstrap: check josim, yosys, pylse"
josim-cli --version
yosys -V
"${ROOT}/.venv/bin/python" -c "import pylse"

echo "bootstrap: cargo test"
cargo test --workspace
echo "bootstrap: pytest"
"${ROOT}/.venv/bin/python" -m pytest "${ROOT}/tools/char"
echo "bootstrap: wasm-pack"
wasm-pack build "${ROOT}/crates/fx-wasm"
echo "bootstrap: ok"
