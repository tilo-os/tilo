#!/bin/sh
# The Lounge's host: the device's one daemon, keeping the public workspace placed
# on it and serving it to the pages that reach it.
#
# It joins a person's devices as any computer does: with the ticket
# `tilo device add` prints, passed as TILO_TICKET the first time the volume
# starts. Everything after that (who is in the workspace, its listing, its
# chat, its bots) is the workspace's own actions, not a setting of this image.
#
# Read from the deployment:
#   TILO_WORKSPACES_STATE           the volume the daemon's identity, profile,
#                                   state and secrets live on
#                                   (default /run/tilo-workspaces)
#   TILO_SERVER_CONFIG              the server configuration it reads, and writes
#                                   on its first start when there is none
#                                   (default $TILO_WORKSPACES_STATE/server.toml)
#   TILO_WORKSPACES_WEB_ENTRY       the Tilo app's web entry the links it mints
#                                   open, written into a configuration it makes
#   TILO_WORKSPACES_LIVE            the address of the packet relay it runs, written
#                                   into a configuration it makes; `none` runs none
#   TILO_KEPT                       the workspaces this host serves of its own,
#                                   comma-separated as ID=Name (or ID alone): the
#                                   server makes each on the first start and keeps it
#
# A deployment that fixes the endpoint's address, a published packet relay or a
# host name mounts its own server configuration instead: nothing here rewrites
# one that exists.
#
# Any other TILO_WORKSPACES_* variable stops the start: this host does not read
# it, and a deployment that still sets what the host no longer honors must not
# start believing it does.
set -eu

fail() {
  echo "tilo-lounge: $*" >&2
  exit 2
}

known='TILO_WORKSPACES_STATE TILO_WORKSPACES_WEB_ENTRY TILO_WORKSPACES_LIVE'
known=$(printf '%s' "$known" | tr '\n' ' ')
for variable in $(env | sed -n 's/^\(TILO_WORKSPACES_[A-Za-z0-9_]*\)=.*/\1/p'); do
  case " $known " in
    *" $variable "*) ;;
    *) fail "$variable is not a setting this image reads; remove it from the deployment" ;;
  esac
done

state=${TILO_WORKSPACES_STATE:-/run/tilo-workspaces}
config=${TILO_SERVER_CONFIG:-$state/server.toml}

# The daemon's identity, profile and state live on the volume, so one ticket is
# spent once per volume, never once per start.
TILO_IDENTITY_ROOT=$state/identity
XDG_STATE_HOME=$state/state
export TILO_IDENTITY_ROOT XDG_STATE_HOME HOME=$state TILO_RUNTIME_PROFILE=$state
export TILO_SERVER_CONFIG=$config
mkdir -p "$state/identity" "$state/state" "$state/secrets"
# The volume is the daemon's runtime profile too, which it refuses to start from while others
# may read it: a fresh volume is made with the mount's or the umask's mode.
chmod 0700 "$state"

# An empty volume is a machine joining an existing owner, and the one secret
# that does it is the ticket `tilo device add` printed on an owner device, which
# the daemon spends at its first start. A volume that already holds its device
# key and login leaves a ticket it is given unspent.
credentials=$state/credentials
mkdir -p -m 0700 "$credentials"
rm -f "$credentials/tilo.ticket"
if [ ! -s "$state/identity/device/cert.bin" ]; then
  if [ -z "${TILO_TICKET:-}" ]; then
    fail 'no identity on this volume: make a ticket with `tilo device add` and pass it as TILO_TICKET'
  fi
  (umask 077 && printf '%s\n' "$TILO_TICKET" > "$credentials/tilo.ticket")
fi
export CREDENTIALS_DIRECTORY=$credentials

# The server configuration is what this host keeps, and where it is reached: the
# deployment's addresses become its settings once, and a later start reads what
# is already there.
if [ ! -s "$config" ]; then
  entry=${TILO_WORKSPACES_WEB_ENTRY:-}
  [ -n "$entry" ] \
    || fail 'TILO_WORKSPACES_WEB_ENTRY is required: the Tilo app web entry the links open'
  mkdir -p "$(dirname "$config")"
  {
    printf 'web_entry = "%s"\n' "$entry"
    live=${TILO_WORKSPACES_LIVE:-}
    if [ "$live" != none ] && [ -n "$live" ]; then
      printf 'packet_relay = "%s"\n' "$live"
    fi
    kept=${TILO_KEPT:-}
    while [ -n "$kept" ]; do
      case $kept in
        *,*) one=${kept%%,*}; kept=${kept#*,} ;;
        *) one=$kept; kept= ;;
      esac
      id=${one%%=*}
      name=${one#*=}
      [ "$name" != "$one" ] || name=$id
      printf '\n[[kept]]\nid = "%s"\nname = "%s"\n' "$id" "$name"
    done
  } > "$config"
fi

exec tilo-runtime
