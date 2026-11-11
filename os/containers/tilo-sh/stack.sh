#!/usr/bin/env bash
# tilo.sh on this machine: the Lounge relay, the front door, the web entry
# and (where the machine can) the Linux VM host, from this checkout, started
# with production's entrypoints and the environment production's manifests
# give them (tilo/infra apps/tilo-sh-relay, apps/tilo-front-door,
# apps/tilo-web, apps/tilo-sh-vm-host), with only the public names turned
# into this machine's loopback ports. By default the programs t builds run as
# plain processes; `up --images` runs the release images instead.
#
# Run `stack.sh help` for what it does and how to use it.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)

usage() {
  cat <<EOF
stack.sh: tilo.sh, whole, on this machine, from this checkout.

  stack.sh up [--images] [--seed] [--discord] [--fresh]
      Start everything and print where it is. By default this checkout's
      programs, built by t (t build: cached and incremental, so only what a
      change touches compiles), run as plain processes, each with its own
      state and runtime directory under the stack's state; the first run
      also builds the web bundle and fetches the font catalog. Each takes
      its environment from the same place its image does: the image's
      entrypoint and ENV, and the manifest's variables below.
      --images runs the release images instead (t test image-lounge
      image-front-door image-web-entry, then t outputs): the rehearsal of
      what ships, which t's tilo-sh-local check runs; the first run on a
      commit waits for those image builds.
      --seed fills the Lounge to its production size for entry-speed work:
      300 members joined through its door and 200 chat messages posted
      through the front door (minutes, once per state). --discord starts a
      fake Discord and points the Lounge's Discord sign-in and bridge at
      it. --fresh wipes the state first (a new Lounge, a new door).
  stack.sh restart NAME  after a code change: rebuild NAME and start it again,
                         the rest left running (relay, door, web, tls,
                         discord, releases; not with --images). The web
                         rebuilds its bundle, the others their programs
  stack.sh env
      The environment that points everything at this stack: eval "\$(stack.sh env)",
      then tilo keys create "My backend", the Python SDK (Tilo()), the TS SDK
      (new Tilo()) and an agent's MCP client all reach the local door.
  stack.sh status        what runs, at which address, and whether it answers
  stack.sh logs NAME     a service's log (relay, tls, door, web, discord, releases)
  stack.sh browser       a Chromium on a fresh profile, opened on the local site
  stack.sh check [--images]
                         bring the stack up and run the end-to-end check on it
                         (check.sh: a startup's visa flow and a fresh visitor's
                         entry time)
  stack.sh down [--wipe] stop it; --wipe also deletes its state

What runs, all on 127.0.0.1 (ports from TILO_SH_PORT_BASE, default 24100):
  relay     tilo-relay room through the Lounge image's entrypoint, as tilo.sh's
            relay
  tls       production's Caddy and Caddyfile in front of it:
            https://127.0.0.1:BASE (eu1.tilo.sh) and https://127.0.0.1:BASE+6
            (live-eu1.tilo.sh)
  door      the front door: MCP, /v1, keys and OAuth,
            http://localhost:BASE+10 (mcp.tilo.sh)
  web       the web entry (its entrypoint and Caddyfile serving the desktop
            bundle): the site, /sign-in, /install.sh, /releases,
            http://localhost:BASE+20 (tilo.sh)
  vm-host   the VM host image (Firecracker): a workspace's Linux programs and
            its Browser run in its VM here, and the relay places it on this
            host. It is privileged, so it runs as root over its own state
            (TILO_SH_VM_HOST_STATE, /var/lib/tilo-sh-local-vm) through sudo;
            skipped, with the reason, where /dev/kvm or root is missing
            (with --images; natively it is skipped, with that reason: it runs
            from its image)
  discord   with --discord, the fake Discord (crates/dev/fake-discord)

Natively: the programs are t's lounge-gate build (the release's code and
opt-level without its whole-program link) in ./target/lounge-gate, built by
t, or by the cargo drop-in TILO_SH_CARGO names; Caddy is
the one on PATH, else a pinned release fetched once. The relay and the door
trust the local Caddy's CA through the fixed bundle path Tilo's programs read,
which bwrap (bubblewrap) puts in front of this machine's own for those
processes alone. Nothing runs as root, and no process sees this session's
D-Bus or runtime directory. TILO_SH_BUNDLE names a built desktop bundle
(build.sh --bundle desktop) instead of building one.
Images: TILO_SH_IMAGE_LOUNGE, TILO_SH_IMAGE_DOOR and TILO_SH_IMAGE_WEB name an
OCI archive or a registry reference instead of this checkout's t build (e.g.
the registry.tilo.so/tilo/lounge@sha256:… production pins).
State: TILO_SH_STATE (default \${XDG_STATE_HOME:-~/.local/state}/tilo-sh-local).
Needs t, bwrap, curl, python3, wasm-bindgen and wasm-opt (for the bundle);
with --images, podman (rootless is fine) instead of bwrap and the toolchain.
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
link_port=$((base + 8))        # the relay's WebTransport listener, UDP: how pages reach it
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

