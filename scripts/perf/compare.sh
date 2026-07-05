#!/usr/bin/env bash
# Diff two perf runs.
# Usage:  scripts/perf/compare.sh baseline.json current.json
set -euo pipefail

BASE="$1"
CUR="$2"

python3 - "$BASE" "$CUR" <<'PY'
import json, sys
base = json.load(open(sys.argv[1]))
cur  = json.load(open(sys.argv[2]))

def pct(before, after):
    if before == 0:
        return "n/a"
    return f"{(after - before) / before * 100:+.1f}%"

def fmt_bytes(n):
    if n > 1024*1024:
        return f"{n / 1024 / 1024:.1f} MB"
    if n > 1024:
        return f"{n / 1024:.1f} KB"
    return f"{n} B"

def diff(name, before, after, unit_fmt=None):
    fmt = unit_fmt or (lambda x: str(x))
    print(f"  {name:24s}  {fmt(before):>12s} → {fmt(after):>12s}   {pct(before, after)}")

print(f"--- {base.get('label', '?')} → {cur.get('label', '?')} ---")

# Boot times
if "boot_ms" in base and "boot_ms" in cur:
    diff("boot_ms", base["boot_ms"], cur["boot_ms"], lambda n: f"{n} ms")

# Binary size dict (any key that's an int)
for k in sorted(set(base) | set(cur)):
    if k in ("label", "state", "console_log", "boot_ms"): continue
    b, a = base.get(k, 0), cur.get(k, 0)
    if isinstance(b, int) and isinstance(a, int):
        diff(k, b, a, fmt_bytes)
PY
