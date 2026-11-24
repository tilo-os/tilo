# The Lounge's own programs run directly, for a test of a page against them:
# the image's entrypoint with a deployment's environment, the daemon the image
# carries on its PATH, and the web entry served beside it. No image or micro-VM
# is built around them, so a change to the page or the server is tried in
# minutes; the image's own test (test-image.sh) boots the built image.
#
# Sourced, not run: `start_lounge BUNDLE WORK` leaves the daemon answering and
# sets `site_origin` and `log`. The workspace it keeps is placed on it by the
# test, through the person's own commands. An empty volume joins a person with
# a ticket, and no person's device is here to print one: the check makes the
# person on a device of its own under WORK, and takes the first ticket a server
# of theirs joins with — the device's own `devices#found`, the action
# `tilo login --new-identity --ticket server` asks, waited on until it answers.
# `LOUNGE_TICKET` names a ticket instead, for a caller that already has one.
# `restart_lounge` stops the Lounge alone and starts it again on the volume it
# kept, the site left serving, as a deployment's restart does. It needs node,
# chromium, curl, and TILO_SERVER_BIN and TILO_CRYPTO_BIN naming built programs,
# and TILO_FONTS_DIR the product's font catalog. The caller's EXIT trap must
# call `stop_lounge`.
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
    ${LOUNGE_TICKET:+TILO_TICKET="$LOUNGE_TICKET"} \
    TILO_SOURCE_REVISION=direct-lounge \
    TILO_SECRETS_DIR="$lounge_volume/secrets" \
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
    [[ -n $socket ]] && break
    kill -0 "$lounge_pid" 2>/dev/null || lounge_fail 'the Lounge stopped before it answered'
    ((SECONDS < deadline)) \
      || lounge_fail "the Lounge did not answer in ${LOUNGE_ANSWER_SECONDS:-30}s"
    sleep 0.1
  done
}

# The person a check's Lounge belongs to: a device of theirs under WORK, the
# port it answers on (past the Lounge's three), its log, and the ticket a server
# of theirs joins with.
person_home= person_port= person_log= server_ticket= person_pid=

# A program as the person's device runs it, with the environment its native
# endpoint is found by.
device_environment=()
person_environment() {
  device_environment=(
    HOME="$person_home" PATH="$lounge_bin:/usr/bin:/bin"
    TMPDIR="$person_home/tmp" XDG_RUNTIME_DIR="$person_home/run"
    XDG_CONFIG_HOME="$person_home/config" XDG_STATE_HOME="$person_home/state"
    XDG_DATA_HOME="$person_home/data" XDG_CACHE_HOME="$person_home/cache"
    TILO_IDENTITY_ROOT="$person_home/data/tilo"
    TILO_DEVICE_PORT="$person_port"
    TILO_SITE="$site_origin"
  )
}

# The first ticket the person's device issues for a server, made as the person
# is founded on it: the device's own `devices#found` action, waited on until the
# person's services have issued what was asked. A server joins its person with
# this ticket, as the entrypoint spends it.
mint_ticket() {
  local socket=$person_home/run/tilo/native.sock
  python3 - "$socket" <<'PYTHON'
import json, socket, sys

socket_path = sys.argv[1]
call = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "tools/call",
    "params": {
        "name": "tilo_do",
        "arguments": {
            "path": "devices#found",
            "args": {"tickets": ["server"], "name": "Lounge"},
            "wait_ms": 150000,
        },
        "_meta": {"so.tilo.mcp": {"structured": True}},
    },
}
stream = socket.socket(socket.AF_UNIX)
stream.settimeout(200)
stream.connect(socket_path)
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
issued = structured.get("result") or {}
for ticket in issued.get("tickets") or []:
    if ticket.get("kind") == "server":
        print(ticket["ticket"])
        break
else:
    raise SystemExit(f"the person issued no server ticket: {json.dumps(answer)[:600]}")
PYTHON
}

