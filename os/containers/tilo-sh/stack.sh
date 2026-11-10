#!/usr/bin/env bash
# tilo.sh on this machine: the Lounge relay, the front door, the web entry
# and (where the machine can) the Linux VM host, from this checkout's own
# images, started with production's entrypoints and the environment
# production's manifests give them (tilo/infra apps/tilo-sh-relay,
# apps/tilo-front-door, apps/tilo-web, apps/tilo-sh-vm-host), with only the
# public names turned into this machine's loopback ports.
#
# Run `stack.sh help` for what it does and how to use it.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)

usage() {
  cat <<EOF
stack.sh: tilo.sh, whole, on this machine, from this checkout's images.

  stack.sh up [--seed] [--discord] [--fresh]
      Start everything and print where it is. Takes the images t built for
      this checkout (t test image-lounge image-front-door image-web-entry,
      then t outputs), so the first run waits for those builds; later runs
      start in seconds. --seed fills the Lounge to its production size
      for entry-speed work: 300 members joined through its door and 200
      chat messages posted through the front door (minutes, once per
      state). --discord starts a fake Discord and points the Lounge's
      Discord sign-in and bridge at it. --fresh wipes the state first (a
      new Lounge, a new door).
  stack.sh env
      The environment that points everything at this stack: eval "\$(stack.sh env)",
      then tilo keys create "My backend", the Python SDK (Tilo()), the TS SDK
      (new Tilo()) and an agent's MCP client all reach the local door.
  stack.sh status        what runs, at which address, and whether it answers
  stack.sh logs NAME     a service's log (relay, tls, door, web, discord, releases)
  stack.sh browser       a Chromium on a fresh profile, opened on the local site
  stack.sh check         bring the stack up and run the end-to-end check on it
                         (check.sh: a startup's visa flow and a fresh visitor's
                         entry time)
  stack.sh down [--wipe] stop it; --wipe also deletes its state

What runs, all on 127.0.0.1 (ports from TILO_SH_PORT_BASE, default 24100):
  relay     the Lounge image (tilo-relay room), as tilo.sh's relay
  tls       production's Caddy in front of it: https://127.0.0.1:BASE
            (eu1.tilo.sh) and https://127.0.0.1:BASE+6 (live-eu1.tilo.sh)
  door      the front door image: MCP, /v1, keys and OAuth,
            http://localhost:BASE+10 (mcp.tilo.sh)
  web       the web entry image: the site, /sign-in, /install.sh, /releases,
            http://localhost:BASE+20 (tilo.sh)
  vm-host   the VM host image (Firecracker): skipped, with the reason. It
            needs root and /dev/kvm, which rootless podman cannot give it,
            and the stack has no root path for it yet
  discord   with --discord, the fake Discord (crates/dev/fake-discord)

Images: TILO_SH_IMAGE_LOUNGE, TILO_SH_IMAGE_DOOR and TILO_SH_IMAGE_WEB name an
OCI archive or a registry reference instead of this checkout's t build (e.g.
the registry.tilo.so/tilo/lounge@sha256:… production pins).
State: TILO_SH_STATE (default \${XDG_STATE_HOME:-~/.local/state}/tilo-sh-local).
Needs podman (rootless is fine), curl, python3, and t for this checkout's images.
Docs: docs/testing-tilo-sh.md.
EOF
}

say() { printf 'tilo-sh: %s\n' "$*" >&2; }
die() { printf 'tilo-sh: %s\n' "$*" >&2; exit 1; }

# -- where everything is -----------------------------------------------------

