#!/usr/bin/env bash
# One KPI group of the scheduled performance run, as a t check (t.toml,
# perf-*). Each group measures this tree and writes its records to
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

chrome() {
  bash tools/ci/ensure-browser-dependencies.sh
  npx --yes @puppeteer/browsers@2 install chrome@stable --path "$TMPDIR/chrome" | tee "$TMPDIR/chrome.txt"
  CHROME=$(awk '{print $2}' "$TMPDIR/chrome.txt" | tail -n 1)
  export CHROME
}

case $group in
  fs)
    cargo test --locked -p tilo-fs --test performance_gate --test document_media_footprint --no-run
    status=0
    for run in 1 2 3 4 5; do
      TILO_FS_PERF_GATE=1 cargo test --locked -p tilo-fs --test performance_gate -- --nocapture --test-threads=1 \
        2> "$out/fs-gate-$run.log" || status=$?
      TILO_FS_PERF_GATE=1 cargo test --locked -p tilo-fs --test document_media_footprint -- --nocapture \
        2>> "$out/fs-gate-$run.log" || status=$?
      grep '^PERF\| PERF ' "$out/fs-gate-$run.log" || true
    done
    env_json dev none kvm > "$out/env.json"
    python3 tools/perf/adapters.py fs-gate --env-json "$out/env.json" "$out"/fs-gate-*.log > "$out/fs.jsonl"
    exit "$status"
    ;;
  agent)
    cargo build --locked --release -p shell --bin shell --features dev-overrides
    test -r "$VK_DRIVER_FILES"
    env_json release lavapipe kvm > "$out/env.json"
    bash tools/with-test-environment.sh python3 scripts/perf/agent_rtt.py \
      --spawn "$target/release/shell" --samples 1000 --warmup 100 \
      --transport "test-control socket, headless, release + dev-overrides, thin LTO" \
      --env-json "$out/env.json" --out "$out/agent.jsonl" --log "$out/compositor.log"
    env_json release none kvm > "$out/env-pipeline.json"
    TILO_AGENT_PIPELINE_REPS=30 TILO_AGENT_PIPELINE_SAMPLES="$out/pipeline" \
      cargo test --locked --release -p agent --lib pipelined_task_benchmark -- --nocapture
    TILO_AGENT_NATIVE_REPS=30 TILO_AGENT_NATIVE_SAMPLES="$out/native" \
      cargo test --locked --release -p agent --lib native_task_benchmark -- --nocapture
    for plan in pipelined sequential; do
      python3 tools/perf/record.py emit --kpi "agent.task.${plan}.wall" --unit ms \
        --samples-file "$out/pipeline/${plan}.txt" --env-json "$out/env-pipeline.json" \
        --meta steps=3 --meta step_ms=100 >> "$out/agent.jsonl"
      python3 tools/perf/record.py emit --kpi "agent.task.native.${plan}.wall" --unit ms \
        --samples-file "$out/native/${plan}.txt" --env-json "$out/env-pipeline.json" \
        --meta steps=3 --meta step_ms=100 >> "$out/agent.jsonl"
    done
    python3 tools/perf/record.py emit --kpi agent.task.native.idle_round_trips --unit count \
      --samples-file "$out/native/idle.txt" --env-json "$out/env-pipeline.json" \
      --meta steps=3 --meta step_ms=100 >> "$out/agent.jsonl"
    ;;
  footprint)
    curl --fail --silent --show-error --retry 3 --retry-all-errors --proto '=https' \
      --output "$out/entries.json" https://apps.tilo.so/entries
    python3 tools/perf/record.py env --profile release --renderer none --virtualization none \
      --runner none --hardware "apps.tilo.so registry index" > "$out/env.json"
    python3 tools/perf/adapters.py tapp-sizes --env-json "$out/env.json" "$out/entries.json" > "$out/footprint.jsonl"
    status=0
    python3 tools/perf/tapp_size_ratchet.py fleet "$out/entries.json" --summary "$out/summary.md" || status=1
    python3 tools/perf/runtime_size_ratchet.py published "$out/entries.json" \
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
      python3 tools/perf/image_size.py --dev-base registry.tilo.so/tilo/tilo-vm:main-ci \
        --arch "$arch" --records "$out/image-$arch.jsonl" --runner none \
        --update-from-previous 'main-{sha7}-{arch}' --git-dir "$TMPDIR/history.git" \
        --json "$out/image-size-$arch.json" --markdown "$out/image-size-$arch.md" > /dev/null || status=1
    done
    cat "$out"/image-*.jsonl > "$out/footprint-image.jsonl" 2>/dev/null || true
    exit "$status"
    ;;
  latency)
    cargo build --locked -p shell --bin shell
    env_json dev lavapipe kvm > "$out/env.json"
    python3 scripts/perf/latency_bench.py --shell-bin "$target/debug/shell" \
      --out "$out/run" --env-json "$out/env.json" --records "$out/latency.jsonl"
    ;;
  render)
    cargo build --locked --release -p ui-examples --example ui-perf
    cargo build --locked --release -p shell --example redraw_bench --example strip_pan_bench
    cargo build --locked --release -p tilo-app --features test-support --example framework-scaling
    cargo build --locked --release -p notes --example notes-latency
    test -r "$VK_DRIVER_FILES"
    examples=$target/release/examples
    env_json release lavapipe kvm > "$out/env.json"
    python3 scripts/perf/render_bench.py timing "$examples/ui-perf" "$examples/redraw_bench" \
      --env-json "$out/env.json" --out "$out" --strip-pan-bench "$examples/strip_pan_bench" \
      --records "$out/render.jsonl"
    for plan in "5000,50000 open framework-open" "5000 scroll framework-scroll" "500,1000,2500 focus framework-focus"; do
      read -r sizes action adapter <<< "$plan"
      "$examples/framework-scaling" "$sizes" "$action" | tee "$out/$adapter.log"
      python3 tools/perf/adapters.py "$adapter" --env-json "$out/env.json" "$out/$adapter.log" >> "$out/render.jsonl"
    done
    "$examples/notes-latency" | tee "$out/notes-latency.log"
    python3 tools/perf/adapters.py notes-latency --env-json "$out/env.json" "$out/notes-latency.log" >> "$out/render.jsonl"
    python3 scripts/perf/render_bench.py counters "$examples/ui-perf" --out "$out/counters"
    ;;
  wakeups)
    cargo build --locked -p shell --example idle-wakeups --features scheduled-idle-benchmark
    cargo build --locked -p shell --bin shell
    status=0
    for run in 1 2 3; do
      TILO_IDLE_SHELL_BIN="$target/debug/shell" TILO_IDLE_WAKEUP_WINDOW_SECS=60 \
        bash tools/test-env.sh "$target/debug/examples/idle-wakeups" > "$out/idle-$run.log" 2>&1 || status=$?
      grep -A40 '^idle wakeups over' "$out/idle-$run.log" || tail -n 40 "$out/idle-$run.log"
    done
    env_json dev lavapipe kvm > "$out/env.json"
    python3 tools/perf/adapters.py idle-wakeups --env-json "$out/env.json" "$out"/idle-*.log > "$out/wakeups.jsonl"
    exit "$status"
    ;;
  search)
    cargo test --locked -p shell --lib --no-run
    status=0
    for run in 1 2 3 4 5; do
      TILO_FILES_NAME_INDEX_NAMES=50000 bash tools/test-env.sh cargo test --locked -p shell --lib \
        shell::search::providers::files::tests::a_watched_change_costs_one_record_not_a_rebuild \
        -- --exact --nocapture --test-threads=1 > "$out/files-name-index-$run.log" 2>&1 || status=$?
      grep 'FILES-NAME-INDEX' "$out/files-name-index-$run.log" || true
    done
    env_json dev none kvm > "$out/env.json"
    python3 tools/perf/adapters.py files-name-index --env-json "$out/env.json" "$out"/files-name-index-*.log \
      > "$out/search.jsonl"
    exit "$status"
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
  hosting-room)
    cargo build --locked --release -p tilo-room-load --features tilo-relay/agents
    env_json release none kvm > "$out/env.json"
    for run in 1 2 3; do
      "$target/release/tilo-room-load" bench --guests 100 --malloc-arenas 2 --hold 60 \
        > "$out/room-load-$run.json" 2> "$out/room-load-$run.log"
    done
    python3 tools/perf/adapters.py room-load --env-json "$out/env.json" "$out"/room-load-*.json \
      > "$out/hosting-room.jsonl"
    ;;
  browser)
    commit_checkout
    cargo fetch --locked
    crates/browser/tilo-browser-runtime/scripts/build.sh "$out/bundle" --bundle desktop
    chrome
    env_json release swiftshader kvm > "$out/env.json"
    node scripts/perf/browser_qualification.mjs "$out/bundle" --out "$out/browser.json" \
      --renderer swiftshader --demo-seconds 0 --rounds 1 --activations 10 --viewport 960x540 \
      --expect-revision "$(git rev-parse HEAD)" | tee "$out/browser-summary.txt"
    python3 tools/perf/adapters.py browser-qualification --env-json "$out/env.json" "$out/browser.json" \
      > "$out/browser.jsonl"
    rm -rf "$out/bundle"
    ;;
  stream)
    commit_checkout
    cargo fetch --locked
    crates/browser/tilo-browser-runtime/scripts/build.sh "$out/bundle" --bundle desktop
    cargo build --locked --release -p tilo-relay --features web,stream-source \
      --bin tilo-relay --example legacy_stream_source
    cargo test --locked --release -p tilo-relay --features stream-source --example legacy_stream_source
    chrome
    env_json release swiftshader kvm > "$out/env.json"
    node scripts/perf/native_stream_qualification.mjs "$out/bundle" --out "$out/stream.json" \
      --relay "$target/release/tilo-relay" --source "$target/release/examples/legacy_stream_source" \
      --renderer swiftshader --viewport 960x540 --warm-up 30 --expect-revision "$(git rev-parse HEAD)" \
      | tee "$out/stream-summary.txt"
    python3 tools/perf/adapters.py native-stream --env-json "$out/env.json" "$out/stream.json" > "$out/stream.jsonl"
    rm -rf "$out/bundle"
    ;;
  *)
    echo "unknown KPI group: $group" >&2
    exit 2
    ;;
esac
