#!/usr/bin/env bash
# Render sample installer states without running installation actions.
set -euo pipefail
if [[ $# != 2 ]]; then
    echo "Usage: $0 /path/to/tilo-installer /path/to/new-output-directory" >&2
    exit 2
fi
binary=$(realpath "$1")
test -x "$binary"
# Require fresh output so stale images cannot satisfy this run.
mkdir "$2"
shots=$(realpath "$2")
mkdir -m 700 "$shots/runtime"
export XDG_RUNTIME_DIR="$shots/runtime"
unset DISPLAY WAYLAND_DISPLAY
file "$binary" | tee "$shots/binary.txt"
sha256sum "$binary" | tee "$shots/sha256.txt"
ldd "$binary" > "$shots/linked-libraries.txt"
if grep -q 'not found' "$shots/linked-libraries.txt"; then
    cat "$shots/linked-libraries.txt" >&2
    exit 1
fi
render() {
    local step=$1 viewport=$2 name=$3
    timeout --kill-after=10s 120 "$binary" --shots "$shots/$name.png" \
        --step "$step" --viewport "$viewport" > "$shots/$name.log" 2>&1
    test -s "$shots/$name.png"
    printf 'INSTALLER_SHOT_PASS %s %s\n' "$step" "$viewport"
}
for step in network disk disk-alongside account keyboard keyboard-default \
    options options-open review review-alongside install finish finish-recovery \
    finish-recovery-saved failed failed-recovery; do
    render "$step" 1280x800 "$step"
done
for step in disk account review; do
    render "$step" 800x600 "$step-800x600"
done