# Found the person this Lounge belongs to, on a device of its own under WORK,
# and take the first ticket a server of theirs joins with. The device is one of
# the person's computers: its own home, its own endpoint, and the product's own
# program the Lounge runs, started as a device with no server configuration.
found_person() {
  local runtime=$lounge_bin/tilo-runtime
  [[ -x $runtime ]] || lounge_fail "no tilo-runtime at $runtime"
  mkdir -p "$person_home/run" "$person_home/config" "$person_home/state" \
    "$person_home/data" "$person_home/cache" "$person_home/tmp"
  person_environment
  # A session of its own: the person's device starts the services they run, and
  # the whole session ends with it.
  env -i "${device_environment[@]}" setsid "$runtime" >> "$person_log" 2>&1 &
  local device=$!
  local socket=$person_home/run/tilo/native.sock
  for _ in $(seq 1 600); do
    [[ -S $socket ]] && break
    kill -0 "$device" 2>/dev/null \
      || lounge_fail "the person's device stopped at its start: $(tail -3 "$person_log" 2>/dev/null)"
    sleep 0.1
  done
  [[ -S $socket ]] || lounge_fail "the person's device did not answer at $socket"
  server_ticket=$(mint_ticket) \
    || lounge_fail "the person's device issued no server ticket: $(tail -3 "$person_log" 2>/dev/null)"
  # The device that issued the ticket certifies the Lounge as one of its
  # person's servers when the Lounge spends it, so it stays up until then.
  person_pid=$device
  lounge_pids+=("$device")
}

# The person's device, once the Lounge has joined: nothing asks it again.
stop_person() {
  [[ -n ${person_pid:-} ]] || return 0
  kill -KILL -- "-$person_pid" 2>/dev/null || kill -KILL "$person_pid" 2>/dev/null || true
  wait "$person_pid" 2>/dev/null || true
  person_pid=
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

  # The volume, as a fresh deployment mounts it: empty, with a throwaway owner
  # root among the operator's secrets.
  local volume=$work/volume
  mkdir -p "$volume/secrets" "$work/bin"
  printf 'a%.0s' $(seq 64) > "$volume/secrets/workspace-root"
  chmod 600 "$volume/secrets/workspace-root"
  # The server as the image carries it: the product's font catalog beside the
  # program, which is where a Tilo binary finds it outside the image (its apps
  # lay text out, and a host that seeds them has no system fonts).
  cp "$server_bin" "$work/bin/tilo-runtime"
  cp -R "$fonts" "$work/bin/fonts"

  # This Lounge's server configuration: its web UI and packet relay on this
  # check's ports, so two runs beside each other bind their own, and the
  # workspace it founds and keeps of its own.
  local id
  id=$(python3 -c 'import uuid; print(uuid.uuid4())')
  mkdir -p "$volume/lounge"
  printf '%s' "$id" > "$volume/lounge/workspace-id"
  {
    printf 'web_entry = "%s/"\n' "$site_origin"
    printf 'web_listen = "127.0.0.1:%s"\n' "$lounge_web_port"
    printf 'packet_relay = "127.0.0.1:%s"\n' "$lounge_live_port"
    printf '\n[[kept]]\nid = "%s"\nname = "Lounge"\n' "$id"
  } > "$volume/server.toml"

  # The daemon keeps a profile private to its account, and the volume is its
  # profile: a deployment's volume is one.
  chmod 700 "$volume"
  lounge_volume=$volume lounge_bin=$work/bin lounge_home=$work
  # An empty volume is a machine joining a person: a ticket a device of theirs
  # prints. `LOUNGE_TICKET` names one; with none, the check makes the person on a
  # device of its own here and takes the first ticket a server of theirs joins
  # with, as a deployment's operator adds its Lounge.
  if [[ -z ${LOUNGE_TICKET:-} && ! -s $volume/identity/device/cert.bin ]]; then
    person_home=$work/person person_port=$((base + 3)) person_log=$work/person.log
    found_person
    LOUNGE_TICKET=$server_ticket
  fi
  run_lounge
  await_lounge
  workspace=$(cat "$volume/lounge/workspace-id" 2>/dev/null) || lounge_fail 'the Lounge kept no workspace id'
  echo "ok: the Lounge is Ready ($workspace, $((SECONDS - started))s)"

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
  stop_person
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
