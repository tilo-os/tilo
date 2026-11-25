#!/usr/bin/env bash
# The Lounge image's entrypoint against a stub daemon: the server configuration
# it writes for the deployment's environment, the identity it hands the daemon,
# and that it refuses an environment the image does not honor instead of
# starting with part of it.
#
# No container: it starts the entrypoint with a `tilo-runtime` on PATH that
# prints its environment and the configuration file, so the whole interface is
# asserted in a second.
#
# usage: test-entrypoint.sh
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/tilo-lounge.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"
cat > "$work/bin/tilo-runtime" <<'STUB'
#!/bin/sh
set -eu
env | sort > "$STUB_ENV"
cat "$TILO_SERVER_CONFIG" > "$STUB_CONFIG"
STUB
chmod +x "$work/bin/tilo-runtime"
export STUB_ENV="$work/env" STUB_CONFIG="$work/config"
config_file=$work/server.toml

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# lounge [NAME=VALUE…]: the entrypoint's daemon invocation, with only the
# environment given here, and a ticket for the empty volume.
lounge() {
  env -i PATH="$work/bin:/usr/bin:/bin" STUB_ENV="$STUB_ENV" STUB_CONFIG="$STUB_CONFIG" \
    TILO_TICKET=tilo1test \
    TILO_SERVER_CONFIG="$config_file" \
    TILO_WORKSPACES_STATE="$work/state" \
    TILO_WORKSPACES_WEB_ENTRY=https://tilo.sh/ \
    "$@" sh "$here/entrypoint.sh"
}

fails_with() {
  local status=0
  "$@" >"$work/out" 2>"$work/err" || status=$?
  [[ $status == 2 ]] || fail "expected a refusal (exit 2), got $status: $(cat "$work/err")"
}

has() { grep -Fqx -- "$1" "$config_file" || fail "the configuration lacks: $1"; }
env_has() { grep -Fqx -- "$1" "$STUB_ENV" || fail "the daemon's environment lacks: $1"; }

# The plain deployment: its web entry and its volume, and a ticket for an empty
# volume, which the daemon spends from the credentials directory.
lounge || fail "the plain deployment did not start: $(cat "$work/err")"
has 'web_entry = "https://tilo.sh/"'
env_has "TILO_SERVER_CONFIG=$config_file"
env_has "TILO_IDENTITY_ROOT=$work/state/identity"
env_has "XDG_STATE_HOME=$work/state/state"
env_has "CREDENTIALS_DIRECTORY=$work/state/credentials"
[[ -s $work/state/credentials/tilo.ticket ]] || fail 'the ticket was not left for the daemon'
grep -Fqx 'tilo1test' "$work/state/credentials/tilo.ticket" || fail 'the ticket is not the one given'
grep -Fq 'packet_relay = ' "$config_file" && fail 'a packet relay was named without one set'

# The deployment's relay becomes the daemon's own setting.
rm -f "$config_file"
lounge TILO_WORKSPACES_LIVE=0.0.0.0:9782 \
  || fail "the populated deployment did not start: $(cat "$work/err")"
has 'packet_relay = "0.0.0.0:9782"'

# A host reached through another relay runs none of its own.
rm -f "$config_file"
lounge TILO_WORKSPACES_LIVE=none || fail "a host reached through a relay did not start: $(cat "$work/err")"
grep -Fq 'packet_relay = ' "$config_file" && fail "a host reached through a relay runs its own"

# A kept workspace carries the entry its founder settles: whom it lets in, at
# which role, and what the directory says of it.
rm -f "$config_file"
lounge TILO_KEPT=4b0d9a52-7b4e-4e0d-9e0e-2b1c2a0c9a11=Lounge \
  TILO_WORKSPACES_ADMISSION=invite TILO_WORKSPACES_DESCRIPTION='Coffee and tea' \
  TILO_WORKSPACES_DEFAULT_ROLE=viewer \
  || fail "a deployment with a kept workspace did not start: $(cat "$work/err")"
has 'id = "4b0d9a52-7b4e-4e0d-9e0e-2b1c2a0c9a11"'
has 'name = "Lounge"'
has 'entry = "invite"'
has 'default_role = "viewer"'
has 'description = "Coffee and tea"'
fails_with lounge TILO_WORKSPACES_ADMISSION=closed
fails_with lounge TILO_WORKSPACES_DEFAULT_ROLE=owner

# A volume that already holds its identity spends no ticket, and needs none.
mkdir -p "$work/spent/identity/device"
printf 'cert' > "$work/spent/identity/device/cert.bin"
env -i PATH="$work/bin:/usr/bin:/bin" STUB_ENV="$STUB_ENV" STUB_CONFIG="$STUB_CONFIG" \
  TILO_SERVER_CONFIG="$config_file" TILO_WORKSPACES_STATE="$work/spent" \
  TILO_WORKSPACES_WEB_ENTRY=https://tilo.sh/ \
  sh "$here/entrypoint.sh" || fail "a signed-in volume did not start: $(cat "$work/err")"
[[ ! -s $work/spent/credentials/tilo.ticket ]] || fail 'a signed-in volume kept a ticket it was given'

# A configuration the deployment already wrote is read, not replaced.
printf 'web_entry = "https://elsewhere.example/"\n' > "$config_file"
lounge || fail "a deployment with a configuration did not start: $(cat "$work/err")"
grep -Fqx 'web_entry = "https://elsewhere.example/"' "$config_file" \
  || fail 'the entrypoint replaced the configuration it was given'

# What the image does not read stops the start rather than running with half
# of the deployment's settings.
fails_with lounge TILO_WORKSPACES_PERSISTENT=lounge
fails_with lounge TILO_WORKSPACES_ROOT_FILE=/run/secrets/root
fails_with lounge TILO_WORKSPACES_BOTS='a=b'
fails_with lounge TILO_WORKSPACES_IROH_LISTEN=0.0.0.0:4433

# An empty volume with no ticket, and a deployment with no web entry where the
# entrypoint writes a configuration, both stop the start.
fails_with env -i PATH="$work/bin:/usr/bin:/bin" STUB_ENV="$STUB_ENV" STUB_CONFIG="$STUB_CONFIG" \
  TILO_SERVER_CONFIG="$work/none.toml" TILO_WORKSPACES_STATE="$work/empty" \
  TILO_WORKSPACES_WEB_ENTRY=https://tilo.sh/ sh "$here/entrypoint.sh"
fails_with env -i PATH="$work/bin:/usr/bin:/bin" STUB_ENV="$STUB_ENV" STUB_CONFIG="$STUB_CONFIG" \
  TILO_TICKET=tilo1test TILO_SERVER_CONFIG="$work/other.toml" \
  TILO_WORKSPACES_STATE="$work/state" sh "$here/entrypoint.sh"

echo "ok: the entrypoint writes the server configuration and refuses what the image does not read"