base=${TILO_SH_PORT_BASE:-24100}
relay_port=$base               # Caddy: the relay's public origin (eu1.tilo.sh)
plain_port=$((base + 1))       # the relay's origin-forwarding listener (9780)
packet_port=$((base + 2))      # the relay's packet relay (8443)
routes_port=$((base + 3))      # the relay's own routes (8790): /mode, /avatars, /discord/callback
iroh_port=$((base + 4))        # the relay's iroh endpoint, UDP (7704)
relay_web_port=$((base + 5))   # the relay's own dev TLS listener, which nothing here uses
live_port=$((base + 6))        # Caddy: the live plane's origin (live-eu1.tilo.sh)
caddy_http_port=$((base + 7))  # Caddy's HTTP port, which serves nothing here
door_port=$((base + 10))       # the front door (mcp.tilo.sh)
site_port=$((base + 20))       # the web entry (tilo.sh)
releases_port=$((base + 22))   # this checkout's tilo command, as the site's /releases serves it
discord_rest_port=$((base + 40))
discord_gateway_port=$((base + 41))
discord_control_port=$((base + 42))

relay_origin=https://127.0.0.1:$relay_port
live_origin=https://127.0.0.1:$live_port
door_origin=http://localhost:$door_port
site_origin=http://localhost:$site_port

state=${TILO_SH_STATE:-${XDG_STATE_HOME:-$HOME/.local/state}/tilo-sh-local}
prefix=tilo-sh-local
commit=$(git -C "$repo" rev-parse HEAD 2>/dev/null || echo unknown)

podman_() { podman "$@"; }
running() { [[ $(podman_ container inspect -f '{{.State.Running}}' "$prefix-$1" 2>/dev/null) == true ]]; }

# -- images ------------------------------------------------------------------

# image NAME CHECK FILE: the image to run for NAME — what TILO_SH_IMAGE_<NAME>
# names, else the archive FILE among t's outputs of CHECK for this checkout,
# downloaded once per commit. Prints a reference podman runs.
image() {
  local name=$1 check=$2 file=$3 override var
  var=TILO_SH_IMAGE_$name
  override=${!var:-}
  if [[ -n $override ]]; then
    if [[ -f $override ]]; then load_archive "$override"; else printf '%s' "$override"; fi
    return
  fi
  local outputs=$state/images/$commit/$check
  if [[ ! -f $outputs/.complete ]]; then
    command -v t >/dev/null || die "no t to build $check for this checkout; or set $var to an image"
    say "fetching $check for $commit from t (it builds it first if no run has)"
    rm -rf "$outputs"
    mkdir -p "$outputs"
    t outputs "$check" --to "$outputs" >&2 || die "t could not give $check's outputs; see t log $check"
    touch "$outputs/.complete"
  fi
  local archive
  archive=$(find "$outputs" -path "*$file" -type f | head -1)
  [[ -n $archive ]] || die "$check's outputs hold no $file (in $outputs)"
  load_archive "$archive"
}

# load_archive PATH: the archive in podman's storage, once per digest.
load_archive() {
  local tag
  tag=localhost/$prefix:$(sha256sum "$1" | cut -c1-16)
  if ! podman_ image exists "$tag"; then
    local loaded
    loaded=$(podman_ load -q -i "$1" | sed -n 's/^Loaded image[s]*: *//p' | tail -1)
    [[ -n $loaded ]] || die "podman could not load $1"
    podman_ tag "$loaded" "$tag"
  fi
  printf '%s' "$tag"
}

# Production's Caddy (apps/tilo-sh-relay/deployment.yaml's tls container).
caddy_image=docker.io/library/caddy@sha256:4c6e91c6ed0e2fa03efd5b44747b625fec79bc9cd06ac5235a779726618e530d

# -- the services -----------------------------------------------------------

# start NAME ARGS…: one service's container, replacing any stopped one of the
# same name. Every container is this stack's own (named tilo-sh-local-*).
start() {
  local name=$1
  shift
  podman_ rm -f "$prefix-$name" >/dev/null 2>&1 || true
  podman_ run -d --name "$prefix-$name" --label tilo-sh-local=1 --log-driver k8s-file \
    --log-opt "path=$state/logs/$name.log" "$@" >/dev/null \
    || die "$name did not start: podman run $*"
}

