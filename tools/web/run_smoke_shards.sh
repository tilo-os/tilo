#!/usr/bin/env bash
# Run the existing mount probes in independent Chromium processes. Each app
# keeps smoke.mjs's foreground activation, observation time and assertions.
set -euo pipefail
base=${1:?usage: run_smoke_shards.sh BASE_URL APP_ID...}
shift
[ "$#" -gt 0 ] || { echo 'at least one app is required' >&2; exit 2; }
: "${CHROME:?CHROME must name the headless Chromium executable}"
: "${RUNNER_TEMP:?RUNNER_TEMP must name the job scratch directory}"
port=${CDP_PORT:-9333}
shards=${TILO_WEB_SMOKE_SHARDS:-2}
[[ "$shards" == 1 || "$shards" == 2 || "$shards" == 3 ]] || { echo 'use one, two or three browser shards' >&2; exit 2; }
[[ "$port" =~ ^[0-9]+$ ]] && ((port > 0 && port + shards - 1 <= 65535)) || { echo 'invalid CDP port' >&2; exit 2; }
apps=("$@")
((shards <= ${#apps[@]})) || shards=${#apps[@]}
profiles=$(mktemp -d "$RUNNER_TEMP/smoke-browsers.XXXXXX")
browsers=()
workers=()
cleanup() {
  # Let the bounded browser supervisors finish after repeated close requests;
  # do not interrupt child waits or replace the completed probes' status.
  trap '' INT TERM
  # Only processes created by this invocation; other job services stay up.
  for pid in "${workers[@]}" "${browsers[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  # Supervisors reap Chromium and its adopted descendants before profiles
  # are removed, including writers which outlive the root browser process.
  for pid in "${workers[@]}"; do
    wait "$pid" 2>/dev/null || true
  done
  local closed=true
  for pid in "${browsers[@]}"; do
    wait "$pid" || closed=false
  done
  "$closed" || { echo 'browser descendants could not be reaped; preserving profiles' >&2; return 1; }
  # Retain bounded retries for filesystem removal after proven process exit.
  for ((attempt=0; attempt<10; attempt++)); do
    if rm -rf -- "$profiles"; then
      return 0
    fi
    sleep .1
  done
  echo 'could not remove browser profiles after child exit' >&2
  return 1
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
for ((shard=0; shard<shards; shard++)); do
  browser_port=$((port + shard))
  python3 tools/web/owned_browser.py "$CHROME" --headless=new --no-sandbox --enable-unsafe-webgpu \
    --enable-features=Vulkan --use-vulkan=swiftshader --use-angle=swiftshader \
    --disable-background-timer-throttling --disable-renderer-backgrounding \
    --disable-backgrounding-occluded-windows \
    --remote-debugging-port="$browser_port" --user-data-dir="$profiles/$shard" \
    about:blank &
  browsers+=("$!")
done
for ((shard=0; shard<shards; shard++)); do
  browser_port=$((port + shard))
  ready=false
  for ((attempt=0; attempt<30; attempt++)); do
    if curl -sf "http://127.0.0.1:$browser_port/json/version" >/dev/null; then
      ready=true
      break
    fi
    sleep 1
  done
  "$ready" || { echo "browser shard $shard failed to start" >&2; exit 1; }
  selected=()
  for ((index=shard; index<${#apps[@]}; index+=shards)); do
    selected+=("${apps[index]}")
  done
  echo "mount smoke shard $shard: ${#selected[@]} apps, CDP $browser_port"
  TILO_WEB_SMOKE_WORKER=1 CDP_PORT="$browser_port" node crates/browser/tilo-web-desktop/scripts/smoke.mjs \
    "$base" "${selected[@]}" &
  workers+=("$!")
done
status=0
# A failed shard must fail the job, while the other shard finishes its probes.
for pid in "${workers[@]}"; do
  wait "$pid" || status=1
done
workers=()
exit "$status"
