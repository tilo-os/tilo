#!/usr/bin/env bash
# Clippy violations may go down, never up.
#
#   tools/clippy-ratchet.sh            # compare against tools/clippy-baseline.txt
#   tools/clippy-ratchet.sh --update   # record the current counts as the baseline
#
# The workspace denies pedantic and a set of restriction lints. There are
# thousands of existing violations, so a plain `cargo clippy` fails and would
# stay failing for months — a gate that is always red gates nothing. This
# compares per-lint counts against a committed baseline instead: new code is
# held to the full standard, and the baseline only ever moves down.
#
# Lints are measured at warn so every crate is checked. Denying them aborts the
# build at the first crate that fails, and the crates behind it are never seen.
#
# Two things this has to get right, both found by deliberately regressing it:
#   * cargo replays cached diagnostics, so a run that rebuilds less reports
#     fewer violations. Every workspace source is touched first, so the count
#     is of the whole tree every time rather than of whatever was stale.
#   * a compile error *lowers* the count, because crates behind the failure are
#     never linted. So a non-zero exit from clippy is a failure, never a pass.

set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BASELINE="$ROOT/tools/clippy-baseline.txt"
cd "$ROOT"

tmp_manifest=$(mktemp) || exit 2
cp Cargo.toml "$tmp_manifest"
restore() { cp "$tmp_manifest" "$ROOT/Cargo.toml"; rm -f "$tmp_manifest"; }
trap restore EXIT

python3 - <<'PY'
import pathlib
p = pathlib.Path("Cargo.toml"); s = p.read_text()
head, sep, rest = s.partition("[workspace.lints.clippy]")
body, sep2, tail = rest.partition("\n[")
p.write_text(head + sep + body.replace('"deny"', '"warn"') + sep2 + tail)
PY

# Force every workspace crate to be re-linted; dependencies stay cached.
find . -name '*.rs' -not -path './target/*' -not -path './vendor/*' \
  -not -path './.claude/*' -exec touch {} + 2>/dev/null

raw=$(mktemp) || exit 2
cargo clippy --workspace --all-targets --message-format=json > "$raw" 2>/dev/null
clippy_rc=$?
if [ "$clippy_rc" -ne 0 ]; then
  echo "clippy exited $clippy_rc — the tree does not compile, so the violation" >&2
  echo "count would be lower than reality. Fix the build first." >&2
  cargo clippy --workspace --all-targets --message-format=short 2>&1 | grep -E '^error' | head -10 >&2
  rm -f "$raw"
  exit 2
fi

counts=$(python3 -c '
import json, sys, collections
c = collections.Counter()
for line in sys.stdin:
    try: d = json.loads(line)
    except ValueError: continue
    if d.get("reason") != "compiler-message": continue
    name = ((d.get("message") or {}).get("code") or {}).get("code", "")
    if name.startswith("clippy::"): c[name] += 1
for name, n in sorted(c.items()): print(f"{n} {name}")' < "$raw")
rm -f "$raw"

if [ -z "$counts" ]; then
  echo "clippy produced no output — treating that as a failure rather than a pass" >&2
  exit 2
fi

if [ "${1:-}" = "--update" ]; then
  printf '%s\n' "$counts" > "$BASELINE"
  echo "baseline updated: $(awk '{s+=$1} END{print s}' <<<"$counts") violations"
  exit 0
fi

[ -f "$BASELINE" ] || { echo "no baseline; run with --update" >&2; exit 2; }

python3 - "$BASELINE" <<PY
import sys, collections
baseline = {}
for line in open(sys.argv[1]):
    n, name = line.split()
    baseline[name] = int(n)
current = {}
for line in """$counts""".strip().splitlines():
    n, name = line.split()
    current[name] = int(n)

regressed = {k: (baseline.get(k, 0), v) for k, v in current.items() if v > baseline.get(k, 0)}
improved = sum(max(0, baseline.get(k, 0) - current.get(k, 0)) for k in baseline)
print(f"total {sum(current.values())} (baseline {sum(baseline.values())})")
if regressed:
    print("\nthese went up:")
    for k, (was, now) in sorted(regressed.items(), key=lambda x: x[1][0] - x[1][1]):
        print(f"  {k}: {was} -> {now}")
    print("\nFix them, or if the increase is deliberate run tools/clippy-ratchet.sh --update")
    sys.exit(1)
if improved:
    print(f"{improved} fewer than the baseline — run --update to lock it in")
print("no lint regressed")
PY
