#!/usr/bin/env bash
# Run only on the remote build service. Tool inputs do not include editor code.
set -Eeuo pipefail
target_root="${CARGO_TARGET_DIR:-$PWD/target}"
root="$target_root/iai-valgrind-fixed"
runner_root="$target_root/iai-runner"
mkdir -p "$root"
# Cache only a completely installed, checksum-verified toolchain. Editor edits
# must not rebuild the instrumenter or silently select another Valgrind.
key="$( { sha256sum tools/perf/build-typing-tools.sh tools/perf/typing-valgrind-accounting.patch; cc --version; rustc -Vv; } | sha256sum | cut -d' ' -f1)"
if [[ -f "$root/cache-key" && "$(< "$root/cache-key")" = "$key" ]] &&
    sha256sum --check --status "$root/install.sha256" &&
    sha256sum --check --status "$root/runner.sha256"; then
    printf 'Verified cached Valgrind and iai-callgrind-runner\n'
    exit 0
fi
trap 'status=$?; for log in configure build install; do if [[ -f "$root/$log.log" ]]; then tail -20 "$root/$log.log"; fi; done; exit "$status"' ERR
archive="$root/valgrind-3.27.1.tar.bz2"
curl --fail --location --silent --show-error --output "$archive.pending" \
    https://sourceware.org/pub/valgrind/valgrind-3.27.1.tar.bz2
printf '%s  %s\n' 5d589152eb8071c02feab8ce6ab719e431a1fbc3e2b1700f5432632a8b9264dc "$archive.pending" | sha256sum -c -
mv "$archive.pending" "$archive"
rm -rf "$root/source" "$root/install"
mkdir "$root/source"
tar --no-same-owner -xf "$archive" -C "$root/source" --strip-components=1
# Preserve guarded data-event slots and partial-block dump accounting. This
# repairs attribution, not simulated events, summaries or execution counts.
patch_file="$PWD/tools/perf/typing-valgrind-accounting.patch"
git -C "$root/source" apply --check "$patch_file"
git -C "$root/source" apply "$patch_file"
(
    cd "$root/source"
    ./configure --enable-only64bit --prefix="$root/install" > "$root/configure.log" 2>&1
    make -j4 > "$root/build.log" 2>&1
    make install > "$root/install.log" 2>&1
)
cp "$root/source/COPYING" "$root/install/COPYING"
{
    printf 'source_sha256=%s\n' 5d589152eb8071c02feab8ce6ab719e431a1fbc3e2b1700f5432632a8b9264dc
    sha256sum tools/perf/typing-valgrind-accounting.patch
    printf 'configure=--enable-only64bit\n'
    cc --version | head -1
    "$root/install/bin/valgrind" --version
} > "$root/build-inputs.txt"
find "$root/install" -type f -print0 | sort -z | xargs -0 sha256sum > "$root/install.sha256"
cargo install --locked --version 0.16.1 iai-callgrind-runner \
    --root "$runner_root"
cargo install --list --root "$runner_root" | grep -Fx 'iai-callgrind-runner v0.16.1:'
sha256sum "$runner_root/bin/iai-callgrind-runner" > "$root/runner.sha256"
printf '%s\n' "$key" > "$root/cache-key.pending"
mv "$root/cache-key.pending" "$root/cache-key"
