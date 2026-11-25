# The Lounge's own programs run directly, for a test of a page against them:
# the image's entrypoint with a deployment's environment, the daemon the image
# carries on its PATH, and the web entry served beside it. No image or micro-VM
# is built around them, so a change to the page or the server is tried in
# minutes; the image's own test (test-image.sh) boots the built image.
#
# Sourced, not run: `start_lounge BUNDLE WORK` leaves the daemon answering and
# sets `site_origin` and `log`. The volume is a fresh deployment's, given its
# owner's root among the operator's secrets: the daemon is that owner's own
# device, so it founds the workspace its configuration keeps, under the root and
# with the entry the deployment names. `LOUNGE_ADMISSION` (open unless set),
# `LOUNGE_DEFAULT_ROLE`, `LOUNGE_DESCRIPTION` and `LOUNGE_WAIT_LIST_MAX` are the
# entry it founds with. `restart_lounge` stops the Lounge alone and starts it
# again on the volume it kept, the site left serving, as a deployment's restart
# does. It needs node, chromium, curl, and TILO_SERVER_BIN and TILO_CRYPTO_BIN
# naming built programs, and TILO_FONTS_DIR the product's font catalog. The
# caller's EXIT trap must call `stop_lounge`.
#
# Its addresses are LOUNGE_PORT_BASE (default 19780) and the two ports after
# it — the daemon's HTTPS web listener, its packet relay, and the site — so a
# second Lounge (another entry, another admission) runs beside the first on a
# base of its own. The site is named LOUNGE_SITE_NAME (default
# 127.0.0.1); `localhost` gives it a name, which a page needs to make a
# passkey (a passkey belongs to a site's name, never to an address).

lounge_here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
lounge_repo=$(cd -- "$lounge_here/../../.." && pwd)
lounge_pids=()

stop_lounge() {
  local pid
  for pid in "${lounge_pids[@]}"; do kill -KILL "$pid" 2>/dev/null || true; done
  for pid in "${lounge_pids[@]}"; do wait "$pid" 2>/dev/null || true; done
  lounge_pids=()
}

lounge_fail() {
  echo "FAIL: $*" >&2
  [[ ! -s ${log:-} ]] || { echo '--- the Lounge said:' >&2; tail -40 "$log" >&2; }
  exit 1
}

# The Lounge's entrypoint, started on the volume start_lounge laid out.
run_lounge() {
  # Every address this deployment serves is loopback, so its endpoint is told
  # one: a host told one address skips the portmapper (UPnP over SSDP, name
  # lookups) it would otherwise probe a router with, and a sandboxed check is
  # refused those.
  # The image's entrypoint, with the environment the check gives the image.
  # `LOUNGE_LOG` is the filter the daemon logs by, for a check that needs to
  # read why it did or did not do something.
  env -i HOME="$lounge_home" PATH="$lounge_bin:/usr/bin:/bin" \
    ${LOUNGE_LOG:+TILO_SERVER_LOG="$LOUNGE_LOG"} \
    TILO_SOURCE_REVISION=direct-lounge \
    TILO_SECRETS_DIR="$lounge_volume/secrets" \
    TILO_WORKSPACES_ROOT_FILE="$lounge_volume/secrets/workspace-root" \
    TILO_WORKSPACES_STATE="$lounge_volume" \
    TILO_SERVER_CONFIG="$lounge_volume/server.toml" \
    sh "$lounge_here/entrypoint.sh" >> "$log" 2>&1 &
  lounge_pid=$!
  lounge_pids+=("$lounge_pid")
}

# Wait for the Lounge run_lounge started to serve, as the deployment's probe
# asking it. The daemon answers on its native endpoint from its start, where
# the device's own actions are served, wherever its profile keeps it: one that
# stops first, or does not answer within LOUNGE_ANSWER_SECONDS (default 30),
# fails.
await_lounge() {
  local deadline=$((SECONDS + ${LOUNGE_ANSWER_SECONDS:-30})) socket
  while :; do
    socket=$(find "$lounge_volume" -name native.sock -print -quit 2>/dev/null)
    # The profile names its endpoint with a link, and the link answers only while the daemon
    # bound the socket it names: one left by a stopped daemon is no answer.
    [[ -n $socket && -S $socket ]] && break
    kill -0 "$lounge_pid" 2>/dev/null || lounge_fail 'the Lounge stopped before it answered'
    ((SECONDS < deadline)) \
      || lounge_fail "the Lounge did not answer in ${LOUNGE_ANSWER_SECONDS:-30}s"
    sleep 0.1
  done
}

