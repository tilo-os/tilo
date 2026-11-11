#!/usr/bin/env bash
# The CI builder's canaries, run inside the candidate image before it is signed or published.
# Kept apart from tools/ci/publish/ci-builder.sh, which builds it, so changing them doesn't
# rebuild the image.
#
#   ci-builder-canary.sh cold DIR  the workspace's build-environment check and the pinned
#                                  toolchain, then a compile through t's compiler cache under a
#                                  fresh nonce (kept as DIR/prefix): it must miss and store,
#                                  never hit
#   ci-builder-canary.sh warm DIR  the same compile in another job, with DIR/prefix's nonce: it
#                                  must find everything the cold one stored
#   ci-builder-canary.sh browser   the native Chromium the browser tests use: CHROME names the
#                                  builder's, and it starts and runs a page
#   ci-builder-canary.sh browser-tests
#                                  the browser regressions that once broke on a builder, run in
#                                  the candidate
#
# `browser` reads the image and a few files, so t keeps its verdict until the image changes and
# every publication waits for it. `browser-tests` compiles the browser peer and the agent
# participant, which reads nearly every source file and so moved with every commit: it runs on
# release candidates, when asked and when t has room. The same tests run on every commit in the
# pinned builder (`test/tilo-session-client`, `test-x86_64/tilo-agent-participant`), so what the
# candidate adds is the image alone.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

mode=${1:-}
case $mode in
  cold | warm)
    dir=${2:?usage: ci-builder-canary.sh cold|warm DIR}
    # t hands every cargo job its compiler cache: its wrapper, bound into the job at
    # /run/t/wrapper, and the socket to the node's cache daemon. This canary exists to prove that
    # cache works in the candidate (the wrapper keys its compiler and runs its compiles), so a job
    # without it fails.
    [[ ${RUSTC_WRAPPER:-} == /run/t/wrapper && -x $RUSTC_WRAPPER && -n ${T_CACHE_SOCKET:-} ]] \
      || die "no compiler cache from t in this job (RUSTC_WRAPPER=${RUSTC_WRAPPER:-unset}, T_CACHE_SOCKET=${T_CACHE_SOCKET:-unset})"
    # The wrapper appends one line per compile to its temporary directory's .t-cache-counts (a VM
    # guest's T_CACHE_COUNTS): `h MS` found in the cache, `m` compiled and stored, `x WHY` not
    # cacheable (a build script's own compile, cargo's --print queries).
    counts=${T_CACHE_COUNTS:-${TMPDIR:-/tmp}/.t-cache-counts}
    if [[ $mode == cold ]]; then
      channel=$(awk -F'"' '/^channel/{print $2}' rust-toolchain.toml)
      tilo-check-build-env
      rustc --version | grep -F " $channel " >/dev/null || die "the builder's rustc is not $channel"
      # The image carries every font this tree pins, verified, so no job fetches them.
      TILO_FONT_ASSETS_OFFLINE=1 python3 tools/font-catalog.py seed crates/sdk/ui-core/assets/fonts \
        /usr/share/tilo/font-artifacts >/dev/null || die "the builder's font seed is not this tree's pins"
      # A nonce nobody used, so the cold compile can't hit and the warm one hits only what this
      # one stored.
      mkdir -p "$dir"
      printf 'builder-canary/%s/%s\n' "$(uname -m)" "$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')" > "$dir/prefix"
    fi
    prefix=$(cat "$dir/prefix")
    [[ $prefix == builder-canary/"$(uname -m)"/* ]] || die "the cold canary's prefix is $prefix"
    # A compile's key holds every argument it is given, so a cfg naming the nonce gives every
    # unit of this build, its dependencies' included, keys of its own. Cargo joins these flags
    # with the workspace's target rustflags; CI's -D warnings stays.
    triple=$(uname -m)-unknown-linux-gnu
    flags_var=CARGO_TARGET_$(tr 'a-z-' 'A-Z_' <<< "$triple")_RUSTFLAGS
    export "$flags_var=${!flags_var:-} --check-cfg=cfg(tilo_builder_canary,values(any())) --cfg=tilo_builder_canary=\"$prefix\""
    before=$( { cat "$counts" 2>/dev/null || true; } | wc -l)
    target=$scratch/builder-canary-target
    rm -rf "$target"
    CARGO_TARGET_DIR=$target bash tools/ci/measure.sh "compiler cache $mode canary" -- \
      cargo build --locked -p tilo-advisory-lock
    rm -rf "$target"
    lines() { tail -n "+$((before + 1))" "$counts" 2>/dev/null || true; }
    n() { lines | grep -c "^$1\( \|$\)" || true; }
    hits=$(n h) misses=$(n m) uncached=$(n x)
    echo "compiler cache $mode canary: hits=$hits stored=$misses uncacheable=$uncached key=$prefix"
    lines | sed -n 's/^x /  not cacheable: /p' | sort | uniq -c
    if [[ $mode == cold ]]; then
      (( hits == 0 )) || die 'the cold canary hit entries under its fresh key'
      (( misses > 0 )) || die 'the cold canary neither missed nor stored under its fresh key'
    else
      (( misses == 0 )) || die 'the warm canary missed compiles the cold one stored'
      (( hits > 0 )) || die 'the warm canary got nothing from the cold one'
    fi
    ;;
  browser)
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
    ;;
  browser-tests)
    test "${CHROME:-}" = /usr/lib64/chromium-browser/chromium-browser \
      || die "CHROME is ${CHROME:-unset}, not the builder's Chromium"
    # A build directory of its own: the sources, target/ in them included, are read-only in a
    # job, and the browser tests' nextest store lives in the build directory.
    export CARGO_TARGET_DIR=$scratch/builder-canary-browser-target
    export TILO_TEST_ASSET_CACHE=$CARGO_TARGET_DIR/test-assets
    mkdir -p "$CARGO_TARGET_DIR"
    # nextest keeps its store (JUnit reports) under the workspace's target/ unless told otherwise.
    store_config=$scratch/builder-canary-nextest.toml
    printf '[store]\ndir = "%s"\n' "$CARGO_TARGET_DIR/nextest" > "$store_config"
    bash tools/ci/nextest.sh cargo nextest run --locked --profile ci --no-tests=fail \
      --target-dir "$CARGO_TARGET_DIR" --tool-config-file "tilo-canary:$store_config" -p tilo-session-client --test carriers --test passkey_root --test node_workspace --test voice_browsers \
      -E 'binary(carriers) or binary(passkey_root) or binary(node_workspace) or binary(voice_browsers)'
    bash tools/ci/nextest.sh cargo nextest run --locked --profile ci --no-tests=fail \
      --target-dir "$CARGO_TARGET_DIR" --tool-config-file "tilo-canary:$store_config" -p tilo-agent-participant --test host_agents \
      -E 'test(=the_host_runs_agents_a_browser_spawns_and_they_survive_a_restart) or test(=a_browser_sees_every_task_of_an_agent_live_and_after_a_reload) or test(=the_creator_and_a_joined_page_both_see_every_task_live)'
    ;;
  *) die 'usage: ci-builder-canary.sh cold|warm DIR, browser or browser-tests' ;;
esac
