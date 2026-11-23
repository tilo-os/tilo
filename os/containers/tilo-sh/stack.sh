#!/usr/bin/env bash
# tilo.sh on this machine: the Lounge server, the API server, the web entry
# and (where the machine can) the Linux VM host, from this checkout, started
# with production's entrypoints and the environment production's manifests
# give them (tilo/infra apps/tilo-sh-server, apps/tilo-api,
# apps/tilo-web, apps/tilo-sh-vm-host), with only the public names turned
# into this machine's loopback ports. By default the whole stack — every
# program, the build, the state — runs on the build server, in this checkout's
# build container, with only the ports a person and their tools use served
# from there on this machine's loopback (see "Where it runs" below);
# TILO_SH_REMOTE=0 runs it here instead, and `up --images` runs the release
# images here.
#
# Run `stack.sh help` for what it does and how to use it.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)
# The build server keeps this checkout at a root of its own (/src), so a command that acts there
# cannot name this script by this machine's path. It names it from the repository instead, which
# is the same at both roots, and every such command starts there.
self=${here#"$repo"/}/stack.sh

usage() {
  cat <<EOF
stack.sh: tilo.sh, whole, on this machine, from this checkout.

  stack.sh up [--images] [--seed] [--discord] [--fresh]
      Start everything and print where it is. By default this checkout's
      programs and web bundle, built on the build server in a build
      directory that stays (a drop-in's build directory persists: a change
      recompiles what it touches,
      incrementally, and only the files that changed come back), run as
      plain processes, each with its own state and runtime directory under
      the stack's state. TILO_SH_BUILD=t builds them with t's checks
      instead (tilo-sh-programs, web-entry-bundle): what the checks
      run, wasm-opt and all, slower. Each takes
      its environment from the same place its image does: the image's
      entrypoint and ENV, and the manifest's variables below.
      --images runs the release images instead (t test image-lounge
      image-api image-web-entry, then t outputs): the rehearsal of
      what ships, which t's tilo-sh-local check runs; the first run on a
      commit waits for those image builds.
      --seed fills the Lounge to its production size for entry-speed work:
      300 members joined through its entry and 200 chat messages posted
      through the API server (minutes, once per state). --discord starts a
      fake Discord and points the Lounge's Discord sign-in and bridge at
      it. --fresh wipes the state first (a new Lounge, a new entry).
  stack.sh restart [--detach] NAME...
                         after a code change: rebuild each NAME and start it
                         again, the rest left running (server, api, web, tls,
                         discord, releases; not with --images). The web
                         rebuilds its bundle, the others their programs. The
                         server lets its agents' turns that are going end
                         first. --detach builds everything named, says
                         whether it built, and returns; the restarts then
                         follow on their own (stack.sh logs restart): how an
                         agent of the stack restarts it from its own Terminal
  stack.sh env
      The environment that points everything at this stack: eval "\$(stack.sh env)",
      then tilo keys create "My backend", the Python SDK (Tilo()), the TS SDK
      (new Tilo()) and an agent's MCP client all reach the local API server.
  stack.sh status        what runs, at which address, and whether it answers
  stack.sh logs NAME     a service's log (server, tls, api, web, discord, releases,
                         mail), or \`remote\` for the stack's own log when it runs on
                         the build server
  stack.sh mail          the mail the entry sent (wait-list confirmations,
                         invitations), with the links in each
  stack.sh mail-name     the address that stand-in mail server takes, before the
                         stack is up: its domain publishes the settings document
  stack.sh mail-account  the account of this stack's stand-in mail server, as
                         a person types it into Mail's settings: a made-up
                         address at this stack's own site name, its password,
                         and that Mail finds its servers by itself from the
                         address, the way a provider publishes them.
                         A stack brought up with TILO_SH_MAIL_STAND_IN=1 has
                         it: an IMAP and an SMTP server with real sockets
                         that hold a few made-up messages and keep the mail
                         they are handed, and an autoconfig document at the
                         address's domain, built with the programs and
                         started with the stack, with its certificate in this
                         stack's trust bundle alone
  stack.sh vaults        whether the server keeps no sealed store of its own:
                         every secret is an entry of a workspace's vault
  stack.sh owner --here [--name NAME]
                         the Lounge's first owner and a key for them, no
                         browser: a person whose root the stack keeps in its
                         own state (made once, the same person after), whom
                         the server starts with as its owner, as production's
                         TILO_WORKSPACES_OWNERS names its own, approves an API
                         key on the spot. Prints export TILO_API_KEY=…, so
                         eval "\$(stack.sh owner --here)" holds it; stack.sh env
                         exports it from then on
  stack.sh tilo ARGS...  tilo as that owner, with their key:
                         stack.sh tilo workspace people wait-list -w <lounge id>
  stack.sh person ARGS...
                         tilo as that owner on their own device, with no key:
                         what the host answers the person alone, as their
                         page asks it (their vault, their mail accounts):
                         stack.sh person do workspace/vault#entries -w <workspace>
  stack.sh owner [KEY] [--name NAME]
                         an administrator link: opening it in any browser
                         (one with no Tilo identity yet too) makes that person
                         an administrator of the Lounge, beside its one owner,
                         and lets them in to invite the others; up prints one.
                         With KEY, the person with that user public key is an
                         administrator at once (NAME is how the Lounge names
                         them): the stack's owner runs
                         tilo workspace people add KEY --role administrator
  stack.sh invite [WORKSPACE] [-- --role ROLE] [--email ADDRESS]
                         an invite link to WORKSPACE (its id; the Lounge when
                         none), made by the stack's owner with tilo workspace people
                         invite: ROLE is viewer, commenter, editor (the
                         default), administrator or owner. Opened in any
                         browser, it makes the person's identity on the spot
                         and lets them in
  stack.sh tilo workspace servers add HOST-LINE -w WORKSPACE
                         the stack's owner has the stack whose own
                         web-host.json is HOST-LINE keep WORKSPACE as well:
                         both keep it and a change on either reaches the
                         other. That stack takes it only for a person it let
                         in: make this stack's owner one of its people there
                         first (stack.sh owner KEY there, KEY this stack's
                         \$TILO_SH_STATE/owner/key). servers ls and servers
                         remove NAME too
  stack.sh browser       a Chromium on a fresh profile, opened on the local site
  stack.sh check
                         bring the stack up and run the end-to-end check on it
                         (check.sh: a startup's visa flow and a fresh visitor's
                         entry time)
  stack.sh down [--wipe] stop it; --wipe also deletes its state (wherever it
                         runs: on the build server too, and here)

Where it runs: on the build server, in this checkout's build container, by
default when the cargo drop-in there runs a program for it and serves this
machine's ports (\`\$cargo exec --forward\`). Then this machine carries only the
ports a person and their tools use, under the same numbers: the site at
http://tilo.localhost:BASE+20, the API server at http://localhost:BASE+10, the
server origin, the mail sink and the releases, plus the UDP the page reaches the
server on (WebTransport, iroh). Everything else (the programs, every service,
the state, the build) stays there, and \`down --wipe\` deletes it there. The
commands that act on the stack run there too (owner, invite, tilo, hold, mail,
logs, restart); \`browser\` opens the page here, and \`check\` needs a stack here.
TILO_SH_REMOTE=0 runs everything here instead (needed for --images and the VM
host, and for \`check\`).

What runs, all on 127.0.0.1 (ports from TILO_SH_PORT_BASE, default 24100):
  server     tilo-server workspace through the Lounge image's entrypoint, as tilo.sh's
            server
  tls       production's Caddy and Caddyfile in front of it, plain HTTP:
            http://127.0.0.1:BASE (eu1.tilo.sh) and http://127.0.0.1:BASE+6
            (live-eu1.tilo.sh)
  api      the API server: MCP, /v1, keys and OAuth,
            http://localhost:BASE+10 (mcp.tilo.sh)
  web       the web entry (its entrypoint and Caddyfile serving the desktop
            bundle): the site, /sign-in, /install.sh, /releases, on
            127.0.0.1:BASE+21 behind tls, which serves it as Cloudflare
            serves tilo.sh: http://tilo.localhost:BASE+20. Every address
            is plain HTTP on this machine, which browsers treat as secure,
            so any browser opens it with no certificate step; WebTransport
            reaches the server with the short-lived certificate it pins by
            hash
  vm-host   the VM host image (Firecracker): a workspace's Linux programs and
            its Browser run in its VM here, and the server places it on this
            host. It is privileged, so it runs as root over its own state
            (TILO_SH_VM_HOST_STATE, /var/lib/tilo-sh-local-vm) through sudo;
            skipped, with the reason, where /dev/kvm or root is missing. It
            always runs from its image; natively it starts with the rest when
            TILO_SH_VM_HOST=1 names it
  mail      the mail sink the entry's mail goes to (production: Resend),
            http://127.0.0.1:BASE+23; stack.sh mail lists what it kept
  discord   with --discord, the fake Discord (tools/fake-discord)
  models    with TILO_SH_FAKE_MODELS=1, the fake OpenAI sign-in and model
            providers (tools/fake-model-providers) at
            http://127.0.0.1:BASE+43, which the server signs people in to
            OpenAI at; its sign-in page takes any code it showed

TILO_SH_PORT_BASE gives the stack its ports, the numbers above from it up to
BASE+44 (the last is the held program's control port on the build server). They must lie outside the range the machine hands to outgoing
connections (32768 and up on many machines; stack.sh reads
/proc/sys/net/ipv4/ip_local_port_range there, also on the build server when
the stack is there), and nothing may answer on them already: up refuses a
base inside that range, naming a safe one, and a base another stack holds (on
this machine or the build server: a second worktree's stack, or a forward left
from one), naming what holds it. A remote stack's bring-up then checks that
what answers through the forward is its own, by a token only this bring-up's
releases serve.

Reached from elsewhere: TILO_SH_REACHED_THROUGH=https://live-eu1.tilo.sh (any
packet relay's URL) has the server start no packet relay of its own and be
reached through that one, its iroh endpoint on every address, so people on
other networks join it with an invitation over iroh while nothing of the
stack is on the web. Its invitation links then carry the server's signed
address themselves, and
\`tilo login '<link>'\` joins with one from anywhere. The state keeps it for
\`restart server\` and \`restart api\`; an empty value goes back to the stack's
own relay.

A running stack belongs to the checkout that started it: up, down and restart
from another checkout refuse (TILO_SH_TAKE_OVER=1 takes it over). Give each
checkout its own TILO_SH_STATE and TILO_SH_PORT_BASE instead.

Natively: the programs are Cargo's lounge-local build (the release's code and
opt-level without its whole-program link, compiled incrementally) and the
bundle build.sh's local one (the same for the desktop bundle, bound and split
as the shipped one, without wasm-opt), both built on the build server by the
cargo drop-in TILO_SH_CARGO names (default \$CARGO) in a build directory that
stays, and only what changed brought back; a drop-in that runs no program
there has the bundle bound and split here. TILO_SH_BUILD=t takes them from
t's checks instead (tilo-sh-programs, lounge-check; web-entry-bundle,
optimized), which start from an empty build directory: what the checks
run, for a rehearsal of it. Caddy is
the one TILO_SH_CADDY names, else the one on PATH, else a pinned release
fetched once. The
server and the API server trust the local Caddy's CA through the fixed bundle path
Tilo's programs read, which bwrap (bubblewrap) puts in front of this machine's
own for those processes alone. No process sees this session's D-Bus or runtime
directory. TILO_SH_BUNDLE names a built bundle
(build.sh) instead of building one, built from this
checkout's commit (TILO_SH_BUNDLE_ANY_COMMIT=1 serves one from another).
Already-built inputs, which a check hands the stack so its bring-up compiles
and lays out nothing (see docs/testing-tilo-sh.md):
  TILO_SH_PROGRAMS  a directory of the programs the other services run
                    (tilo-api with tilolang-process beside it, which a task's
                    tilolang programs start in, fake-discord for --discord,
                    mail-stand-in for TILO_SH_MAIL_STAND_IN=1 and
                    fake-model-providers for TILO_SH_FAKE_MODELS=1)
  TILO_SH_LOUNGE    the server's laid-out context, as lounge-context.sh makes it
                    (bin/, fonts/, surfaces/, entrypoint.sh)
  TILO_SH_SITE      the web entry's laid-out site, as web-entry-context.sh makes
                    it (site/, install.sh): the web builds no bundle then
  TILO_SH_CLI_DIR   a built release of the tilo command (cli-x86_64), which
                    the site serves at /releases instead of packing one
Images: TILO_SH_IMAGE_LOUNGE, TILO_SH_IMAGE_API and TILO_SH_IMAGE_WEB name an
OCI archive or a registry reference instead of this checkout's t build (e.g.
the registry.tilo.so/tilo/lounge@sha256:… production pins).
TILO_SH_VM_HOST=0 leaves the VM host out where it could run (it runs as
root through sudo).
TILO_SH_MAIL_STAND_IN=1 adds a stand-in mail server to the stack, for
setting a mail account up in a browser with no real mail server: one
made-up login at this stack's site name, a few made-up messages, the mail
it is handed kept in that inbox, and the settings document a provider
publishes at the address's domain, so Mail finds the servers by itself.
\`stack.sh mail-account\` prints what a person types. Up without --images
only: it is built with this checkout's programs, so a rehearsal of what
ships has none.
TILO_SH_TERMINAL_ON_MACHINE=1, on a stack with no VM host, has the
workspaces' Terminal start its programs (cargo, git, ...) on this machine as
the stack's own user, with the PATH it was brought up with; nothing confines
them, so it is for a machine that is one person's own, like a development VM.
The state keeps it; =0 turns it off.
State: TILO_SH_STATE (default \${XDG_STATE_HOME:-~/.local/state}/tilo-sh-local).
Needs a cargo drop-in that builds on a build server (TILO_SH_CARGO names it,
else \$CARGO; with TILO_SH_BUILD=t, t) and curl, which is all a remote stack
uses here. A stack here also needs bwrap and python3 (with a drop-in that runs
no program on its build server, also wasm-bindgen for the bundle); with
--images, t and podman (rootless is fine) instead of bwrap.
Docs: docs/testing-tilo-sh.md.
EOF
}

say() { printf 'tilo-sh: %s\n' "$*" >&2; }
die() { printf 'tilo-sh: %s\n' "$*" >&2; exit 1; }

# -- where everything is -----------------------------------------------------

base=${TILO_SH_PORT_BASE:-24100}
server_port=$base               # Caddy: the server's public origin (eu1.tilo.sh)
plain_port=$((base + 1))       # the server's origin-forwarding listener (9780)
packet_port=$((base + 2))      # the relay's packet relay (8443)
routes_port=$((base + 3))      # the server's own routes (8790): /mode, /avatars, /discord/callback
iroh_port=$((base + 4))        # the server's iroh endpoint, UDP (7704)
server_web_port=$((base + 5))   # the server's own dev TLS listener, which nothing here uses
live_port=$((base + 6))        # Caddy: the live session's origin (live-eu1.tilo.sh)
caddy_http_port=$((base + 7))  # Caddy's HTTP port, which serves nothing here
link_port=$((base + 8))        # the server's WebTransport listener, UDP: how pages reach it
api_port=$((base + 10))       # the API server (mcp.tilo.sh)
site_port=$((base + 20))       # Caddy: the site's public origin (tilo.sh, Cloudflare's edge)
web_port=$((base + 21))        # the web entry's own plain listener (8080), behind it
releases_port=$((base + 22))   # this checkout's tilo command, as the site's /releases serves it
mail_port=$((base + 23))       # the mail sink the entry's mail goes to instead of Resend
mail_stand_in_imap_port=$((base + 24))  # the stand-in mail server's IMAP, when TILO_SH_MAIL_STAND_IN=1
mail_stand_in_smtp_port=$((base + 25))  # its SMTP
mail_stand_in_document_port=443        # its settings document, where a mail program asks (https)
discord_rest_port=$((base + 40))
discord_gateway_port=$((base + 41))
discord_control_port=$((base + 42))
models_port=$((base + 43))     # the fake OpenAI sign-in and model providers
control_port=$((base + 44))    # the held program's control port, for a stack on the build server
person_port=$((base + 45))     # the stack's person's own device, UDP: never the 7703 a real login here holds
last_port=$person_port   # the highest port a stack on this base takes

# Plain HTTP on this machine, which a browser treats as a secure context as it
# does https://, so any browser profile opens the site with no certificate to
# trust or click through (a browser asks about a certificate only for the page
# it opens, never for the calls that page makes to the server).
server_origin=http://127.0.0.1:$server_port
live_origin=http://127.0.0.1:$live_port
api_origin=http://localhost:$api_port
# A name of its own under localhost, not an address: a browser's secure
# context is the page's host name, which an address does not give it.
site_host=tilo.localhost
site_origin=http://$site_host:$site_port
# The site's health check, at the address the web entry listens on: the names
# under localhost are this machine's, which a resolver here need not know, so
# the stack asks the address and carries the name the entry routes by.
site_health=http://127.0.0.1:$site_port/healthz
site_host_header="Host: $site_host:$site_port"

state=${TILO_SH_STATE:-${XDG_STATE_HOME:-$HOME/.local/state}/tilo-sh-local}
# A remote stack's state is named from its checkout (`remote_state`), wherever the build server
# keeps that checkout.
[[ $state == /* ]] || state=$repo/$state
# The containers of the stack on these ports: a second stack on other ports
# (cargo web --check runs one) never stops or removes the first one's.
prefix=tilo-sh-local
[[ $base == 24100 ]] || prefix=tilo-sh-local-$base
# This checkout's commit. A copy that is not a git checkout (the build server's
# mirror) is told it, so what the stack lays out and the server reports is the
# commit its sources are from.
commit=$(git -C "$repo" rev-parse HEAD 2>/dev/null || echo "${TILO_SH_REMOTE_COMMIT:-unknown}")

native=$state/native   # what the native stack builds and lays out
runs=$state/run         # each native process's runtime directory and pid
# Where the programs and the bundle are built: on the build server by the
# cargo drop-in TILO_SH_CARGO names (`builder`, the default), or by t's checks
# (`t`, from_t below).
build=${TILO_SH_BUILD:-builder}
case $build in
  builder|t) ;;
  *) echo "tilo-sh: TILO_SH_BUILD is builder or t, not $build" >&2; exit 2 ;;
esac
# The cargo drop-in that builds: the one TILO_SH_CARGO names, else $CARGO's,
# else this machine's cargo.
cargo=${TILO_SH_CARGO:-${CARGO:-cargo}}
# Whether the cargo drop-in runs a program on the build server and serves this
# machine's ports from there (`$cargo exec --forward`), which is what a remote
# stack needs. Asked once.
remote_capable() {
  if [[ -z ${remote_capable_said:-} ]]; then
    local help
    remote_capable_said=0
    if help=$($cargo exec --help 2>/dev/null) && [[ $help == *--forward* ]]; then remote_capable_said=1; fi
  fi
  [[ $remote_capable_said == 1 ]]
}

# Where the services run: here, or on the build server in this checkout's
# build container with the ports below served from there on this machine's
# loopback under the same numbers (remote_mode). TILO_SH_REMOTE=1 sends them
# there, =0 keeps them here; by default they go there when the cargo drop-in
# can run a program on the build server and hold this machine's ports open
# for it (`exec --forward`).
remote=${TILO_SH_REMOTE:-auto}
remote_why=
case $remote in
  auto)
    # A stack handed what a check built (its programs, its site, its bundle) and
    # the checks' own browsers and VM host are of this machine: those run here.
    # TILO_SH_VM_HOST=0 only leaves the VM host out, which a stack on the build
    # server does anyway, so it keeps the stack where it would otherwise go.
    if [[ $build == t || -n ${TILO_SH_PROGRAMS:-} || -n ${TILO_SH_LOUNGE:-} \
      || -n ${TILO_SH_SITE:-} || -n ${TILO_SH_BUNDLE:-} || -n ${TILO_SH_CLI_DIR:-} \
      || ${TILO_SH_VM_HOST:-0} != 0 ]]; then
      remote=0
      remote_why='its programs and its site were given here (TILO_SH_PROGRAMS, TILO_SH_SITE, TILO_SH_BUILD=t, TILO_SH_VM_HOST=1 or the like)'
    elif remote_capable; then
      remote=1
    else
      remote=0
      remote_why="the cargo drop-in ($cargo) does not run a program on a build server and serve this machine's ports from there (\`exec --forward\`)"
    fi ;;
  1|yes)
    remote_capable || die "TILO_SH_REMOTE=1 needs a cargo drop-in that runs a program on a build server and serves this machine's ports from it (\`$cargo exec --forward\`); $cargo has none"
    remote=1 ;;
  0|no) remote=0 ;;
  *) die "TILO_SH_REMOTE is 1 (on the build server), 0 (here) or unset, not $remote" ;;
esac
# A stack that is up somewhere stays there: its mode says where,
# and up/down/restart read it (see the dispatch below). One whose state is here
# moves only when TILO_SH_REMOTE says so, since on the build server it would
# start without that state.
case $(cat "$state/mode" 2>/dev/null) in
  remote) remote=1 ;;
  native|images)
    if [[ -z ${TILO_SH_REMOTE:-} ]]; then
      remote=0
      remote_why="it is up here already, its state in $state"
    fi ;;
esac
# Where a remote stack's own state lives: in the checkout's target directory on the build server
# (the one part of it the build server keeps for the stack), named from the checkout's top, as
# the build server's checkout need not be at this one's path (t's is at /src).
remote_state=${TILO_SH_REMOTE_STATE:-target/tilo-stack/$base}

# The programs: the release's code and opt-level without the whole-program
# link, so a change relinks in seconds instead of LTO's minutes. Built here
# incrementally (Cargo.toml's lounge-local) into ./target, or by t's
# tilo-sh-programs as the Lounge's checks build them (lounge-check). Either
# way the services run a copy (copy_programs), never the build's own files,
# which the next build rewrites. A caller that built them already names their
# directory (TILO_SH_PROGRAMS, TILO_SH_LOUNGE).
profile=lounge-local
[[ $build != t ]] || profile=lounge-check
if [[ -n ${TILO_SH_PROGRAMS:-} ]]; then
  programs=$TILO_SH_PROGRAMS
else
  programs=$native/programs
fi
# The server's laid-out context, as lounge-context.sh makes it (below).
lounge=${TILO_SH_LOUNGE:-}
# The one trust bundle Tilo's programs read (crates/system/tilo-native-certs).
ca_path=/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem

# Where each service finds its files, as it sees them: the image's paths
# (the volumes the deployment mounts), or under the state natively.
use_mode() {
  mode=$1
  if [[ $mode == native || $mode == remote ]]; then
    secrets_path=$state/secrets
    api_state=$state/api
  else
    secrets_path=/run/secrets
    api_state=/state
  fi
}
use_mode "$(cat "$state/mode" 2>/dev/null || echo native)"

# -- this base's ports ---------------------------------------------------------

# Every port a stack answers on where it is (and, for one on the build
# server, here as a forward), by the kind of socket: the ones a base reserves,
# whether or not this bring-up uses each (the fake Discord's are forwarded).
stack_tcp_ports() {
  printf '%s\n' "$server_port" "$plain_port" "$routes_port" "$server_web_port" \
    "$live_port" "$caddy_http_port" "$api_port" "$site_port" "$web_port" \
    "$releases_port" "$mail_port" "$mail_stand_in_imap_port" \
    "$mail_stand_in_smtp_port" "$discord_rest_port" "$discord_gateway_port" \
    "$discord_control_port" "$models_port" "$control_port"
}
stack_udp_ports() { printf '%s\n' "$packet_port" "$iroh_port" "$link_port"; }

# check_ports: refuse a base this machine cannot give the stack whole.
#
# Two ways a base is not the stack's own. The kernel hands free ports from its
# temporary-port range to outgoing connections (ip_local_port_range: 32768 and
# up on many machines), so a port of the stack's inside it can be taken at any
# moment, and the service that wanted it cannot bind (a base of 38500 put the
# relay's 38502 there). And something already listening there is another
# stack's, or a forward left from one: a bring-up then never binds, while the
# forward answers for the other one, which once passed for success (base 34900,
# another worktree's stack on the build server). The same check answers on this
# machine and, from the stack's own `up` there, on the build server: it reads
# that machine's /proc, so it names what the kernel there says, and the holder.
check_ports() {
  local attempt=0 out line where='this machine'
  local -a tcp_ports=() udp_ports=() held=()
  local low='' high='' port span=$((last_port - base))
  # The stack's own `up` on the build server is given this (remote_env).
  [[ -z ${TILO_SH_REMOTE_COMMIT:-} ]] || where='the build server'
  mapfile -t tcp_ports < <(stack_tcp_ports)
  mapfile -t udp_ports < <(stack_udp_ports)
  command -v python3 >/dev/null || die 'needs python3 to check which ports its base needs'
  while :; do
    out=$(python3 -I "$here/port-check.py" --range \
      --tcp "$(join_comma "${tcp_ports[@]}")" --udp "$(join_comma "${udp_ports[@]}")") \
      || die "could not check which ports base $base needs ($here/port-check.py)"
    low='' high='' held=() entry=
    while IFS= read -r line; do
      case $line in
        'range unknown') ;;
        'range '*) read -r _ low high <<< "$line" ;;
        # A stack started inside its own control program holds that one port itself.
        'held '*)
          entry=${line#held }
          if [[ -n ${TILO_SH_CONTROL_PORT:-} && $entry == "tcp $TILO_SH_CONTROL_PORT by pid "* ]]; then
            continue
          fi
          held+=("$entry") ;;
      esac
    done <<< "$out"
    # Something may still be letting go of a stack just stopped.
    ((${#held[@]})) || break
    ((attempt < 10)) || break
    attempt=$((attempt + 1))
    sleep 0.5
  done
  if [[ -n $low ]]; then
    for port in "${tcp_ports[@]}" "${udp_ports[@]}"; do
      ((port >= low && port <= high)) || continue
      local below=$((low - span - 1)) above=$((65535 - span))
      die "TILO_SH_PORT_BASE=$base puts this stack on port $port, inside the range $where hands to outgoing connections ($low-$high): any connection can take that port from the stack, and the service that wanted it cannot bind. Use a base at or below $below (24100 is the default) or between $((high + 1)) and $above."
    done
  fi
  if ((${#held[@]})); then
    die "base $base is already someone else's: these ports answer before this stack starts on $where —
$(printf '  %s\n' "${held[@]}")
Stop what holds them (another checkout's stack.sh down, or the forward it left), or give this checkout its own TILO_SH_PORT_BASE and TILO_SH_STATE, then stack.sh up again."
  fi
}

join_comma() { local IFS=,; printf '%s' "$*"; }

# -- one stack's own ports ----------------------------------------------------

# The mark this bring-up leaves where the stack's releases serve it
# (`/tilo-cli/latest/stack-token` on its releases port): what answers on the
# base's ports is this stack's only if it serves this token. An opaque mark,
# not a secret: it names the stack's state, it opens nothing. A remote stack's
# forward once reached another stack holding the same base, and a bring-up
# that never came up was called up.
stack_token=${TILO_SH_STACK_TOKEN:-$(cat "$state/releases/tilo-cli/latest/stack-token" 2>/dev/null || true)}
write_stack_token() {
  [[ -n $stack_token ]] || return 0
  local dir=$state/releases/tilo-cli/latest
  mkdir -p "$dir"
  printf '%s\n' "$stack_token" > "$dir/stack-token"
}

podman_() { podman "$@"; }

# One command at a time over one state: two bring-ups over the same
# directories build into, swap and start each other's files. A second stack
# beside this one takes its own TILO_SH_STATE and TILO_SH_PORT_BASE. The lock
# is fd 9, which every long-running process this starts closes.
lock_state() {
  mkdir -p "$state"
  exec 9>"$state/lock"
  flock -n 9 || die "another stack.sh is working on $state (its pid: $(cat "$state/lock.pid" 2>/dev/null || echo unknown)); wait for it, or give this one its own TILO_SH_STATE and TILO_SH_PORT_BASE"
  echo $$ > "$state/lock.pid"
  claim_state
}
# claim_state: a running stack belongs to the checkout that started it. Another
# checkout stopping or restarting it took someone's stack down under them (an
# agent that forgot its own TILO_SH_STATE stopped the one a person was using),
# so that asks for TILO_SH_TAKE_OVER=1; a stack with nothing running is anyone's.
claim_state() {
  local owner svc
  owner=$(cat "$state/checkout" 2>/dev/null || true)
  if [[ -n $owner && $owner != "$repo" && ${TILO_SH_TAKE_OVER:-0} != 1 ]]; then
    for svc in tls server api web; do
      running "$svc" && die "the stack in $state runs from $owner, not this checkout ($repo): give this one its own TILO_SH_STATE and TILO_SH_PORT_BASE, or TILO_SH_TAKE_OVER=1 to take that one over"
    done
  fi
  printf '%s\n' "$repo" > "$state/checkout"
}
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

# Production's Caddy (apps/tilo-sh-server/deployment.yaml's tls container).
caddy_image=docker.io/library/caddy@sha256:4c6e91c6ed0e2fa03efd5b44747b625fec79bc9cd06ac5235a779726618e530d

# -- the services -----------------------------------------------------------

# start NAME ARGS…: one service's container, replacing any stopped one of the
# same name. Every container is this stack's own (named tilo-sh-local-*).
# Each resolves the site's name as production's resolve tilo.sh: the
# images' own resolvers know nothing of *.localhost.
start() {
  local name=$1
  shift
  podman_ rm -f "$prefix-$name" >/dev/null 2>&1 || true
  podman_ run -d --name "$prefix-$name" --label tilo-sh-local=1 --log-driver k8s-file \
    --add-host "$site_host:127.0.0.1" --log-opt "path=$state/logs/$name.log" "$@" >/dev/null \
    || die "$name did not start: podman run $*"
}

wait_for() { # WHAT URL SECONDS [CURL ARGS…]
  local what=$1 url=$2 seconds=$3
  shift 3
  local deadline=$((SECONDS + seconds))
  until curl -fsS --max-time 2 "$@" "$url" >/dev/null 2>&1; do
    ((SECONDS < deadline)) || not_answered "$what" "did not answer at $url within ${seconds}s"
    sleep 0.5
  done
}

not_answered() { # WHAT WHY
  # What the step that did not answer said: a bring-up that stops here says
  # why in the same breath, not a log away.
  local what=$1 log=$state/logs/${1%% *}.log
  [[ ! -f $log ]] || { printf -- '--- %s, its last lines:\n' "$log" >&2; tail -20 "$log" >&2; }
  die "$what $2; stack.sh logs ${what%% *}"
}

# The server answers /mode from its start: "starting" while it opens every
# workspace its state holds (a minute, for one that holds a repository's
# files, as the image's does), then "up" once people get in. It has a few
# runtime workers, and while its start keeps them busy a probe can go
# unanswered for seconds, more so on a build server other builds load: an
# unanswered probe is waited out like "starting", for as long as the server
# is alive, up to TILO_SH_SERVER_START_SECONDS (600). One that stopped fails
# at once; one still not up at the limit fails with what it last said.
wait_server_up() {
  local what='server (its readiness probe, /mode)' url="http://127.0.0.1:$routes_port/mode"
  local limit=${TILO_SH_SERVER_START_SECONDS:-600} began=$SECONDS told=$SECONDS said='' last=''
  until said=$(curl -sS --max-time 5 "$url" 2>/dev/null) && [[ $said == *'"server":"up"'* ]]; do
    running server || not_answered "$what" 'stopped before it served'
    case $said in
      *'"server":"starting"'*) last='it was still opening its workspaces' ;;
      *) [[ -n $last ]] || last='it never answered' ;;
    esac
    ((SECONDS - began < limit)) \
      || not_answered "$what" "was not up at $url within ${limit}s ($last; TILO_SH_SERVER_START_SECONDS waits longer)"
    if ((SECONDS - told >= 15)); then
      told=$SECONDS
      if [[ $said == *'"server":"starting"'* ]]; then
        say "waiting for the server to open its workspaces ($((SECONDS - began))s; $url says starting)"
      else
        say "waiting for the server to answer at $url ($((SECONDS - began))s; it is alive, its probe goes unanswered yet)"
      fi
    fi
    sleep 0.5
  done
}

# The server's environment: apps/tilo-sh-server/deployment.yaml, its names
# turned into this stack's addresses, its secrets into throwaway local ones.
server_env() {
  # TILO_SH_REACHED_THROUGH=<packet relay URL>: the server starts no packet relay
  # of its own and is reached through that one (tilo.sh's is
  # https://live-eu1.tilo.sh), its iroh endpoint on every address: how a stack
  # behind a router nobody dials into is reached by people elsewhere, with
  # nothing of it on the web. The stack's state keeps it, so `restart server`
  # serves it the same way; an empty value goes back to its own relay.
  if [[ -n ${TILO_SH_REACHED_THROUGH+set} ]]; then
    printf '%s' "$TILO_SH_REACHED_THROUGH" > "$state/reached-through"
  fi
  local through live_bind=127.0.0.1:$packet_port live_published=$live_origin
  local iroh_bind=127.0.0.1:$iroh_port
  through=$(cat "$state/reached-through" 2>/dev/null || true)
  if [[ -n $through ]]; then
    live_bind=none
    live_published=$through
    iroh_bind=0.0.0.0:$iroh_port
  fi
  local env=(
    TILO_WORKSPACES_PERSISTENT=lounge
    TILO_WORKSPACES_NAME=Lounge
    "TILO_WORKSPACES_DESCRIPTION=The Tilo community: chat, notes and a shared desktop"
    TILO_WORKSPACES_DEFAULT_ROLE=editor
    TILO_WORKSPACES_LAYOUT=shared
    "TILO_WORKSPACES_SERVER_ORIGIN=$server_origin"
    "TILO_WORKSPACES_LIVE=$live_bind"
    "TILO_WORKSPACES_IROH_LISTEN=$iroh_bind"
    "TILO_WORKSPACES_LIVE_ORIGIN=$live_published"
    "TILO_WORKSPACES_WEB_ENTRY=$site_origin/"
    "TILO_WORKSPACES_SITE_ORIGINS=$site_origin"
    # The site is plain http on this machine: the server's local mode, in which
    # its entry mails links to such a page (production's is https).
    TILO_WORKSPACES_LOCAL=1
    "TILO_WORKSPACES_ROOT_FILE=$secrets_path/workspace-root-lounge"
    TILO_WORKSPACES_FEEDBACK_CHANNEL=feedback
    # Its mail goes to this stack's mail sink (stack.sh mail reads it) where
    # production's goes to Resend.
    TILO_WORKSPACES_CONTACT=hello@tilo.localhost
    "TILO_WORKSPACES_MAIL_FROM=Tilo <hello@tilo.localhost>"
    "TILO_WORKSPACES_RESEND_KEY_FILE=$secrets_path/resend-key"
    "TILO_WORKSPACES_RESEND_URL=http://127.0.0.1:$mail_port/emails"
    # Production's listeners, which its pod binds on all addresses, here on
    # loopback only.
    "TILO_WORKSPACES_SERVER_PLAIN=127.0.0.1:$plain_port"
    "TILO_WORKSPACES_HTTP=127.0.0.1:$routes_port"
    "TILO_WORKSPACES_WEB=127.0.0.1:$server_web_port"
    "TILO_WORKSPACES_LINK=127.0.0.1:$link_port"
    "TILO_WORKSPACES_LINK_ORIGIN=https://127.0.0.1:$link_port"
  )
  # Production's entry: only people invited, or added by an owner
  # (stack.sh owner KEY), join, and a stranger leaves an address on its wait
  # list. TILO_SH_ADMISSION=open lets anyone in as a guest instead, for the
  # checks whose visitors arrive as strangers. A service started again later
  # (`restart server`) serves the entry the stack was brought up with, which its
  # state keeps: the restart carries no environment of the up that made it.
  local admission=${TILO_SH_ADMISSION:-$(cat "$state/admission" 2>/dev/null || echo invite)}
  # The Lounge's first owner: the stack's own person, as production's
  # deployment names its own. Everyone else comes in through them.
  env+=("TILO_WORKSPACES_OWNERS=$(stack_person)")
  case $admission in
    invite) env+=(TILO_WORKSPACES_ADMISSION=invite TILO_WORKSPACES_WAIT_LIST_MAX=500) ;;
    open) env+=(TILO_WORKSPACES_ADMISSION=open) ;;
    *) die "TILO_SH_ADMISSION is invite (production's) or open, not $admission" ;;
  esac
  # Its servers, and the stacks beside it, answer on this machine's own
  # addresses: a model provider or another stack named by one is called
  # there.
  env+=(TILO_WORKSPACES_CALL_OWN_NETWORK=1)
  if [[ ${TILO_SH_FAKE_MODELS:-0} == 1 ]]; then
    env+=(
      "TILO_WORKSPACES_OPENAI_SIGN_IN=http://127.0.0.1:$models_port/auth"
      "TILO_WORKSPACES_CHATGPT_BACKEND=http://127.0.0.1:$models_port/backend-api/codex"
      "TILO_WORKSPACES_OPENAI_API=http://127.0.0.1:$models_port/openai/v1"
    )
  fi
  # Not here: the summary model key (no model is asked locally).
  # TILO_SH_TERMINAL_ON_MACHINE=1: with no VM host, the workspaces' Terminal
  # (a person's and their agents') starts its programs on this machine, as the
  # stack's own user, with the PATH and toolchain of whoever brought it up:
  # for a stack that is one person's own machine, such as a development VM
  # whose agent builds the very stack it talks through. Nothing confines those
  # programs. The state keeps it, with that environment, so `restart server`
  # serves it the same way; TILO_SH_TERMINAL_ON_MACHINE=0 turns it off again.
  if [[ -n ${TILO_SH_TERMINAL_ON_MACHINE:-} ]]; then
    if [[ $TILO_SH_TERMINAL_ON_MACHINE == 1 ]]; then
      { printf 'PATH=%s:%s\n' "${lounge:-$native/lounge}/bin" "$PATH"
        local toolchain
        for toolchain in CARGO_HOME RUSTUP_HOME; do
          [[ -z ${!toolchain:-} ]] || printf '%s=%s\n' "$toolchain" "${!toolchain}"
        done
        # This stack's own settings, so stack.sh started from that Terminal (an
        # agent's `restart --detach server web`) acts on this stack, at its
        # ports, rather than on a stack at the defaults.
        printf 'TILO_SH_STATE=%s\nTILO_SH_PORT_BASE=%s\n' "$state" "$base"
        local setting
        while IFS= read -r setting; do
          [[ $setting == TILO_SH_STATE || $setting == TILO_SH_PORT_BASE ]] \
            || printf '%s=%s\n' "$setting" "${!setting}"
        done < <(compgen -e | grep '^TILO_SH_')
        [[ -z ${CARGO_BUILD_JOBS:-} ]] || printf 'CARGO_BUILD_JOBS=%s\n' "$CARGO_BUILD_JOBS"
      } > "$state/terminal-on-machine.env"
    else
      rm -f "$state/terminal-on-machine.env"
    fi
  fi
  if [[ -f $state/terminal-on-machine.env ]] && ! vm_host_running; then
    env+=(TILO_TERMINAL_ON_MACHINE=1)
    local line
    while IFS= read -r line; do env+=("$line"); done < "$state/terminal-on-machine.env"
  fi
  # A workspace's programs and its Browser run in its VM on this stack's own
  # VM host, when it is up: where to place a workspace, which device holds it,
  # and the log its page actions are counted in.
  if vm_host_running; then
    local line
    while IFS= read -r line; do env+=("$line"); done < <(vm_host env)
  fi
  printf '%s\n' "${env[@]}"
}

# Why the stack runs no VM host here: what this machine lacks, or that this
# run does not ask for one (natively it starts from its image only when
# TILO_SH_VM_HOST=1 names the images it runs).
vm_skip_reason() { vm_host_blocker; }

# Whether this stack's VM host is up, whatever this run asked for: a stack
# started with it places workspaces there, and `down` stops it.
vm_host_running() { [[ -n $(vm_host status 2>/dev/null | sed -n 's/^tilo-sh-vm-host-0 *running$/running/p') ]]; }

# Who the server is and where it is dialled, as it wrote it in its state when its browser
# listener started (the web entry's `TILO_WEB_HOME_HOST_INFO`).
host_info() {
  local kept=$state/server/lounge/server/server/web-host.json
  [[ -r $kept ]] || die "the server said nothing of itself ($kept): is it up?"
  cat "$kept"
}

# The API server's environment: apps/tilo-api/deployment.yaml.
api_env() {
  printf '%s\n' \
    "TILO_API_LISTEN=127.0.0.1:$api_port" \
    "TILO_API_ORIGIN=$api_origin" \
    "TILO_API_WEB_ENTRY=$site_origin" \
    "TILO_API_HOST=$(host_info)" \
    "TILO_API_STATE=$api_state/api.json"
}

# The web entry's environment: apps/tilo-web/deployment.yaml. INFO is what
# host_info read. The app a person lands in is the deployment's: production
# opens Chat, and a page whose checks arrive on the workspace's shell names
# none (TILO_SH_HOME_APP=).
web_env() {
  local workspace=$1 info=$2
  printf '%s\n' \
    "TILO_WEB_HOME_HOST=127.0.0.1:$server_port" \
    "TILO_WEB_HOME_HOST_INFO=$info" \
    TILO_WEB_HOME_NAME=Lounge \
    "TILO_WEB_HOME_APP=${TILO_SH_HOME_APP-so.tilo.chat}" \
    "TILO_WEB_HOME_ENTRY=$server_origin/peer/v1/directory/$workspace/" \
    TILO_WEB_HOME_FEEDBACK=c_feedback \
    "TILO_WEB_DIRECTORY=$server_origin/peer/v1/directory" \
    "TILO_WEB_API=$api_origin" \
    "TILO_WEB_SERVER=http://127.0.0.1:$plain_port" \
    "TILO_RELEASES_ORIGIN=http://127.0.0.1:$releases_port/tilo-cli"
}

# as_user UID: the arguments that run a container as UID, the image's own
# user in production. Under rootless podman this account is UID inside the
# container (keep-id), so the volumes it mounts are already that user's.
as_user() {
  printf -- '--userns\0keep-id:uid=%s,gid=%s\0--user\0%s:%s\0' "$1" "$1" "$1" "$1"
}

env_args() { local line; while IFS= read -r line; do printf -- '--env\0%s\0' "$line"; done; }

# The bundle at the fixed path Tilo's binaries read: this machine's roots.
# The stack's own addresses are plain HTTP, so it adds no authority of its
# own; the services reach the internet (font catalog, releases) as this
# machine does. TILO_SH_EXTRA_ROOTS names a PEM file of more roots, on the
# machine the stack is on, for a rehearsal against a stand-in server (a
# mail server's own certificate), which only this stack then trusts. The
# stand-in mail server's certificate is kept under the state, beside the
# login it takes, and is trusted the same way while it is there.
trust_bundle() {
  local system
  for system in /etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem /etc/ssl/certs/ca-certificates.crt; do
    if [[ -f $system ]]; then
      cat "$system" > "$state/trust-bundle.pem"
      if [[ -n ${TILO_SH_EXTRA_ROOTS:-} ]]; then
        [[ -f $TILO_SH_EXTRA_ROOTS ]] || die "TILO_SH_EXTRA_ROOTS names no file: $TILO_SH_EXTRA_ROOTS"
        cat "$TILO_SH_EXTRA_ROOTS" >> "$state/trust-bundle.pem"
      fi
      if [[ -f $state/mail-stand-in/certificate.pem ]]; then
        cat "$state/mail-stand-in/certificate.pem" >> "$state/trust-bundle.pem"
      fi
      return
    fi
  done
  die 'this machine has no CA bundle at /etc/pki/ca-trust or /etc/ssl/certs'
}

# The mail service the server's entry posts to, here a sink that keeps each mail as a
# file under the state (stack.sh mail lists them) and sends nothing. Started
# before the server, whose entry mails as soon as someone leaves an address.
start_mail() {
  local log=$state/logs/mail.log
  [[ -f $state/mail.pid ]] && kill "$(cat "$state/mail.pid")" 2>/dev/null || true
  : > "$log"
  node "$repo/os/containers/tilo-lounge/integration/mail-sink.mjs" "$state/mail" "$mail_port" \
    >> "$log" 2>&1 < /dev/null 9>&- &
  local pid=$!
  echo "$pid" > "$state/mail.pid"
  # A sink that exited failed; one still starting on a loaded machine is given a minute.
  local deadline=$((SECONDS + 60))
  until grep -q '"url"' "$log" 2>/dev/null; do
    kill -0 "$pid" 2>/dev/null || die "the mail sink exited: $(cat "$log")"
    ((SECONDS < deadline)) || die "the mail sink did not start within a minute: $(cat "$log")"
    sleep 0.1
  done
}

# The stand-in mail server (TILO_SH_MAIL_STAND_IN=1): an IMAP and an SMTP
# server with real sockets that take one made-up login, show an empty inbox
# and send nothing. It is built with the programs and started by the stack's
# own `up`, before the server, so a person sets a mail account up in a
# browser against this stack and signs in, with no real mail server and
# nobody's address or password. Its made-up login and password lie under the
# state (stack.sh mail-account prints them), and its certificate joins this
# stack's trust bundle alone.
start_mail_stand_in() {
  local dir=$state/mail-stand-in log=$state/logs/mail-stand-in.log login password
  [[ -x $programs/mail-stand-in ]] \
    || die "TILO_SH_MAIL_STAND_IN=1 needs mail-stand-in beside the other programs, and $programs holds none: a directory TILO_SH_PROGRAMS names has to be built with it (t's tilo-sh-programs builds one)"
  mkdir -p "$dir"
  stop_mail_stand_in
  # The account is at this stack's own site name, so the address's domain is a
  # name this machine answers: the stand-in publishes the settings document a
  # mail provider keeps there, and Mail finds the servers from the address.
  login=$(mail_name)
  password=$(cat "$dir/password" 2>/dev/null || true)
  if [[ -z $password ]]; then
    password=$(od -An -tx1 -N12 /dev/urandom | tr -d ' \n')
    (umask 077 && printf '%s\n' "$password" > "$dir/password")
  fi
  printf '%s\n' "$login" > "$dir/login"
  # The provider's names: the address's own domain, and the name a mail program
  # asks for its settings document under.
  local domain=${login#*@}
  autoconfig_document "$dir/config-v1.1.xml" "$domain"
  : > "$log"
  # The settings document is served over https on the port every mail program
  # asks for, which a machine allows only to root or inside a network namespace
  # of its own (the story check brings the stack up in one). Where it cannot be
  # bound, a rehearsal names the servers by hand.
  # A provider names its servers under its domain, and its certificate is for
  # those names and the ones a mail program asks its document under.
  setsid "$programs/mail-stand-in" --name localhost --name "$domain" \
    --name "autoconfig.$domain" --name "imap.$domain" --name "smtp.$domain" \
    --login "$login" \
    --password-file "$dir/password" --write-certificate "$dir/certificate.pem" \
    --imap "127.0.0.1:$mail_stand_in_imap_port" --smtp "127.0.0.1:$mail_stand_in_smtp_port" \
    --imap-tls --autoconfig "127.0.0.1:$mail_stand_in_document_port" \
    --autoconfig "[::1]:$mail_stand_in_document_port" \
    --autoconfig-document "$dir/config-v1.1.xml" \
    >> "$log" 2>&1 < /dev/null 9>&- &
  local pid=$!
  printf '%s\n' "$pid" > "$dir/pid"
  # A stand-in that exited failed; one still starting on a loaded machine is given a minute.
  local deadline=$((SECONDS + 60))
  until grep -q 'mail stand-in: IMAP at' "$log" 2>/dev/null; do
    kill -0 "$pid" 2>/dev/null || die "the mail stand-in exited: $(cat "$log")"
    ((SECONDS < deadline)) || die "the mail stand-in did not start within a minute: $(cat "$log")"
    sleep 0.1
  done
  if grep -q 'no autoconfig document' "$log" 2>/dev/null; then
    say "the stand-in cannot serve the settings document on port $mail_stand_in_document_port (that takes root, or a namespace of the stack's own): Mail asks for the servers, which stack.sh mail-account prints"
  fi
  say "mail stand-in on $mail_stand_in_imap_port (IMAP) and $mail_stand_in_smtp_port (SMTP); stack.sh mail-account prints the account"
}

# The settings document a mail provider publishes at its address's domain for
# mail programs to find its servers (RFC 6186's autoconfig) — the file its
# domain names, for the address's domain `domain`: where this stack's stand-in
# reads and hands in mail, and the address it takes.
autoconfig_document() {
  cat > "$1" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<clientConfig version="1.1">
  <emailProvider id="$2">
    <domain>$2</domain>
    <displayName>The local tilo.sh stack's mail</displayName>
    <displayShortName>the stack</displayShortName>
    <incomingServer type="imap">
      <hostname>imap.$2</hostname>
      <port>$mail_stand_in_imap_port</port>
      <socketType>SSL</socketType>
      <authentication>password-cleartext</authentication>
      <username>%EMAILADDRESS%</username>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.$2</hostname>
      <port>$mail_stand_in_smtp_port</port>
      <socketType>STARTTLS</socketType>
      <authentication>password-cleartext</authentication>
      <username>%EMAILADDRESS%</username>
    </outgoingServer>
  </emailProvider>
</clientConfig>
EOF
}

stop_mail_stand_in() {
  local pidfile=$state/mail-stand-in/pid pid deadline
  [[ -f $pidfile ]] || return 0
  pid=$(cat "$pidfile")
  kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
  deadline=$((SECONDS + 10))
  while kill -0 "$pid" 2>/dev/null && ((SECONDS < deadline)); do sleep 0.1; done
  rm -f "$pidfile"
}

# mail-name: the address the stack's stand-in mail server takes, known before the
# stack is up: what a person types, and the domain its provider publishes under.
mail_name() {
  printf '%s\n' "lab@$site_host"
}

# mail-account: what a person types into a mail account's settings to reach
# this stack's stand-in server (the stack brought up with
# TILO_SH_MAIL_STAND_IN=1).
mail_account() {
  local dir=$state/mail-stand-in login password
  [[ -f $dir/login && -f $dir/password ]] \
    || die 'this stack has no stand-in mail server: TILO_SH_MAIL_STAND_IN=1 stack.sh up'
  login=$(cat "$dir/login") password=$(cat "$dir/password")
  cat <<EOF
A mail account for this stack, as a person types it:

  Email address:  $login
  Password:       $password

Mail finds its servers from the address by itself: the stand-in publishes the
document a provider keeps at https://autoconfig.$site_host/mail/config-v1.1.xml.
Should it not, name them:

  Incoming (IMAP): localhost:$mail_stand_in_imap_port, SSL/TLS
  Outgoing (SMTP): localhost:$mail_stand_in_smtp_port, STARTTLS

It holds a few made-up messages, keeps the mail it is handed in the inbox, and
takes no other login.
EOF
}

# invite [WORKSPACE] [-- --role ROLE] [--email ADDRESS]: an invite link to
# WORKSPACE (the Lounge when none), made by the stack's own person, its owner,
# with `tilo workspace people invite`, as any owner makes one. Opened in any browser,
# one with no Tilo identity yet included, the page makes the person's identity
# and joins.
invite_link() {
  local workspace=''
  local role=editor
  local -a asked=()
  if [[ $# -gt 0 && $1 != -- ]]; then workspace=$1; shift; fi
  [[ ${1:-} != -- ]] || shift
  while [[ $# -gt 0 ]]; do
    case $1 in
      --role)
        [[ $# -ge 2 ]] || die 'invite --role: which'
        role=$2
        shift 2
        ;;
      --email)
        [[ $# -ge 2 ]] || die 'invite --email: the address'
        asked+=(--email "$2")
        shift 2
        ;;
      *) die "invite [WORKSPACE] [-- --role viewer|commenter|editor|administrator|owner] [--email ADDRESS], not $1" ;;
    esac
  done
  [[ -n $workspace ]] || workspace=$(lounge_id)
  owner_tilo workspace people invite --role "$role" "${asked[@]}" -w "$workspace" | head -1
}

# The Lounge's workspace id, once it runs.
lounge_id() {
  local id_file=$state/server/lounge/workspace-id
  [[ -s $id_file ]] || die "the Lounge is not running ($id_file); stack.sh up first"
  cat "$id_file"
}

# owner [KEY] [--name NAME]: without KEY, an administrator link: opening it
# in any browser, one with no Tilo identity yet included, makes that person an
# administrator of the Lounge and lets them in. The Lounge has one owner, the
# stack's own person, and only they hand it on, from a device of their own.
# With KEY, the person whose user public key (hex) is KEY is an administrator
# at once: the stack's own person adds them (`tilo workspace people add KEY
# --role administrator`), and a key of theirs reaches the Lounge before any
# device of theirs joins. NAME is how the Lounge names them then.
declare_owner() {
  local key=''
  local name=''
  local here=0
  while [[ $# -gt 0 ]]; do
    case $1 in
      --here) here=1 ;;
      --name)
        [[ $# -ge 2 ]] || die 'owner --name: the name the person goes by'
        name=$2
        shift
        ;;
      *) key=$1 ;;
    esac
    shift
  done
  if [[ $here == 1 ]]; then
    [[ -z $key ]] || die 'owner --here is the stack'"'"'s own person; KEY names someone else'
    owner_here "$name"
    return
  fi
  if [[ -z $key ]]; then
    [[ -z $name ]] || die 'owner --name names a KEY; whoever opens the administrator link names themself'
    owner_link
    return
  fi
  [[ $key =~ ^[0-9a-f]{64}$ ]] || die 'owner KEY: the person'"'"'s user public key, 64 hex characters (tilo login --json names it as user_public_key)'
  local -a named=()
  [[ -z $name ]] || named=(--name "$name")
  owner_tilo workspace people add "$key" --role administrator "${named[@]}" -w "$(lounge_id)" >/dev/null \
    || die "the Lounge did not make $key an administrator"
  say "$key is an administrator of the Lounge: let in at once${name:+ as $name}"
}

# The stack's own person: tilo run with a home of its own under the stack's
# state, so their root and the keys they approve never touch this account's
# own Tilo. `stack.sh tilo …` and owner --here run tilo this way.
# The person is kept as a Tilo session keeps one (its identity under .config,
# this device's key under .local/share/tilo): a session given this home is them.
as_owner() {
  owner_run "$(find_tilo)" "$@"
}

# owner_run COMMAND…: COMMAND as the stack's own person (as_owner), with their home and
# runtime directory.
owner_run() {
  local home=$state/owner/home run
  mkdir -p "$home"
  # Their own runtime directory, where the holder of their device listens: one this account
  # makes private, since tilo refuses a temporary directory's tilo-UID someone else made (a
  # build container's temporary directory is every container's), and short, since a socket's
  # path is.
  run=${TMPDIR:-/tmp}/tilo-sh-$(printf '%s' "$home" | sha256sum | cut -c1-12)
  mkdir -p -m 700 "$run"
  XDG_RUNTIME_DIR=$run \
  HOME=$home XDG_CONFIG_HOME=$home/.config XDG_STATE_HOME=$home/.local/state \
    XDG_DATA_HOME=$home/.local/share XDG_CACHE_HOME=$home/.cache \
    TILO_IDENTITY_ROOT=$home/.local/share/tilo \
    TILO_DEVICE_PORT=$person_port \
    TILO_API_URL=$api_origin TILO_SITE=$site_origin "$@" 9>&-
}

# The daemon of the stack's person's device, once they logged in, run as their service would
# run it. tilo starts it only through this account's systemd user unit or launchd agent, which
# serve this account's own login: their unit would take its place, and with the person's
# private runtime directory there is no user bus to reach. So the stack runs it, and
# stop_people_holders stops it with the rest of the stack.
owner_daemon() {
  local runtime log=$state/logs/person.log
  [[ -f $state/owner/home/.local/state/tilo/device/login.json ]] || return 0
  [[ -z $(people_holders) ]] || return 0
  runtime=$(dirname "$(find_tilo)")/tilo-runtime
  [[ -x $runtime ]] || die "no tilo-runtime beside $(find_tilo)"
  mkdir -p "$state/logs"
  (owner_run setsid "$runtime" >> "$log" 2>&1 < /dev/null &)
  for _ in $(seq 1 60); do
    [[ -z $(people_holders) ]] || return 0
    sleep 0.5
  done
  die "the person's device did not start (stack.sh logs person, or $log)"
}

# The user public key of the stack's own person, made the first time: the
# Lounge's first owner, which the server is started with (TILO_WORKSPACES_OWNERS),
# as production's deployment names its own.
stack_person() {
  local made
  if [[ ! -s $state/owner/key ]]; then
    mkdir -p "$state/owner"
    made=$(as_owner login --new-identity --json) \
      || die 'tilo made no person for the stack (tilo login --new-identity)'
    python3 -c 'import json,sys; print(json.loads(sys.argv[1])["user_public_key"])' "$made" \
      > "$state/owner/key" || die "tilo named no person: $made"
  fi
  cat "$state/owner/key"
}

# The API key the stack's own person approved for themself, approved the
# first time it is asked for.
owner_api_key() {
  local answer api_key said=$state/owner/keys-create.err status=0
  if [[ ! -s $state/owner/api-key ]]; then
    mkdir -p "$state/owner"
    # A person named by owner/key but kept on no computer here (a stack another server's person
    # moves to) approves nothing until they sign in: tilo would wait for a browser instead. An
    # earlier tilo (TILO_SH_TILO_BIN) keeps them in its state folder instead.
    [[ -d $state/owner/home/.config/tilo/identity \
      || -f $state/owner/home/.local/state/tilo/identity/user-identity/privkey.bin ]] \
      || die "the stack's person ($(cat "$state/owner/key")) is not signed in here: stack.sh person login --recovery-key, with their 24 words on standard input"
    answer=$(as_owner keys create 'Local stack' --json 2>"$said") || status=$?
    if ((status != 0)); then
      # Why, in tilo's own words (it says them on stderr), and whether the
      # API server it asked answers at all: a bring-up that stopped early
      # leaves none to approve the key.
      local why
      why=$(cat "$said" 2>/dev/null)
      why=${why:-${answer:-tilo keys create exited $status and said nothing}}
      curl -fsS --max-time 5 "$api_origin/health" >/dev/null 2>&1 \
        || why+=$'\n'"the API server does not answer at $api_origin/health: stack.sh up did not finish (stack.sh status, stack.sh logs api)"
      die "the owner's key was not approved: $why"
    fi
    rm -f "$said"
    api_key=$(python3 -c 'import json,sys
answer = json.loads(sys.argv[1])
print(next(answer[field] for field in ("key", "token", "api_key") if answer.get(field)))' "$answer") \
      || die "tilo answered no key: $answer"
    printf '%s\n' "$api_key" > "$state/owner/api-key"
    chmod 600 "$state/owner/api-key"
  fi
  cat "$state/owner/api-key"
}

# tilo as the stack's own person, with their API key.
owner_tilo() {
  local api_key
  api_key=$(owner_api_key) || exit 1
  TILO_API_KEY=$api_key as_owner "$@"
}

# tilo as the stack's own person on the device their root certified, with no
# key: a key is the person's agent, which the host refuses their vault and
# their mail accounts.
owner_person() {
  stack_person >/dev/null
  owner_daemon
  (unset TILO_API_KEY && as_owner "$@")
  owner_daemon
}

# owner --here [NAME]: the stack's own person, the Lounge's first owner,
# approves an API key for themself there and then, as the person's own Tilo
# approves one: no browser. NAME is how the Lounge names them from then on.
# Prints the key as an export line and keeps it for stack.sh env.
owner_here() {
  local name=${1:-}
  local api_key
  stack_person >/dev/null
  api_key=$(owner_api_key) || exit 1
  if [[ -n $name ]]; then
    owner_tilo workspace people name "$name" -w "$(lounge_id)" >/dev/null \
      || die "the Lounge did not name its owner $name"
  fi
  say "the owner's key is kept for stack.sh env; stack.sh tilo … acts as them"
  printf 'export TILO_API_KEY=%s\n' "$api_key"
}

# An administrator link: whoever opens it is an administrator from then on,
# as the stack's own person invites them (`tilo workspace people invite --role
# administrator`).
owner_link() {
  local link
  link=$(owner_tilo workspace people invite --role administrator -w "$(lounge_id)" | head -1) \
    || die 'the Lounge made no administrator link'
  say 'opening this link in any browser makes that person an administrator of the Lounge:'
  printf '%s\n' "$link"
}

# The mails the entry sent, oldest first: who to, the subject and the links in
# each, which a person would follow.
list_mail() {
  [[ -d $state/mail ]] || die 'no mail yet (the stack keeps it in '"$state"'/mail)'
  python3 -I - "$state/mail" <<'PY'
import json, pathlib, re, sys
for path in sorted(pathlib.Path(sys.argv[1]).glob('*.json')):
    mail = json.loads(path.read_text())
    to = mail.get('to')
    to = ', '.join(to) if isinstance(to, list) else to
    print(f"{path.name}  to {to}: {mail.get('subject')}")
    for link in dict.fromkeys(re.findall(r'https?://[^\s"<>]+', mail.get('text') or mail.get('html') or '')):
        print(f'    {link}')
PY
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
      >> "$state/logs/releases.log" 2>&1 < /dev/null 9>&- &
    disown
  fi
  [[ -f $state/releases.pid ]] && kill "$(cat "$state/releases.pid")" 2>/dev/null || true
  python3 -m http.server --bind 127.0.0.1 --directory "$state/releases" "$releases_port" \
    >> "$state/logs/releases.log" 2>&1 < /dev/null 9>&- &
  echo $! > "$state/releases.pid"
}

# The VM host's own state, which is root's (a raw block device it formats,
# its jails and its VMs), apart from this stack's.
vm_state=${TILO_SH_VM_HOST_STATE:-/var/lib/tilo-sh-local-vm}

# Whether the VM host can run here, and if not why: it is privileged in
# production (KVM, its own cgroup tree, a raw block device it formats, a jail
# helper given capabilities), which rootless podman cannot give it, so it runs
# as root over its own state, through sudo.
# TILO_SH_VM_HOST=0 leaves it out on a machine that could run it, so the
# stack never asks for root there (check.sh reads the same variable: 1 adds
# the hosted Browser's steps, which need it).
vm_host_blocker() {
  [[ ${TILO_SH_VM_HOST:-} != 0 ]] || { echo 'TILO_SH_VM_HOST=0 leaves it out'; return; }
  if [[ $mode == native && ${TILO_SH_VM_HOST:-} != 1 ]]; then
    echo 'it runs from its image: TILO_SH_VM_HOST=1 starts it beside this stack, from the images TILO_SH_IMAGE_VM_HOST and TILO_SH_HOSTED_GUEST name'
    return
  fi
  [[ -e /dev/kvm ]] || { echo 'this machine has no /dev/kvm'; return; }
  if [[ $EUID != 0 ]] && ! sudo -n true 2>/dev/null; then
    echo 'it needs root (a raw block device for its state, cgroup delegation and setcap for its jail helper) and sudo here asks for a password; run: sudo -E env TILO_SH_STATE=$vm_state bash os/containers/tilo-sh/vm-host.sh up'
    return
  fi
  command -v t >/dev/null || { [[ -n ${TILO_SH_IMAGE_VM_HOST:-} && -n ${TILO_SH_HOSTED_GUEST:-} ]] || { echo 't is not here to build its images, and TILO_SH_IMAGE_VM_HOST and TILO_SH_HOSTED_GUEST name none'; return; }; }
  # Natively the host never builds its images: they are named, or t made them.
  if [[ $mode == native && ( -z ${TILO_SH_IMAGE_VM_HOST:-} || -z ${TILO_SH_HOSTED_GUEST:-} ) ]]; then
    echo 'natively it runs from the images TILO_SH_IMAGE_VM_HOST and TILO_SH_HOSTED_GUEST name, and one is not named'
    return
  fi
}

# vm-host RUN…: os/containers/tilo-sh/vm-host.sh, as root when this stack is
# not, over the VM host's own state; the images t built for this checkout, and
# the server this stack runs (its device, the secrets it mounts).
# A caller that names the images itself (t's checks hand them their outputs)
# has them used as named, archive or directory.
vm_host() {
  local sudo=()
  [[ $EUID == 0 ]] || sudo=(sudo -n)
  # Where the server that places here reads what the host writes for it: the
  # image's volumes with --images, the stack's own state natively.
  local secrets_view=/run/secrets workspaces_view=/run/tilo-workspaces
  if [[ $mode == native ]]; then secrets_view=$state/secrets workspaces_view=$state/server; fi
  "${sudo[@]}" env \
    TILO_SH_STATE="$vm_state" \
    "TILO_SH_IMAGE_VM_HOST=${TILO_SH_IMAGE_VM_HOST:-$(image_archive image-vm-host image.oci.tar)}" \
    "TILO_SH_HOSTED_GUEST=${TILO_SH_HOSTED_GUEST:-$state/images/$commit/image-hosted-guest}" \
    "TILO_VM_TICKET=${TILO_VM_TICKET:-}" \
    "TILO_SH_TILO_SERVER=$programs/tilo-server" \
    "TILO_SH_SERVER_SECRETS=$state/secrets" \
    "TILO_SH_SERVER_SECRETS_VIEW=$secrets_view" \
    "TILO_SH_SERVER_WORKSPACES_VIEW=$workspaces_view" \
    "TILO_SH_SERVER_STATE=$state/server/lounge/server" \
    "TILO_SH_SERVER_IROH=127.0.0.1:$iroh_port" \
    "TILO_SH_BROWSER_FREE_ACTIONS=${TILO_SH_BROWSER_FREE_ACTIONS:-}" \
    "TILO_SH_HOSTED_VM_SIZE=${TILO_SH_HOSTED_VM_SIZE:-}" \
    bash "$here/vm-host.sh" "$@" 9>&-
}

# The ticket this stack's VM host joins the person's devices with: printed on
# the stack's own person's device (`tilo device add`) at each
# start. The host spends it once and keeps its login on its own state disk,
# which is root's; a host that already holds one ignores the ticket it is given.
# A ticket lets a computer in, so no failure prints it.
host_ticket() {
  local issued
  issued=$(owner_person device add --server --json --no-wait) \
    || die 'the stack'"'"'s person printed no ticket for the VM host (tilo device add)'
  TILO_VM_TICKET=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["ticket"])' <<<"$issued") \
    || die "the stack's person printed no ticket (tilo device add --json answered no \"ticket\")"
}

# The tilo command to drive the stack with: TILO_SH_TILO_BIN, else this
# checkout's release build the site serves at /releases (with the daemon
# tilo-runtime it starts from beside itself), else the command the
# stack built for itself (the same one, before the site packs it), else a build
# left here. The stack's own build is where it runs; a build left here is not on
# the build server, so a command that acts on a stack there finds its command
# beside the stack.
find_tilo() {
  if [[ -n ${TILO_SH_TILO_BIN:-} ]]; then printf '%s' "$TILO_SH_TILO_BIN"; return; fi
  local tarball=$state/releases/tilo-cli/latest/tilo-linux-x86_64.tar.gz candidate
  if [[ -s $tarball ]]; then
    mkdir -p "$state/cli"
    tar -xzf "$tarball" -C "$state/cli" tilo tilo-runtime && { printf '%s' "$state/cli/tilo"; return; }
  fi
  for candidate in "$programs/tilo" "$repo/target/release/tilo" "$repo/target/debug/tilo"; do
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
# TILO_SH_SEED_MEMBERS (300) people join through its entry (the seed-lounge
# example), and one of them posts TILO_SH_SEED_MESSAGES (200) chat messages
# through the API server with a key of their own. Once per state, and as many
# as the chat should hold: what a post costs must not grow with what the chat
# already holds, and `seed-history.py` prints how long each one took as it
# goes, so a dearer last post than first is a regression to chase rather than
# a reason to seed less.
seed() {
  local workspace=$1 out=$state/seed
  if [[ -f $out/done ]]; then say "the Lounge is already seeded ($(cat "$out/done"))"; return; fi
  mkdir -p "$out"
  local seeder=${TILO_SH_SEEDER:-$repo/target/debug/examples/seed-lounge}
  if [[ ! -x $seeder ]]; then
    say 'building the seeder (cargo.sh build -p tilo-agent-session --example seed-lounge)'
    (cd "$repo" && "$here/cargo.sh" build -p tilo-agent-session --example seed-lounge >&2) \
      || die 'could not build the seeder; or set TILO_SH_SEEDER'
  fi
  local members
  members=$(curl -fsS -X POST "$server_origin/peer/v1/directory" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["workspaces"][0]["members"])')
  local want=$((${TILO_SH_SEED_MEMBERS:-300} - members))
  if ((want > 0)); then
    say "seeding $want members through the Lounge's entry"
    "$seeder" "$state/server/lounge/server" "$workspace" "$want" > "$out/members.json" \
      || die 'seeding the members failed'
  fi
  say 'seeding the history: a member posts chat through the API server'
  node "$here/integration/person.mjs" "$site_origin" "$server_origin" "$out/poster" \
    --key 'Lounge history' --tilo "$(find_tilo)" --api "$api_origin" > "$out/poster.json" \
    || die "the poster did not get a key (see $out/poster)"
  local key
  key=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["key"])' "$out/poster.json")
  # The server's records live in its volume, which the container's user owns.
  sdk_python "$here/integration/seed-history.py" "$api_origin" "$key" "$state/server/lounge/server" \
    "${TILO_SH_SEED_MESSAGES:-200}" > "$out/history.json" || die 'seeding the history failed'
  printf '%s members, %s\n' "$((members + (want > 0 ? want : 0)))" "$(cat "$out/history.json")" > "$out/done"
  say "seeded: $(cat "$out/done")"
}

# -- the native stack -------------------------------------------------------
#
# The same services as plain processes: the programs t builds for this
# checkout, each started the way its image starts it (the image's entrypoint
# where it has one), with the image's ENV and the manifest's variables
# (server_env, api_env, web_env), only the paths that live in the image
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
# stack has to be told where that is on this machine, not left to guess. So
# does a NAME the image does not set: its line would be printed nowhere, and
# what it was to tell the program would never reach it.
image_env_here() {
  local file=$1 line name value moved pair placed=' '
  shift
  while IFS= read -r line; do
    name=${line%%=*} value=${line#*=} moved=
    for pair in "$@"; do
      [[ ${pair%%=*} == "$name" ]] && { moved=${pair#*=}; placed+="$name "; break; }
    done
    if [[ -n $moved ]]; then
      printf '%s=%s\n' "$name" "$moved"
    elif [[ $value == /* ]]; then
      die "$file sets $name=$value, a path in the image; stack.sh's native mode has to say where that is here"
    else
      printf '%s\n' "$line"
    fi
  done < <(containerfile_env "$file")
  for pair in "$@"; do
    [[ $placed == *" ${pair%%=*} "* ]] \
      || die "stack.sh hands ${pair%%=*} to a program it starts from $file, which sets no ${pair%%=*}: it would reach nobody, so the image's ENV has to name it"
  done
}

# Caddy: production's tls container and the web entry image both run it. The
# one the caller built (TILO_SH_CADDY, which t's tilo-sh-caddy hands the checks
# so their bring-up downloads nothing), else the one on PATH, else the pinned
# release (caddy.env), fetched once and checked against its published SHA-512.
. "$here/caddy.env"
find_caddy() {
  if [[ -n ${TILO_SH_CADDY:-} ]]; then
    [[ -f $TILO_SH_CADDY && -x $TILO_SH_CADDY ]] || die "TILO_SH_CADDY names $TILO_SH_CADDY, which is not an executable file"
    printf '%s' "$TILO_SH_CADDY"; return
  fi
  if command -v caddy >/dev/null; then command -v caddy; return; fi
  local dir=$native/caddy-$CADDY_VERSION
  if [[ ! -x $dir/caddy ]]; then
    say "fetching Caddy $CADDY_VERSION (no caddy on PATH)"
    local fetch=${native:?}/caddy-fetch
    rm -rf "${fetch:?}"
    mkdir -p "$fetch" "$dir"
    curl -fsSL -o "$fetch/caddy.tar.gz" \
      "https://github.com/caddyserver/caddy/releases/download/v$CADDY_VERSION/caddy_${CADDY_VERSION}_linux_amd64.tar.gz" \
      || die 'could not fetch Caddy; install one (dnf install caddy) or set TILO_SH_CADDY'
    printf '%s  %s\n' "$CADDY_SHA512" "$fetch/caddy.tar.gz" | sha512sum -c --quiet - >&2 \
      || die "the fetched Caddy is not release $CADDY_VERSION's"
    tar -xzf "$fetch/caddy.tar.gz" -C "$dir" caddy
    rm -rf "${fetch:?}"
  fi
  printf '%s' "$dir/caddy"
}

# The programs, for every service at once: one build, so the features Cargo
# unifies are the same each time and a change relinks only what it touches.
# A caller that built them already (TILO_SH_PROGRAMS) compiles nothing.
build_programs() {
  local began=$SECONDS
  if [[ -n ${TILO_SH_PROGRAMS:-} ]]; then
    # Every service's own start says what it is missing; the API server is the one
    # this stack always runs from here.
    [[ -x $programs/tilo-api ]] || die "TILO_SH_PROGRAMS names $programs, which holds no tilo-api"
    # The API server starts a task's tilolang programs in the program beside it. A caller's
    # build without one still comes up; its `tilo` tool then answers that none is installed.
    [[ -x $programs/tilolang-process ]] \
      || say "$programs holds no tilolang-process beside tilo-api: a task's tilolang programs will not start"
    say "programs given ($programs, built by the caller)"
    return
  fi
  if from_t; then
    t_build programs tilo-sh-programs lounge-fonts
    copy_programs "$t_fetched/target/publish/tilo-sh-programs"
    say "programs built ($((SECONDS - began))s)"
    return
  fi
  local from=$repo/target/$profile
  if on_builder; then
    builder_build programs
    from=$repo/target/$builder_stage/programs
  else
    say "building the programs ($cargo build --profile $profile; only what changed compiles)"
    # Three builds, as the images build them: Cargo turns on every feature any package of a
    # build asks of a shared crate for all of them. The server and its worker apart from the
    # rest, or the server would link the desktop libraries the daemon needs and the Lounge image
    # does not carry; the stand-in mail server, an example, apart too, since it brings its
    # package's dev-dependencies, and a server built with test-only features loads fixture fonts
    # its app workers cannot.
    (cd "$repo" && TILO_SH_CARGO=$cargo "$here/cargo.sh" build --profile "$profile" --features tilo-server/agents \
      -p tilo-server -p discord-worker --bin tilo-server --bin discord-worker >&2 \
      && TILO_SH_CARGO=$cargo "$here/cargo.sh" build --profile "$profile" \
        -p tilo-api -p fake-discord -p fake-model-providers -p tilo-cli -p tilo-runtime -p tilolang-process \
        --bin tilo-api --bin fake-discord --bin fake-model-providers --bin tilo --bin tilo-runtime --bin tilolang-process >&2 \
      && TILO_SH_CARGO=$cargo "$here/cargo.sh" build --profile "$profile" \
        -p tilo-account-probe --example mail_stand_in >&2) \
      || die "the build failed (above); if it built but did not bring the programs back, TILO_SH_CARGO names another cargo drop-in to build with"
  fi
  copy_programs "$from"
  say "programs built ($((SECONDS - began))s)"
}

# Whether the cargo drop-in builds on a build server and runs a program there,
# bringing files of its build directory back (`$cargo exec --fetch`): the
# builds then run there in one command (builder_build).
on_builder() {
  if [[ -z ${builder_capable:-} ]]; then
    local help
    builder_capable=0
    if help=$($cargo exec --help 2>/dev/null) && [[ $help == *--fetch* ]]; then builder_capable=1; fi
  fi
  [[ $builder_capable == 1 ]]
}

# builder_build programs|bundle…: one command on the build server
# (build-on-builder.sh) that builds what is named, at once, and stages the
# programs and the bundle's module in one directory of this checkout's build
# directory there, which one fetch mirrors into ./target/tilo-sh: only the
# files that changed travel. What one bring-up built already is not built
# again by it.
built_on_builder=
builder_stage=tilo-sh
builder_build() {
  local what=() item began=$SECONDS run
  for item in "$@"; do
    [[ " $built_on_builder " == *" $item "* ]] || what+=("$item")
  done
  [[ ${#what[@]} -gt 0 ]] || return 0
  run=$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')
  local args=("$run" "$builder_stage" "${what[@]}")
  say "building the ${what[*]} on the build server ($cargo exec build-on-builder.sh; only what changed compiles)"
  # A drop-in that mirrors a directory back takes the stage's name alone; one that copies the files
  # a glob matches (a routing drop-in hands the call to a remote builder, whose exec copies files)
  # needs the stage's files named, since a stage is a directory of files below it.
  local fetch=$builder_stage help
  if ! remote_capable || { help=$($cargo exec --help 2>/dev/null) && [[ $help == *'a glob'* ]]; }; then
    fetch=$builder_stage/'**'
  fi
  (cd "$repo" && $cargo --fetch "$fetch" exec bash os/containers/tilo-sh/build-on-builder.sh "${args[@]}" >&2) \
    || die "the build on the build server failed (above)"
  [[ $(cat "$repo/target/$builder_stage/run" 2>/dev/null) == "$run" ]] \
    || die "$repo/target/$builder_stage is not what this build made on the build server: did the fetch bring it back?"
  built_on_builder+=" ${what[*]}"
  say "${what[*]} built on the build server and fetched ($((SECONDS - began))s)"
}

# Where the fetch leaves the bundle's module.
bundle_stage() {
  echo "$builder_stage/bundle"
}

# The programs every service runs: a copy of the build's, not its files
# themselves, since the next build rewrites the files it changed in place
# while a service runs from these. Copy-on-write where the file system shares
# blocks. A program that did not change keeps its file, so what is laid out
# from it (the server's context, the CLI's release) is kept too; one that did
# is renamed into place, and a service running the old one keeps it.
copy_programs() {
  local from=$1 file stand_in=$1/examples/mail_stand_in
  mkdir -p "$programs"
  for file in tilo-server discord-worker tilo-api fake-discord fake-model-providers tilo tilo-runtime tilolang-process; do
    [[ -x $from/$file ]] || die "the build left no $from/$file; TILO_SH_CARGO names another cargo drop-in to build with"
    cmp -s "$from/$file" "$programs/$file" && continue
    cp --reflink=auto "$from/$file" "$programs/.$file.next" || die "could not copy $file"
    mv -f "$programs/.$file.next" "$programs/$file"
  done
  # The stand-in mail server is an example, since it makes a certificate with
  # the crate's test-time dependencies; the stack starts it as mail-stand-in.
  [[ -x $stand_in ]] || die "the build left no $stand_in; TILO_SH_CARGO names another cargo drop-in to build with"
  if ! cmp -s "$stand_in" "$programs/mail-stand-in"; then
    cp --reflink=auto "$stand_in" "$programs/.mail-stand-in.next" || die 'could not copy mail-stand-in'
    mv -f "$programs/.mail-stand-in.next" "$programs/mail-stand-in"
  fi
}

# What a layout made from FILEs (and this checkout's commit) was made from:
# their identities, which copy_programs keeps for a program that did not
# change.
made_from() {
  { printf '%s\n' "$commit"; stat -L -c '%n %s %Y %i' "$@"; } | sha256sum | cut -c1-16
}

# The server's files as the image's context lays them out (lounge-context.sh,
# the image's own build step): the programs, the app surfaces from
# the apps' history and the product font catalog (lounge-fonts.sh, made again when its sources change).
# TILO_SH_LOUNGE names a context laid out already, which is what a check hands
# the stack: laying it out reads the apps' history and the catalog, seconds a
# check has no reason to spend twice.
lay_out_lounge() {
  if [[ -z $lounge ]]; then
    # The catalog t's lounge-fonts check fetched with the programs, else made
    # here, and made again whenever what it is made from changed: a build
    # refuses a catalog whose manifest is not its own ("manifest differs from
    # this Tilo build"), so one made for an older checkout leaves text unlaid.
    local fonts=$native/fonts
    if from_t; then
      fonts=$t_fetched/target/publish/lounge/fonts
    else
      local made sources
      sources=$(cd "$repo" && find crates/ui/ui-core/assets/fonts tools/font-catalog.py \
        tools/ci/publish/lounge-fonts.sh -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum | cut -c1-16)
      made=$(cat "$native/fonts.made-from" 2>/dev/null || true)
      if [[ ! -f $native/fonts/manifest.toml || $made != "$sources" ]]; then
        say 'preparing the product font catalog for this checkout (tools/ci/publish/lounge-fonts.sh)'
        bash "$repo/tools/ci/publish/lounge-fonts.sh" "$native/fonts" >&2 || die 'could not prepare the fonts'
        printf '%s\n' "$sources" > "$native/fonts.made-from"
      fi
    fi
    # Laid out again only when what it is made from changed: the server's
    # programs, the catalog, the files the image's context takes and this
    # commit, whose history the app surfaces come from.
    local stamp container=$repo/os/containers/tilo-lounge
    stamp=$(made_from "$programs/tilo-server" "$programs/discord-worker" "$fonts/manifest.toml" \
      "$repo/tools/ci/publish/lounge-context.sh" "$container/Containerfile" "$container/Containerfile.dockerignore" \
      "$container/entrypoint.sh" "$repo/apps/discord/bridge/program.json")
    if [[ $(cat "$native/lounge.made-from" 2>/dev/null) != "$stamp" || ! -x $native/lounge/bin/tilo-server ]]; then
      TILO_LOUNGE_SURFACE_COMMIT=$commit \
        bash "$repo/tools/ci/publish/lounge-context.sh" "$native/lounge" "$programs" "$fonts" >/dev/null \
        || die 'could not lay out the server (lounge-context.sh)'
      printf '%s\n' "$stamp" > "$native/lounge.made-from"
    fi
    lounge=$native/lounge
  fi
  [[ -x $lounge/bin/tilo-server ]] || die "TILO_SH_LOUNGE names $lounge, which holds no bin/tilo-server"
  [[ -f $lounge/entrypoint.sh ]] || die "TILO_SH_LOUNGE names $lounge, which holds no entrypoint.sh"
  # The image installs the catalog at /usr/share/fonts/tilo; outside it a Tilo
  # program looks beside itself, and TILO_FONT_CATALOG names where the catalog
  # is instead. The variable is the one the programs read, so a context this
  # stack was handed — a check's, laid out and published by t — stays read-only
  # to it; the link is for anything that only looks beside the program.
  [[ -d $lounge/fonts ]] || die "TILO_SH_LOUNGE names $lounge, which holds no fonts"
  font_catalog=$lounge/fonts
  if [[ -w $lounge/bin ]]; then ln -sfn ../fonts "$lounge/bin/fonts"; fi
}

# The bundle the web entry serves (build.sh);
# TILO_SH_BUNDLE names a built one instead. By default build.sh's quick
# profile, the development one `cargo web` builds too, compiled and bound on
# the build server and brought up to date in place (--update): a module that
# did not change is not bound again, and only the files that changed come back. The site the web
# entry serves is laid out from it afterwards (lay_out_web), so a build that
# stops halfway is never served. With TILO_SH_BUILD=t, t's
# web-entry-bundle.
build_bundle() {
  if [[ -n ${TILO_SH_BUNDLE:-} || -n ${TILO_SH_SITE:-} ]]; then return; fi
  local began=$SECONDS out=$native/bundle-local
  if from_t; then
    t_build "the web entry's bundle" web-entry-bundle
    t_bundle
    say "bundle built ($((SECONDS - began))s)"
    return
  fi
  local from=()
  if on_builder; then
    builder_build bundle
    from=(--wasm-from "$repo/target/$(bundle_stage)")
    say "laying out the web entry's bundle around the module the build server made (build.sh --wasm-from)"
  else
    say "building the web entry's bundle (build.sh --profile quick --update, compiled by $cargo)"
  fi
  mkdir -p "$out"
  CARGO=$here/cargo.sh TILO_SH_CARGO=$cargo TILO_SOURCE_REVISION=$commit \
    "$repo/crates/browser/tilo-browser-runtime/scripts/build.sh" "$out" \
    --profile quick --update "${from[@]}" >&2 \
    || die 'the web entry bundle did not build (above)'
  [[ -L $native/bundle ]] || rm -rf "${native:?}/bundle"
  ln -sfn "${out##*/}" "$native/bundle"
  say "bundle built ($((SECONDS - began))s)"
}

# Whether DIR holds a finished desktop bundle with the module the page starts
# from.
bundle_whole() {
  [[ -f $1/build.json && -s $1/pkg/tilo_browser_runtime.js && -s $1/pkg/tilo_browser_runtime_bg.wasm ]]
}

# -- t's builds ---------------------------------------------------------------
# With TILO_SH_BUILD=t the programs and the desktop bundle are what t's
# checks build: tilo-sh-programs (every program, in one lounge-check
# build), lounge-fonts (the server's font catalog) and web-entry-bundle
# (the bundle without its whole-program link: the compile, wasm-bindgen, the
# split and wasm-opt, all on t). Each check is cached by what it read, so a
# change reruns the checks it reaches and nothing else, and this machine only
# fetches: into the same directory each time, where t brings only the files
# that changed.
from_t() { [[ -z ${TILO_SH_PROGRAMS:-} && $build == t ]]; }
t_fetched=$native/t

# t_build WHAT CHECK…: one t run of the CHECKs for this checkout, and what
# each CHECK made fetched into $t_fetched: `t outputs` gives one check's own
# outputs alone, so each named check is fetched.
# A checkout with no changes is built as its commit (--rev): git's files,
# never ones edited while the build runs. One with changes is built as t
# finds it when the run starts, which is when t takes its copy.
t_build() {
  local what=$1 began=$SECONDS run rev=()
  shift
  [[ -n $(git -C "$repo" status --porcelain 2>/dev/null) ]] || rev=(--rev "$commit")
  say "building $what on t (t test ${rev[*]} $*)"
  run=$(cd "$repo" && t --json test "${rev[@]}" --detach "$@" \
    | python3 -I -c 'import json, sys; print(json.load(sys.stdin)["run"])') \
    || die "t did not start a run of $*"
  # Its results as they come, without the advice about other checks t ends on.
  (cd "$repo" && t follow "$run" 2>&1 | sed -u '/^opt-in checks/,$d' >&2; exit "${PIPESTATUS[0]}") \
    || die "t could not build $what (run $run; t log CHECK says why)"
  say "$what built on t ($((SECONDS - began))s)"
  began=$SECONDS
  mkdir -p "$t_fetched" "$state/logs"
  local check log
  for check in "$@"; do
    log=$state/logs/t-outputs-${check//\//-}.log
    (cd "$repo" && t outputs "$check" --run "$run" --to "$t_fetched" > "$log" 2>&1) \
      || { tail -5 "$log" >&2; die "t could not give run $run's outputs of $check ($log)"; }
    say "$check fetched: $(grep -cv ' (unchanged)$' "$log") files new or changed, $(grep -c ' (unchanged)$' "$log") kept ($((SECONDS - began))s)"
  done
}

# The bundle as its build.json lists it: t's fetch keeps files an earlier
# bundle had, which this one must not serve.
t_bundle() {
  local from=$t_fetched/target/publish/web-entry/bundle next=$native/bundle.next
  bundle_whole "$from" || die "t's outputs hold no whole desktop bundle ($from)"
  rm -rf "$next"
  mkdir -p "$next"
  python3 -I -c '
import json, subprocess, sys
src, dst = sys.argv[1:]
names = [*json.load(open(src + "/build.json"))["files"], "build.json"]
subprocess.run(["cp", "--reflink=auto", "--parents", "--", *names, dst], cwd=src, check=True)
' "$from" "$next" || die "could not copy the bundle from $from"
  rm -rf "${native:?}/bundle"
  mv "$next" "$native/bundle"
}

# The site as the image holds it: laid out by layout.py, precompressed by
# compress.sh, the installer beside it (web-entry-context.sh, Containerfile).
# TILO_SH_SITE names a site laid out already (web-entry-context.sh's output:
# site/, install.sh), which a check hands the stack: the files the image
# serves, laid out and precompressed once, not per bring-up.
lay_out_web() {
  local container=$repo/crates/browser/tilo-browser-runtime/container
  mkdir -p "$native/web"
  if [[ -n ${TILO_SH_SITE:-} ]]; then
    web_site=$TILO_SH_SITE/site
    web_install=$TILO_SH_SITE
    [[ -f $web_site/index.html ]] || die "TILO_SH_SITE names $TILO_SH_SITE, which holds no site/index.html"
    [[ -f $web_install/install.sh ]] || die "TILO_SH_SITE names $TILO_SH_SITE, which holds no install.sh"
    say "the site is the one given ($web_site)"
    return
  fi
  local bundle=${TILO_SH_BUNDLE:-$native/bundle}
  bundle_whole "$bundle" || die "no whole desktop bundle at $bundle (build.json and pkg/tilo_browser_runtime{.js,_bg.wasm}); stack.sh restart web builds one"
  # The page and the server speak one protocol only when they come from one
  # commit: a page built before a change to setup codes, served beside a server
  # built after it, showed codes the server never knew.
  local built
  built=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1])).get("revision", ""))' "$bundle/build.json")
  [[ $built == "$commit" || $commit == unknown || ${TILO_SH_BUNDLE_ANY_COMMIT:-0} == 1 ]] \
    || die "the desktop bundle at $bundle was built from ${built:0:11}, the programs from ${commit:0:11}: build the bundle from this checkout (stack.sh restart web), or TILO_SH_BUNDLE_ANY_COMMIT=1 to serve it anyway"
  local stamp
  stamp=$(sha256sum "$bundle/build.json" | cut -c1-16)
  if [[ $(cat "$native/web/stamp" 2>/dev/null) != "$stamp" ]]; then
    rm -rf "${native:?}/web"
    mkdir -p "$native/web/install"
    python3 -I "$container/layout.py" "$bundle" "$native/web/site" >&2 || die 'layout.py failed'
    if command -v brotli >/dev/null && command -v zstd >/dev/null; then
      local began=$SECONDS
      # Each file's compressed forms are kept by its content's hash: only
      # the modules that changed are compressed again. What the site no longer
      # links is dropped.
      sh "$container/compress.sh" "$native/web/site" fast "$native/compressed" || die 'compress.sh failed'
      find "$native/compressed" -type f -links 1 -delete
      say "site precompressed ($((SECONDS - began))s)"
    else
      say 'no brotli or zstd here: the site is served uncompressed (the image serves it precompressed)'
    fi
    printf '%s\n' "$stamp" > "$native/web/stamp"
  fi
  mkdir -p "$native/web/install"
  # The installer downloads from the site that serves it, as tilo.sh's does
  # from tilo.sh: here this site's /releases, which serve this checkout's
  # tilo command, never the one published on tilo.sh. It trusts the key this
  # site signs them with in place of Tilo's release key, so the command the
  # first screen gives works as pasted, in any shell.
  local installer releases=https://tilo.sh/releases key
  installer=$(<"$repo/dist/install/tilo.sh") key=$(<"$repo/keys/cosign.pub")
  [[ $installer == *"$releases"* && $installer == *"$key"* ]] \
    || die "dist/install/tilo.sh no longer names $releases and keys/cosign.pub; say here where it downloads from and what it trusts"
  installer=${installer//"$releases"/"$site_origin/releases"}
  printf '%s\n' "${installer//"$key"/"$(<"$state/release.pub")"}" > "$native/web/install/install.sh"
  web_site=$native/web/site
  web_install=$native/web/install
}

# The site's /releases: this checkout's tilo command, packed as a release packs
# it (cli-release.sh pack), or the release a caller built (TILO_SH_CLI_DIR, the
# cli-x86_64 check's output).
lay_out_releases() {
  local dir=$state/releases/tilo-cli/latest
  mkdir -p "$dir"
  if [[ -n ${TILO_SH_CLI_DIR:-} ]]; then
    cp "$TILO_SH_CLI_DIR"/tilo-* "$dir/" || die "TILO_SH_CLI_DIR holds no tilo-* release files"
  else
    local stamp
    stamp=$(made_from "$programs/tilo")
    if [[ $(cat "$dir/.made-from" 2>/dev/null) != "$stamp" || ! -s $dir/tilo-linux-x86_64.tar.gz ]]; then
      bash "$repo/tools/ci/publish/cli-release.sh" pack "$programs/tilo" "$dir/tilo-linux-x86_64.tar.gz" \
        || die 'could not pack the tilo command'
      printf '%s\n' "$stamp" > "$dir/.made-from"
    fi
  fi
  sign_releases "$dir"
}

# DIR's archives listed in SHA256SUMS and that signed, as a release is
# (cli-release.sh sums, sign), so the site's installer takes them: with this
# stack's own throwaway key, not Tilo's release key, which the installer this
# site serves trusts instead (lay_out_web).
sign_releases() {
  local dir=$1 key=$state/secrets/release-signing.pem
  if [[ ! -s $key ]]; then
    chmod u+w "$state/secrets"
    (umask 077 && openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out "$key" 2>/dev/null) \
      || die 'could not make the local release key (openssl)'
  fi
  openssl pkey -in "$key" -pubout -out "$state/release.pub" 2>/dev/null || die 'could not read the local release key'
  (cd "$dir" && LC_ALL=C sha256sum tilo-*.tar.gz > SHA256SUMS) || die "no tilo archive in $dir"
  openssl dgst -sha256 -sign "$key" -out "$dir/SHA256SUMS.sig" "$dir/SHA256SUMS" \
    || die 'could not sign the local release'
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
  # The server reads its own log filter, not RUST_LOG.
  [[ -z ${TILO_SERVER_LOG:-} ]] || printf 'TILO_SERVER_LOG=%s\n' "$TILO_SERVER_LOG"
  [[ -z ${RUST_BACKTRACE:-} ]] || printf 'RUST_BACKTRACE=%s\n' "$RUST_BACKTRACE"
}

# run_native NAME DIR COMMAND…: COMMAND as the service NAME, in a session of
# its own, with only the environment $runs/NAME.env sets. bwrap puts the
# stack's trust bundle (this machine's roots) at the one
# path Tilo's programs read, for this process alone, as the containers mount
# it; everything else it sees is this machine as it is.
run_native() {
  local name=$1 dir=$2
  shift 2
  stop_native "$name"
  local envs=()
  mapfile -t envs < "$runs/$name.env"
  # bwrap as the caller's PATH finds it (the preflight's check), since the service's own
  # environment, which env -i starts it with, need not name where it is.
  setsid env -i "${envs[@]}" \
    "$(command -v bwrap)" --dev-bind / / --ro-bind "$state/trust-bundle.pem" "$ca_path" --chdir "$dir" -- "$@" \
    >> "$state/logs/$name.log" 2>&1 < /dev/null 9>&- &
  echo $! > "$runs/$name.pid"
}

stop_native() {
  local pidfile=$runs/$1.pid pid
  # The server first lets its agents' turns that are going end (five minutes at
  # most), so a restart never cuts one off halfway.
  local patience=5
  [[ $1 != server ]] || patience=310
  [[ -f $pidfile ]] || return 0
  pid=$(cat "$pidfile")
  if kill -0 "$pid" 2>/dev/null; then
    kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
    local deadline=$((SECONDS + patience))
    while kill -0 "$pid" 2>/dev/null && ((SECONDS < deadline)); do sleep 0.1; done
    kill -KILL -- "-$pid" 2>/dev/null || true
    # Gone, every process of its group: one still exiting holds the ports the
    # next start binds, which a restart then finds taken.
    deadline=$((SECONDS + 10))
    while { kill -0 -- "-$pid" 2>/dev/null || kill -0 "$pid" 2>/dev/null; } && ((SECONDS < deadline)); do
      sleep 0.1
    done
  fi
  rm -f "$pidfile"
}

start_native_tls() {
  local caddy
  caddy=$(find_caddy)
  cp "$here/Caddyfile" "$state/tls/Caddyfile"
  # The tls container's environment: the Caddyfile's ports, and Caddy's data
  # and config where the container mounts them.
  { private_env tls "$(dirname "$caddy")"
    printf '%s\n' "XDG_DATA_HOME=$state/tls" "XDG_CONFIG_HOME=$state/tls/config" \
      "TILO_SH_SERVER_PORT=$server_port" "TILO_SH_LIVE_PORT=$live_port" "TILO_SH_PACKET_PORT=$packet_port" \
      "TILO_SH_ROUTES_PORT=$routes_port" "TILO_SH_PLAIN_PORT=$plain_port" "TILO_SH_CADDY_HTTP_PORT=$caddy_http_port" \
      "TILO_SH_SITE_PORT=$site_port" "TILO_SH_WEB_PORT=$web_port"
  } > "$runs/tls.env"
  [[ -f $state/trust-bundle.pem ]] || trust_bundle
  run_native tls "$state/tls" "$caddy" run --config "$state/tls/Caddyfile" --adapter caddyfile
}

start_native_server() {
  local lounge=${lounge:-$native/lounge}
  { private_env server "$lounge/bin"
    image_env_here "$repo/os/containers/tilo-lounge/Containerfile" \
      "TILO_WORKSPACES_STATE=$state/server" "TILO_SECRETS_DIR=$state/server/secrets"
    # Where the image keeps the app surfaces the server publishes at start,
    # and the product font catalog its components lay text out from.
    printf '%s\n' "TILO_SURFACES=$lounge/surfaces"
    printf '%s\n' "TILO_FONT_CATALOG=${font_catalog:-$lounge/fonts}"
    server_env
  } > "$runs/server.env"
  run_native server "$state/server" sh "$lounge/entrypoint.sh"
}

start_native_api() {
  host_info > /dev/null
  { private_env api "$programs"
    api_env
  } > "$runs/api.env"
  run_native api "$state/api" "$programs/tilo-api"
}

start_native_web() {
  local workspace=$1 caddy container=$repo/crates/browser/tilo-browser-runtime/container info
  caddy=$(find_caddy)
  info=$(host_info)

  { private_env web "$(dirname "$caddy")"
    image_env_here "$container/Containerfile" \
      "XDG_DATA_HOME=$runs/web/data" "XDG_CONFIG_HOME=$runs/web/config"
    # The image's paths and port, here (entrypoint.sh).
    printf '%s\n' "TILO_WEB_SITE=$web_site" "TILO_WEB_INSTALL=$web_install" \
      "TILO_WEB_RUN=$runs/web/entry" "TILO_WEB_CADDYFILE=$container/Caddyfile" \
      TILO_WEB_BIND=127.0.0.1 "TILO_WEB_PORT=$web_port"
    web_env "$workspace" "$info"
  } > "$runs/web.env"
  run_native web "$native/web" sh "$container/entrypoint.sh"
}

# The releases are what the package registry would serve: a plain file server.
start_native_releases() {
  mkdir -p "$runs/releases"
  printf '%s\n' "PATH=/usr/bin:/bin" "HOME=$runs/releases" > "$runs/releases.env"
  run_native releases "$state/releases" python3 -m http.server --bind 127.0.0.1 "$releases_port"
}

# The fake OpenAI sign-in and model providers, where TILO_SH_FAKE_MODELS=1
# asks for them: the server signs people in to OpenAI there.
start_native_models() {
  : > "$state/logs/models.log"
  private_env models "$programs" > "$runs/models.env"
  run_native models "$runs/models" "$programs/fake-model-providers" --listen "127.0.0.1:$models_port"
  local deadline=$((SECONDS + 60))
  until grep -q '^OPENAI_ISSUER=' "$state/logs/models.log" 2>/dev/null; do
    ((SECONDS < deadline)) || die "the fake model providers did not start within a minute: $(cat "$state/logs/models.log")"
    sleep 0.2
  done
}

start_native_discord() {
  : > "$state/logs/discord.log"
  private_env discord "$programs" > "$runs/discord.env"
  run_native discord "$runs/discord" "$programs/fake-discord" --rest "127.0.0.1:$discord_rest_port" \
    --gateway "127.0.0.1:$discord_gateway_port" --control "127.0.0.1:$discord_control_port"
  local deadline=$((SECONDS + 60))
  until grep -q '^DISCORD_API_BASE=' "$state/logs/discord.log" 2>/dev/null; do
    ((SECONDS < deadline)) || die "the fake Discord did not start within a minute: $(cat "$state/logs/discord.log")"
    sleep 0.2
  done
  discord_api=http://127.0.0.1:$discord_rest_port
  discord_secrets
}

# A step's limit is what the step needs on a machine that is not busy with
# something else: every wait here is local, and one that runs past it says
# which step and where to read why, instead of hanging on the whole check's
# timeout.
wait_server() {
  wait_server_up
  wait_for 'tls (the server through Caddy)' "$server_origin/mode" 30
}

wait_api() { wait_for 'API server (its health check)' "http://127.0.0.1:$api_port/health" 20; }

wait_web() { wait_for 'web (its health check)' "$site_health" 30 -H "$site_host_header"; }

native_preflight() {
  # Root is the job sandbox's identity in a check, which is where TILO_SH_STATE
  # is named; on a developer's machine root would leave root's files behind.
  [[ $EUID != 0 || -n ${TILO_SH_STATE:-} ]] \
    || die 'run it as a regular user, or name the state it may own (TILO_SH_STATE)'
  if [[ -z ${TILO_SH_PROGRAMS:-} ]]; then
    if from_t; then command -v t >/dev/null || die 'needs t, which builds the programs (TILO_SH_BUILD=t)'
    else command -v "${cargo%% *}" >/dev/null || die "needs $cargo, which builds the programs (TILO_SH_CARGO names another cargo drop-in)"; fi
  fi
  command -v bwrap >/dev/null \
    || die 'needs bwrap (bubblewrap: dnf install bubblewrap), which gives the server and the API server the local trust bundle'
  command -v openssl >/dev/null \
    || die 'needs openssl (dnf install openssl), which makes the key the stack signs its releases of the tilo command with'
  [[ -f $ca_path ]] || die "this machine has no trust bundle at $ca_path, the one path Tilo's programs read"
}

# The server's own device, which its VM host's member takes the workspaces' runs
# from: made before either starts, so the server starts once with the host
# already placed.
host_device_key() {
  [[ -s $state/server/lounge/server/host-device.key ]] && return 0
  mkdir -p "$state/server/lounge/server"
  local key
  key=$(od -An -tx1 -N32 /dev/urandom | tr -d ' \n')
  (umask 077 && printf "$(printf '%s' "$key" | sed 's/../\\x&/g')" > "$state/server/lounge/server/host-device.key")
}

up_native() {
  local discord=$1 began=$SECONDS phase=$SECONDS
  native_preflight
  mkdir -p "$state"/{logs,server,secrets,api,tls/caddy,tls/config} "$native" "$runs"
  # The bundle builds beside the programs, on every bring-up: what did not
  # change is kept, so an unchanged one takes seconds. On the build server both
  # are one command; from t, two runs of checks at once.
  local bundle_pid=
  if [[ -z ${TILO_SH_BUNDLE:-} && -z ${TILO_SH_SITE:-} ]]; then
    if [[ -z ${TILO_SH_PROGRAMS:-} ]] && ! from_t && on_builder; then
      builder_build programs bundle
    else
      build_bundle > "$state/logs/bundle.log" 2>&1 &
      bundle_pid=$!
    fi
  fi
  build_programs
  lay_out_lounge
  lay_out_releases
  if [[ -n $bundle_pid ]]; then
    say "waiting for the desktop bundle (stack.sh logs bundle)"
    wait "$bundle_pid" || { tail -30 "$state/logs/bundle.log" >&2; die 'the desktop bundle did not build'; }
  elif [[ -z ${TILO_SH_BUNDLE:-} && -z ${TILO_SH_SITE:-} ]]; then
    build_bundle
  fi
  lay_out_web
  say "built and laid out ($((SECONDS - began))s)"
  echo native > "$state/mode"
  use_mode native
  echo "${TILO_SH_ADMISSION:-invite}" > "$state/admission"
  make_secrets
  # The stack's own TLS first: the VM host's member device joins the workspace
  # at the server, whose home relay is that TLS origin, so the host is given the
  # authority that signed it — which this makes. The host starts before the
  # server, which reads where to place a workspace once, when it starts.
  start_mail
  # Before the TLS and the bundle, so its certificate is trusted by the server
  # that signs a mail account in.
  [[ ${TILO_SH_MAIL_STAND_IN:-0} != 1 ]] || start_mail_stand_in
  start_native_tls
  trust_bundle
  # After the trust bundle, which every service reads.
  [[ -z ${discord:-} ]] || start_native_discord
  [[ ${TILO_SH_FAKE_MODELS:-0} != 1 ]] || start_native_models
  say "tls up ($((SECONDS - phase))s)"
  phase=$SECONDS
  local vm_skipped
  vm_skipped=$(vm_host_blocker)
  if [[ -z $vm_skipped ]]; then
    [[ -n ${TILO_SH_IMAGE_VM_HOST:-} ]] || die 'TILO_SH_IMAGE_VM_HOST names no VM host image'
    [[ -n ${TILO_SH_HOSTED_GUEST:-} ]] || die 'TILO_SH_HOSTED_GUEST names no guest image'
    host_ticket
    host_device_key
    say 'starting the VM host (Firecracker, as root)'
    vm_host up || die 'the VM host did not start; sudo -E bash '"$here"'/vm-host.sh status says why'
    say "vm-host up ($((SECONDS - phase))s)"
    phase=$SECONDS
  fi
  start_native_server
  wait_server
  local workspace
  workspace=$(cat "$state/server/lounge/workspace-id") || die 'the server kept no workspace id'
  say "server ready: the Lounge is $workspace ($((SECONDS - phase))s, $((SECONDS - began))s in)"
  phase=$SECONDS
  start_native_api
  wait_api
  say "API server up ($((SECONDS - phase))s)"
  phase=$SECONDS
  start_native_releases
  start_native_web "$workspace"
  wait_web
  say "web up ($((SECONDS - phase))s)"
}

# restart [--detach] NAME…: each NAME rebuilt and started again, in the order
# given. With --detach everything named is built first, here, so a build that
# fails says so and leaves every service as it is; then the restarts follow on
# their own, in a session of their own that outlives the caller
# (logs/restart.log), and this returns at once. That is how a service's own
# program restarts it: an agent of the stack's server, from its Terminal, has
# its turn end before the server stops, which waits for it.
restart() {
  local detach=0 names=() name
  while [[ $# -gt 0 ]]; do
    case $1 in
      --detach) detach=1 ;;
      -*) die "restart takes --detach and service names, not $1" ;;
      *) names+=("$1") ;;
    esac
    shift
  done
  [[ ${#names[@]} -gt 0 ]] || die 'restart [--detach] NAME…: server, api, web, tls, discord or releases'
  if [[ $detach == 1 ]]; then
    [[ $mode == native ]] || die '--detach rebuilds the native stack; the images are as they were built: stack.sh up --images again'
    native_preflight
    for name in "${names[@]}"; do
      case $name in
        server|api|discord|releases) build_programs ;;
        web) build_bundle ;;
        tls) ;;
        *) die "restart takes server, api, web, tls, discord or releases, not $name" ;;
      esac
    done
    say "built; restarting ${names[*]} on its own once this returns (stack.sh logs restart)"
    # Held by this command until it returns: the restart takes it once it is free.
    exec 9>&-
    setsid nohup env TILO_SH_RESTART_AFTER=$$ bash "$here/stack.sh" restart "${names[@]}" \
      >> "$state/logs/restart.log" 2>&1 < /dev/null &
    return 0
  fi
  if [[ -n ${TILO_SH_RESTART_AFTER:-} ]]; then
    # Started by a detached restart: the command that started it returns first.
    while kill -0 "$TILO_SH_RESTART_AFTER" 2>/dev/null; do sleep 0.2; done
  fi
  for name in "${names[@]}"; do restart_one "$name"; done
}

restart_one() {
  local name=$1
  [[ $mode == native ]] || die 'restart rebuilds the native stack; the images run as built: stack.sh up --images again'
  native_preflight
  # What the server reads of the operator's secrets, on a stack whose state was
  # kept before this checkout made it: restarting one part must not need a
  # whole `up` first.
  make_secrets
  local began=$SECONDS
  if running discord; then discord_api=http://127.0.0.1:$discord_rest_port; fi
  case $name in
    server)
      build_programs
      stop_native server
      lay_out_lounge
      start_native_server
      wait_server ;;
    api)
      build_programs
      stop_native api
      start_native_api
      wait_api ;;
    web)
      build_bundle
      stop_native web
      lay_out_web
      start_native_web "$(cat "$state/server/lounge/workspace-id")"
      wait_web ;;
    tls)
      start_native_tls
      wait_for 'tls (the server through Caddy)' "$server_origin/mode" 30 ;;
    discord)
      build_programs
      start_native_discord
      say 'the server reads the Discord settings when it starts: stack.sh restart server points a server started without it here' ;;
    releases)
      build_programs
      lay_out_releases ;;
    *) die "restart takes server, api, web, tls, discord or releases, not $name" ;;
  esac
  say "$name restarted in $((SECONDS - began))s"
}

