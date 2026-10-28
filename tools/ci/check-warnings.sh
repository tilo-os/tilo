#!/usr/bin/env bash
# Fail if any workspace crate emits a rustc warning.
#
#   tools/ci/check-warnings.sh
#
# Not `RUSTFLAGS=-D warnings`: cargo passes RUSTFLAGS to dependencies too, so
# that turns someone else's warning into our build failure — russh already
# emits one. This reads the JSON diagnostics instead and keeps only those whose
# source file is inside this repository.
#
# Clippy lints are t's clippy family's.

set -uo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"

raw=$(mktemp) || exit 2
trap 'rm -f "$raw"' EXIT

cargo check --workspace --all-targets --message-format=json > "$raw" 2>/dev/null
rc=$?
if [ "$rc" -ne 0 ]; then
  echo "cargo check failed (exit $rc) — the tree does not compile, so a clean" >&2
  echo "warning count would mean nothing." >&2
  cargo check --workspace --all-targets --message-format=short 2>&1 | grep -E '^error' | head -10 >&2
  exit 2
fi

python3 - "$raw" <<'PY'
import json, sys, collections, os
counts = collections.Counter()
seen = set()
lines = []
for line in open(sys.argv[1]):
    try:
        d = json.loads(line)
    except ValueError:
        continue
    if d.get("reason") != "compiler-message":
        continue
    m = d.get("message") or {}
    if m.get("level") != "warning":
        continue
    code = ((m.get("code") or {}) or {}).get("code") or ""
    if code.startswith("clippy::"):
        continue
    spans = m.get("spans") or []
    if not spans:
        continue
    f = spans[0].get("file_name") or ""
    # A dependency's own warning is not ours to fix.
    if f.startswith("/") or f.startswith(".."):
        continue
    key = (code, f, spans[0].get("line_start"))
    if key in seen:
        continue
    seen.add(key)
    counts[code or "(uncoded)"] += 1
    lines.append(f"  {f}:{spans[0].get('line_start')}  {code}: {m.get('message')}")

total = sum(counts.values())
if total == 0:
    print("no warnings from workspace crates")
    raise SystemExit(0)
print(f"{total} warning(s) from workspace crates:\n")
for l in lines[:60]:
    print(l)
if len(lines) > 60:
    print(f"  ... and {len(lines) - 60} more")
print("\nby lint:")
for k, v in counts.most_common():
    print(f"  {v:5d}  {k}")
raise SystemExit(1)
PY