native=$state/native   # what the native stack builds and lays out
runs=$state/run         # each native process's runtime directory and pid
# The programs t builds for it: the release's code and opt-level without the
# whole-program link (Cargo.toml's lounge-gate, which the Lounge's own checks
# run), so a change relinks in a minute instead of LTO's many.
profile='lounge-gate'
programs=$repo/target/$profile
# The one trust bundle Tilo's programs read (crates/system/tilo-native-certs).
ca_path=/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem

# Where each service finds its files, as it sees them: the image's paths
# (the volumes the deployment mounts), or under the state natively.
use_mode() {
  mode=$1
  if [[ $mode == native ]]; then
    secrets_path=$state/secrets
    bots_path=$native/lounge/bots
    door_state=$state/door
  else
    secrets_path=/run/secrets
    bots_path=/usr/local/share/tilo/bots
    door_state=/state
  fi
}
use_mode "$(cat "$state/mode" 2>/dev/null || echo native)"

podman_() { podman "$@"; }
running() {
  if [[ $mode == native ]]; then
    [[ -f $runs/$1.pid ]] && kill -0 "$(cat "$runs/$1.pid")" 2>/dev/null
  else
    [[ $(podman_ container inspect -f '{{.State.Running}}' "$prefix-$1" 2>/dev/null) == true ]]
  fi
}

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
  archive=$(image_archive "$check" "$file")
  [[ -n $archive ]] || die "$check's outputs hold no $file (in $outputs)"
  load_archive "$archive"
}

# image_archive CHECK FILE: the one file of CHECK's outputs this state has fetched,
# wherever t's output layout put it.
image_archive() {
  find "$state/images/$commit/$1" -path "*$2" -type f 2>/dev/null | head -1
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
    "TILO_ROOMS_ROOT_FILE=$secrets_path/room-root-lounge"
    TILO_ROOMS_FEEDBACK_CHANNEL=feedback
    # Production's listeners, which its pod binds on all addresses, here on
    # loopback only.
    "TILO_ROOMS_RELAY_PLAIN=127.0.0.1:$plain_port"
    "TILO_ROOMS_HTTP=127.0.0.1:$routes_port"
    "TILO_ROOMS_WEB=127.0.0.1:$relay_web_port"
    "TILO_ROOMS_LINK=127.0.0.1:$link_port"
    "TILO_ROOMS_LINK_ORIGIN=https://127.0.0.1:$link_port"
  )
  # Not here: the moderation and summary model keys (no model is asked
  # locally), and the Discord bridge's real server unless --discord.
  if [[ -n ${discord_api:-} ]]; then
    env+=(
      TILO_ROOMS_DISCORD_CLIENT_ID=1559266815148953650
      "TILO_ROOMS_DISCORD_CLIENT_SECRET_FILE=$secrets_path/discord-client-secret"
      "TILO_ROOMS_DISCORD_AUTHORIZE_URL=$discord_api/oauth2/authorize"
      "TILO_ROOMS_DISCORD_API_BASE=$discord_api"
      # Production's bridge, on the fake's one server (fake-discord's
      # GUILD), its bot token the fake's (TOKEN) in the secrets.
      "TILO_ROOMS_BOTS=$bots_path/discord-bridge.json guild=1000 server=Tilo mirror-channels=yes"
    )
  fi
  # A workspace's programs and its Browser run in its VM on this stack's own
  # VM host, when it is up: where to place a workspace, which device holds it,
  # and the ledger its page actions are counted in.
  if [[ $mode == images ]] && vm_host_running; then
    local line
    while IFS= read -r line; do env+=("$line"); done < <(vm_host env)
  fi
  printf '%s\n' "${env[@]}"
}

# Why the stack runs no VM host here: what this machine lacks, or, for the
# native stack, that the VM host runs from its image (up --images).
vm_skip_reason() {
  local blocker
  blocker=$(vm_host_blocker)
  if [[ -n $blocker ]]; then
    printf '%s' "$blocker"
  elif [[ $mode != images ]]; then
    printf '%s' 'it runs from its image, as root: stack.sh up --images starts it'
  fi
}

# Whether this stack's VM host is up.
vm_host_running() { [[ -n $(vm_host status 2>/dev/null | sed -n 's/^tilo-sh-vm-host-0 *running$/running/p') ]]; }

# The front door's environment: apps/tilo-front-door/deployment.yaml.
door_env() {
  printf '%s\n' \
    "TILO_DOOR_LISTEN=127.0.0.1:$door_port" \
    "TILO_DOOR_ISSUER=$door_origin" \
    "TILO_DOOR_WEB_ENTRY=$site_origin" \
    "TILO_DOOR_RELAY=$relay_origin" \
    "TILO_DOOR_STATE=$door_state/door.json"
}

