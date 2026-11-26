# The Lounge's own programs run directly, for a test of a page against them:
# the image's entrypoint with a deployment's environment, the tilo-server the
# image carries on its PATH, and the web entry served beside it. No image or
# micro-VM is built around them, so a change to the page or the server is tried
# in minutes; the image's own test (test-image.sh) boots the built image.
#
# Sourced, not run: `start_lounge BUNDLE WORK` leaves the Lounge Ready and the
# site served, and sets `server_origin`, `site_origin`, `workspace` and `log`.
# It names no owner: a deployment's owners come from TILO_WORKSPACES_OWNERS, and
# a test that needs one starts a stack.sh stack instead.
# `restart_lounge` stops the Lounge alone and starts it again on the volume it
# kept, the site left serving, as a deployment's restart does. It needs node,
# chromium, curl, and TILO_SERVER_BIN and TILO_CRYPTO_BIN naming built programs,
# and TILO_FONTS_DIR the product's font catalog. TILO_LOUNGE_APPS, when set,
# names the apps its workspace runs (tools/ci/publish/lounge-apps.sh): the
# launcher goes beside the server, and the workspace installs the packages at
# its start. The caller's EXIT trap must call `stop_lounge`.
#
# Its addresses are LOUNGE_PORT_BASE (default 19780) and the four ports after
# it, so a second Lounge (another entry, another admission) runs beside the
# first on a base of its own. The site is named LOUNGE_SITE_NAME (default
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
  # `LOUNGE_ADMISSION` is the entry: `open` (the check's own Lounge) or
  # `invite`, which keeps a wait list (`LOUNGE_WAIT_LIST_MAX`, and the mail
  # settings a test names for its sink).
  # `LOUNGE_LOG` is the filter the server and its app workers log by, for a
  # check that needs to read why one did or did not do something: the server
  # reads `TILO_SERVER_LOG` and a worker `RUST_LOG`, both in the same syntax.
  env -i HOME="$lounge_home" PATH="$lounge_bin:/usr/bin:/bin" \
    ${LOUNGE_LOG:+RUST_LOG="$LOUNGE_LOG"} \
    ${lounge_apps:+TILO_RUNTIME_BUNDLE_DIR="$lounge_apps/runtimes"} \
    ${lounge_apps:+TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE="$lounge_apps/runtime-publishers.txt"} \
    ${lounge_apps:+TILO_WORKSPACES_PACKAGES="$lounge_apps/packages"} \
    ${LOUNGE_LOG:+TILO_SERVER_LOG="$LOUNGE_LOG"} \
    TILO_SOURCE_REVISION=direct-lounge \
    TILO_SECRETS_DIR="$lounge_volume/secrets" \
    TILO_WORKSPACES_STATE="$lounge_volume" \
    TILO_WORKSPACES_PERSISTENT=lounge \
    TILO_WORKSPACES_NAME=Lounge \
    TILO_WORKSPACES_DESCRIPTION='The Tilo community: chat, notes and a shared desktop' \
    TILO_WORKSPACES_DEFAULT_ROLE=editor \
    TILO_WORKSPACES_ADMISSION="${LOUNGE_ADMISSION:-open}" \
    TILO_WORKSPACES_WAIT_LIST_MAX="${LOUNGE_WAIT_LIST_MAX:-}" \
    TILO_WORKSPACES_CONTACT="${LOUNGE_CONTACT:-}" \
    TILO_WORKSPACES_MAIL_FROM="${LOUNGE_MAIL_FROM:-}" \
    TILO_WORKSPACES_RESEND_KEY_FILE="${LOUNGE_RESEND_KEY_FILE:-}" \
    TILO_WORKSPACES_RESEND_URL="${LOUNGE_RESEND_URL:-}" \
    TILO_WORKSPACES_MAIL_TEMPLATES="${LOUNGE_MAIL_TEMPLATES:-}" \
    TILO_WORKSPACES_SERVER_ORIGIN="$server_origin" \
    TILO_WORKSPACES_LIVE="127.0.0.1:$lounge_live_port" \
    TILO_WORKSPACES_IROH_LISTEN="127.0.0.1:0" \
    TILO_WORKSPACES_SERVER_PLAIN="127.0.0.1:$lounge_plain_port" \
    TILO_WORKSPACES_HTTP="127.0.0.1:$lounge_http_port" \
    TILO_WORKSPACES_WEB="127.0.0.1:$lounge_web_port" \
    TILO_WORKSPACES_WEB_ENTRY="$site_origin/" \
    TILO_WORKSPACES_SITE_ORIGINS="$site_origin" \
    TILO_WORKSPACES_LOCAL=1 \
    TILO_WORKSPACES_ROOT_FILE="$lounge_volume/secrets/workspace-root" \
    TILO_WORKSPACES_FETCH_PROXY=0 \
    TILO_WORKSPACES_CHAT_CHANNELS=general \
    TILO_WORKSPACES_FEEDBACK_CHANNEL=feedback \
    TILO_WORKSPACES_PERSON_STORAGE_GB=10 \
    sh "$lounge_here/entrypoint.sh" >> "$log" 2>&1 &
  lounge_pid=$!
  lounge_pids+=("$lounge_pid")
}