# The operator's secrets, as the server's Secret holds them: a throwaway
# workspace root, made once. Who is an owner is the workspace's own governance
# from then on.
make_secrets() {
  # Each secret is made once: a file kept from an earlier run was made
  # read-only then, and is not written over.
  if [[ ! -s $state/secrets/workspace-root-lounge ]]; then
    od -An -tx1 -N32 /dev/urandom | tr -d ' \n' > "$state/secrets/workspace-root-lounge"
  fi
  if [[ ! -s $state/secrets/discord-client-secret ]]; then
    printf 'local-fake-discord-secret' > "$state/secrets/discord-client-secret"
  fi
  # The stack's own: the VM host writes its members and plans here as root,
  # for the server and the host's own service to read.
  find "$state/secrets" -maxdepth 1 -type f -user "$EUID" -exec chmod 0400 {} +
  if [[ ! -s $state/secrets/resend-key ]]; then
    chmod u+w "$state/secrets"
    printf 're_local_mail_sink' > "$state/secrets/resend-key"
    chmod 0444 "$state/secrets/resend-key"
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
  if [[ $remote != 1 ]]; then
    [[ -z $remote_why ]] || say "running the stack here: $remote_why"
  fi
  if [[ -n $images && $remote == 1 ]]; then
    die '--images runs the release images under podman on this machine: run it with TILO_SH_REMOTE=0, or up without --images to run this checkout'"'"'s programs on the build server'
  fi
  [[ -z $fresh ]] || down_any --wipe
  # One stack at a time: what the other mode left running goes first.
  if [[ -f $state/mode ]]; then down_any >/dev/null 2>&1 || true; fi
  if [[ -z $stack_token ]]; then
    stack_token=$(od -An -tx1 -N16 /dev/urandom | tr -d ' \n')
  fi
  # Its base must be this stack's here, before anything builds; a stack on the
  # build server makes the same check there, where its services bind.
  check_ports
  mkdir -p "$state/logs"
  write_stack_token
  if [[ -n $images ]]; then
    up_images "$discord"
  elif [[ $remote == 1 ]]; then
    up_remote "$seed" "$discord"
  else
    up_native "$discord"
  fi
  if [[ $remote == 1 ]]; then
    vm_skipped='a remote stack runs the programs on the build server; the VM host runs from its images beside a stack here (TILO_SH_REMOTE=0)'
  else
    vm_skipped=$(vm_skip_reason)
  fi

  [[ -z $vm_skipped ]] || say "vm-host skipped: $vm_skipped"

  printf '%s\n' "$((SECONDS - began))" > "$state/bring-up-seconds"
  if [[ -n $seed && $remote != 1 ]]; then
    seed "$(cat "$state/server/lounge/workspace-id")"
  fi
  say "up in $((SECONDS - began))s ($mode)"
  if [[ $remote == 1 ]]; then status_remote; else status; fi
  # The Lounge's owner is the stack's own person (stack.sh tilo acts as
  # them); a link from them makes whoever opens it, in any browser, an
  # administrator, who invites the others.
  local link=''
  if [[ $remote == 1 ]]; then
    link=$(remote_cmd owner | head -1) || true
  else
    link=$(owner_tilo workspace people invite --role administrator -w "$(lounge_id)" | head -1) || true
  fi
  if [[ -n $link ]]; then
    say 'opening this link in any browser makes that person an administrator of the Lounge'
    printf '%s\n' "$link"
  else
    say 'the Lounge made no administrator link: stack.sh owner asks again'
  fi
}

