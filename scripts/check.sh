#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx
#
# Lo que tiene que pasar antes de subir un cambio: formato, clippy y tests.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
# Aquí no se salta ningún test: si falta algo para ejecutarlo, falla.
KARBYX_TESTS_STRICT=1 cargo test --workspace --no-fail-fast
echo "Todo bien."
