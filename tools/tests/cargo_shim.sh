#!/usr/bin/env bash
set -euo pipefail

# These cases exercise human/container cache policy; agent routing has separate
# recording-stub coverage and must never inherit the caller's agent identity.
unset CODEX_THREAD_ID CODEX_SESSION_ID CLAUDECODE
unset QCARGO_REMOTE_ONLY QCARGO_REMOTE_ACTIVE QCARGO_REMOTE_CARGO

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/tilo-cargo-shim-test.XXXXXX")
trap 'chmod -R u+w "$work" 2>/dev/null || true; rm -rf -- "$work"' EXIT

home=$work/home
bin=$work/bin
project=$work/project
mkdir -p "$home/.cargo/bin" "$bin" "$project"
cp "$repo/tools/ci/cargo-shim" "$bin/cargo"

cat >"$home/.cargo/bin/cargo" <<'CARGO'
#!/usr/bin/env bash
for name in TMPDIR CARGO_TARGET_DIR RUSTC_WRAPPER SCCACHE_SERVER_PORT SCCACHE_DIR; do
    if [[ ${!name+x} ]]; then
        printf '%s=set:%s\n' "$name" "${!name}"
    else
        printf '%s=unset\n' "$name"
    fi
done
CARGO
chmod 0755 "$home/.cargo/bin/cargo"

# Presence is enough for the shim; it must not try to contact a real server.
cat >"$bin/sccache" <<'SCCACHE'
#!/usr/bin/env bash
exit 99
SCCACHE
chmod 0755 "$bin/sccache"

run_container() {
    (
        cd "$project"
        HOME="$home" CARGO_HOME="$home/.cargo" PATH="$bin:/usr/bin:/bin" CONTAINER_ID=test \
            "$bin/cargo" build
    )
}

plain=$(TMPDIR=/var/tmp/tilo-build-scratch \
    CARGO_TARGET_DIR=/var/tmp/tilo-build-target \
    RUSTC_WRAPPER= SCCACHE_SERVER_PORT=4999 SCCACHE_DIR=/var/tmp/tilo-cache \
    run_container)
grep -Fqx 'TMPDIR=set:/var/tmp/tilo-build-scratch' <<<"$plain"
grep -Fqx 'CARGO_TARGET_DIR=set:/var/tmp/tilo-build-target' <<<"$plain"
grep -Fqx 'RUSTC_WRAPPER=set:' <<<"$plain"
grep -Fqx 'SCCACHE_SERVER_PORT=set:4999' <<<"$plain"
grep -Fqx 'SCCACHE_DIR=set:/var/tmp/tilo-cache' <<<"$plain"

automatic=$(env -u TMPDIR -u CARGO_TARGET_DIR -u RUSTC_WRAPPER \
    -u SCCACHE_SERVER_PORT -u SCCACHE_DIR \
    HOME="$home" CARGO_HOME="$home/.cargo" PATH="$bin:/usr/bin:/bin" CONTAINER_ID=test \
    "$bin/cargo" build)
grep -Fqx 'TMPDIR=unset' <<<"$automatic"
grep -Fqx 'CARGO_TARGET_DIR=unset' <<<"$automatic"
grep -Fqx 'RUSTC_WRAPPER=set:sccache' <<<"$automatic"
grep -Fqx 'SCCACHE_SERVER_PORT=set:4226' <<<"$automatic"
grep -Fqx "SCCACHE_DIR=set:$home/.cache/sccache-distrobox" <<<"$automatic"

cat >"$bin/distrobox" <<'DISTROBOX'
#!/usr/bin/env bash
set -euo pipefail
[[ $1 == enter && -n $2 && $3 == -- && $4 == cargo ]]
shift 4
for name in TMPDIR CARGO_TARGET_DIR RUSTC_WRAPPER SCCACHE_SERVER_PORT SCCACHE_DIR; do
    if [[ ${!name+x} ]]; then
        printf '%s=set:%s\n' "$name" "${!name}"
    else
        printf '%s=unset\n' "$name"
    fi
done
DISTROBOX
chmod 0755 "$bin/distrobox"
printf 'test-container\n' >"$project/.build-container"

routed=$(env -u CONTAINER_ID \
    HOME="$home" CARGO_HOME="$home/.cargo" PATH="$bin:/usr/bin:/bin" \
    TMPDIR=/var/tmp/tilo-routed-scratch \
    CARGO_TARGET_DIR=/var/tmp/tilo-routed-target \
    RUSTC_WRAPPER= SCCACHE_SERVER_PORT=4888 SCCACHE_DIR=/var/tmp/tilo-routed-cache \
    bash -c 'cd "$1"; exec "$2" build' _ "$project" "$bin/cargo")
grep -Fqx 'TMPDIR=set:/var/tmp/tilo-routed-scratch' <<<"$routed"
grep -Fqx 'CARGO_TARGET_DIR=set:/var/tmp/tilo-routed-target' <<<"$routed"
grep -Fqx 'RUSTC_WRAPPER=set:' <<<"$routed"
grep -Fqx 'SCCACHE_SERVER_PORT=set:4888' <<<"$routed"
grep -Fqx 'SCCACHE_DIR=set:/var/tmp/tilo-routed-cache' <<<"$routed"

printf 'cargo shim environment contract: PASS\n'