up_images() {
  local discord=$1
  command -v podman >/dev/null || die 'needs podman'
  # The stand-in is built with this checkout's programs, which the images are
  # not: a stack here starts it, a rehearsal of what ships does not.
  [[ ${TILO_SH_MAIL_STAND_IN:-0} != 1 ]] \
    || die 'TILO_SH_MAIL_STAND_IN=1 starts a stand-in mail server with this checkout'"'"'s programs: up without --images'
  # Rootless, as on any developer's machine: rootful podman gives the web
  # entry no pasta network, and keep-id is what maps the images' users onto
  # this account's files. In a VM as root, check-in-vm.sh runs it as a user.
  [[ $EUID != 0 ]] || die 'run it as a regular user (rootless podman), not root'
  local began=$SECONDS
  mkdir -p "$state"/{logs,server,secrets,api,tls/caddy,tls/config}

  local lounge api web
  lounge=$(image LOUNGE image-lounge /lounge/image.oci.tar)
  api=$(image API image-api image.oci.tar)
  web=$(image WEB image-web-entry /web-entry/image.oci.tar)
  say "images ready ($((SECONDS - began))s)"

  echo images > "$state/mode"
  use_mode images
  echo "${TILO_SH_ADMISSION:-invite}" > "$state/admission"
  make_secrets
  # The server's own device, which its VM host's member takes the workspaces'
  # runs from: made here, before either starts, so the server starts once with
  # the host already placed.
  host_device_key

  # The VM host, when this machine can run it and the server root is there: it
  # is root's, and it starts before the server so the server reads where to
  # place a workspace from the start.
  vm_skipped=$(vm_host_blocker)
  if [[ -z $vm_skipped ]]; then
    # The images are fetched (or loaded) here unless the caller named them:
    # the guest is read from the outputs `t outputs` left, not from podman, so
    # the host unpacks it onto its own state disk.
    [[ -n ${TILO_SH_IMAGE_VM_HOST:-} ]] || image VM_HOST image-vm-host image.oci.tar >/dev/null
    [[ -n ${TILO_SH_HOSTED_GUEST:-} ]] || image HOSTED_GUEST image-hosted-guest image.oci.tar >/dev/null
    host_ticket
    say 'starting the VM host (Firecracker, as root)'
    vm_host up || die 'the VM host did not start; sudo -E bash '"$here"'/vm-host.sh status says why'
  fi

  if [[ -n $discord ]]; then
    start_discord
  fi
  start_mail

  # The server's TLS, first: the API server trusts its CA.
  cp "$here/Caddyfile" "$state/tls/Caddyfile"
  local tls_user=()
  mapfile -d '' tls_user < <(as_user 10001)
  start tls --network host --read-only "${tls_user[@]}" \
    --env XDG_DATA_HOME=/data --env XDG_CONFIG_HOME=/config \
    --env "TILO_SH_SERVER_PORT=$server_port" \
    --env "TILO_SH_LIVE_PORT=$live_port" --env "TILO_SH_PACKET_PORT=$packet_port" \
    --env "TILO_SH_ROUTES_PORT=$routes_port" --env "TILO_SH_PLAIN_PORT=$plain_port" \
    --env "TILO_SH_CADDY_HTTP_PORT=$caddy_http_port" \
    --env "TILO_SH_SITE_PORT=$site_port" --env "TILO_SH_WEB_PORT=$web_port" \
    -v "$state/tls/Caddyfile:/etc/caddy/Caddyfile:ro,Z" \
    -v "$state/tls/caddy:/data/caddy:Z" -v "$state/tls/config:/config:Z" \
    "$caddy_image" caddy run --config /etc/caddy/Caddyfile --adapter caddyfile

  # The server: the Lounge image, as the deployment starts it (uid 10001, a
  # read-only root, the state volume, the secrets read-only, a /tmp).
  # The server reaches its own live origin through Caddy (its endpoint's home
  # relay), so it trusts the local CA as production trusts Cloudflare's: in
  # the bundle at the path every Tilo client, iroh's included, reads.
  trust_bundle
  local server_args=()
  mapfile -d '' server_args < <(server_env | env_args)
  local server_user=()
  mapfile -d '' server_user < <(as_user 10001)
  start server --network host --read-only --tmpfs /tmp "${server_user[@]}" \
    -v "$state/server:/run/tilo-workspaces:Z" -v "$state/secrets:/run/secrets:ro,Z" \
    -v "$state/trust-bundle.pem:/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem:ro,Z" \
    --env "TILO_SOURCE_REVISION=local-$commit" \
    "${server_args[@]}" "$lounge"
  wait_server_up
  wait_for 'tls (the server through Caddy)' "$server_origin/mode" 60
  local workspace
  workspace=$(cat "$state/server/lounge/workspace-id") || die 'the server kept no workspace id'
  say "server ready: the Lounge is $workspace ($((SECONDS - began))s)"

  # The API server: uid 65532, its state on its own volume.
  host_info > /dev/null
  local api_args=()
  mapfile -d '' api_args < <(api_env | env_args)
  local api_user=()
  mapfile -d '' api_user < <(as_user 65532)
  start api --network host --read-only --tmpfs /tmp "${api_user[@]}" \
    -v "$state/api:/state:Z" \
    -v "$state/trust-bundle.pem:/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem:ro,Z" \
    "${api_args[@]}" "$api"
  wait_for 'API server (its health check)' "http://127.0.0.1:$api_port/health" 60

  # The web entry: its image listens on 8080 alone, so it gets its own
  # network, published on the port tls serves the site from, from which it reaches the server's
  # plain listener (the pages it renders) and the releases on this machine's loopback.
  start_releases
  local web_args=() info
  info=$(host_info)
  mapfile -d '' web_args < <(web_env "$workspace" "$info" | env_args)
  start web --network "pasta:-T,$plain_port,-T,$releases_port" -p "127.0.0.1:$web_port:8080" \
    --read-only --tmpfs /tmp --user 10001 \
    "${web_args[@]}" "$web"
  wait_for 'web (its health check)' "$site_health" 60 -H "$site_host_header"

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
    --control "127.0.0.1:$discord_control_port" > "$state/logs/discord.log" 2>&1 9>&- &
  echo $! > "$state/discord.pid"
  local deadline=$((SECONDS + 60))
  until grep -q '^DISCORD_API_BASE=' "$state/logs/discord.log" 2>/dev/null; do
    ((SECONDS < deadline)) || die "the fake Discord did not start within a minute: $(cat "$state/logs/discord.log")"
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
  for name in web releases api server tls discord models; do
    stop_native "$name"
  done
  # The VM host's disk and identity outlive a plain down, as the stack's state
  # does: the next up is the same member with the workspaces' files it keeps.
  if vm_host_running || [[ -n $wipe && -d $vm_state/vm ]]; then
    vm_host down ${wipe:+--wipe} || say 'the VM host did not stop; sudo -E bash '"$here"'/vm-host.sh down'"${wipe:+ --wipe}"
    # A ticket belongs to one VM host state disk: a wiped host takes a new one.
    [[ -z $wipe ]] || rm -f "$vm_state/ticket"
  fi
  if command -v podman >/dev/null; then
    for name in web api server tls; do
      podman_ rm -f -t 10 "$prefix-$name" >/dev/null 2>&1 || true
    done
  fi
  for pid in releases discord mail; do
    [[ -f $state/$pid.pid ]] && { kill "$(cat "$state/$pid.pid")" 2>/dev/null || true; rm -f "$state/$pid.pid"; }
  done
  stop_mail_stand_in
  stop_people_holders
  if [[ -n $wipe ]]; then
    # The volumes belong to the containers' users (10001, 65532) inside this
    # account's user namespace: only that namespace removes them.
    podman_ unshare rm -rf "$state/server" "$state/api" "$state/tls" "$state/secrets" "$state/seed" "$state/mail" "$state/mail-stand-in" 2>/dev/null \
      || rm -rf "$state/server" "$state/api" "$state/tls" "$state/secrets" "$state/seed" "$state/mail" "$state/mail-stand-in"
    rm -rf "${runs:?}" "$state/trust-bundle.pem" "$state/mode" "$state/admission"
    say "stopped, and its state is gone (builds kept in $native, images in $state/images)"
  else
    say "stopped; its state stays in $state (stack.sh down --wipe deletes it)"
  fi
}