wait_for() { # WHAT URL SECONDS [CURL ARGS…]
  local what=$1 url=$2 seconds=$3
  shift 3
  local deadline=$((SECONDS + seconds))
  until curl -fsS --max-time 2 "$@" "$url" >/dev/null 2>&1; do
    ((SECONDS < deadline)) || die "$what did not answer at $url within ${seconds}s; stack.sh logs ${what%% *}"
    sleep 0.5
  done
}

# The relay's environment: apps/tilo-sh-relay/deployment.yaml, its names
# turned into this stack's addresses, its secrets into throwaway local ones.
relay_env() {
  local env=(
    TILO_ROOMS_PERSISTENT=lounge
    TILO_ROOMS_NAME=Lounge
    "TILO_ROOMS_DESCRIPTION=The Tilo community: chat, notes and a shared desktop"
    TILO_ROOMS_DEFAULT_ROLE=editor
    TILO_ROOMS_LAYOUT=shared
    "TILO_ROOMS_RELAY_ORIGIN=$relay_origin"
    "TILO_ROOMS_LIVE=127.0.0.1:$packet_port"
    "TILO_ROOMS_IROH_LISTEN=127.0.0.1:$iroh_port"
    "TILO_ROOMS_LIVE_ORIGIN=$live_origin"
    "TILO_ROOMS_WEB_ENTRY=$site_origin/"
    "TILO_ROOMS_SITE_ORIGINS=$site_origin"
    TILO_ROOMS_ROOT_FILE=/run/secrets/room-root-lounge
    TILO_ROOMS_FEEDBACK_CHANNEL=feedback
    # Production's listeners, which its pod binds on all addresses, here on
    # loopback only.
    "TILO_ROOMS_RELAY_PLAIN=127.0.0.1:$plain_port"
    "TILO_ROOMS_HTTP=127.0.0.1:$routes_port"
    "TILO_ROOMS_WEB=127.0.0.1:$relay_web_port"
  )
  # Not here: the moderation and summary model keys (no model is asked
  # locally), and the Discord bridge's real server unless --discord.
  if [[ -n ${discord_api:-} ]]; then
    env+=(
      TILO_ROOMS_DISCORD_CLIENT_ID=1559266815148953650
      TILO_ROOMS_DISCORD_CLIENT_SECRET_FILE=/run/secrets/discord-client-secret
      "TILO_ROOMS_DISCORD_AUTHORIZE_URL=$discord_api/oauth2/authorize"
      "TILO_ROOMS_DISCORD_API_BASE=$discord_api"
      # Production's bridge, on the fake's one server (fake-discord's
      # GUILD), its bot token the fake's (TOKEN) in the secrets.
      "TILO_ROOMS_BOTS=/usr/local/share/tilo/bots/discord-bridge.json guild=1000 server=Tilo mirror-channels=yes"
    )
  fi
  printf '%s\n' "${env[@]}"
}

# The front door's environment: apps/tilo-front-door/deployment.yaml.
door_env() {
  printf '%s\n' \
    "TILO_DOOR_LISTEN=127.0.0.1:$door_port" \
    "TILO_DOOR_ISSUER=$door_origin" \
    "TILO_DOOR_WEB_ENTRY=$site_origin" \
    "TILO_DOOR_RELAY=$relay_origin" \
    TILO_DOOR_STATE=/state/door.json
}

# The web entry's environment: apps/tilo-web/deployment.yaml.
web_env() {
  local room=$1
  printf '%s\n' \
    "TILO_WEB_HOME_HOST=127.0.0.1:$relay_port" \
    TILO_WEB_HOME_NAME=Lounge \
    TILO_WEB_HOME_APP=so.tilo.chat \
    "TILO_WEB_HOME_DOOR=$relay_origin/peer/v1/directory/$room/" \
    TILO_WEB_HOME_FEEDBACK=c_feedback \
    "TILO_WEB_DIRECTORY=$relay_origin/peer/v1/directory" \
    "TILO_WEB_SHELF=http://127.0.0.1:$plain_port" \
    "TILO_RELEASES_ORIGIN=http://127.0.0.1:$releases_port/tilo-cli"
}