# The web entry's environment: apps/tilo-web/deployment.yaml. INFO is what
# page_host printed.
web_env() {
  local room=$1 info=$2
  printf '%s\n' \
    "TILO_WEB_HOME_HOST=127.0.0.1:$relay_port" \
    "TILO_WEB_HOME_HOST_INFO=$info" \
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

# The VM host's own state, which is root's (a raw block device it formats,
# its jails and its VMs), apart from this stack's.
vm_state=${TILO_SH_VM_HOST_STATE:-/var/lib/tilo-sh-local-vm}

# Whether the VM host can run here, and if not why: it is privileged in
# production (KVM, its own cgroup tree, a raw block device it formats, a jail
# helper given capabilities), which rootless podman cannot give it, so it runs
# as root over its own state, through sudo.
vm_host_blocker() {
  [[ -e /dev/kvm ]] || { echo 'this machine has no /dev/kvm'; return; }
  if [[ $EUID != 0 ]] && ! sudo -n true 2>/dev/null; then
    echo 'it needs root (a raw block device for its state, cgroup delegation and setcap for its jail helper) and sudo here asks for a password; run: sudo -E env TILO_SH_STATE=$vm_state bash os/containers/tilo-sh/vm-host.sh up'
    return
  fi
  command -v t >/dev/null || { [[ -n ${TILO_SH_IMAGE_VM_HOST:-} && -n ${TILO_SH_HOSTED_GUEST:-} ]] || { echo 't is not here to build its images, and TILO_SH_IMAGE_VM_HOST and TILO_SH_HOSTED_GUEST name none'; return; }; }
}

# vm-host RUN…: os/containers/tilo-sh/vm-host.sh, as root when this stack is
# not, over the VM host's own state; the images t built for this checkout, and
# the relay this stack runs (its device, its root, the secrets it mounts).
# A caller that names the images itself (t's checks hand them their outputs)
# has them used as named, archive or directory.
vm_host() {
  local sudo=()
  [[ $EUID == 0 ]] || sudo=(sudo -n)
  # The enrolment program the caller named, else the one `up` settled on, which
  # it leaves in the state: a later `vm_host env` reads the relay's device with
  # it, in a shell that has nothing else of `up`.
  local enrol=${enroll:-${TILO_SH_TILO_ENROLL:-$(cat "$state/vm-host-enroll" 2>/dev/null || true)}}
  "${sudo[@]}" env \
    TILO_SH_STATE="$vm_state" \
    "TILO_SH_IMAGE_VM_HOST=${TILO_SH_IMAGE_VM_HOST:-$(image_archive image-vm-host image.oci.tar)}" \
    "TILO_SH_HOSTED_GUEST=${TILO_SH_HOSTED_GUEST:-$state/images/$commit/image-hosted-guest}" \
    "TILO_SH_TILO_ENROLL=$enrol" \
    "TILO_SH_RELAY_SECRETS=$state/secrets" \
    "TILO_SH_RELAY_ROOT=$state/secrets/room-root-lounge" \
    "TILO_SH_RELAY_STATE=$state/relay/lounge/relay" \
    "TILO_SH_RELAY_IROH=127.0.0.1:$iroh_port" \
    "TILO_SH_BROWSER_FREE_ACTIONS=${TILO_SH_BROWSER_FREE_ACTIONS:-}" \
    bash "$here/vm-host.sh" "$@"
}

# tilo-enroll, which makes the VM host's member device: this checkout's.
find_enroll() {
  if [[ -n ${TILO_SH_TILO_ENROLL:-} ]]; then printf '%s' "$TILO_SH_TILO_ENROLL"; return; fi
  local candidate=${CARGO_TARGET_DIR:-$repo/target}/debug/tilo-enroll
  if [[ ! -x $candidate ]]; then
    say 'building tilo-enroll (cargo.sh build -p tilo-relay --bin tilo-enroll)'
    (cd "$repo" && "$here/cargo.sh" build -p tilo-relay --bin tilo-enroll >&2) \
      || die 'could not build tilo-enroll; or set TILO_SH_TILO_ENROLL'
  fi
  printf '%s' "$candidate"
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
  die 'no tilo command: set TILO_SH_TILO_BIN, or build one (os/containers/tilo-sh/cargo.sh build -p tilo-cli --bin tilo)'
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
# through the front door with a key of their own. Once per state, and as many
# as the chat should hold: what a post costs must not grow with what the chat
# already holds, and `seed-history.py` prints how long each one took as it
# goes, so a dearer last post than first is a regression to chase rather than
# a reason to seed less.
seed() {
  local room=$1 out=$state/seed
  if [[ -f $out/done ]]; then say "the Lounge is already seeded ($(cat "$out/done"))"; return; fi
  mkdir -p "$out"
  local seeder=${TILO_SH_SEEDER:-$repo/target/debug/examples/seed-lounge}
  if [[ ! -x $seeder ]]; then
    say 'building the seeder (cargo.sh build -p tilo-agent-participant --example seed-lounge)'
    (cd "$repo" && "$here/cargo.sh" build -p tilo-agent-participant --example seed-lounge >&2) \
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

# -- the native stack -------------------------------------------------------
#
# The same services as plain processes: the programs t builds for this
# checkout, each started the way its image starts it (the image's entrypoint
# where it has one), with the image's ENV and the manifest's variables
# (relay_env, door_env, web_env), only the paths that live in the image
# moved under the stack's state.

# containerfile_env FILE: the ENV the final stage of FILE sets, one NAME=VALUE
# per line, its ${SOURCE_REVISION} this checkout's.
containerfile_env() {
  python3 -I - "$1" "local-$commit" <<'EOF'
import shlex, sys
path, revision = sys.argv[1], sys.argv[2]
instructions, current = [], ""
for line in open(path):
    if line.strip().startswith("#"):
        continue  # comments, also those inside a continued instruction
    current += line.rstrip("\n")
    if current.endswith("\\"):
        current = current[:-1] + " "
        continue
    if current.strip():
        instructions.append(current.strip())
    current = ""
stage = []
for instruction in instructions:
    word = instruction.split(None, 1)[0].upper()
    if word == "FROM":
        stage = []
    elif word == "ENV":
        stage.append(instruction.split(None, 1)[1])
for env in stage:
    for pair in shlex.split(env):
        name, _, value = pair.partition("=")
        print(f"{name}={value.replace('${SOURCE_REVISION}', revision)}")
EOF
}

# image_env_here FILE NAME=PATH…: FILE's ENV for a process outside the image,
# each variable naming a path in the image given the path named here. One that
# names an image path this list does not move stops the start: the native
# stack has to be told where that is on this machine, not left to guess.
image_env_here() {
  local file=$1 line name value moved pair
  shift
  while IFS= read -r line; do
    name=${line%%=*} value=${line#*=} moved=
    for pair in "$@"; do
      [[ ${pair%%=*} == "$name" ]] && { moved=${pair#*=}; break; }
    done
    if [[ -n $moved ]]; then
      printf '%s=%s\n' "$name" "$moved"
    elif [[ $value == /* ]]; then
      die "$file sets $name=$value, a path in the image; stack.sh's native mode has to say where that is here"
    else
      printf '%s\n' "$line"
    fi
  done < <(containerfile_env "$file")
}

# Caddy: production's tls container and the web entry image both run it. The
# one on PATH, else this release, fetched once and checked against its
# published SHA-512 (the web entry image's version).
caddy_version=2.11.7
caddy_sha512=a7a433a1b133efc3c8d10eb0b99d52a24b5ef5c322dc77f5282182b1c0402139ab83f3a99f0c52409df77d20123fb0b523edad8a66d8f5e49136197bf61ef0e7
find_caddy() {
  if [[ -n ${TILO_SH_CADDY:-} ]]; then printf '%s' "$TILO_SH_CADDY"; return; fi
  if command -v caddy >/dev/null; then command -v caddy; return; fi
  local dir=$native/caddy-$caddy_version
  if [[ ! -x $dir/caddy ]]; then
    say "fetching Caddy $caddy_version (no caddy on PATH)"
    local fetch=${native:?}/caddy-fetch
    rm -rf "${fetch:?}"
    mkdir -p "$fetch" "$dir"
    curl -fsSL -o "$fetch/caddy.tar.gz" \
      "https://github.com/caddyserver/caddy/releases/download/v$caddy_version/caddy_${caddy_version}_linux_amd64.tar.gz" \
      || die 'could not fetch Caddy; install one (dnf install caddy) or set TILO_SH_CADDY'
    printf '%s  %s\n' "$caddy_sha512" "$fetch/caddy.tar.gz" | sha512sum -c --quiet - >&2 \
      || die "the fetched Caddy is not release $caddy_version's"
    tar -xzf "$fetch/caddy.tar.gz" -C "$dir" caddy
    rm -rf "${fetch:?}"
  fi
  printf '%s' "$dir/caddy"
}

# The programs, for every service at once: one build, so the features Cargo
# unifies are the same each time and a change relinks only what it touches.
build_programs() {
  local began=$SECONDS
  say "building the programs (${TILO_SH_CARGO:-t} build --profile $profile; only what changed compiles)"
  (cd "$repo" && "$here/cargo.sh" build --profile "$profile" --features tilo-relay/agents \
    -p tilo-relay -p discord-worker -p tilo-front-door -p fake-discord -p tilo-cli \
    --bin tilo-relay --bin discord-worker --bin tilo-front-door --bin fake-discord --bin tilo >&2) \
    || die "the build failed (above); if it built but did not bring the programs back, TILO_SH_CARGO names another cargo drop-in to build with"
  local program
  for program in tilo-relay discord-worker tilo-front-door fake-discord tilo; do
    [[ -x $programs/$program ]] || die "the build left no $programs/$program; TILO_SH_CARGO names another cargo drop-in to build with"
  done
  say "programs built ($((SECONDS - began))s)"
}

# The relay's files as the image's context lays them out (lounge-context.sh,
# the image's own build step): the programs, the bots, the app surfaces from
# the apps' history and the product font catalog (lounge-fonts.sh, once).
lay_out_lounge() {
  if [[ ! -f $native/fonts/manifest.toml ]]; then
    say 'fetching the product font catalog (tools/ci/publish/lounge-fonts.sh, once)'
    bash "$repo/tools/ci/publish/lounge-fonts.sh" "$native/fonts" >&2 || die 'could not prepare the fonts'
  fi
  bash "$repo/tools/ci/publish/lounge-context.sh" "$native/lounge" "$programs" "$native/fonts" >/dev/null \
    || die 'could not lay out the relay (lounge-context.sh)'
  # The image installs the catalog at /usr/share/fonts/tilo; outside it a Tilo
  # program finds it in fonts/ beside itself.
  ln -sfn ../fonts "$native/lounge/bin/fonts"
}

# The desktop bundle the web entry serves (build.sh --bundle desktop, the
# web-desktop-bundle check's build), its compile on t; TILO_SH_BUNDLE names a
# built one instead. Built into a new directory and swapped in.
build_bundle() {
  if [[ -n ${TILO_SH_BUNDLE:-} ]]; then return; fi
  local began=$SECONDS next=${native:?}/bundle.next
  say "building the desktop bundle (build.sh --bundle desktop, compiled by ${TILO_SH_CARGO:-t})"
  rm -rf "${next:?}"
  CARGO=$here/cargo.sh "$repo/crates/browser/tilo-browser-runtime/scripts/build.sh" "$next" --bundle desktop >&2 \
    || die 'the desktop bundle did not build (above)'
  rm -rf "${native:?}/bundle"
  mv "$next" "$native/bundle"
  say "bundle built ($((SECONDS - began))s)"
}

# The site as the image holds it: laid out by layout.py, precompressed by
# compress.sh, the installer beside it (web-entry-context.sh, Containerfile).
lay_out_web() {
  local bundle=${TILO_SH_BUNDLE:-$native/bundle} container=$repo/crates/browser/tilo-browser-runtime/container
  [[ -f $bundle/build.json ]] || die "no desktop bundle at $bundle"
  local stamp
  stamp=$(sha256sum "$bundle/build.json" | cut -c1-16)
  if [[ $(cat "$native/web/stamp" 2>/dev/null) != "$stamp" ]]; then
    rm -rf "${native:?}/web"
    mkdir -p "$native/web/install"
    python3 -I "$container/layout.py" "$bundle" "$native/web/site" >&2 || die 'layout.py failed'
    if command -v brotli >/dev/null && command -v zstd >/dev/null; then
      local began=$SECONDS
      sh "$container/compress.sh" "$native/web/site" || die 'compress.sh failed'
      say "site precompressed ($((SECONDS - began))s)"
    else
      say 'no brotli or zstd here: the site is served uncompressed (the image serves it precompressed)'
    fi
    printf '%s\n' "$stamp" > "$native/web/stamp"
  fi
  mkdir -p "$native/web/install"
  cp "$repo/dist/install/tilo.sh" "$native/web/install/install.sh"
}

# The site's /releases: this checkout's tilo command, packed as a release packs
# it (cli-release.sh pack).
lay_out_releases() {
  local dir=$state/releases/tilo-cli/latest
  mkdir -p "$dir"
  bash "$repo/tools/ci/publish/cli-release.sh" pack "$programs/tilo" "$dir/tilo-linux-x86_64.tar.gz" \
    || die 'could not pack the tilo command'
}

# private_env NAME PATH: a home, runtime, config, data, state, cache and
# temporary directory of NAME's own under $runs/NAME, and its PATH, as env
# lines. run_native gives a service nothing else of this login's
# environment, so none reaches this session's D-Bus, runtime directory or
# settings.
private_env() {
  local home=${runs:?}/$1
  rm -rf "${home:?}/tmp"
  mkdir -p "$home"/{home,config,data,state,cache,tmp}
  chmod 0700 "$home"
  printf '%s\n' "HOME=$home/home" "XDG_RUNTIME_DIR=$home" "XDG_CONFIG_HOME=$home/config" \
    "XDG_DATA_HOME=$home/data" "XDG_STATE_HOME=$home/state" "XDG_CACHE_HOME=$home/cache" \
    "TMPDIR=$home/tmp" "PATH=$2:/usr/bin:/bin" LANG=C.UTF-8
  [[ -z ${RUST_LOG:-} ]] || printf 'RUST_LOG=%s\n' "$RUST_LOG"
  [[ -z ${RUST_BACKTRACE:-} ]] || printf 'RUST_BACKTRACE=%s\n' "$RUST_BACKTRACE"
}

# run_native NAME DIR COMMAND…: COMMAND as the service NAME, in a session of
# its own, with only the environment $runs/NAME.env sets. bwrap puts the
# stack's trust bundle (this machine's roots and the local Caddy's) at the one
# path Tilo's programs read, for this process alone, as the containers mount
# it; everything else it sees is this machine as it is.
run_native() {
  local name=$1 dir=$2
  shift 2
  stop_native "$name"
  local envs=()
  mapfile -t envs < "$runs/$name.env"
  setsid env -i "${envs[@]}" \
    bwrap --dev-bind / / --ro-bind "$state/trust-bundle.pem" "$ca_path" --chdir "$dir" -- "$@" \
    >> "$state/logs/$name.log" 2>&1 < /dev/null &
  echo $! > "$runs/$name.pid"
}

stop_native() {
  local pidfile=$runs/$1.pid pid
  [[ -f $pidfile ]] || return 0
  pid=$(cat "$pidfile")
  if kill -0 "$pid" 2>/dev/null; then
    kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
    local deadline=$((SECONDS + 10))
    while kill -0 "$pid" 2>/dev/null && ((SECONDS < deadline)); do sleep 0.2; done
    kill -KILL -- "-$pid" 2>/dev/null || true
  fi
  rm -f "$pidfile"
}

start_native_tls() {
  local caddy
  caddy=$(find_caddy)
  cp "$here/Caddyfile" "$state/tls/Caddyfile"
  # The tls container's environment: the Caddyfile's ports, and Caddy's data
  # (its local CA) and config where the container mounts them.
  { private_env tls "$(dirname "$caddy")"
    printf '%s\n' "XDG_DATA_HOME=$state/tls" "XDG_CONFIG_HOME=$state/tls/config" \
      "TILO_SH_RELAY_PORT=$relay_port" "TILO_SH_LIVE_PORT=$live_port" "TILO_SH_PACKET_PORT=$packet_port" \
      "TILO_SH_ROUTES_PORT=$routes_port" "TILO_SH_PLAIN_PORT=$plain_port" "TILO_SH_CADDY_HTTP_PORT=$caddy_http_port"
  } > "$runs/tls.env"
  # Caddy makes the CA the bundle takes in, so it starts before there is one.
  [[ -f $state/trust-bundle.pem ]] || cat "$ca_path" > "$state/trust-bundle.pem"
  run_native tls "$state/tls" "$caddy" run --config "$state/tls/Caddyfile" --adapter caddyfile
}

start_native_relay() {
  local lounge=$native/lounge
  { private_env relay "$lounge/bin"
    image_env_here "$repo/os/containers/tilo-lounge/Containerfile" \
      "TILO_ROOMS_STATE=$state/relay" "TILO_SECRETS_DIR=$state/relay/secrets"
    # Where the image keeps the app surfaces the relay publishes at start.
    printf '%s\n' "TILO_SURFACES=$lounge/surfaces"
    relay_env
  } > "$runs/relay.env"
  run_native relay "$state/relay" sh "$lounge/entrypoint.sh"
}

start_native_door() {
  { private_env door "$programs"
    door_env
  } > "$runs/door.env"
  run_native door "$state/door" "$programs/tilo-front-door"
}

# Who the Lounge's host is and where its page dials it, which the web entry
# hands its page (TILO_WEB_HOME_HOST_INFO): what `tilo-relay page-host` prints
# for the running relay, asked at its public origin, as production's operator
# asks it to set apps/tilo-web. LOUNGE is the relay's image with --images.
page_host() {
  local lounge=${1:-}
  if [[ $mode == images ]]; then
    podman_ run --rm --network host --read-only --entrypoint /usr/local/bin/tilo-relay \
      -v "$state/trust-bundle.pem:$ca_path:ro,Z" "$lounge" page-host "$relay_origin"
  else
    env -i PATH=/usr/bin:/bin "HOME=$runs" \
      bwrap --dev-bind / / --ro-bind "$state/trust-bundle.pem" "$ca_path" -- \
      "$programs/tilo-relay" page-host "$relay_origin"
  fi
}

start_native_web() {
  local room=$1 caddy container=$repo/crates/browser/tilo-browser-runtime/container info
  caddy=$(find_caddy)
  info=$(page_host) || die 'the relay did not say who it is (tilo-relay page-host)'

  { private_env web "$(dirname "$caddy")"
    image_env_here "$container/Containerfile" \
      "XDG_DATA_HOME=$runs/web/data" "XDG_CONFIG_HOME=$runs/web/config"
    # The image's paths and port, here (entrypoint.sh).
    printf '%s\n' "TILO_WEB_SITE=$native/web/site" "TILO_WEB_INSTALL=$native/web/install" \
      "TILO_WEB_RUN=$runs/web/entry" "TILO_WEB_CADDYFILE=$container/Caddyfile" \
      TILO_WEB_BIND=127.0.0.1 "TILO_WEB_PORT=$site_port"
    web_env "$room" "$info"
  } > "$runs/web.env"
  run_native web "$native/web" sh "$container/entrypoint.sh"
}

# The releases are what the package registry would serve: a plain file server.
start_native_releases() {
  mkdir -p "$runs/releases"
  printf '%s\n' "PATH=/usr/bin:/bin" "HOME=$runs/releases" > "$runs/releases.env"
  run_native releases "$state/releases" python3 -m http.server --bind 127.0.0.1 "$releases_port"
}

start_native_discord() {
  : > "$state/logs/discord.log"
  private_env discord "$programs" > "$runs/discord.env"
  run_native discord "$runs/discord" "$programs/fake-discord" --rest "127.0.0.1:$discord_rest_port" \
    --gateway "127.0.0.1:$discord_gateway_port" --control "127.0.0.1:$discord_control_port"
  local deadline=$((SECONDS + 20))
  until grep -q '^DISCORD_API_BASE=' "$state/logs/discord.log" 2>/dev/null; do
    ((SECONDS < deadline)) || die "the fake Discord did not start: $(cat "$state/logs/discord.log")"
    sleep 0.2
  done
  discord_api=http://127.0.0.1:$discord_rest_port
  discord_secrets
}

wait_relay() {
  wait_for 'relay (its readiness probe, /mode)' "http://127.0.0.1:$routes_port/mode" 300
  wait_for 'tls (the relay through Caddy)' "$relay_origin/mode" 60 --cacert "$state/local-ca.pem"
}

native_preflight() {
  [[ $EUID != 0 ]] || die 'run it as a regular user, not root'
  command -v "${TILO_SH_CARGO:-t}" >/dev/null || die "needs ${TILO_SH_CARGO:-t}, which builds the programs"
  command -v bwrap >/dev/null \
    || die 'needs bwrap (bubblewrap: dnf install bubblewrap), which gives the relay and the door the local trust bundle'
  [[ -f $ca_path ]] || die "this machine has no trust bundle at $ca_path, the one path Tilo's programs read"
}

up_native() {
  local seed=$1 discord=$2 began=$SECONDS
  native_preflight
  mkdir -p "$state"/{logs,relay,secrets,door,tls/caddy,tls/config} "$native" "$runs"
  # The bundle compiles on t beside the programs; only its first build, or
  # `restart web`, waits for it.
  local bundle_pid=
  if [[ -z ${TILO_SH_BUNDLE:-} && ! -f $native/bundle/build.json ]]; then
    build_bundle > "$state/logs/bundle.log" 2>&1 &
    bundle_pid=$!
  fi
  build_programs
  lay_out_lounge
  lay_out_releases
  if [[ -n $bundle_pid ]]; then
    say "waiting for the desktop bundle (stack.sh logs bundle)"
    wait "$bundle_pid" || { tail -30 "$state/logs/bundle.log" >&2; die 'the desktop bundle did not build'; }
  fi
  lay_out_web
  say "built and laid out ($((SECONDS - began))s)"
  echo native > "$state/mode"
  use_mode native
  make_secrets
  [[ -z $discord ]] || start_native_discord
  start_native_tls
  trust_bundle
  start_native_relay
  wait_relay
  local room
  room=$(cat "$state/relay/lounge/room-id") || die 'the relay kept no workspace id'
  say "relay ready: the Lounge is $room ($((SECONDS - began))s)"
  start_native_door
  wait_for 'door (its health check)' "http://127.0.0.1:$door_port/health" 60
  start_native_releases
  start_native_web "$room"
  wait_for 'web (its health check)' "$site_origin/healthz" 60
}

restart() {
  local name=${1:-}
  [[ -n $name ]] || die 'restart NAME: relay, door, web, tls, discord or releases'
  [[ $mode == native ]] || die 'restart rebuilds the native stack; the images run as built: stack.sh up --images again'
  native_preflight
  local began=$SECONDS
  if running discord; then discord_api=http://127.0.0.1:$discord_rest_port; fi
  case $name in
    relay)
      build_programs
      stop_native relay
      lay_out_lounge
      start_native_relay
      wait_relay ;;
    door)
      build_programs
      start_native_door
      wait_for 'door (its health check)' "http://127.0.0.1:$door_port/health" 60 ;;
    web)
      build_bundle
      stop_native web
      lay_out_web
      start_native_web "$(cat "$state/relay/lounge/room-id")"
      wait_for 'web (its health check)' "$site_origin/healthz" 60 ;;
    tls)
      start_native_tls
      wait_for 'tls (the relay through Caddy)' "$relay_origin/mode" 60 --cacert "$state/local-ca.pem" ;;
    discord)
      build_programs
      start_native_discord
      say 'the relay reads the Discord settings when it starts: stack.sh restart relay points a relay started without it here' ;;
    releases)
      build_programs
      lay_out_releases ;;
    *) die "restart takes relay, door, web, tls, discord or releases, not $name" ;;
  esac
  say "$name restarted in $((SECONDS - began))s"
}

# The operator's secrets, as the relay's Secret holds them: a throwaway
# workspace root, made once.
make_secrets() {
  if [[ ! -s $state/secrets/room-root-lounge ]]; then
    od -An -tx1 -N32 /dev/urandom | tr -d ' \n' > "$state/secrets/room-root-lounge"
    printf 'local-fake-discord-secret' > "$state/secrets/discord-client-secret"
    chmod 0400 "$state/secrets"/*
  fi
}

discord_secrets() {
  if [[ ! -s $state/secrets/discord-bot-token ]]; then
    chmod u+w "$state/secrets"
    printf 'fake.bot.token' > "$state/secrets/discord-bot-token"
    chmod 0400 "$state/secrets/discord-bot-token"
  fi
}

up() {
  local seed='' discord='' fresh='' images=''
  for arg in "$@"; do
    case $arg in
      --images) images=1 ;;
      --seed) seed=1 ;;
      --discord) discord=1 ;;
      --fresh) fresh=1 ;;
      -h|--help) usage; return ;;
      *) die "up takes --images, --seed, --discord and --fresh, not $arg" ;;
    esac
  done
  local began=$SECONDS
  vm_skipped=''
  [[ -z $fresh ]] || down --wipe
  # One stack at a time: what the other mode left running goes first.
  if [[ -f $state/mode ]]; then down >/dev/null 2>&1 || true; fi
  mkdir -p "$state/logs"
  if [[ -n $images ]]; then
    up_images "$discord"
  else
    up_native "$seed" "$discord"
    vm_skipped=$(vm_skip_reason)
  fi

  [[ -z $vm_skipped ]] || say "vm-host skipped: $vm_skipped"

  printf '%s\n' "$((SECONDS - began))" > "$state/bring-up-seconds"
  if [[ -n $seed ]]; then
    seed "$(cat "$state/relay/lounge/room-id")"
  fi
  say "up in $((SECONDS - began))s ($mode)"
  status
}

up_images() {
  local discord=$1
  command -v podman >/dev/null || die 'needs podman'
  # Rootless, as on any developer's machine: rootful podman gives the web
  # entry no pasta network, and keep-id is what maps the images' users onto
  # this account's files. In a VM as root, check-in-vm.sh runs it as a user.
  [[ $EUID != 0 ]] || die 'run it as a regular user (rootless podman), not root'
  local began=$SECONDS
  mkdir -p "$state"/{logs,relay,secrets,door,tls/caddy,tls/config}

  local lounge door web
  lounge=$(image LOUNGE image-lounge /lounge/image.oci.tar)
  door=$(image DOOR image-front-door image.oci.tar)
  web=$(image WEB image-web-entry /web-entry/image.oci.tar)
  say "images ready ($((SECONDS - began))s)"

  echo images > "$state/mode"
  use_mode images
  make_secrets
  # The relay's own device, which its VM host's member takes the workspaces'
  # runs from: made here, before either starts, so the relay starts once with
  # the host already placed.
  if [[ ! -s $state/relay/lounge/relay/host-device.key ]]; then
    mkdir -p "$state/relay/lounge/relay"
    local seed
    seed=$(od -An -tx1 -N32 /dev/urandom | tr -d ' \n')
    (umask 077 && printf "$(printf '%s' "$seed" | sed 's/../\\x&/g')" > "$state/relay/lounge/relay/host-device.key")
  fi

  # The VM host, when this machine can run it and the relay root is there: it
  # is root's, and it starts before the relay so the relay reads where to
  # place a workspace from the start.
  vm_skipped=$(vm_host_blocker)
  if [[ -z $vm_skipped ]]; then
    # The images are fetched (or loaded) here unless the caller named them:
    # the guest is read from the outputs `t outputs` left, not from podman, so
    # the host unpacks it onto its own state disk.
    [[ -n ${TILO_SH_IMAGE_VM_HOST:-} ]] || image VM_HOST image-vm-host image.oci.tar >/dev/null
    [[ -n ${TILO_SH_HOSTED_GUEST:-} ]] || image HOSTED_GUEST image-hosted-guest image.oci.tar >/dev/null
    enroll=$(find_enroll)  # read by relay_env, so the stack's own is used
    printf '%s\n' "$enroll" > "$state/vm-host-enroll"
    say 'starting the VM host (Firecracker, as root)'
    vm_host up || die 'the VM host did not start; sudo -E bash '"$here"'/vm-host.sh status says why'
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
  local web_args=() info
  info=$(page_host "$lounge") || die 'the relay did not say who it is (tilo-relay page-host)'
  mapfile -d '' web_args < <(web_env "$room" "$info" | env_args)
  start web --network "pasta:-T,$plain_port,-T,$releases_port" -p "127.0.0.1:$site_port:8080" \
    --read-only --tmpfs /tmp --user 10001 \
    "${web_args[@]}" "$web"
  wait_for 'web (its health check)' "$site_origin/healthz" 60

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
  discord_secrets
}

down() {
  local wipe=
  [[ ${1:-} == --wipe ]] && wipe=1
  local name
  # Both kinds: the native processes, and the image mode's containers and
  # VM host.
  for name in web releases door relay tls discord; do
    stop_native "$name"
  done
  if [[ $mode == images && -z $(vm_host_blocker) ]]; then
    vm_host down || say 'the VM host did not stop; sudo -E bash '"$here"'/vm-host.sh down'
  fi
  if command -v podman >/dev/null; then
    for name in web door relay tls; do
      podman_ rm -f -t 10 "$prefix-$name" >/dev/null 2>&1 || true
    done
  fi
  for pid in releases discord; do
    [[ -f $state/$pid.pid ]] && { kill "$(cat "$state/$pid.pid")" 2>/dev/null || true; rm -f "$state/$pid.pid"; }
  done
  if [[ -n $wipe ]]; then
    # The volumes belong to the containers' users (10001, 65532) inside this
    # account's user namespace: only that namespace removes them.
    podman_ unshare rm -rf "$state/relay" "$state/door" "$state/tls" "$state/secrets" "$state/seed" 2>/dev/null \
      || rm -rf "$state/relay" "$state/door" "$state/tls" "$state/secrets" "$state/seed"
    rm -rf "${runs:?}" "$state/trust-bundle.pem" "$state/local-ca.pem" "$state/mode"
    say "stopped, and its state is gone (builds kept in $native, images in $state/images)"
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
  if [[ $mode == images && -z $(vm_host_blocker) ]]; then
    printf '%-9s %-8s %s\n' vm-host up "workspaces' VMs; vm-host.sh status says which"
  else
    printf '%-9s %-8s %s\n' vm-host skipped "$(vm_skip_reason)"
  fi
  if running discord || { [[ -f $state/discord.pid ]] && kill -0 "$(cat "$state/discord.pid")" 2>/dev/null; }; then
    printf '%-9s %-8s %s\n' discord up "http://127.0.0.1:$discord_rest_port (control http://127.0.0.1:$discord_control_port)"
  fi
  if [[ $mode == native ]]; then
    printf '\nRunning this checkout'"'"'s programs; after a change, %s restart relay (or door, web, …).\n' "$0"
  else
    printf '\nRunning the images (up --images).\n'
  fi
  printf 'Next: eval "$(%s env)", then tilo keys create "My backend"; or %s browser.\n' "$0" "$0"
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
  restart) restart "$@" ;;
  logs) [[ -n ${1:-} ]] || die 'logs NAME: relay, tls, door, web, discord, releases, bundle'; cat "$state/logs/$1.log" ;;
  browser) browser "$@" ;;
  check)
    if [[ ${1:-} == --images ]]; then shift; export TILO_SH_IMAGES=1; fi
    exec bash "$here/check.sh" "$@" ;;
  help|-h|--help) usage ;;
  *) usage >&2; exit 2 ;;
esac
