#!/usr/bin/env bash
# Run the editor's standard iai-callgrind benchmark, retaining raw counters.
set -euo pipefail
target_root="${CARGO_TARGET_DIR:-$PWD/target}"
out="$target_root/iai"
mkdir -p "$out"
export TMPDIR="$target_root/iai-tmp"
mkdir -p "$TMPDIR"
chmod 700 "$TMPDIR"
# The separately cached tool dependency supplies this exact instrumenter.
# Unpatched 3.27.1 can misattribute guarded stores to another instruction's Ir.
runner_root="$target_root/iai-runner"
tool_root="$target_root/iai-valgrind-fixed"
test -x "$runner_root/bin/iai-callgrind-runner"
cargo install --list --root "$runner_root" | grep -Fx 'iai-callgrind-runner v0.16.1:'
export PATH="$runner_root/bin:$tool_root/install/bin:$PATH"
export VALGRIND_LIB="$tool_root/install/libexec/valgrind"
test -x "$VALGRIND_LIB/callgrind-amd64-linux"
valgrind --version > "$out/valgrind-version.txt"
cp "$tool_root/build-inputs.txt" "$out/instrumenter-build-inputs.txt"
sha256sum "$VALGRIND_LIB/callgrind-amd64-linux" > "$out/instrumenter.sha256"
rustc -Vv > "$out/compiler.txt"
sha256sum apps/code-editor/benches/typing.rs > "$out/benchmark-source.sha256"
sha256sum Cargo.toml Cargo.lock apps/code-editor/Cargo.toml \
    tools/perf/typing-iai.sh > "$out/build-inputs.sha256"
production_sources=(
    apps/code-editor/src/app.rs
    apps/code-editor/src/editor_view.rs
    apps/code-editor/src/editor_scroll.rs
)
# Baseline checkouts without the extracted same-sync module still hash the
# production editing path they do carry.
if [ -f apps/code-editor/src/editor_sync.rs ]; then
    production_sources+=(apps/code-editor/src/editor_sync.rs)
fi
production_sources+=(crates/sdk/ui-core/src/text_render/cache.rs)
sha256sum "${production_sources[@]}" > "$out/production-source.sha256"
mode="${1:-}"
if [ "$mode" = --build-only ]; then
    if cargo bench --locked -p code-editor --bench typing --features benchmarks \
        --no-run --timings --message-format=json > "$out/build.jsonl"; then
        :
    else
        status=$?
        python3 - "$out/build.jsonl" <<'PY'
import json
import pathlib
import sys
for line in pathlib.Path(sys.argv[1]).read_text().splitlines():
    message = json.loads(line).get('message', {})
    if message.get('rendered'):
        print(message['rendered'], file=sys.stderr)
PY
        exit "$status"
    fi
    python3 - "$out/build.jsonl" "$out/benchmark-executable" <<'PY'
import json
import pathlib
import sys
artifacts = [json.loads(line) for line in pathlib.Path(sys.argv[1]).read_text().splitlines()]
executables = {artifact['executable'] for artifact in artifacts
               if artifact.get('reason') == 'compiler-artifact'
               and artifact.get('target', {}).get('name') == 'typing'
               and 'bench' in artifact.get('target', {}).get('kind', [])
               and artifact.get('executable')}
assert len(executables) == 1, executables
pathlib.Path(sys.argv[2]).write_text(executables.pop() + '\n')
PY
elif [ "$mode" = --run-executable ]; then
    shift
    test -x "${1:?missing Cargo benchmark executable}"
    printf '%s\n' "$1" > "$out/benchmark-executable"
    shift
fi
if [[ "$mode" = --build-only || "$mode" = --run-executable ]]; then
    # Export only the executable, not Cargo's matching object/metadata files.
    built="$(< "$out/benchmark-executable")"
    packaged="$target_root/iai-built/deps/typing"
    mkdir -p "$(dirname "$packaged")"
    cp "$built" "$packaged"
    # A remote build's absolute font-cache path is not a runtime artifact.
    # Bundle the verified catalog beside the executable, as installed apps do.
    fonts="$(python3 tools/font-catalog.py root crates/sdk/ui-core/assets/fonts --scope fixtures)"
    rm -rf "$(dirname "$packaged")/fonts"
    python3 tools/font-catalog.py stage "$fonts" "$(dirname "$packaged")/fonts" --scope fixtures
    printf '%s\n' "$packaged" > "$out/benchmark-executable"
    sha256sum "$packaged" > "$out/benchmark-executable.sha256"
fi
if [ "$mode" != --build-only ]; then
    # Run the exact built artifact through Cargo's configured environment wrapper.
    # Do not rebuild a second time in the execution stage.
    sha256sum --check "$out/benchmark-executable.sha256"
    executable="$(< "$out/benchmark-executable")"
    python3 tools/font-catalog.py verify "$(dirname "$executable")/fonts" --scope fixtures \
        > "$out/font-catalog-verification.txt"
    PYTHONDONTWRITEBYTECODE=1 python3 tools/perf/typing-iai-check-test.py
    # Verify both real fixtures without instrumentation before collecting counters.
    # These are correctness checks, not latency measurements.
    for spec in '0 single_character_to_scene' '1 single_key_to_scene'; do
        read -r benchmark function_name <<< "$spec"
        for case in 0 1 2; do
            tools/cargo-runner.sh "$executable" --iai-run typing "$benchmark" "$case" \
                "typing::typing::$function_name"
        done
    done
    # Keep ASLR enabled: unprivileged containers need not grant personality(2).
    # Cache counters may vary with addresses; compare instruction work separately.
    # Cargo supplies --bench to its runner. Preserve any explicit iai arguments.
    if [ "$#" -eq 0 ]; then set -- --bench; fi
    tools/cargo-runner.sh "$executable" \
        --allow-aslr=true --save-summary=json --output-format=json "$@" | tee "$out/typing.jsonl"
    python3 tools/perf/typing-iai-check.py "$out"
fi