# as_user UID: the arguments that run a container as UID, the image's own
# user in production. Under rootless podman this account is UID inside the
# container (keep-id), so the volumes it mounts are already that user's.
as_user() {
  printf -- '--userns\0keep-id:uid=%s,gid=%s\0--user\0%s:%s\0' "$1" "$1" "$1" "$1"
}

env_args() { local line; while IFS= read -r line; do printf -- '--env\0%s\0' "$line"; done; }

# The bundle the door verifies the relay with: this machine's roots and the
# local Caddy's, at the fixed path Tilo's binaries read (production: the
# image's own bundle, and Cloudflare's certificate in front of the relay).
trust_bundle() {
  local root=$state/tls/caddy/pki/authorities/local/root.crt
  local deadline=$((SECONDS + 30))
  until [[ -s $root ]]; do
    ((SECONDS < deadline)) || die "Caddy made no local CA at $root; stack.sh logs tls"
    sleep 0.2
  done
  cp "$root" "$state/local-ca.pem"
  local system
  for system in /etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem /etc/ssl/certs/ca-certificates.crt; do
    [[ -f $system ]] && { cat "$system" "$root" > "$state/trust-bundle.pem"; return; }
  done
  cp "$root" "$state/trust-bundle.pem"
}

# The site's /releases: this checkout's tilo command, where the release
# tooling would publish it (<base>/<version>/<file>), as `latest`. Fetched
# from t in the background, so bring-up never waits on that build: until it
# lands /releases answers 404, and TILO_SH_CLI_DIR names a directory holding
# a built tilo-linux-x86_64.tar.gz instead.
start_releases() {
  local dir=$state/releases/tilo-cli/latest outputs=$state/images/$commit/cli-x86_64
  mkdir -p "$dir"
  if [[ -n ${TILO_SH_CLI_DIR:-} ]]; then
    cp "$TILO_SH_CLI_DIR"/tilo-* "$dir/" || die "TILO_SH_CLI_DIR holds no tilo-* release files"
  elif [[ -f $outputs/.complete ]]; then
    find "$outputs" -type f -name 'tilo-*' -exec cp {} "$dir/" \;
  elif command -v t >/dev/null; then
    ( rm -rf "$outputs" && mkdir -p "$outputs" \
      && t outputs cli-x86_64 --to "$outputs" && touch "$outputs/.complete" \
      && find "$outputs" -type f -name 'tilo-*' -exec cp {} "$dir/" \; ) \
      >> "$state/logs/releases.log" 2>&1 < /dev/null &
    disown
  fi
  [[ -f $state/releases.pid ]] && kill "$(cat "$state/releases.pid")" 2>/dev/null || true
  python3 -m http.server --bind 127.0.0.1 --directory "$state/releases" "$releases_port" \
    >> "$state/logs/releases.log" 2>&1 < /dev/null &
  echo $! > "$state/releases.pid"
}

# Whether the VM host can run here, and if not why: it is privileged in
# production (KVM, its own cgroup tree, a raw block device it formats, a jail
# helper given capabilities), which rootless podman cannot give it.
vm_host_blocker() {
  [[ -e /dev/kvm ]] || { echo 'this machine has no /dev/kvm'; return; }
  echo 'it needs root (a raw block device for its state, cgroup delegation and setcap for its jail helper), which rootless podman cannot give it; shell commands and hosted browsers need it, workspaces do not'
}

