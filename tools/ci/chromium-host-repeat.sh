#!/usr/bin/env bash
# The engine suites a plan names, each run as many times as the plan says, on
# a host built as the `chromium-host` check builds it. For a failure that shows
# once in many runs of the whole check: `t test chromium-host-repeat --force`
# after editing the plan. Opt-in; no other check runs or reads this.
#
# A plan is lines of
#   <times> <suite>.py [<what the suite takes after the host: test names, ...>]
# and optionally one line `burn <n>`: n busy loops that compete with the
# suites for the processor, as the other checks of a full node do. `#` starts
# a comment. Every run of every line is made; the check fails if any did, and
# ends with how many of each passed. A failed run's output is printed whole
# (without Chromium's complaints about a session bus the node has none of), a
# passed run's is not. `page_share.py` takes a second program this does not
# build, and is not for a plan.
set -uo pipefail

plan=${1:-tools/ci/chromium-host-repeat.plan}
tests=crates/engines/chromium-host/chromium/tests
host=target/chromium/tilo-chromium-host

bash tools/ci/ensure-browser-dependencies.sh || exit 1
command -v cmake >/dev/null || dnf install -y cmake || exit 1
python3 scripts/browser/build-host.py --jobs 8 || exit 1
ctest --test-dir target/chromium-build --output-on-failure || exit 1
# Chromium refuses root, and the host never turns its sandbox off: the suites run as a user.
chmod -R a+rX target/chromium "$tests" crates/engines/chromium-host/chromium/src crates/engines/chromium-host/assets/extension-manager crates/engines/chromium-host/src/extension_toolbar.rs
mkdir -p -m 1777 /tmp/host-test

burners=()
stop_burning() { [ ${#burners[@]} -eq 0 ] || kill "${burners[@]}" 2>/dev/null; }
trap stop_burning EXIT
burn=$(awk '$1 == "burn" { print $2 }' "$plan" | tail -n 1)
case "${burn:-0}" in ''|*[!0-9]*) echo "$plan: burn takes a whole number" >&2; exit 1 ;; esac
for _ in $(seq 1 "${burn:-0}"); do
  ( while :; do :; done ) &
  burners+=($!)
done
[ "${burn:-0}" -eq 0 ] || echo "burning $burn processors beside the suites"

summary=()
failed=0
output=$(mktemp)
while read -r times suite names; do
  case "$times" in ''|'#'*|burn) continue ;; esac
  case "$times" in *[!0-9]*) echo "$plan: \"$times $suite\" does not start with how many times" >&2; exit 1 ;; esac
  [ -f "$tests/$suite" ] || { echo "$plan: no suite $tests/$suite" >&2; exit 1; }
  passed=0
  for run in $(seq 1 "$times"); do
    began=$SECONDS
    # shellcheck disable=SC2086  # the names are separate arguments
    if HOME=/tmp/host-test TMPDIR=/tmp/host-test unshare --user --map-user=1000 --map-group=1000 -- \
        python3 "$tests/$suite" "$host" $names >"$output" 2>&1; then
      passed=$((passed + 1))
      echo "passed $run/$times in $((SECONDS - began))s: $suite $names"
    else
      echo "FAILED $run/$times in $((SECONDS - began))s: $suite $names"
      grep -v -E 'dbus/(bus|object_proxy)\.cc|Failed to adjust OOM score' "$output"
      echo "(end of failed run $run/$times of $suite $names)"
    fi
  done
  [ "$passed" -eq "$times" ] || failed=1
  summary+=("$passed/$times passed: $suite $names")
done < "$plan"
rm -f "$output"

echo
echo "engine suites, repeated:"
printf '  %s\n' "${summary[@]}"
[ ${#summary[@]} -gt 0 ] || { echo "$plan names no suite" >&2; exit 1; }
exit "$failed"
