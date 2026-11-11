#!/usr/bin/bash
# Make rpm's SQLite database safe to carry across a container layer.
#
# Podman's overlay diff can intermittently capture an in-place rewritten
# rpmdb.sqlite with torn secondary indexes.  Rebuilding produces a complete
# database at a fresh inode.  rpm cannot swap its rebuild directory over the
# bootc image's overlay-backed /usr/share/rpm directory, so validate the
# rebuild and replace the database file atomically inside the existing
# directory instead.
set -euo pipefail

db_link=/usr/lib/sysimage/rpm
db_dir="$(readlink -f "$db_link")"
db_parent="$(dirname "$db_dir")"
db_name="$(basename "$db_dir")"

test -f "$db_dir/rpmdb.sqlite"

# A failed overlay directory swap leaves the complete rebuilt database in a
# sibling named rpmrebuilddb.<pid>.  Remove only leftovers from an earlier
# interrupted finalization before asking rpm for a new one.
find "$db_parent" -maxdepth 1 -type d -name "${db_name}rebuilddb.*" \
    -exec rm -rf -- {} +

rebuild_log="$(mktemp /tmp/tilo-rpm-rebuild.XXXXXX)"
trap 'rm -f -- "$rebuild_log"' EXIT
if rpm --rebuilddb >"$rebuild_log" 2>&1; then
    # On filesystems where rpm can perform its own directory swap, there is
    # nothing else to copy.  The checks below still enforce the contract.
    :
else
    rebuilt="$(find "$db_parent" -maxdepth 1 -type d \
        -name "${db_name}rebuilddb.*" -print -quit)"
    if test -z "$rebuilt" || ! test -f "$rebuilt/rpmdb.sqlite"; then
        cat "$rebuild_log" >&2
        echo "rpm did not leave a usable rebuilt database" >&2
        exit 1
    fi
    rpm --dbpath "$rebuilt" --verifydb

    install -o root -g root -m 0644 \
        "$rebuilt/rpmdb.sqlite" "$db_dir/.rpmdb.sqlite.new"
    mv -f -- "$db_dir/.rpmdb.sqlite.new" "$db_dir/rpmdb.sqlite"
    rm -rf -- "$rebuilt"
fi

rpm --verifydb
owner="$(rpm -qf /usr/bin/basename)"
case "$owner" in
    coreutils-*) ;;
    *)
        echo "rpmdb file index is corrupt: /usr/bin/basename -> $owner" >&2
        exit 1
        ;;
esac

# rpm's successful verification/query can itself reopen SQLite in WAL mode.
# Remove the derived sidecars only after the last rpm process has exited so the
# next container layer receives the verified main database and nothing else.
rm -f -- "$db_dir/rpmdb.sqlite-wal" "$db_dir/rpmdb.sqlite-shm"
# rpm opens its database in WAL mode, and SQLite records that in the file
# itself: a WAL database cannot be opened where it cannot create its -shm
# sidecar, so on the booted read-only /usr every rpm query failed ("unable to
# open database file"). Store it in rollback-journal mode, which reads fine
# from a read-only filesystem.
python3 - "$db_dir/rpmdb.sqlite" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
mode = db.execute("PRAGMA journal_mode=DELETE").fetchone()[0]
db.close()
if mode != "delete":
    sys.exit(f"rpmdb journal mode is {mode}, not delete")
PY
rm -f -- "$db_dir/rpmdb.sqlite-wal" "$db_dir/rpmdb.sqlite-shm"

# DNF's transaction history and log contain transaction diagnostics, including
# timestamps and process-dependent details. They are diagnostic build
# state, not package state, and must disappear in the same RUN as each DNF call:
# removing them only at the end of the image cannot undo earlier OCI layers.
/usr/libexec/tilo/clean-dnf-layer-state