# The tilo command to drive the stack with: TILO_SH_TILO_BIN, else this
# checkout's release build the site serves at /releases, else a local build.
find_tilo() {
  if [[ -n ${TILO_SH_TILO_BIN:-} ]]; then printf '%s' "$TILO_SH_TILO_BIN"; return; fi
  local tarball=$state/releases/tilo-cli/latest/tilo-linux-x86_64.tar.gz
  if [[ -s $tarball ]]; then
    mkdir -p "$state/cli"
    tar -xzf "$tarball" -C "$state/cli" tilo && { printf '%s' "$state/cli/tilo"; return; }
  fi
  local candidate
  for candidate in "$repo/target/release/tilo" "$repo/target/debug/tilo"; do
    [[ -x $candidate ]] && { printf '%s' "$candidate"; return; }
  done
  die 'no tilo command: set TILO_SH_TILO_BIN, or build one (rcargo build -p tilo-cli --bin tilo)'
}

# The Python SDK as this checkout has it: uv with its lock where uv is, else
# the sources on the path beside a system httpx (the t check's image).
sdk_python() {
  if command -v uv >/dev/null; then
    uv run --quiet --project "$repo/sdk/python" python "$@"
  else
    PYTHONPATH=$repo/sdk/python/src${PYTHONPATH:+:$PYTHONPATH} python3 "$@"
  fi
}

# The Lounge at production's membership, by the paths people take:
# TILO_SH_SEED_MEMBERS (300) people join through its door (the seed-lounge
# example), and one of them posts TILO_SH_SEED_MESSAGES (200) chat messages
# through the front door with a key of their own. Once per state. Each post
# takes longer as the chat grows (about 5 s at a few hundred), which is why
# the history stops short of production's 3.5 MB of records.
seed() {
  local room=$1 out=$state/seed
  if [[ -f $out/done ]]; then say "the Lounge is already seeded ($(cat "$out/done"))"; return; fi
  mkdir -p "$out"
  local seeder=${TILO_SH_SEEDER:-$repo/target/debug/examples/seed-lounge}
  if [[ ! -x $seeder ]]; then
    say 'building the seeder (rcargo build -p tilo-agent-participant --example seed-lounge)'
    (cd "$repo" && rcargo build -p tilo-agent-participant --example seed-lounge >&2) \
      || die 'could not build the seeder; or set TILO_SH_SEEDER'
  fi
  local members
  members=$(curl -fsS --cacert "$state/local-ca.pem" -X POST "$relay_origin/peer/v1/directory" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["workspaces"][0]["members"])')
  local want=$((${TILO_SH_SEED_MEMBERS:-300} - members))
  if ((want > 0)); then
    say "seeding $want members through the Lounge's door"
    "$seeder" "$relay_origin" "$room" "$want" "$state/local-ca.pem" > "$out/members.json" \
      || die 'seeding the members failed'
  fi
  say 'seeding the history: a member posts chat through the front door'
  node "$here/integration/person.mjs" "$site_origin" "$relay_origin" "$out/poster" \
    --key 'Lounge history' --tilo "$(find_tilo)" --door "$door_origin" > "$out/poster.json" \
    || die "the poster did not get a key (see $out/poster)"
  local key
  key=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["key"])' "$out/poster.json")
  # The relay's records live in its volume, which the container's user owns.
  sdk_python "$here/integration/seed-history.py" "$door_origin" "$key" "$state/relay/lounge/relay" \
    "${TILO_SH_SEED_MESSAGES:-200}" > "$out/history.json" || die 'seeding the history failed'
  printf '%s members, %s\n' "$((members + (want > 0 ? want : 0)))" "$(cat "$out/history.json")" > "$out/done"
  say "seeded: $(cat "$out/done")"
}

