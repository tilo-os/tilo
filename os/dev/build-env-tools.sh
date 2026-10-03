#!/usr/bin/env bash
# Build the tool catalog (os/env-tools-catalog.txt) for one env-runtime.
#
#   build-env-tools.sh RUNTIME_TAPP OUT_DIR [IDENTITY]
#
#   RUNTIME_TAPP  the env-runtime image for this machine's architecture
#   OUT_DIR       receives one .tapp per tool, catalog.json (cache key ->
#                 tool id) and report.json (sizes and build times)
#   IDENTITY      publisher identity file ({"secret_key_hex": ...}); a
#                 throwaway key when absent
#
# Each tool is built by `tilo-env-tools build` in its bwrap build sandbox:
# the runtime as an overlay lower, dnf as uid 0 of its own user namespace.
# The state directory (dnf metadata, overlay uppers) must be on a filesystem
# overlayfs accepts as an upper: TILO_ENV_TOOLS_STATE, default OUT_DIR/../state.
# Needs: tilo-env-tools on PATH or as TILO_ENV_TOOLS, bwrap, python3.
set -euo pipefail

die() { printf 'build-env-tools: %s\n' "$*" >&2; exit 2; }

[ "$#" -ge 2 ] && [ "$#" -le 3 ] || die "usage: $0 RUNTIME_TAPP OUT_DIR [IDENTITY]"
runtime_tapp=$1
out=$2
identity=${3:-}
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
catalog_list="$repo/os/env-tools-catalog.txt"
bin=${TILO_ENV_TOOLS:-tilo-env-tools}
state=${TILO_ENV_TOOLS_STATE:-$(dirname "$out")/state}

[ -f "$runtime_tapp" ] || die "$runtime_tapp does not exist"
command -v "$bin" >/dev/null || die "$bin not found"
command -v bwrap >/dev/null || die "bwrap not found"
mkdir -p "$out" "$state"

identity_args=()
[ -z "$identity" ] || identity_args=(--identity "$identity")

results="$out/results.jsonl"
: > "$results"
failed=()
while IFS= read -r line; do
  line="${line%%#*}"
  line="$(printf '%s' "$line" | xargs)"
  [ -n "$line" ] || continue
  read -r -a packages <<<"$line"
  printf '\n==> %s\n' "${packages[*]}" >&2
  started=$(date +%s)
  if result="$("$bin" build --runtime-tapp "$runtime_tapp" --state "$state" \
      --out-dir "$out" --catalog "$out/catalog.json" "${identity_args[@]}" \
      -- "${packages[@]}")"; then
    wall=$(( $(date +%s) - started ))
    printf '%s' "$result" | python3 -c '
import json, sys
d = json.load(sys.stdin)
d["wall_seconds"] = int(sys.argv[1])
print(json.dumps(d))' "$wall" >> "$results"
  else
    failed+=("${packages[*]}")
  fi
done < "$catalog_list"

python3 - "$results" "$out/report.json" <<'PY'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
json.dump(rows, open(sys.argv[2], "w"), indent=2)
print("| tool | packages | files | tree | tapp | build | wall |")
print("|---|---|---|---|---|---|---|")
mib = lambda n: f"{(n or 0) / 1048576:.1f} MiB"
for r in rows:
    print(f"| {' '.join(r['requested'])} | {len(r['key']['nevras'])} | {r['files']} | "
          f"{mib(r['bytes'])} | {mib(r['tapp_bytes'])} | {r['build_seconds'] or 0:.0f} s | "
          f"{r['wall_seconds']} s |")
PY
rm -f "$results"

if ((${#failed[@]})); then
  printf '::error::tools failed to build: %s\n' "${failed[*]}"
  exit 1
fi
