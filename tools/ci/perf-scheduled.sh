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

case $group in
  fs)
    cargo test --locked -p tilo-fs --test it --test document_media_footprint --no-run
    status=0
    for run in 1 2 3 4 5; do
      TILO_FS_PERF_CHECK=1 cargo test --locked -p tilo-fs --test it performance_timing:: -- --nocapture --test-threads=1 \
        2> "$out/fs-check-$run.log" || status=$?
      TILO_FS_PERF_CHECK=1 cargo test --locked -p tilo-fs --test document_media_footprint -- --nocapture \
        2>> "$out/fs-check-$run.log" || status=$?
      # The checks' assertion messages name the scenario that failed.
      grep '^PERF\| PERF \|exceeded \|panicked at' "$out/fs-check-$run.log" || true
    done
    env_json dev none kvm > "$out/env.json"
    python3 tools/perf/adapters.py fs-check --env-json "$out/env.json" "$out"/fs-check-*.log > "$out/fs.jsonl"
    exit "$status"
    ;;
  agent)
    # What a person waits for when an agent works in their session: the round
    # trip of the calls an agent makes — list the actions, read the state,
    # invoke one, and the same through the MCP server — and how many complete
    # at once. A whole app task (Notes) is not timed here: a builder starts
    # no app, its sandboxes needing a /proc of their own, so such a step would
    # measure nothing. tilo-runtime's dev-overrides carry the throwaway identity the
    # run's session mints for the shell it opens.
    cargo build --locked --release -p tilo-session --bin tilo-session -p tilo-daemon --bin tilo-runtime \
      --features dev-overrides
    test -r "$VK_DRIVER_FILES"
    env_json release lavapipe kvm > "$out/env.json"
    bash tools/with-test-environment.sh python3 scripts/perf/agent_rtt.py \
      --spawn "$target/release/tilo-session" --samples 1000 --warmup 100 \
      --transport "test-control socket, headless, release + dev-overrides, thin LTO" \
      --env-json "$out/env.json" --out "$out/agent.jsonl" --log "$out/compositor.log"
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
    # What a person notices: how long the session takes to draw the tilolang
    # shell's first frame, what a frame costs while the shell shows something
    # moving and while a note is typed into, and what frames offered at rest
    # cost. session_perf measures the session's own loop, which draws the
    # shell, per profile: `arriving` is the shell on an untouched desktop,
    # `apps` the shell with a note window in each place, `activities` the
    # floating layer's live activity cards over it. Its budgets are
    # [devices.x86.session.*] in tools/perf/kpi-budgets.toml, so a phase over
    # budget fails this check.
    # Around it: the toolkit's own 4K redraw (ui-perf) and the framework's
    # scaling with item counts.
    cargo build --locked --release -p tilo-session --example session_perf
    cargo build --locked --release -p ui-examples --example ui-perf
    cargo build --locked --release -p tilo-app --features test-support --example framework-scaling
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
  server)
    cargo build --locked --release -p tilo-runtime --features test-support --example server_load
    env_json release none kvm > "$out/env.json"
    "$target/release/examples/server_load" --members 100 --minutes 10 --server-cpus 2 \
      --out "$out/run" | tee "$out/server-load.log"
    python3 tools/perf/adapters.py server-load --env-json "$out/env.json" "$out/run/server-load.json" \
      > "$out/server.jsonl"
    ;;
  *)
    echo "unknown KPI group: $group" >&2
    exit 2
    ;;
esac