# What this deployment is: the device acts as the owner root it was handed, and
# the kept workspace is founded with the entry the deployment names, listed by the
# directory it serves. A Lounge that founded nothing, or founded as its own device
# key instead of the root, fails.
verify_founded() {
  local socket people directory entry
  socket=$(find "$lounge_volume" -name native.sock -print -quit 2>/dev/null)
  [[ -n $socket ]] || lounge_fail 'the Lounge keeps no native endpoint'
  people=$(python3 - "$socket" <<'PYTHON'
import json, socket, sys
call = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "tools/call",
    "params": {
        "name": "tilo_do",
        "arguments": {"path": "devices#info", "args": {}, "wait_ms": 30000},
        "_meta": {"so.tilo.mcp": {"structured": True}},
    },
}
stream = socket.socket(socket.AF_UNIX)
stream.settimeout(60)
stream.connect(sys.argv[1])
stream.sendall((json.dumps(call) + "\n").encode())
answer = b""
while not answer.endswith(b"\n"):
    chunk = stream.recv(1 << 20)
    if not chunk:
        break
    answer += chunk
answer = json.loads(answer)
result = answer.get("result", {})
structured = result.get("structuredContent") or {}
if not structured:
    structured = json.loads(result["content"][0]["text"])
print(" ".join((structured.get("result") or {}).get("people") or []))
PYTHON
)
  [[ " $people " == *" $owner_root_public "* ]] \
    || lounge_fail "the Lounge does not act as the owner root $owner_root_public: its device names $people"
  for _ in $(seq 1 100); do
    directory=$(curl -fsS -k -X POST -H "Origin: $server_origin" --data '' \
      "$server_origin/peer/v1/directory" 2>/dev/null) && break
    kill -0 "$lounge_pid" 2>/dev/null || lounge_fail 'the Lounge stopped before it answered its directory'
    sleep 0.1
  done
  [[ -n $directory ]] || lounge_fail 'the Lounge never answered its directory'
  entry=$(DIRECTORY_PAGE="$directory" WORKSPACE="$workspace" \
    DESCRIPTION="${LOUNGE_DESCRIPTION:-Lounge}" ADMISSION="${LOUNGE_ADMISSION:-open}" \
    python3 - <<'PYTHON'
import json, os
page = json.loads(os.environ["DIRECTORY_PAGE"])
entry = next(
    (e for e in page.get("workspaces", []) if e.get("workspace") == os.environ["WORKSPACE"]),
    None,
)
if entry is None:
    raise SystemExit(f"the directory lists no workspace {os.environ['WORKSPACE']}")
if entry.get("name") != "Lounge" or entry.get("description") != os.environ["DESCRIPTION"]:
    raise SystemExit(f"the directory lists it as {entry}")
if bool(entry.get("invitationOnly")) != (os.environ["ADMISSION"] == "invite"):
    raise SystemExit(f"the directory lists its entry as {entry}")
print(f"{entry['workspace']} as {entry['name']} ({entry['description']}, admission {os.environ['ADMISSION']})")
PYTHON
) || lounge_fail 'the kept workspace is not listed with the entry the deployment names'
  echo "ok: the kept workspace is founded under the owner root $owner_root_public: the directory lists $entry"
}