up() {
  local seed= discord= fresh=
  for arg in "$@"; do
    case $arg in
      --seed) seed=1 ;;
      --discord) discord=1 ;;
      --fresh) fresh=1 ;;
      -h|--help) usage; return ;;
      *) die "up takes --seed, --discord and --fresh, not $arg" ;;
    esac
  done
  command -v podman >/dev/null || die 'needs podman'
  # Rootless, as on any developer's machine: rootful podman gives the web
  # entry no pasta network, and keep-id is what maps the images' users onto
  # this account's files. In a VM as root, check-in-vm.sh runs it as a user.
  [[ $EUID != 0 ]] || die 'run it as a regular user (rootless podman), not root'
  local began=$SECONDS
  [[ -z $fresh ]] || down --wipe
  mkdir -p "$state"/{logs,relay,secrets,door,tls/caddy,tls/config}
  : > "$state/logs/stack.log"

  local lounge door web
  lounge=$(image LOUNGE image-lounge /lounge/image.oci.tar)
  door=$(image DOOR image-front-door image.oci.tar)
  web=$(image WEB image-web-entry /web-entry/image.oci.tar)
  say "images ready ($((SECONDS - began))s)"

  # The operator's secrets, as the relay's Secret holds them: a throwaway
  # workspace root, made once.
  if [[ ! -s $state/secrets/room-root-lounge ]]; then
    od -An -tx1 -N32 /dev/urandom | tr -d ' \n' > "$state/secrets/room-root-lounge"
    printf 'local-fake-discord-secret' > "$state/secrets/discord-client-secret"
    chmod 0400 "$state/secrets"/*
  fi

  if [[ -n $discord ]]; then
    start_discord
  fi

  # The relay's TLS, first: the door trusts its CA.
  cp "$here/Caddyfile" "$state/tls/Caddyfile"
  local tls_user=()
  mapfile -d '' tls_user < <(as_user 10001)
  start tls --network host --read-only "${tls_user[@]}" \
    --env XDG_DATA_HOME=/data --env XDG_CONFIG_HOME=/config \
    --env "TILO_SH_RELAY_PORT=$relay_port" \
    --env "TILO_SH_LIVE_PORT=$live_port" --env "TILO_SH_PACKET_PORT=$packet_port" \
    --env "TILO_SH_ROUTES_PORT=$routes_port" --env "TILO_SH_PLAIN_PORT=$plain_port" \
--env "TILO_SH_CADDY_HTTP_PORT=$caddy_http_port" \
    -v "$state/tls/Caddyfile:/etc/caddy/Caddyfile:ro,Z" \
    -v "$state/tls/caddy:/data/caddy:Z" -v "$state/tls/config:/config:Z" \
    "$caddy_image" caddy run --config /etc/caddy/Caddyfile --adapter caddyfile

  # The relay: the Lounge image, as the deployment runs it (uid 10001, a
  # read-only root, the state volume, the secrets read-only, a /tmp).
  # The relay reaches its own live origin through Caddy (its endpoint's home
  # relay), so it trusts the local CA as production trusts Cloudflare's: in
  # the bundle at the path every Tilo client, iroh's included, reads.
  trust_bundle
  local relay_args=()
  mapfile -d '' relay_args < <(relay_env | env_args)
  local relay_user=()
  mapfile -d '' relay_user < <(as_user 10001)
  start relay --network host --read-only --tmpfs /tmp "${relay_user[@]}" \
    -v "$state/relay:/run/tilo-rooms:Z" -v "$state/secrets:/run/secrets:ro,Z" \
    -v "$state/trust-bundle.pem:/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem:ro,Z" \
    --env "TILO_SOURCE_REVISION=local-$commit" \
    "${relay_args[@]}" "$lounge"
  wait_for 'relay (its readiness probe, /mode)' "http://127.0.0.1:$routes_port/mode" 300
  wait_for 'tls (the relay through Caddy)' "$relay_origin/mode" 60 --cacert "$state/local-ca.pem"
  local room
  room=$(cat "$state/relay/lounge/room-id") || die 'the relay kept no workspace id'
  say "relay ready: the Lounge is $room ($((SECONDS - began))s)"

  # The front door: uid 65532, its state on its own volume.
  local door_args=()
  mapfile -d '' door_args < <(door_env | env_args)
  local door_user=()
  mapfile -d '' door_user < <(as_user 65532)
  start door --network host --read-only --tmpfs /tmp "${door_user[@]}" \
    -v "$state/door:/state:Z" \
    -v "$state/trust-bundle.pem:/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem:ro,Z" \
    "${door_args[@]}" "$door"
  wait_for 'door (its health check)' "http://127.0.0.1:$door_port/health" 60

  # The web entry: its image listens on 8080 alone, so it gets its own
  # network, published on the site's port, from which it reaches the relay's
  # plain listener (its shelf) and the releases on this machine's loopback.
  start_releases
  local web_args=()
  mapfile -d '' web_args < <(web_env "$room" | env_args)
  start web --network "pasta:-T,$plain_port,-T,$releases_port" -p "127.0.0.1:$site_port:8080" \
    --read-only --tmpfs /tmp --user 10001 \
    "${web_args[@]}" "$web"
  wait_for 'web (its health check)' "$site_origin/healthz" 60

  local blocker
  blocker=$(vm_host_blocker)
  say "vm-host skipped: $blocker"

  printf '%s\n' "$((SECONDS - began))" > "$state/bring-up-seconds"
  if [[ -n $seed ]]; then
    seed "$room"
  fi
  say "up in $((SECONDS - began))s"
  status
}

start_discord() {
  local bin=${TILO_SH_FAKE_DISCORD:-}
  if [[ -z $bin ]]; then
    bin=$repo/target/debug/fake-discord
    if [[ ! -x $bin ]]; then
      say 'building the fake Discord (t build -p fake-discord)'
      (cd "$repo" && t build -p fake-discord >&2) || die 'could not build fake-discord; or set TILO_SH_FAKE_DISCORD'
    fi
  fi
  [[ -f $state/discord.pid ]] && kill "$(cat "$state/discord.pid")" 2>/dev/null || true
  "$bin" --rest "127.0.0.1:$discord_rest_port" --gateway "127.0.0.1:$discord_gateway_port" \
    --control "127.0.0.1:$discord_control_port" > "$state/logs/discord.log" 2>&1 &
  echo $! > "$state/discord.pid"
  local deadline=$((SECONDS + 20))
  until grep -q '^DISCORD_API_BASE=' "$state/logs/discord.log" 2>/dev/null; do
    ((SECONDS < deadline)) || die "the fake Discord did not start: $(cat "$state/logs/discord.log")"
    sleep 0.2
  done
  discord_api=http://127.0.0.1:$discord_rest_port
  if [[ ! -s $state/secrets/discord-bot-token ]]; then
    chmod u+w "$state/secrets"
    printf 'fake.bot.token' > "$state/secrets/discord-bot-token"
    chmod 0400 "$state/secrets/discord-bot-token"
  fi
}

down() {
  local wipe=
  [[ ${1:-} == --wipe ]] && wipe=1
  local name
  for name in web door relay tls; do
    podman_ rm -f -t 10 "$prefix-$name" >/dev/null 2>&1 || true
  done
  for pid in releases discord; do
    [[ -f $state/$pid.pid ]] && { kill "$(cat "$state/$pid.pid")" 2>/dev/null || true; rm -f "$state/$pid.pid"; }
  done
  if [[ -n $wipe ]]; then
    # The volumes belong to the containers' users (10001, 65532) inside this
    # account's user namespace: only that namespace removes them.
    podman_ unshare rm -rf "$state/relay" "$state/door" "$state/tls" "$state/secrets" "$state/seed" 2>/dev/null \
      || rm -rf "$state/relay" "$state/door" "$state/tls" "$state/secrets" "$state/seed"
    say "stopped, and its state is gone (images kept in $state/images)"
  else
    say "stopped; its state stays in $state (stack.sh down --wipe deletes it)"
  fi
}

status() {
  local svc url
  printf '%-9s %-8s %s\n' service state address
  for svc in "tls|$relay_origin/mode" "relay|http://127.0.0.1:$routes_port/mode" \
    "door|http://127.0.0.1:$door_port/health" "web|$site_origin/healthz"; do
    local name=${svc%%|*} probe=${svc#*|} shown answer
    case $name in
      tls) shown="$relay_origin (relay), $live_origin (live plane)" ;;
      relay) shown="the Lounge, behind tls" ;;
      door) shown="$door_origin (MCP: $door_origin/mcp)" ;;
      web) shown="$site_origin" ;;
    esac
    if running "$name" && curl -fsS --max-time 2 --cacert "$state/local-ca.pem" "$probe" >/dev/null 2>&1; then
      answer=up
    elif running "$name"; then answer=starting; else answer=down; fi
    printf '%-9s %-8s %s\n' "$name" "$answer" "$shown"
  done
  printf '%-9s %-8s %s\n' vm-host skipped "$(vm_host_blocker)"
  if [[ -f $state/discord.pid ]] && kill -0 "$(cat "$state/discord.pid")" 2>/dev/null; then
    printf '%-9s %-8s %s\n' discord up "http://127.0.0.1:$discord_rest_port (control http://127.0.0.1:$discord_control_port)"
  fi
  printf '\nNext: eval "$(%s env)", then tilo keys create "My backend"; or %s browser.\n' "$0" "$0"
}

print_env() {
  [[ -f $state/local-ca.pem ]] || die 'the stack has not run here yet: stack.sh up'
  cat <<EOF
# tilo.sh on this machine (stack.sh). The tilo command, the SDKs and agents
# reach the local door; nothing here touches tilo.sh.
export TILO_DOOR=$door_origin
export TILO_SITE=$site_origin
# The relay's TLS is the local Caddy's CA: the tilo command (tilo login),
# Node (the TS SDK, MCP clients) and Python's httpx (the Python SDK) trust it.
export TILO_TEST_CA_FILE=$state/local-ca.pem
export NODE_EXTRA_CA_CERTS=$state/local-ca.pem
export SSL_CERT_FILE=$state/trust-bundle.pem
# An agent's MCP client, e.g. Claude Code:
#   claude mcp add --transport http tilo-local $door_origin/mcp
# a one-workspace key's MCP: $door_origin/w/<workspace>/mcp with
#   --header "Authorization: Bearer <token>"
EOF
}

browser() {
  local chrome=${CHROME:-}
  if [[ -z $chrome ]]; then
    chrome=$(ls -d "$HOME"/.cache/ms-playwright/chromium-*/chrome-linux*/chrome 2>/dev/null | sort -V | tail -1)
  fi
  [[ -x $chrome ]] || die 'no Chromium: set CHROME, or install Playwright'"'"'s (bunx playwright install chromium)'
  local profile
  profile=$(mktemp -d "${XDG_RUNTIME_DIR:-/tmp}/tilo-sh-browser.XXXXXX")
  say "a fresh visitor on $site_origin (profile $profile; the relay's local certificate is accepted)"
  "$chrome" --user-data-dir="$profile" --no-first-run --ignore-certificate-errors "$site_origin" "${@}"
}

# Sourced (check.sh does), it only defines the addresses and functions above.
[[ ${BASH_SOURCE[0]} == "$0" ]] || return 0
cmd=${1:-help}
shift || true
case $cmd in
  up) up "$@" ;;
  down) down "$@" ;;
  status) status ;;
  env) print_env ;;
  logs) [[ -n ${1:-} ]] || die 'logs NAME: relay, tls, door, web, discord, releases'; cat "$state/logs/$1.log" ;;
  browser) browser "$@" ;;
  check) exec bash "$here/check.sh" "$@" ;;
  help|-h|--help) usage ;;
  *) usage >&2; exit 2 ;;
esac
