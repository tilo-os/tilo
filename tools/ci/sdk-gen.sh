#!/usr/bin/env bash
# The SDKs' types and the API's OpenAPI description, written from the tree by `tilo sdk gen`.
# They are built where they are used and never committed (docs/policies.md): the sdk-gen check
# writes them once, and the SDKs' suites, their release and the docs read its output, in an
# image that has no Rust toolchain to write them itself.
#
#   sdk-gen.sh write OUT        OUT/python, OUT/typescript and OUT/openapi/openapi.json, with
#                               `$TILO` when it names a built `tilo`, else built from the tree
#   sdk-gen.sh place SDK [GEN]  the types in GEN (TILO_SDK_GEN, else /target/sdk-gen) copied
#                               into SDK, a copy of the tree's sdk/: into each of its python/
#                               and typescript/ that is there
#
# A person running an SDK's suite in their checkout writes the types where the package keeps
# them, as `place` does: `sdk-gen.sh write target/sdk-gen && sdk-gen.sh place sdk target/sdk-gen`.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
usage='usage: sdk-gen.sh write OUT | place SDK [GEN]'

die() {
  printf 'sdk-gen.sh: %s\n' "$*" >&2
  exit 1
}

write() {
  local out=$1 tilo
  if [[ -n ${TILO:-} ]]; then
    tilo=("$TILO")
  else
    tilo=("${CARGO:-cargo}" run --locked -q -p tilo-cli --)
  fi
  rm -rf "$out"
  mkdir -p "$out"
  # The generator reads the apps this build ships, compiled into it, so where it runs is no input.
  (cd "$root" && "${tilo[@]}" sdk gen --lang python --out "$out/python")
  (cd "$root" && "${tilo[@]}" sdk gen --lang ts --out "$out/typescript")
  (cd "$root" && "${tilo[@]}" sdk gen --lang openapi --out "$out/openapi")
}

place() {
  local sdk=$1 gen=$2
  [[ -f $gen/python/__init__.py && -f $gen/typescript/index.ts ]] \
    || die "no generated types in $gen: write them with 'tools/ci/sdk-gen.sh write $gen' (t: the sdk-gen check)"
  [[ -d $sdk/python || -d $sdk/typescript ]] || die "no SDK in $sdk"
  if [[ -d $sdk/python ]]; then
    rm -rf "$sdk/python/src/tilo/types"
    cp -R "$gen/python" "$sdk/python/src/tilo/types"
  fi
  if [[ -d $sdk/typescript ]]; then
    rm -rf "$sdk/typescript/src/types"
    cp -R "$gen/typescript" "$sdk/typescript/src/types"
  fi
}

case ${1:-} in
  write) [[ $# == 2 ]] || die "$usage"; write "$2" ;;
  place) [[ $# == 2 || $# == 3 ]] || die "$usage"; place "$2" "${3:-${TILO_SDK_GEN:-/target/sdk-gen}}" ;;
  *) die "$usage" ;;
esac
