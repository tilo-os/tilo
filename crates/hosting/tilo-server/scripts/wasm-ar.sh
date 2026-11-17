#!/usr/bin/env bash
# Apple ar/ranlib cannot index WebAssembly objects. Use Zig's LLVM archiver.
set -euo pipefail
exec "${TILO_ZIG:-zig}" ar "$@"
