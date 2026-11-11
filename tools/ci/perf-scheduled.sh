#!/usr/bin/env bash
# One KPI group of the scheduled performance run. Each group measures this tree and writes its records to
# /target/perf/<group>/<group>.jsonl (with its logs beside them); perf-compare
# takes every group's records, compares them with the store and, on main,
# publishes them.
#
#   perf-scheduled.sh <group>
#
# Records name the measured commit: T_COMMIT where t gives one (scheduled and
# dispatched runs), else HEAD where the check has Git, else none, for a
# working tree's numbers that are never published. Their runner class is the
# t node pool, so they form series of their own beside the Forgejo runners'.
set -euo pipefail

group=${1:?usage: perf-scheduled.sh <group>}
out=${CARGO_TARGET_DIR:-/target}/perf/$group
rm -rf "$out"
mkdir -p "$out"
GITHUB_SHA=${T_COMMIT:-$(git rev-parse HEAD 2>/dev/null || printf '0%.0s' {1..40})}
export GITHUB_SHA
runner=t
target=${CARGO_TARGET_DIR:-target}
# Build scripts fetch test fixtures; keep them with the build, out of the sources.
export TILO_TEST_ASSET_CACHE=${TILO_TEST_ASSET_CACHE:-$target/test-assets}

env_json() { # profile renderer virtualization
  python3 tools/perf/record.py env --profile "$1" --renderer "$2" --virtualization "$3" --runner "$runner"
}

# The release bundle names its commit and refuses a tree that differs from it.
# t gives scheduled and dispatched runs the commit as T_COMMIT; the tree is its
# files, so a repository holding just that commit makes it a clean checkout.
commit_checkout() {
  git rev-parse HEAD >/dev/null 2>&1 && return
  [[ -n ${T_COMMIT:-} ]] || { echo "$group measures a commit: run it on t's schedule or with t dispatch" >&2; exit 2; }
  git init -q
  git fetch -q --depth=1 --no-tags https://code.tilo.so/tilo/tilo.git "$T_COMMIT"
  git reset -q "$T_COMMIT"
}

# The browser desktop bundle the web entry serves, which the run's
# web-desktop-bundle check built for this commit (the bundle records it): the same release
# build, made once for every consumer instead of again for each measurement.
desktop_bundle() {
  local bundle=${TILO_PERF_DESKTOP_BUNDLE:-/target/publish/web-desktop/bundle}
  [[ -f $bundle/index.html ]] || { echo "$group measures the web-desktop-bundle check's bundle, not found at $bundle" >&2; exit 2; }
  printf '%s\n' "$bundle"
}

chrome() {
  bash tools/ci/ensure-browser-dependencies.sh
  npx --yes @puppeteer/browsers@2 install chrome@stable --path "$TMPDIR/chrome" | tee "$TMPDIR/chrome.txt"
  CHROME=$(awk '{print $2}' "$TMPDIR/chrome.txt" | tail -n 1)
  export CHROME
}