start_lounge() {
  local started=$SECONDS
  local bundle=${1:?start_lounge BUNDLE WORK} work=${2:?start_lounge BUNDLE WORK}
  local server_bin=${TILO_SERVER_BIN:?TILO_SERVER_BIN names the server to run}
  local crypto=${TILO_CRYPTO_BIN:?TILO_CRYPTO_BIN names the certificate mint}
  local fonts=${TILO_FONTS_DIR:?TILO_FONTS_DIR names the product font catalog}
  log=$work/lounge.log
  local site_log=$work/site.log
  # This Lounge's own addresses.
  local base=${LOUNGE_PORT_BASE:-19780}
  lounge_web_port=$base lounge_live_port=$((base + 1))
  local site_port=$((base + 2))
  server_origin=https://127.0.0.1:$lounge_web_port
  site_origin=https://${LOUNGE_SITE_NAME:-127.0.0.1}:$site_port

  [[ -f $bundle/build.json ]] || lounge_fail "no desktop bundle at $bundle"
  python3 -I "$lounge_repo/crates/core/tilo-browser-runtime/container/layout.py" "$bundle" "$work/site"
  "$crypto" generate-tls-fixture 127.0.0.1 "$work/site.ca.crt" "$work/site.key" "$work/site.crt" \
    || lounge_fail 'no certificate for the site'

  # The volume, as a fresh deployment mounts it: empty, with the owner's root
  # among the operator's secrets. The root is the person the Lounge belongs to,
  # kept here as on any of their own devices: the deployment founds its kept
  # workspace as them, under this root, and needs no ticket.
  local volume=$work/volume
  mkdir -p "$volume/secrets" "$work/bin"
  "$crypto" generate-ed25519-key "$work/owner-root.seed" "$work/owner-root.pub" \
    || lounge_fail 'no owner root for this Lounge'
  od -An -tx1 -v "$work/owner-root.seed" | tr -d ' \n' > "$volume/secrets/workspace-root"
  chmod 600 "$volume/secrets/workspace-root"
  owner_root_public=$(tr -d ' \n' < "$work/owner-root.pub")
  # The server as the image carries it: the product's font catalog beside the
  # program, which is where a Tilo binary finds it outside the image (its apps
  # lay text out, and a host that seeds them has no system fonts).
  cp "$server_bin" "$work/bin/tilo-runtime"
  cp -R "$fonts" "$work/bin/fonts"

  # This Lounge's server configuration: its web UI and packet relay on this
  # check's ports, so two runs beside each other bind their own, and the
  # workspace its owner root founds and keeps, with the entry the deployment
  # names. `open` (the default) admits anyone who asks; `invite` lists it and
  # takes people by invitation.
  local id admission role description
  id=$(python3 -c 'import uuid; print(uuid.uuid4())')
  admission=${LOUNGE_ADMISSION:-open}
  role=${LOUNGE_DEFAULT_ROLE:-editor}
  description=${LOUNGE_DESCRIPTION:-Lounge}
  mkdir -p "$volume/lounge"
  printf '%s' "$id" > "$volume/lounge/workspace-id"
  {
    printf 'web_entry = "%s/"\n' "$site_origin"
    printf 'web_listen = "127.0.0.1:%s"\n' "$lounge_web_port"
    printf 'packet_relay = "127.0.0.1:%s"\n' "$lounge_live_port"
    printf '\n[[kept]]\nid = "%s"\nname = "Lounge"\nentry = "%s"\ndefault_role = "%s"\ndescription = "%s"\n' \
      "$id" "$admission" "$role" "$description"
    [ -z "${LOUNGE_WAIT_LIST_MAX:-}" ] \
      || printf 'wait_list_max = %s\n' "$LOUNGE_WAIT_LIST_MAX"
  } > "$volume/server.toml"

  # The daemon keeps a profile private to its account, and the volume is its
  # profile: a deployment's volume is one.
  chmod 700 "$volume"
  lounge_volume=$volume lounge_bin=$work/bin lounge_home=$work
  run_lounge
  await_lounge
  workspace=$(cat "$volume/lounge/workspace-id" 2>/dev/null) || lounge_fail 'the Lounge kept no workspace id'
  echo "ok: the Lounge is Ready ($workspace, $((SECONDS - started))s)"
  verify_founded

  # Who the host is, as the operator hands it to the web entry
  # (TILO_WEB_HOME_HOST_INFO), so the page dials it without asking it first. The
  # socket alone is the device answering: the line comes once it has spent the
  # ticket and keeps what it serves.
  local info line said=$((SECONDS + ${LOUNGE_LINE_SECONDS:-120}))
  while :; do
    line=$(find "$lounge_volume" -name web-host.json -print -quit 2>/dev/null)
    [[ -n $line ]] && break
    kill -0 "$lounge_pid" 2>/dev/null || lounge_fail 'the Lounge stopped before it said what it is'
    ((SECONDS < said)) || lounge_fail 'the Lounge never said what it is (web-host.json)'
    sleep 0.2
  done
  info=$(cat "$line") || lounge_fail 'the server said nothing of itself (web-host.json)'
  node "$lounge_here/integration/site.mjs" "$work/site" "$work/site.crt" "$work/site.key" \
    "$server_origin" "$server_origin" Lounge \
    "$server_origin/peer/v1/directory/$workspace/" "$site_port" "$info" c_feedback >> "$site_log" 2>&1 &
  local site_pid=$!
  lounge_pids+=("$site_pid")
  for _ in $(seq 300); do
    grep -q "\"url\": *\"https://127.0.0.1:$site_port\"" "$site_log" && break
    kill -0 "$site_pid" 2>/dev/null || lounge_fail "the web entry stopped: $(cat "$site_log")"
    sleep 0.1
  done
  grep -q "\"url\": *\"https://127.0.0.1:$site_port\"" "$site_log" || lounge_fail "the web entry did not start: $(cat "$site_log")"
  echo "ok: the web entry is served at $site_origin (the Lounge and it up in $((SECONDS - started))s)"
}

# The Lounge stopped and started again on the volume it kept, the site left
# serving: what a deployment's restart is, and what a page that was here reads
# afterwards is what the workspace kept.
restart_lounge() {
  local started=$SECONDS pid kept=()
  kill -KILL "$lounge_pid" 2>/dev/null || true
  wait "$lounge_pid" 2>/dev/null || true
  for pid in "${lounge_pids[@]}"; do [[ $pid == "$lounge_pid" ]] || kept+=("$pid"); done
  lounge_pids=("${kept[@]}")
  run_lounge
  await_lounge
  [[ $(cat "$lounge_volume/lounge/workspace-id") == "$workspace" ]] || lounge_fail 'a restart on the same volume served a different workspace'
  echo "ok: the Lounge started again on its own volume ($workspace, $((SECONDS - started))s)"
}
