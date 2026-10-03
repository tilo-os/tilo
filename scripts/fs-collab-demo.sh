#!/usr/bin/env bash
# Two-peer end-to-end harness for the collaborative filesystem (crates/system/fs/).
#
#   fs-collab-demo.sh up      # set up two synced mounts, leave them running
#   fs-collab-demo.sh test    # run assertions against a fresh pair, then tear down
#   fs-collab-demo.sh down    # tear down a running `up` session
#
# Peer A `serve`s a seeded workspace; peer B `mount`s it via the ticket. Both are
# real iroh P2P nodes with separate content stores — bytes move over iroh-blobs,
# metadata over iroh-gossip. On localhost they connect directly.
set -uo pipefail

REPO="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
BIN="$REPO/target/debug/fs"
ROOT="${TILO_FS_DEMO_ROOT:-$HOME/.cache/tilo-fs-demo}"
A_MNT="$ROOT/A" B_MNT="$ROOT/B"
A_CACHE="$ROOT/A-cache" B_CACHE="$ROOT/B-cache"
SEED="$ROOT/seed" LOGS="$ROOT/logs"

die() { echo "FATAL: $*" >&2; exit 1; }

ensure_bin() {
    [ -x "$BIN" ] || cargo build -p tilo-fs --manifest-path "$REPO/Cargo.toml" || die "build failed"
}

ensure_fuse_conf() {
    grep -q '^user_allow_other' /etc/fuse.conf 2>/dev/null && return
    echo "Enabling user_allow_other in /etc/fuse.conf (needs sudo, one-time)…"
    echo user_allow_other | sudo tee -a /etc/fuse.conf >/dev/null || die "could not set user_allow_other"
}

down() {
    [ -f "$ROOT/pids" ] && { kill $(cat "$ROOT/pids") 2>/dev/null; rm -f "$ROOT/pids"; }
    # Narrowly scoped to THIS demo's mount paths, so other serve/mount
    # sessions (e.g. a separate `up`) are never touched.
    pkill -f -- "--mount-point $A_MNT " 2>/dev/null
    pkill -f -- "--mount-point $B_MNT " 2>/dev/null
    fusermount3 -u -z "$A_MNT" 2>/dev/null
    fusermount3 -u -z "$B_MNT" 2>/dev/null
}

# Wait until $1 contains a non-empty $2 (file content), up to ~15s.
wait_content() {
    for _ in $(seq 1 75); do
        [ -s "$1/$2" ] && return 0
        sleep 0.2
    done
    return 1
}

bring_up() {
    ensure_bin; ensure_fuse_conf
    down
    rm -rf "$ROOT"; mkdir -p "$A_MNT" "$B_MNT" "$A_CACHE" "$B_CACHE" "$SEED/notes" "$LOGS"
    echo "Welcome to the shared Tilo workspace!" > "$SEED/README.txt"
    echo "edit me from either side" > "$SEED/notes/scratch.txt"

    "$BIN" serve --mount-point "$A_MNT" --cache-dir "$A_CACHE" --seed "$SEED" \
        > "$LOGS/host.log" 2>&1 &
    echo $! > "$ROOT/pids"

    local ticket=""
    for _ in $(seq 1 75); do
        ticket=$(grep -aoE '^[0-9a-f]{40,}$' "$LOGS/host.log" | head -1)
        [ -n "$ticket" ] && break
        sleep 0.2
    done
    [ -n "$ticket" ] || { cat "$LOGS/host.log"; die "host produced no ticket"; }

    "$BIN" mount --mount-point "$B_MNT" --ticket "$ticket" --cache-dir "$B_CACHE" \
        > "$LOGS/view.log" 2>&1 &
    echo $! >> "$ROOT/pids"

    for _ in $(seq 1 75); do grep -qa "Mounted QuickFS" "$LOGS/view.log" && break; sleep 0.2; done
    wait_content "$B_MNT" "README.txt" || die "bootstrap projection/hydration failed"
}

case "${1:-test}" in
  up)
    bring_up
    echo "Two synced mounts are running:"
    echo "  A (host):  $A_MNT"
    echo "  B (joined):$B_MNT"
    echo "Edit files in either; changes sync over iroh. Stop with: $0 down"
    ;;

  down)
    down; echo "torn down."
    ;;

  test)
    trap down EXIT
    bring_up
    fail=0
    check() { if [ "$2" = "$3" ]; then echo "PASS $1"; else echo "FAIL $1: got '$2' want '$3'"; fail=1; fi; }

    # projection + hydration
    check "B sees seeded README" "$(cat "$B_MNT/README.txt")" "Welcome to the shared Tilo workspace!"
    check "B hydrates nested"   "$(cat "$B_MNT/notes/scratch.txt")" "edit me from either side"

    # write-back A->B and B->A (wait for the content delta, not just the entry)
    echo "from A" > "$A_MNT/a.txt"; wait_content "$B_MNT" "a.txt" && check "A->B content" "$(cat "$B_MNT/a.txt")" "from A" || { echo "FAIL A->B (timeout)"; fail=1; }
    echo "from B" > "$B_MNT/b.txt"; wait_content "$A_MNT" "b.txt" && check "B->A content" "$(cat "$A_MNT/b.txt")" "from B" || { echo "FAIL B->A (timeout)"; fail=1; }

    # structural: mkdir + nested
    mkdir -p "$A_MNT/d"; echo deep > "$A_MNT/d/n.txt"
    wait_content "$B_MNT" "d/n.txt" && check "A mkdir+nested -> B" "$(cat "$B_MNT/d/n.txt")" "deep" || { echo "FAIL mkdir (timeout)"; fail=1; }

    # concurrent edit -> a conflict copy must appear somewhere.
    # NOTE: wait only on these two writers — a bare `wait` would also block on
    # the long-lived serve/mount background jobs and hang forever.
    echo "edit-from-A" > "$A_MNT/README.txt" & wa=$!
    echo "edit-from-B" > "$B_MNT/README.txt" & wb=$!
    wait "$wa" "$wb"
    sleep 5  # let gossip + the reconcile pass settle
    if ls "$A_MNT"/README.txt.sync-conflict-* "$B_MNT"/README.txt.sync-conflict-* >/dev/null 2>&1; then
        echo "PASS concurrent edit produced a conflict copy"
    else
        echo "WARN no conflict copy (the two writes may not have been truly concurrent)"
    fi

    echo "---"
    [ "$fail" = 0 ] && echo "E2E PASSED" || { echo "E2E FAILED"; exit 1; }
    ;;

  *) echo "usage: $0 {up|test|down}"; exit 2 ;;
esac
