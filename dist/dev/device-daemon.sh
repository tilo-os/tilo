# Sourced by the development sessions (dist/dev/nested-session.sh, dist/dev/shell-run.sh): start
# this computer's device daemon the way the person's own service starts it, before the session.
#
# `start_device_daemon BIN LOG` starts plain `BIN/tilo-runtime` in the caller's environment (its
# HOME, XDG tree, XDG_RUNTIME_DIR and TILO_IDENTITY_ROOT are the session's), as the person's user
# unit runs it: over the person's own profile, its native endpoint at
# `$XDG_RUNTIME_DIR/tilo/native.sock`. It writes to LOG, appending. Once the daemon answers, a
# computer no person's certificate names yet founds its person through it (`devices#found`), as a
# computer's first `tilo` command does: the daemon keeps that person's root and their workspaces.
# The session started after it finds the daemon running and asks it to start the shell
# (`shells#start`), which hands the shell the link it reaches its workspaces on.
#
# It sets `device_daemon_pid`, the daemon's process group: the caller stops it with the rest.

start_device_daemon() {
    local bin=$1 log=$2
    local socket=$XDG_RUNTIME_DIR/tilo/native.sock
    mkdir -p -m 700 "$XDG_RUNTIME_DIR/tilo"
    # This computer's own app catalogue: the apps it runs are the builds in its own store. The
    # official store stays named and disabled, as a workspace that keeps no store keeps its row.
    local catalogue=$HOME/.local/state/tilo/runtime owner
    owner=$(sed -n 's/^TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY=//p' \
        "$(dirname -- "${BASH_SOURCE[0]}")/../../os/publication.env")
    [[ $owner =~ ^[0-9a-f]{64}$ ]] || { echo "no official registry owner key" >&2; return 1; }
    install -d -m 700 "$catalogue"
    printf '[[registry]]\nname = "Tilo main"\nbase_url = "https://apps.tilo.so"\nowner_pubkey_hex = "%s"\nenabled = false\n' \
        "$owner" > "$catalogue/registries.toml"
    env -u TILO_RUNTIME_PROFILE -u WAYLAND_DISPLAY -u DISPLAY \
        setsid "$bin/tilo-runtime" >> "$log" 2>&1 &
    device_daemon_pid=$!
    local tries=600
    until [[ -S $socket ]]; do
        kill -0 "$device_daemon_pid" 2>/dev/null \
            || { tail -40 "$log" >&2; echo "the device daemon ended as it started (see $log)" >&2; return 1; }
        ((tries--)) || { echo "the device daemon did not open $socket (see $log)" >&2; return 1; }
        sleep 0.1
    done
    python3 -I - "$socket" <<'PY'
import json, socket, sys

# The call `tilo`'s first command makes (tilo_api::daemon::DaemonPlace::found).
ask = {"jsonrpc": "2.0", "id": 1, "method": "tools/call",
       "params": {"name": "tilo_do", "arguments": {"path": "devices#found", "args": {}}}}
with socket.socket(socket.AF_UNIX) as native:
    native.settimeout(120)
    native.connect(sys.argv[1])
    native.sendall((json.dumps(ask) + "\n").encode())
    answer = native.makefile().readline()
if '"error"' in answer or '"isError":true' in answer.replace(" ", ""):
    if "this device is a person's already" not in answer:
        sys.exit(f"the device daemon founded no person: {answer.strip()}")
PY
}