case $group in
  fs)
    cargo test --locked -p tilo-fs --test performance_timing --test document_media_footprint --no-run
    status=0
    for run in 1 2 3 4 5; do
      TILO_FS_PERF_GATE=1 cargo test --locked -p tilo-fs --test performance_timing -- --nocapture --test-threads=1 \
        2> "$out/fs-gate-$run.log" || status=$?
      TILO_FS_PERF_GATE=1 cargo test --locked -p tilo-fs --test document_media_footprint -- --nocapture \
        2>> "$out/fs-gate-$run.log" || status=$?
      # The gates' assertion messages name the scenario that failed.
      grep '^PERF\| PERF \|exceeded \|panicked at' "$out/fs-gate-$run.log" || true
    done
    env_json dev none kvm > "$out/env.json"
    python3 tools/perf/adapters.py fs-gate --env-json "$out/env.json" "$out"/fs-gate-*.log > "$out/fs.jsonl"
    exit "$status"
    ;;
  agent)
    # What a person waits for when an agent works in their session: the round
    # trip of the calls an agent makes — list the actions, read the state,
    # invoke one, and the same through the MCP server — and how many complete
    # at once. A whole app task (Notes) cannot be timed here: a builder starts
    # no app, its sandboxes needing a /proc of their own, so such a step would
    # measure nothing. `scripts/perf/agent_task.py` times that on a session
    # where apps start, and the agent's own task pipeline is the
    # `agent-native` group's. tilo-runtime's dev-overrides carry the throwaway
    # identity the run's session mints for the desk it opens.
    cargo build --locked --release -p tilo-session --bin tilo-session -p tilo-runtime --bin tilo-runtime-mcp \
      --features dev-overrides
    test -r "$VK_DRIVER_FILES"
    env_json release lavapipe kvm > "$out/env.json"
    bash tools/with-test-environment.sh python3 scripts/perf/agent_rtt.py \
      --spawn "$target/release/tilo-session" --samples 1000 --warmup 100 \
      --transport "test-control socket, headless, release + dev-overrides, thin LTO" \
      --env-json "$out/env.json" --out "$out/agent.jsonl" --log "$out/compositor.log"
    ;;
  agent-native)
    # The agent's own task pipeline, without the desktop: a release build of the agent's
    # tests, which shares nothing with the session's build above, so it runs as a check of its
    # own beside it. Its records are the agent group's.
    env_json release none kvm > "$out/env-pipeline.json"
    TILO_AGENT_NATIVE_REPS=30 TILO_AGENT_NATIVE_SAMPLES="$out/native" \
      cargo test --locked --release -p agent --lib native_task_benchmark -- --nocapture
    for plan in pipelined sequential; do
      python3 tools/perf/record.py emit --kpi "agent.task.native.${plan}.wall" --unit ms \
        --samples-file "$out/native/${plan}.txt" --env-json "$out/env-pipeline.json" \
        --meta steps=3 --meta step_ms=100 >> "$out/agent-native.jsonl"
    done
    python3 tools/perf/record.py emit --kpi agent.task.native.idle_round_trips --unit count \
      --samples-file "$out/native/idle.txt" --env-json "$out/env-pipeline.json" \
      --meta steps=3 --meta step_ms=100 >> "$out/agent-native.jsonl"
    ;;
  footprint)
    curl --fail --silent --show-error --retry 3 --retry-all-errors --proto '=https' \
      --output "$out/entries.json" https://apps.tilo.so/entries
    python3 tools/perf/record.py env --profile release --renderer none --virtualization none \
      --runner none --hardware "apps.tilo.so registry index" > "$out/env.json"
    python3 tools/perf/adapters.py tapp-sizes --env-json "$out/env.json" "$out/entries.json" > "$out/footprint.jsonl"
    # The registry serves the last release until the next one publishes, so its packages are
    # held to the budgets of the commit that built them, fetched from the forge.
    release=(--git-dir "$TMPDIR/release.git" --fetch-from https://code.tilo.so/tilo/tilo.git)
    status=0
    python3 tools/perf/tapp_size_ratchet.py fleet "$out/entries.json" "${release[@]}" \
      --summary "$out/summary.md" || status=1
    python3 tools/perf/runtime_size_ratchet.py published "$out/entries.json" "${release[@]}" \
      --measurements "$out/runtime.json" --summary "$out/summary.md" || status=1
    [[ -s $out/runtime.json ]] || exit 1
    python3 tools/perf/adapters.py runtime-sizes --env-json "$out/env.json" "$out/runtime.json" \
      >> "$out/footprint.jsonl"
    exit "$status"
    ;;
  footprint-image)
    # The previous build an update starts from: main's first parents, commits only.
    git clone -q --bare --filter=tree:0 --single-branch --branch main \
      https://code.tilo.so/tilo/tilo.git "$TMPDIR/history.git"
    status=0
    for arch in x86_64 aarch64; do
      python3 tools/perf/image_size.py --dev-base "registry.tilo.so/tilo/tilo-vm:$arch-ci" \
        --arch "$arch" --records "$out/image-$arch.jsonl" --runner none \
        --update-from-previous 'main-{sha7}-{arch}' --git-dir "$TMPDIR/history.git" \
        --json "$out/image-size-$arch.json" --markdown "$out/image-size-$arch.md" > /dev/null || status=1
    done
    cat "$out"/image-*.jsonl > "$out/footprint-image.jsonl" 2>/dev/null || true
    exit "$status"
    ;;
  render)
    # What a person notices: how long the desk takes to draw its first frame,
    # what a frame costs while it animates and while a document is typed
    # into, and what frames offered at rest cost. session_perf measures the
    # shared desktop surface on the frame path both hosts draw with, per
    # profile: `arriving` is the native session's chrome, `apps` the page's
    # chrome and app paint, `activities` the floating layer's cards over a
    # desk. Its budgets are [devices.x86.session.*] in
    # tools/perf/kpi-budgets.toml, so a phase over budget fails this check.
    # Around it: the toolkit's own 4K redraw (ui-perf), the framework's
    # scaling with item counts, and a document's keystroke-to-frame latency.
    cargo build --locked --release -p tilo-desktop --example session_perf
    cargo build --locked --release -p ui-examples --example ui-perf
    cargo build --locked --release -p tilo-app --features test-support --example framework-scaling
    cargo build --locked --release -p notes --example notes-latency
    test -r "$VK_DRIVER_FILES"
    examples=$target/release/examples
    env_json release lavapipe kvm > "$out/env.json"
    status=0
    for profile in arriving apps activities; do
      TILO_PERF_DEVICE=x86 "$examples/session_perf" --profile "$profile" \
        --json "$out/session-$profile.json" || status=$?
    done
    python3 tools/perf/check_kpi.py check "$out"/session-*.json || status=$?
    python3 scripts/perf/render_bench.py timing "$examples/ui-perf" \
      --env-json "$out/env.json" --out "$out" --records "$out/render.jsonl" || status=$?
    for plan in "5000,50000 open framework-open" "5000 scroll framework-scroll" "500,1000,2500 focus framework-focus"; do
      read -r sizes action adapter <<< "$plan"
      "$examples/framework-scaling" "$sizes" "$action" | tee "$out/$adapter.log" || status=$?
      python3 tools/perf/adapters.py "$adapter" --env-json "$out/env.json" "$out/$adapter.log" >> "$out/render.jsonl" || status=$?
    done
    "$examples/notes-latency" | tee "$out/notes-latency.log" || status=$?
    python3 tools/perf/adapters.py notes-latency --env-json "$out/env.json" "$out/notes-latency.log" >> "$out/render.jsonl" || status=$?
    python3 scripts/perf/render_bench.py counters "$examples/ui-perf" --out "$out/counters" || status=$?
    # A benchmark that fails is reported by its own exit status, and its group
    # still compares the records the others left: a pipeline's last command is
    # what a shell reports, so each step keeps its own.
    exit "$status"
    ;;
  search)
    # What a person notices: the palette answers while they type. The
    # measurement is the palette search's own: every registry app's entry
    # ranked against a query, best of 25 tries, under its budget (2 ms
    # optimised, 10 ms in the test profile). A query over budget fails the
    # test, and so this check; nothing is recorded for comparison.
    # The test binary runs inside tools/with-test-environment.sh (the cargo
    # runner), like every other test.
    cargo test --release --locked -p tilo-desktop-search --lib \
      providers::tests::a_query_against_every_registry_app_stays_under_two_milliseconds \
      -- --ignored --exact --nocapture --test-threads=1 2>&1 | tee "$out/palette-keystroke.log"
    ;;
  hosting)
    cargo build --locked --release -p tilo-relay --example relay_load
    cargo run --locked -p tilo-relay --example keystroke_writes | tee "$out/keystroke-writes.log"
    env_json release none kvm > "$out/env.json"
    "$target/release/examples/relay_load" --collaborators 100 --minutes 10 --relay-cpus 2 \
      --out "$out/run" | tee "$out/hosting-load.log"
    python3 tools/perf/adapters.py hosting-load --env-json "$out/env.json" "$out/run/hosting-load.json" \
      > "$out/hosting.jsonl"
    ;;
  browser)
    commit_checkout
    bundle=$(desktop_bundle)
    chrome
    env_json release swiftshader kvm > "$out/env.json"
    node scripts/perf/browser_qualification.mjs "$bundle" --out "$out/browser.json" \
      --renderer swiftshader --demo-seconds 0 --rounds 1 --activations 10 --typing-keys 16 --scroll-steps 24 --viewport 960x540 \
      --expect-revision "$(git rev-parse HEAD)" | tee "$out/browser-summary.txt"
    python3 tools/perf/adapters.py browser-qualification --env-json "$out/env.json" "$out/browser.json" \
      > "$out/browser.jsonl"
    ;;
  *)
    echo "unknown KPI group: $group" >&2
    exit 2
    ;;
esac
