#!/usr/bin/env bash
# The CI builder's canaries, run inside the candidate image-ci-builder-ARCH built
# (`image = { check = "image-ci-builder-ARCH" }` in t.toml), before it is signed or published.
# Kept apart from tools/ci/publish/ci-builder.sh, which builds it, so changing them doesn't
# rebuild the image.
#
#   ci-builder-canary.sh cold DIR  the workspace's build-environment check and the pinned
#                                  toolchain, then a compile with the CI linker through t's
#                                  compiler cache under a fresh prefix (kept as DIR/prefix): it
#                                  must miss and write, never hit or fail
#   ci-builder-canary.sh warm DIR  the same compile in another job, keyed by DIR/prefix: it
#                                  must read everything the cold one wrote
#   ci-builder-canary.sh browser   the native Chromium the browser tests use, and the browser
#                                  regressions that once broke on a builder
#   ci-builder-canary.sh x11-bridge the shell's X11 clipboard tests against the candidate's
#                                  patched xwayland-satellite, the in-flight paste handoff
#                                  regressions included
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

mode=${1:-}
case $mode in
  cold | warm)
    dir=${2:?usage: ci-builder-canary.sh cold|warm DIR}
    # t hands every job its compiler cache unless the job chose its own: its wrapper, copied
    # into the job's /tmp, and the node's cache daemon. This canary exists to prove that cache
    # works in the candidate (the wrapper runs on its glibc, keys its compiler and links
    # against its package database), so a job without it fails.
    counts=/tmp/.t/compile-cache-counts
    [[ ${RUSTC_WRAPPER:-} == /tmp/.t/rustc-wrapper && -x $RUSTC_WRAPPER \
      && -n ${T_COMPILE_CACHE:-} && -n ${T_COMPILE_CACHE_TOKEN:-} ]] \
      || die "no compiler cache from t in this job (RUSTC_WRAPPER=${RUSTC_WRAPPER:-unset}): t gave no wrapper, or a server without [compile_cache]"
    if [[ $mode == cold ]]; then
      channel=$(awk -F'"' '/^channel/{print $2}' rust-toolchain.toml)
      tilo-check-build-env
      rustc --version | grep -F " $channel " >/dev/null || die "the builder's rustc is not $channel"
      # A key nobody used, so the cold compile can't hit and the warm one hits only what this
      # one wrote.
      mkdir -p "$dir"
      printf 'builder-canary/%s/%s\n' "$(uname -m)" "$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')" > "$dir/prefix"
    fi
    prefix=$(cat "$dir/prefix")
    [[ $prefix == builder-canary/"$(uname -m)"/* ]] || die "the cold canary's prefix is $prefix"
    # The wrapper keys every compile by each RUSTC_* variable but RUSTC_WRAPPER, so this one
    # gives the canary's compiles keys of their own.
    export RUSTC_TILO_CANARY_KEY=$prefix CARGO_INCREMENTAL=0
    # The wrapper appends one line per compile it could cache: h hit, w served after waiting,
    # m missed and written, c compiled after a wait in vain, k unkeyed, u daemon unanswered,
    # x uncacheable option.
    before=$( { cat "$counts" 2>/dev/null || true; } | wc -l)
    target=$scratch/builder-canary-target
    rm -rf "$target"
    CARGO_TARGET_DIR=$target bash tools/ci/measure.sh "compiler cache $mode canary" -- \
      cargo build --locked -p tilo-advisory-lock
    rm -rf "$target"
    outcomes=$( { tail -n "+$((before + 1))" "$counts" 2>/dev/null || true; } | cut -c1 | sort | uniq -c | tr -s ' \n' ' ')
    n() { { tail -n "+$((before + 1))" "$counts" 2>/dev/null || true; } | grep -c "^$1" || true; }
    hits=$(( $(n h) + $(n w) )) misses=$(n m) failed=$(( $(n c) + $(n k) + $(n u) ))
    echo "compiler cache $mode canary: hits=$hits misses=$misses failed=$failed (outcomes:${outcomes:- none}) key=$prefix"
    (( failed == 0 )) || die "the $mode canary compiled past the cache: daemon unanswered, inputs unkeyed or a wait in vain"
    if [[ $mode == cold ]]; then
      (( hits == 0 )) || die 'the cold canary hit entries under its fresh key'
      (( misses > 0 )) || die 'the cold canary neither missed nor wrote under its fresh key'
    else
      (( misses == 0 )) || die 'the warm canary missed compiles the cold one wrote'
      (( hits > 0 )) || die 'the warm canary got nothing from the cold one'
    fi
    ;;
  browser)
    # A build directory of its own: the sources, target/ in them included, are read-only in a
    # job, and the browser tests' nextest store lives in the build directory.
    export CARGO_TARGET_DIR=$scratch/builder-canary-browser-target
    export TILO_TEST_ASSET_CACHE=$CARGO_TARGET_DIR/test-assets
    mkdir -p "$CARGO_TARGET_DIR"
    # nextest keeps its store (JUnit reports) under the workspace's target/ unless told otherwise.
    store_config=$scratch/builder-canary-nextest.toml
    printf '[store]\ndir = "%s"\n' "$CARGO_TARGET_DIR/nextest" > "$store_config"
    test "${CHROME:-}" = /usr/lib64/chromium-browser/chromium-browser \
      || die "CHROME is ${CHROME:-unset}, not the builder's Chromium"
    "$CHROME" --version
    node --input-type=module <<'JS'
import assert from 'node:assert/strict';
import {launch} from './tools/web/chrome.mjs';
const browser = await launch({args: process.getuid?.() === 0 ? ['--no-sandbox'] : []});
try {
  const tab = await browser.tab();
  assert.equal(await tab.evaluate('6 * 7'), 42);
} finally {
  await browser.close();
}
JS
    bash tools/ci/t-nextest.sh cargo nextest run --locked --profile ci --no-tests=fail \
      --target-dir "$CARGO_TARGET_DIR" --tool-config-file "tilo-canary:$store_config" -p tilo-browser-peer --test end_to_end --test passkey_root --test node_workspace --test voice_browsers \
      -E 'binary(end_to_end) or binary(passkey_root) or binary(node_workspace) or binary(voice_browsers)'
    bash tools/ci/t-nextest.sh cargo nextest run --locked --profile ci --no-tests=fail \
      --target-dir "$CARGO_TARGET_DIR" --tool-config-file "tilo-canary:$store_config" -p tilo-agent-participant --test host_agents \
      -E 'test(=the_host_runs_agents_a_browser_spawns_and_they_survive_a_restart) or test(=a_browser_sees_every_task_of_an_agent_live_and_after_a_reload) or test(=the_creator_and_a_joined_page_both_see_every_task_live)'
    ;;
  x11-bridge)
    export CARGO_TARGET_DIR=$scratch/builder-canary-x11-target
    export TILO_TEST_ASSET_CACHE=$CARGO_TARGET_DIR/test-assets
    mkdir -p "$CARGO_TARGET_DIR"
    store_config=$scratch/builder-canary-nextest.toml
    printf '[store]\ndir = "%s"\n' "$CARGO_TARGET_DIR/nextest" > "$store_config"
    # The installed bridge must be the build tools/ci/install-xwayland-satellite.sh records.
    diff <(bash tools/ci/install-xwayland-satellite.sh --stamp os/external-assets/github-assets.tsv \
        os/external-assets/xwayland-satellite) /usr/share/licenses/tilo-xwayland-satellite/SOURCE \
      || die "the builder's xwayland-satellite is not the pinned, patched build"
    bash tools/ci/t-nextest.sh cargo nextest run --locked --profile ci --no-tests=fail \
      --target-dir "$CARGO_TARGET_DIR" --tool-config-file "tilo-canary:$store_config" -p shell --test it \
      -E 'binary(it) and test(/^clipboard_e2e::x11_/)'
    ;;
  *) die 'usage: ci-builder-canary.sh cold|warm DIR, browser or x11-bridge' ;;
esac
