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
#   ci-builder-canary.sh warm DIR  the same compile in another job, from DIR/prefix: a fresh
#                                  compiler process must read what the cold one wrote
#   ci-builder-canary.sh browser   the native Chromium the browser tests use, and the browser
#                                  regressions that once broke on a builder
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

mode=${1:-}
case $mode in
  cold | warm)
    dir=${2:?usage: ci-builder-canary.sh cold|warm DIR}
    # t hands every job its compiler cache unless the job chose its own; this canary exists
    # to prove that cache works in the candidate, so a job without it fails.
    [[ ${RUSTC_WRAPPER:-} == sccache && -n ${SCCACHE_WEBDAV_ENDPOINT:-} && -n ${SCCACHE_WEBDAV_TOKEN:-} ]] \
      || die "no compiler cache from t in this job (RUSTC_WRAPPER=${RUSTC_WRAPPER:-unset}): no sccache on the candidate's PATH, or a server without [compile_cache]"
    if [[ $mode == cold ]]; then
      channel=$(awk -F'"' '/^channel/{print $2}' rust-toolchain.toml)
      tilo-check-build-env
      rustc --version | grep -F " $channel " >/dev/null || die "the builder's rustc is not $channel"
      # A prefix nobody used, so the cold compile can't hit and the warm one hits only
      # what this one wrote.
      mkdir -p "$dir"
      printf 'builder-canary/%s/%s\n' "$(uname -m)" "$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')" > "$dir/prefix"
    fi
    prefix=$(cat "$dir/prefix")
    [[ $prefix == builder-canary/"$(uname -m)"/* ]] || die "the cold canary's prefix is $prefix"
    # A server of this job's own, which reports a cache it can't reach instead of compiling
    # past it.
    export SCCACHE_WEBDAV_KEY_PREFIX=$prefix TILO_SCCACHE_KEY_PREFIX=$prefix \
      SCCACHE_IGNORE_SERVER_IO_ERROR=0 SCCACHE_SERVER_UDS=$scratch/sccache-canary.sock \
      CARGO_INCREMENTAL=0
    sccache --stop-server >/dev/null 2>&1 || true
    sccache --zero-stats >/dev/null
    target=$scratch/builder-canary-target
    rm -rf "$target"
    CARGO_TARGET_DIR=$target bash tools/ci/measure.sh "compiler cache $mode canary" -- \
      cargo build --locked -p tilo-advisory-lock
    bash tools/ci/sccache-canary.sh "$mode"
    sccache --stop-server >/dev/null 2>&1 || true
    rm -rf "$target"
    ;;
  browser)
    # A build directory of its own: the sources, target/ in them included, are read-only in a
    # job, and the browser tests' nextest store lives in the build directory.
    export CARGO_TARGET_DIR=$scratch/builder-canary-browser-target
    export TILO_TEST_ASSET_CACHE=$CARGO_TARGET_DIR/test-assets
    mkdir -p "$CARGO_TARGET_DIR"
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
      --target-dir "$CARGO_TARGET_DIR" -p tilo-browser-peer --test end_to_end --test passkey_root --test node_workspace --test voice_browsers \
      -E 'binary(end_to_end) or binary(passkey_root) or binary(node_workspace) or binary(voice_browsers)'
    bash tools/ci/t-nextest.sh cargo nextest run --locked --profile ci --no-tests=fail \
      --target-dir "$CARGO_TARGET_DIR" -p tilo-agent-participant --test host_agents \
      -E 'test(=the_host_runs_agents_a_browser_spawns_and_they_survive_a_restart) or test(=a_browser_sees_every_task_of_an_agent_live_and_after_a_reload) or test(=the_creator_and_a_joined_page_both_see_every_task_live)'
    ;;
  *) die 'usage: ci-builder-canary.sh cold|warm DIR, or browser' ;;
esac
