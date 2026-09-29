#!/usr/bin/env bash
# Short-lived media helpers must not become unbounded desktop attack surfaces.
set -uo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$TEST_DIR/../.." && pwd)"
AUDIO="$ROOT/crates/platform/tilo-media/src/audio.rs"
RUNTIME="$ROOT/crates/platform/tilo-media/src/audio_runtime.rs"
OCR="$ROOT/crates/media/ocr/src/lib.rs"
SETTINGS_AUDIO="$ROOT/apps/settings/src/panes/audio.rs"
EVENT_SOUND="$ROOT/crates/platform/tilo-media/src/event_sound.rs"
SHELL_CAPTURE="$ROOT/crates/session/shell/src/compositor/audio_capture.rs"

fails=0
checks=0
fail() { echo "FAIL: $*" >&2; fails=$((fails + 1)); }
ok() { checks=$((checks + 1)); }
must_contain() {
    local file="$1" pattern="$2" reason="$3"
    if [ ! -r "$file" ]; then fail "cannot read $file"; return; fi
    if grep -Eq "$pattern" "$file"; then ok; else fail "$reason"; fi
}
must_not_contain() {
    local file="$1" pattern="$2" reason="$3"
    if [ ! -r "$file" ]; then fail "cannot read $file"; return; fi
    if grep -Eq "$pattern" "$file"; then fail "$reason"; else ok; fi
}

must_contain "$AUDIO" 'bounded_command::run_prepared' \
    "audio graph controls need bounded subprocess execution"
must_contain "$AUDIO" 'bounded_command::run\(' \
    "pw-dump enumeration needs bounded subprocess execution"
must_not_contain "$AUDIO" '\.output\(\)' \
    "audio graph code contains an unbounded output capture"
must_contain "$RUNTIME" 'bounded_command::run_prepared' \
    "short-lived audio controls need bounded subprocess execution"
must_contain "$RUNTIME" '\.process_group\(0\)' \
    "interactive audio sessions need isolated process ownership"
must_contain "$RUNTIME" 'bounded_command::terminate_tree' \
    "interactive audio teardown must remove descendants"
must_contain "$OCR" 'run_prepared_with_limits' \
    "OCR needs independent document and diagnostic ceilings"
must_not_contain "$OCR" '\.spawn\(\)' \
    "OCR contains a process path outside the bounded runner"
must_contain "$SETTINGS_AUDIO" 'bounded_command::run\(' \
    "Settings audio probes do not use bounded execution"
must_not_contain "$SETTINGS_AUDIO" 'Command::new|\.output\(\)|wait_with_output' \
    "Settings audio contains a raw subprocess path"
must_contain "$EVENT_SOUND" 'bounded_command::run_prepared' \
    "event sounds do not use descendant-cleaning bounded execution"
must_not_contain "$EVENT_SOUND" '\.spawn\(\)|child\.kill\(\)' \
    "event sounds contain direct-child-only execution"
must_contain "$SHELL_CAPTURE" 'bounded_command::run_prepared' \
    "screen-recording probes need bounded subprocess execution"
must_not_contain "$SHELL_CAPTURE" '\.output\(\)|\.status\(\)' \
    "screen-recording probes contain unbounded command execution"
must_contain "$SHELL_CAPTURE" '\.process_group\(0\)' \
    "long-lived screen audio capture needs isolated process ownership"
must_contain "$SHELL_CAPTURE" 'bounded_command::terminate_tree' \
    "screen audio teardown must remove recorder descendants"

if [ "$fails" -gt 0 ]; then
    echo "media subprocess invariants: $fails failed, $checks passed" >&2
    exit 1
fi
echo "OK: media subprocess invariants ($checks checks)"
