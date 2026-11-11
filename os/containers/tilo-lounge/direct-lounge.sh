# The Lounge's own programs run directly, for a test of a page against them:
# the image's entrypoint with the gate's environment, the tilo-relay the image
# carries on its PATH, and the web entry served beside it. No image or micro-VM
# is built around them, so a change to the page or the relay is tried in
# minutes; the Lounge gate (test-integration.sh) still runs the built image.
#
# Sourced, not run: `start_lounge BUNDLE WORK` leaves the Lounge Ready and the
# site served, and sets `relay_origin`, `site_origin`, `room`, `log` and
# `roles` (the roles file the operator declares, which the Lounge re-reads). It
# needs node, chromium, curl, and TILO_RELAY_BIN and TILO_CRYPTO_BIN naming
# built programs, and TILO_FONTS_DIR the product's font catalog. The caller's
# EXIT trap must call `stop_lounge`.

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

start_lounge() {
  local bundle=${1:?start_lounge BUNDLE WORK} work=${2:?start_lounge BUNDLE WORK}
  local relay_bin=${TILO_RELAY_BIN:?TILO_RELAY_BIN names the relay to run}
  local crypto=${TILO_CRYPTO_BIN:?TILO_CRYPTO_BIN names the certificate mint}
  local fonts=${TILO_FONTS_DIR:?TILO_FONTS_DIR names the product font catalog}
  log=$work/lounge.log
  local site_log=$work/site.log
  # The gate's own addresses.
  local http_port=18790 plain_port=19780 live_port=19781 site_port=19782 web_port=9781
  relay_origin=https://127.0.0.1:$web_port
  site_origin=https://127.0.0.1:$site_port

  [[ -f $bundle/build.json ]] || lounge_fail "no desktop bundle at $bundle"
  python3 -I "$lounge_repo/crates/browser/tilo-browser-runtime/container/layout.py" "$bundle" "$work/site"
  "$crypto" generate-tls-fixture 127.0.0.1 "$work/site.ca.crt" "$work/site.key" "$work/site.crt" \
    || lounge_fail 'no certificate for the site'

  # The volume, as a fresh deployment mounts it: empty, with a throwaway owner
  # root among the operator's secrets.
  local volume=$work/volume
  mkdir -p "$volume/secrets" "$work/bin"
  printf 'a%.0s' $(seq 64) > "$volume/secrets/room-root"
  chmod 600 "$volume/secrets/room-root"
  roles=$volume/roles.json
  # The relay as the image carries it: the product's font catalog beside the
  # program, which is where a Tilo binary finds it outside the image (its apps
  # lay text out, and a host that seeds them has no system fonts).
  cp "$relay_bin" "$work/bin/tilo-relay"
  cp -R "$fonts" "$work/bin/fonts"

  # The image's entrypoint, with the environment the gate gives the image.
  # `LOUNGE_ADMISSION` is the door: `open` (the gate's own Lounge) or
  # `invite`, which keeps a wait list (`LOUNGE_WAIT_LIST_MAX`, and the mail
  # settings a test names for its sink).
  env -i HOME="$work" PATH="$work/bin:/usr/bin:/bin" \
    TILO_SOURCE_REVISION=direct-lounge \
    TILO_SECRETS_DIR="$volume/secrets" \
    TILO_ROOMS_STATE="$volume" \
    TILO_ROOMS_PERSISTENT=lounge \
    TILO_ROOMS_NAME=Lounge \
    TILO_ROOMS_DESCRIPTION='The Tilo community: chat, notes and a shared desktop' \
    TILO_ROOMS_DEFAULT_ROLE=editor \
    TILO_ROOMS_ADMISSION="${LOUNGE_ADMISSION:-open}" \
    TILO_ROOMS_WAIT_LIST_MAX="${LOUNGE_WAIT_LIST_MAX:-}" \
    TILO_ROOMS_CONTACT="${LOUNGE_CONTACT:-}" \
    TILO_ROOMS_MAIL_FROM="${LOUNGE_MAIL_FROM:-}" \
    TILO_ROOMS_RESEND_KEY_FILE="${LOUNGE_RESEND_KEY_FILE:-}" \
    TILO_ROOMS_RESEND_URL="${LOUNGE_RESEND_URL:-}" \
    TILO_ROOMS_MAIL_TEMPLATES="${LOUNGE_MAIL_TEMPLATES:-}" \
    TILO_ROOMS_RELAY_ORIGIN="$relay_origin" \
    TILO_ROOMS_LIVE="127.0.0.1:$live_port" \
    TILO_ROOMS_RELAY_PLAIN="127.0.0.1:$plain_port" \
    TILO_ROOMS_HTTP="127.0.0.1:$http_port" \
    TILO_ROOMS_WEB_ENTRY="$site_origin/" \
    TILO_ROOMS_SITE_ORIGINS="$site_origin" \
    TILO_ROOMS_ROOT_FILE="$volume/secrets/room-root" \
    TILO_ROOMS_ROLES_FILE="$roles" \
    TILO_ROOMS_FETCH_PROXY=0 \
    TILO_ROOMS_CHAT_CHANNELS=general \
    TILO_ROOMS_PERSON_STORAGE_GB=10 \
    sh "$lounge_here/entrypoint.sh" >> "$log" 2>&1 &
  local lounge_pid=$!
  lounge_pids+=("$lounge_pid")
  for _ in $(seq 1200); do
    curl -fsS --max-time 2 "http://127.0.0.1:$http_port/mode" >/dev/null 2>&1 && break
    kill -0 "$lounge_pid" 2>/dev/null || lounge_fail 'the Lounge stopped before it was Ready'
    sleep 0.2
  done
  room=$(cat "$volume/lounge/room-id" 2>/dev/null) || lounge_fail 'the Lounge kept no workspace id'
  echo "ok: the Lounge is Ready ($room)"

  # Who the host is, as the operator hands it to the web entry
  # (TILO_WEB_HOME_HOST_INFO), so the page dials it without asking it first.
  local info
  info=$("$relay_bin" page-host "http://127.0.0.1:$plain_port") \
    || lounge_fail 'the relay did not say who it is (tilo-relay page-host)'
  node "$lounge_here/integration/site.mjs" "$work/site" "$work/site.crt" "$work/site.key" \
    "$relay_origin" Lounge "$relay_origin/peer/v1/directory/$room/" "$site_port" "$info" >> "$site_log" 2>&1 &
  local site_pid=$!
  lounge_pids+=("$site_pid")
  for _ in $(seq 300); do
    grep -q "\"url\": *\"$site_origin\"" "$site_log" && break
    kill -0 "$site_pid" 2>/dev/null || lounge_fail "the web entry stopped: $(cat "$site_log")"
    sleep 0.1
  done
  grep -q "\"url\": *\"$site_origin\"" "$site_log" || lounge_fail "the web entry did not start: $(cat "$site_log")"
  echo "ok: the web entry is served at $site_origin"
}