# The daemons (`tilo-runtime`) the stack's people started with a home under its state, as
# their tilo does once it logs in, stopped, and the workspace folders they mounted there let go:
# they outlive the stack otherwise, and a wipe cannot remove a folder still mounted. A holder
# that read its person's root hides its environment, so it is known by the lock it holds on
# its device's holder.lock (/proc/locks names the process).
stop_people_holders() {
  local pid mount pids=()
  while read -r pid; do pids+=("$pid"); done < <(people_holders)
  ((${#pids[@]})) && kill "${pids[@]}" 2>/dev/null
  for pid in "${pids[@]}"; do
    for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
    kill -9 "$pid" 2>/dev/null
  done
  while read -r _ mount _; do
    mount=$(printf '%b' "$mount")
    [[ $mount == "$state"/* ]] || continue
    fusermount3 -u "$mount" 2>/dev/null || fusermount -u "$mount" 2>/dev/null || true
  done < /proc/self/mounts
}

# The processes holding a device's holder.lock under the stack's state, one per line.
people_holders() {
  local lock inode
  while IFS= read -r lock; do
    inode=$(stat -c %i "$lock" 2>/dev/null) || continue
    awk -v inode="$inode" '{ split($6, at, ":"); if (at[3] == inode) print $5 }' /proc/locks
  done < <(find "$state" -xdev -name holder.lock 2>/dev/null)
}

# host_keeps_no_vault: whether the server of the stack that is up keeps no
# sealed store of its own beside the workspaces it serves: every sign-in,
# provider key and vault item is an entry of each workspace's one vault, and
# the server converts a store it kept before as it starts. It fails, naming
# each, when a sealed store or its key lies under the server's state.
host_keeps_no_vault() {
  local found
  found=$(find "$state/server" \( -path '*/workspaces/workspaces/*' -o -path '*/workspaces/people/*' \) \
    \( -name vault.key -o -name vault.enc \)) || {
    say "the state under $state/server could not be read whole, so whether the server keeps a store of its own was not seen"
    return 1
  }
  if [[ -n $found ]]; then
    say "the server keeps sealed stores of its own beside its workspaces' vaults:"
    printf '%s\n' "$found" >&2
    return 1
  fi
  printf '%s\n' "the server keeps no sealed store of its own: every secret is in a workspace's vault"
}

status() {
  local svc url
  printf '%-9s %-8s %s\n' service state address
  for svc in "tls|$server_origin/mode" "server|http://127.0.0.1:$routes_port/mode" \
    "api|http://127.0.0.1:$api_port/health" "web|$site_health"; do
    local name=${svc%%|*} probe=${svc#*|} shown answer args=()
    case $name in
      tls) shown="$server_origin (server), $live_origin (live session)" ;;
      server) shown="the Lounge, behind tls" ;;
      api) shown="$api_origin (MCP: $api_origin/mcp)" ;;
      web) shown="$site_origin" ;;
    esac
    [[ $name != web ]] || args=(-H "$site_host_header")
    if running "$name" && curl -fsS --max-time 2 "${args[@]}" "$probe" >/dev/null 2>&1; then
      answer=up
    elif running "$name"; then answer=starting; else answer=down; fi
    printf '%-9s %-8s %s\n' "$name" "$answer" "$shown"
  done
  local vm_skipped
  vm_skipped=$(vm_skip_reason)
  if [[ -z $vm_skipped ]]; then
    printf '%-9s %-8s %s\n' vm-host up "workspaces' VMs; vm-host.sh status says which"
  else
    printf '%-9s %-8s %s\n' vm-host skipped "$vm_skipped"
  fi
  if running discord || { [[ -f $state/discord.pid ]] && kill -0 "$(cat "$state/discord.pid")" 2>/dev/null; }; then
    printf '%-9s %-8s %s\n' discord up "http://127.0.0.1:$discord_rest_port (control http://127.0.0.1:$discord_control_port)"
  fi
  if running models; then
    printf '%-9s %-8s %s\n' models up "http://127.0.0.1:$models_port (OpenAI's sign-in page: http://127.0.0.1:$models_port/auth/codex/device)"
  fi
  if [[ $mode == native ]]; then
    printf '\nRunning this checkout'"'"'s programs; after a change, %s restart server (or api, web, …).\n' "$0"
  else
    printf '\nRunning the images (up --images).\n'
  fi
  printf 'Next: eval "$(%s env)", then tilo keys create "My backend"; or %s browser.\n' "$0" "$0"
}

print_env() {
  [[ -f $state/mode ]] || die 'the stack has not run here yet: stack.sh up'
  cat <<EOF
# tilo.sh on this machine (stack.sh). The tilo command, the SDKs and agents
# reach the local API server; nothing here touches tilo.sh.
export TILO_API_URL=$api_origin
export TILO_SITE=$site_origin
EOF
  # The key the stack's own owner approved (stack.sh owner --here), once there is one.
  if [[ -s $state/owner/api-key ]]; then
    printf 'export TILO_API_KEY=%s\n' "$(cat "$state/owner/api-key")"
  else
    printf '%s\n' '# No key yet: eval "$(stack.sh owner --here)" makes an owner and one.'
  fi
  cat <<EOF
# An agent's MCP client: the streamable HTTP server $api_origin/mcp;
# a one-workspace key's MCP: $api_origin/w/<workspace>/mcp with the header
#   "Authorization: Bearer <token>"
EOF
}

# -- on the build server ----------------------------------------------------
#
# The same stack, run on the build server in this checkout's build container,
# with the ports a person and their tools use served from there on this
# machine's loopback under the same numbers: the page opens on
# http://tilo.localhost:BASE+20 and an agent's key is approved at
# http://localhost:BASE+10, exactly as for a stack here. Only those ports and a
# long-running `exec` cross the wire; every process, the state and the build
# stay on the build server, which is what keeps this machine free.

remote_mode() { [[ $(cat "$state/mode" 2>/dev/null || true) == remote ]]; }

# Whether the commands that act on the stack run on the build server: its mode
# says so, or this call was told to (TILO_SH_REMOTE=1, a state that was lost).
remote_here() { remote_mode || [[ $remote == 1 ]]; }

# The bring-up process holding this stack's ports, while it still is that (a
# pid file outlives a reboot, and the pid it names may be someone else's now).
supervisor_pid() {
  local pid
  pid=$(cat "$state/supervisor.pid" 2>/dev/null || true)
  [[ -n $pid ]] || return 1
  kill -0 "$pid" 2>/dev/null || return 1
  { tr '\0' '\n' < "/proc/$pid/environ"; } 2>/dev/null \
    | grep -qxF "TILO_SH_STATE=$remote_state" \
    && printf '%s' "$pid"
}

# The ports this machine keeps: the server's public origin and the addresses a
# page dials, the API server the SDKs and agents use, the web entry and the
# releases, the mail sink and the stand-in mail server's two, the fake Discord
# when one runs, and the fake model providers, whose sign-in page a person's
# browser opens.
forward_tcp() {
  printf '%s\n' "$server_port,$live_port,$plain_port,$routes_port,$api_port,$site_port,$web_port,$releases_port,$mail_port,$mail_stand_in_imap_port,$mail_stand_in_smtp_port,$discord_rest_port,$discord_gateway_port,$discord_control_port,$models_port,$control_port"
}

# The page reaches the server over QUIC (WebTransport, then iroh), which ssh
# cannot carry: the drop-in serves these through its own datagram channel.
forward_udp() { printf '%s\n' "$packet_port,$iroh_port,$link_port"; }

# The sockets of the ssh masters on this machine: an ssh that multiplexes
# commands names itself `ssh: <socket> [mux]`, and every generation of the
# drop-in's master keeps one.
mux_sockets() {
  local proc line
  for proc in /proc/[0-9]*; do
    [[ -O $proc && -r $proc/cmdline ]] || continue
    line=$(tr '\0' '\n' < "$proc/cmdline" 2>/dev/null) || continue
    line=${line%%$'\n'*}
    [[ $line == 'ssh: '*' [mux]' ]] || continue
    line=${line#ssh: }
    line=${line%' [mux]'}
    [[ -S $line ]] && printf '%s\n' "$line"
  done
}

# cancel_forwards COMMA,PORTS: frees the ports `PORTS` were forwarded on. A
# client asking a shared ssh master to forward exits without taking the
# forward with it, and the master keeps listening: the stack's ports would
# stay taken after it is down, and the next up would find them in use. Each
# port is cancelled on every master; one a master does not carry changes
# nothing there.
cancel_forwards() {
  local sockets port socket
  local -a ports=()
  IFS=, read -r -a ports <<< "${1:-}"
  ((${#ports[@]})) || return 0
  command -v ssh >/dev/null 2>&1 || return 0
  sockets=$(mux_sockets) || true
  [[ -n $sockets ]] || return 0
  while IFS= read -r socket; do
    for port in "${ports[@]}"; do
      [[ -n $port ]] || continue
      ssh -S "$socket" -O cancel -L "127.0.0.1:$port:127.0.0.1:$port" forward-cancel \
        </dev/null >/dev/null 2>&1 || true
    done
  done <<< "$sockets"
}

# The environment the stack on the build server runs with: the caller's
# TILO_SH_* (they configure the stack), with this side's state and mode
# replaced, and the container's own cargo, which needs no drop-in there. The
# paths this machine keeps a program at do not go: the stack there has its own
# state, its own build and its own tilo command. The command the drop-in starts
# is handed them with `env`, so a drop-in need not forward anything itself.
remote_env() {
  local line
  while IFS= read -r line; do
    case ${line%%=*} in
      TILO_SH_STATE|TILO_SH_PORT_BASE|TILO_SH_REMOTE|TILO_SH_CARGO|TILO_SH_REMOTE_STATE|TILO_SH_STACK_TOKEN|TILO_SH_TILO_BIN) continue ;;
      TILO_SH_*) printf '%s\n' "$line" ;;
    esac
  done < <(env | grep '^TILO_SH_' || true)
  # The services' log filters (private_env), which a restart gives them there.
  [[ -z ${RUST_LOG:-} ]] || printf 'RUST_LOG=%s\n' "$RUST_LOG"
  [[ -z ${TILO_SERVER_LOG:-} ]] || printf 'TILO_SERVER_LOG=%s\n' "$TILO_SERVER_LOG"
  printf 'TILO_SH_STATE=%s\n' "$remote_state"
  printf 'TILO_SH_PORT_BASE=%s\n' "$base"
  printf 'TILO_SH_REMOTE=0\n'
  printf 'TILO_SH_CONTROL_PORT=%s\n' "$control_port"
  [[ -z $stack_token ]] || printf 'TILO_SH_STACK_TOKEN=%s\n' "$stack_token"
  printf 'TILO_SH_CARGO=%s\n' "${TILO_SH_REMOTE_CARGO:-cargo}"
  printf 'TILO_SH_REMOTE_COMMIT=%s\n' "$commit"
}

# remote_cmd ARGS…: this stack's own stack.sh command where the stack is, so
# the owner, the invites, the log and the mail are read and made in one place.
# The script is named from the repository (`self`), since the build server keeps a copy of it at
# a root of its own.
#
# A remote stack lives in the container its cargo drop-in builds in, and these commands have to act
# there: a drop-in that routes other work elsewhere (exec handed to a builder) is asked for these
# in its own container, since that other place holds neither the stack nor its state. The opt-out
# below is the drop-in's own switch for one call; it means nothing to another drop-in, which runs
# the call where it builds anyway, and a drop-in's cargo-subcommand switch is not passed here.
remote_cmd() {
  if [[ $mode == remote ]] && control_ready; then
    control_cmd "$@"
    return
  fi
  local envs=()
  mapfile -t envs < <(remote_env)
  ( cd "$repo" && RCARGO_T=0 "$cargo" exec -- env "${envs[@]}" bash "$self" "$@" )
}

# remote_shell COMMAND: a command on the build server, for what stack.sh has
# no subcommand for.
remote_shell() {
  local envs=()
  mapfile -t envs < <(remote_env)
  ( cd "$repo" && RCARGO_T=0 "$cargo" exec -- env "${envs[@]}" bash -c "$1" )
}

# Whether the build container runs on this machine in this machine's network (same kernel, same
# network namespace), as it does for a checkout on the build server itself.
builder_shares_loopback() {
  local here there
  here="$(cat /proc/sys/kernel/random/boot_id 2>/dev/null) $(readlink /proc/self/ns/net 2>/dev/null)"
  there=$(cd "$repo" && RCARGO_T=0 "$cargo" exec -- sh -c 'echo "$(cat /proc/sys/kernel/random/boot_id) $(readlink /proc/self/ns/net)"' 2>/dev/null | tail -n 1) || return 1
  [[ $here == "$there" ]]
}

# start_supervisor ARGS…: the stack on the build server as one long-running
# program, which is what keeps its ports served here and its memory admitted
# there while it runs. The stack's own bring-up comes first, so the process is
# only kept alive once its services answer; `down` ends it. The drop-in's own
# environment carries the state it serves, which is how its pid file is known
# to be this stack's (`supervisor_pid`).
start_supervisor() {
  local envs=() start control
  mapfile -t envs < <(remote_env)
  # The held program listens on its control port first, so the acting commands reach it from the
  # start, then starts the stack and serves them: a job's state is its own, so nothing else can
  # enter it to act on the stack, and ending the program ends the job.
  control=${self%/*}/control.py
  start=$(printf '%q ' "$@")
  # The server sandboxes what it runs (bubblewrap): on t the program's job must be one that may
  # start sandboxes of its own, as a drop-in that builds on another machine always may.
  local nested=() forward=(--forward "$(forward_tcp)" --forward "udp:$(forward_udp)")
  [[ $(basename -- "$cargo") != t ]] || nested=(--nested)
  # Brought up from the build server itself, the build container is in this machine's network: its
  # ports are this machine's already, and a forward would take them before the stack could.
  if builder_shares_loopback; then forward=(); fi
  ( cd "$repo" && setsid env "TILO_SH_STATE=$remote_state" RCARGO_T=0 "$cargo" exec "${nested[@]}" \
      "${forward[@]}" \
      -- env "${envs[@]}" bash -c "exec python3 $(printf '%q' "$control") serve --port $(printf '%q' "$control_port") --token $(printf '%q' "$stack_token") --repo . --self $(printf '%q' "$self") -- $start" ) \
    >> "$state/logs/remote.log" 2>&1 < /dev/null 9>&- &
  printf '%s\n' "$!" > "$state/supervisor.pid"
}

# Whether the held program answers on its control port here, through the forward: a stack on a
# builder that keeps a job's state to itself is acted on this way, and one on a builder whose
# container outlives the program the older way.
control_ready() {
  (exec 3<>"/dev/tcp/127.0.0.1/$control_port") >/dev/null 2>&1
}

# control_cmd ARGS… / control_down [ARGS…]: start stack.sh on the held program, with the stack's
# token; the program prints what the command printed and exits with its status.
control_cmd() {
  python3 "$here/control.py" request --port "$control_port" --token "$stack_token" -- "$@"
}
control_down() {
  python3 "$here/control.py" down --port "$control_port" --token "$stack_token" -- "$@"
}

# up_remote [--seed] [--discord]: the stack on the build server, its ports here.
up_remote() {
  local seed=$1 discord=$2 args=()
  [[ -z $seed ]] || args+=(--seed)
  [[ -z $discord ]] || args+=(--discord)
  mkdir -p "$state/logs"
  (cd "$repo" && mkdir -p "$(dirname -- "$remote_state")")
  echo remote > "$state/mode"
  mode=remote
  printf '%s\n' "$remote_state" > "$state/remote-state"
  : > "$state/logs/remote.log"
  say "starting the stack on the build server ($cargo exec; its state is $remote_state there)"
  start_supervisor up "${args[@]}"
  if ! wait_remote_up; then
    # Nothing is left holding the ports for a stack that did not come up.
    stop_supervisor
    cancel_forwards "$(forward_tcp)"
    return 1
  fi
}

# Waits until the stack answers here, through the forward: the page's health
# check, then the API server's. A bring-up that died says why, with its log.
wait_remote_up() {
  local pid deadline=$((SECONDS + ${TILO_SH_REMOTE_TIMEOUT:-1800})) said=$SECONDS
  # The process start_supervisor just started, as it wrote it: supervisor_pid
  # knows it by the environment the drop-in starts with, which it may not have
  # exec'd into yet, and a bring-up that died must not be waited on for the
  # whole timeout.
  pid=$(cat "$state/supervisor.pid" 2>/dev/null || true)
  say "waiting for the stack through the forward (log: $state/logs/remote.log)"
  until curl -fsS --max-time 2 -H "$site_host_header" "$site_health" >/dev/null 2>&1; do
    if [[ -n $pid ]] && ! kill -0 "$pid" 2>/dev/null; then
      printf -- '--- %s, its last lines:\n' "$state/logs/remote.log" >&2
      tail -20 "$state/logs/remote.log" >&2
      say 'the stack on the build server did not come up (above)'
      return 1
    fi
    if ((SECONDS >= deadline)); then
      tail -20 "$state/logs/remote.log" >&2
      say "the stack did not answer on $site_origin within ${TILO_SH_REMOTE_TIMEOUT:-1800}s ($state/logs/remote.log)"
      return 1
    fi
    if ((SECONDS - said >= 30)); then
      said=$SECONDS
      say "$((SECONDS))s: $(tail -1 "$state/logs/remote.log" 2>/dev/null)"
    fi
    sleep 0.5
  done
  # What answered is this stack's only if its releases serve the token this
  # bring-up made. A forward that reached another stack on the same base
  # answered for a bring-up that never came up, and `up` printed success.
  local answered
  answered=$(curl -fsS --max-time 2 \
    "http://127.0.0.1:$releases_port/tilo-cli/latest/stack-token" 2>/dev/null) || answered=''
  if [[ $answered != "$stack_token" ]]; then
    printf -- '--- %s, its last lines:\n' "$state/logs/remote.log" >&2
    tail -20 "$state/logs/remote.log" >&2
    say "the stack answering on base $base is not this one (its releases serve '${answered:-nothing}' at $releases_port/tilo-cli/latest/stack-token, not this bring-up's token): another stack holds these ports here or on the build server, and this bring-up stops. Give this checkout its own TILO_SH_PORT_BASE and TILO_SH_STATE, then stack.sh up"
    return 1
  fi
  wait_for 'API server (its health check)' "$api_origin/health" 30
}

# What runs, as this machine sees it through the forward: a stack whose forward
# is down answers nowhere, which is what its line says.
status_remote() {
  local svc name probe shown answer args pid
  printf '%-9s %-8s %s\n' service state address
  for svc in "tls|$server_origin/mode" "server|http://127.0.0.1:$routes_port/mode" \
    "api|$api_origin/health" "web|$site_health"; do
    name=${svc%%|*} probe=${svc#*|} args=()
    case $name in
      tls) shown="$server_origin (server), $live_origin (live origin)" ;;
      server) shown="the Lounge, behind tls" ;;
      api) shown="$api_origin (MCP: $api_origin/mcp)" ;;
      web) shown="$site_origin"; args=(-H "$site_host_header") ;;
    esac
    if curl -fsS --max-time 2 "${args[@]}" "$probe" >/dev/null 2>&1; then answer=up; else answer=down; fi
    printf '%-9s %-8s %s\n' "$name" "$answer" "$shown"
  done
  pid=$(supervisor_pid || true)
  if [[ -n $pid ]]; then
    printf '%-9s %-8s %s\n' forward up "the stack's ports on 127.0.0.1 from $base (page: $site_origin, API: $api_origin), with the page's UDP ($packet_port,$iroh_port,$link_port), from the build server"
  else
    printf '%-9s %-8s %s\n' forward down "nothing serves the stack's ports here: stack.sh up"
  fi
  printf '%-9s %-8s %s\n' vm-host skipped "it runs from its images, beside a stack here (TILO_SH_REMOTE=0)"
  printf '\nThe programs and the state are on the build server (%s there); %s logs remote reads its log.\n' \
    "$remote_state" "$0"
  printf 'Next: eval "$(%s env)", then tilo keys create "My backend"; or %s browser.\n' "$0" "$0"
}

# stop_supervisor: the bring-up process holding this stack's ports, and with it the forward.
stop_supervisor() {
  local pid deadline
  if pid=$(supervisor_pid); then
    kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
    deadline=$((SECONDS + 10))
    while kill -0 "$pid" 2>/dev/null && ((SECONDS < deadline)); do sleep 0.1; done
    kill -KILL -- "-$pid" 2>/dev/null || true
  fi
  rm -f "$state/supervisor.pid"
}

# down_any ARGS…: stops the stack wherever it runs.
down_any() {
  if remote_here; then down_remote "$@"; else down "$@"; fi
}

# down_remote [--wipe]: the services stop where they run, then the forward.
down_remote() {
  local wipe= stopped=1 via_control=
  [[ ${1:-} == --wipe ]] && wipe=1
  if [[ $mode == remote ]] && control_ready; then
    via_control=1
    if control_down ${wipe:+--wipe}; then :; else stopped=0; fi
  elif ! remote_cmd down ${wipe:+--wipe}; then
    # A stack whose services are running there still writes to its state: say
    # what is left instead of claiming it is gone.
    stopped=0
    say 'the stack on the build server did not answer (above): its services may still be running there'
  fi
  stop_supervisor
  # Its ports are this machine's again: the master keeps a forward after the
  # client that asked for it ends, and the next up would find them in use.
  cancel_forwards "$(forward_tcp)"
  if [[ -n $wipe && -n $via_control ]]; then
    rm -rf "${state:?}" 2>/dev/null || true
    say 'stopped; the job it ran in is gone with it, and this machine keeps nothing of it'
  elif [[ -n $wipe ]]; then
    if ((stopped)) && remote_shell "rm -rf $(printf '%q' "$remote_state")" 2>/dev/null; then
      # This machine keeps what it printed and its connection's log, and nothing else.
      rm -rf "${state:?}" 2>/dev/null || true
      say 'stopped; the build server and this machine keep nothing of it'
    else
      say "stopped here; $remote_state on the build server was not removed: stack.sh down --wipe again once it answers"
      return 1
    fi
  else
    say "stopped; its state stays on the build server ($remote_state); stack.sh down --wipe deletes it"
  fi
}

browser() {
  local chrome=${CHROME:-}
  if [[ -z $chrome ]]; then
    chrome=$(ls -d "$HOME"/.cache/ms-playwright/chromium-*/chrome-linux*/chrome 2>/dev/null | sort -V | tail -1)
  fi
  [[ -x $chrome ]] || die 'no Chromium: set CHROME, or install Playwright'"'"'s (bunx playwright install chromium)'
  local profile
  profile=$(mktemp -d "${XDG_RUNTIME_DIR:-/tmp}/tilo-sh-browser.XXXXXX")
  say "a fresh visitor on $site_origin (profile $profile)"
  "$chrome" --user-data-dir="$profile" --no-first-run "$site_origin" "${@}"
}

# Sourced (check.sh does), it only defines the addresses and functions above.
[[ ${BASH_SOURCE[0]} == "$0" ]] || return 0
cmd=${1:-help}
shift || true
case $cmd in
  up|down|restart) lock_state ;;
esac
# A stack whose mode says it runs on the build server does everything that
# touches it there; only what a person does here (opening the page, the log of
# the connection) stays.
if remote_here && [[ $cmd != up ]]; then
  case $cmd in
    down) down_any "$@" ;;
    restart) remote_cmd restart "$@" ;;
    status) status_remote ;;
    env) remote_cmd env ;;
    owner) remote_cmd owner "$@" ;;
    invite) remote_cmd invite "$@" ;;
    tilo) remote_cmd tilo "$@" ;;
    person) remote_cmd person "$@" ;;
    hold | unhold) remote_cmd "$cmd" "$@" ;;
    logs)
      if [[ ${1:-} == remote ]]; then cat "$state/logs/remote.log"; else remote_cmd logs "$@"; fi ;;
    mail) remote_cmd mail ;;
    mail-name) remote_cmd mail-name ;;
    mail-account) remote_cmd mail-account ;;
    keys) remote_cmd keys ;;
    browser) browser "$@" ;;
    check)
      die 'the end-to-end check drives a browser on the machine and reads the state of the stack it checks: run it against a stack here with TILO_SH_REMOTE=0 stack.sh check' ;;
    help|-h|--help) usage ;;
    *) usage >&2; exit 2 ;;
  esac
  exit
fi
case $cmd in
  up) up "$@" ;;
  down) down "$@" ;;
  status) status ;;
  env) print_env ;;
  restart) restart "$@" ;;
  logs) [[ -n ${1:-} ]] || die 'logs NAME: server, tls, api, web, discord, releases, mail, bundle'; cat "$state/logs/$1.log" ;;
  mail) list_mail ;;
  mail-name) mail_name ;;
  mail-account) mail_account ;;
  vaults) host_keeps_no_vault ;;
  owner) declare_owner "$@" ;;
  invite) invite_link "$@" ;;
  tilo) owner_tilo "$@" ;;
  person) owner_person "$@" ;;
  browser) browser "$@" ;;
  check)
    exec bash "$here/check.sh" "$@" ;;
  help|-h|--help) usage ;;
  *) usage >&2; exit 2 ;;
esac