# Wait for the Lounge run_lounge started to serve, as the deployment's probe
# asks it. Its probe answers from its start: "starting" while it seeds and
# opens its workspaces, then "up". It is waited for while it says it is
# starting and runs; one that does not answer within LOUNGE_ANSWER_SECONDS
# (default 30) fails.
await_lounge() {
  local said deadline=$((SECONDS + ${LOUNGE_ANSWER_SECONDS:-30}))
  until said=$(curl -sS --max-time 2 "http://127.0.0.1:$lounge_http_port/mode" 2>/dev/null) \
    && [[ $said == *'"server":"up"'* ]]; do
    kill -0 "$lounge_pid" 2>/dev/null || lounge_fail 'the Lounge stopped before it was Ready'
    [[ $said == *'"server":"starting"'* ]] || ((SECONDS < deadline)) \
      || lounge_fail "the Lounge did not answer its probe in ${LOUNGE_ANSWER_SECONDS:-30}s"
    sleep 0.1
  done
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
  lounge_plain_port=$base lounge_live_port=$((base + 1)) lounge_web_port=$((base + 3))
  lounge_http_port=$((base + 4))
  local site_port=$((base + 2))
  server_origin=https://127.0.0.1:$lounge_web_port
  site_origin=https://${LOUNGE_SITE_NAME:-127.0.0.1}:$site_port

  [[ -f $bundle/build.json ]] || lounge_fail "no desktop bundle at $bundle"
  python3 -I "$lounge_repo/crates/browser/tilo-browser-runtime/container/layout.py" "$bundle" "$work/site"
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
  cp "$server_bin" "$work/bin/tilo-server"
  cp -R "$fonts" "$work/bin/fonts"
  lounge_apps=
  if [[ -n ${TILO_LOUNGE_APPS:-} ]]; then
    lounge_apps=$TILO_LOUNGE_APPS
    [[ -x $lounge_apps/bin/tilo-launcher && -d $lounge_apps/packages ]] \
      || lounge_fail "no Lounge apps in $lounge_apps"
    cp "$lounge_apps/bin/tilo-launcher" "$work/bin/"
  fi

  lounge_volume=$volume lounge_bin=$work/bin lounge_home=$work
  run_lounge
  await_lounge
  workspace=$(cat "$volume/lounge/workspace-id" 2>/dev/null) || lounge_fail 'the Lounge kept no workspace id'
  echo "ok: the Lounge is Ready ($workspace, $((SECONDS - started))s)"

  # Who the host is, as the operator hands it to the web entry
  # (TILO_WEB_HOME_HOST_INFO), so the page dials it without asking it first.
  local info
  info=$(cat "$lounge_volume/lounge/server/server/web-host.json") \
    || lounge_fail 'the server said nothing of itself (web-host.json)'
  node "$lounge_here/integration/site.mjs" "$work/site" "$work/site.crt" "$work/site.key" \
    "$server_origin" "http://127.0.0.1:$lounge_plain_port" Lounge \
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
