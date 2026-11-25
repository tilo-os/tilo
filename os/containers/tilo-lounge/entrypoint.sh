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
#   TILO_WORKSPACES_ADMISSION       who a kept workspace's entry lets in: `open`
#                                   admits anyone who asks, at the default role;
#                                   `invite` only people it invited (default open)
#   TILO_WORKSPACES_DESCRIPTION     what a kept workspace is, as the public
#                                   directory shows it (default: its name)
#   TILO_WORKSPACES_DEFAULT_ROLE    the role everyone who joins through the entry
#                                   holds to begin with: viewer, commenter or
#                                   editor (default editor)
#   TILO_WORKSPACES_WAIT_LIST_MAX   how many addresses an invitation-only kept
#                                   workspace's wait list keeps
#   TILO_WORKSPACES_CONTACT         where someone asks to be taken off a kept
#                                   workspace's wait list
#   TILO_WORKSPACES_MAIL_FROM       the sender a kept workspace's wait list mails
#                                   an address from; mail needs the API key
#                                   TILO_WORKSPACES_RESEND_KEY_FILE names
#   TILO_WORKSPACES_MAIL_TEMPLATES  the directory its mails' words are read from
#   TILO_WORKSPACES_RESEND_KEY_FILE the file the mail API key is read from, kept
#                                   with the volume's secrets rather than in a
#                                   workspace's records
#   TILO_WORKSPACES_RESEND_URL      where mail is posted (default: Resend's API)
#   TILO_WORKSPACES_LOCAL           this deployment's addresses are this
#                                   machine's own (a local stack), so the links
#                                   its mails carry open here
#   TILO_WORKSPACES_ROOT_FILE       the owner's root, mounted from the operator's
#                                   secrets: its 32-byte seed as 64 hexadecimal
#                                   characters, readable by this image's user alone.
#                                   A volume given it acts as that owner's own
#                                   device and founds the kept workspaces; empty
#                                   volumes then need no ticket
#
# A kept workspace is founded by the founder's own device, under its root: a volume
# given its owner's root (TILO_WORKSPACES_ROOT_FILE) is that device, and a volume a
# ticket joined keeps the workspaces its owners place on it without founding any.
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

known='TILO_WORKSPACES_STATE TILO_WORKSPACES_WEB_ENTRY TILO_WORKSPACES_LIVE
TILO_WORKSPACES_ADMISSION TILO_WORKSPACES_DESCRIPTION TILO_WORKSPACES_DEFAULT_ROLE
TILO_WORKSPACES_WAIT_LIST_MAX TILO_WORKSPACES_CONTACT TILO_WORKSPACES_MAIL_FROM
TILO_WORKSPACES_MAIL_TEMPLATES
TILO_WORKSPACES_RESEND_KEY_FILE TILO_WORKSPACES_RESEND_URL TILO_WORKSPACES_LOCAL
TILO_WORKSPACES_ROOT_FILE'
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
# The volume is the daemon's profile, which it refuses unless only its owner reads it: a volume
# a platform made (or a directory a stack made) is the daemon's alone from here on.
mkdir -p "$state"
chmod 0700 "$state" || fail "the volume $state is not this image's user's to make private"
mkdir -p "$state/identity" "$state/state" "$state/secrets"
# The volume is the daemon's runtime profile too, which it refuses to start from while others
# may read it: a fresh volume is made with the mount's or the umask's mode.
chmod 0700 "$state"

# An empty volume is a machine joining an existing owner, and the one secret
# that does it is the ticket `tilo device add` printed on an owner device, which
# the daemon spends at its first start. A volume given its owner's root instead
# is that owner's own device, and needs no ticket. A volume that already holds
# its device key and login leaves a ticket it is given unspent.
credentials=$state/credentials
mkdir -p -m 0700 "$credentials"
rm -f "$credentials/tilo.ticket"
owner_root=${TILO_WORKSPACES_ROOT_FILE:-}
if [ -n "$owner_root" ] && [ ! -r "$owner_root" ]; then
  fail "TILO_WORKSPACES_ROOT_FILE names no readable file: $owner_root"
fi
if [ ! -s "$state/identity/device/cert.bin" ]; then
  if [ -n "$owner_root" ]; then
    : # the root the deployment hands makes this device; no ticket joins it
  elif [ -z "${TILO_TICKET:-}" ]; then
    fail 'no identity on this volume: make a ticket with `tilo device add` and pass it as TILO_TICKET, or give it its owner root as TILO_WORKSPACES_ROOT_FILE'
  else
    (umask 077 && printf '%s\n' "$TILO_TICKET" > "$credentials/tilo.ticket")
  fi
fi
export CREDENTIALS_DIRECTORY=$credentials

# `invite` (the setting the public deployment ships) lists the workspace and
# closes its entry: only people invited or pre-admitted join. `open` lets anyone
# in as the default role. Anything else stops the start, on every start: a
# deployment that still sets what this image no longer honors must not run.
admission=${TILO_WORKSPACES_ADMISSION:-open}
case $admission in
  open|invite) ;;
  *) fail "TILO_WORKSPACES_ADMISSION is open (anyone who asks joins) or invite (only people it invited), not $admission" ;;
esac
role=${TILO_WORKSPACES_DEFAULT_ROLE:-editor}
case $role in
  viewer|commenter|editor) ;;
  *) fail "TILO_WORKSPACES_DEFAULT_ROLE is viewer, commenter or editor, not $role" ;;
esac

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
      # A listed workspace says what it is: the deployment's description, or the
      # name people know it by.
      description=${TILO_WORKSPACES_DESCRIPTION:-$name}
      printf '\n[[kept]]\nid = "%s"\nname = "%s"\nentry = "%s"\ndefault_role = "%s"\ndescription = "%s"\n' \
        "$id" "$name" "$admission" "$role" "$description"
      # The wait list's size, contact and mail are the workspace's own entry
      # settings.
      # The mail API key is not: it stays where this deployment reads it
      # (`TILO_WORKSPACES_RESEND_KEY_FILE`), never in the workspace's records.
      if [ -n "${TILO_WORKSPACES_WAIT_LIST_MAX:-}" ]; then
        printf 'wait_list_max = %s\n' "$TILO_WORKSPACES_WAIT_LIST_MAX"
      fi
      if [ -n "${TILO_WORKSPACES_CONTACT:-}" ]; then
        printf 'contact = "%s"\n' "$TILO_WORKSPACES_CONTACT"
      fi
      if [ -n "${TILO_WORKSPACES_MAIL_FROM:-}" ]; then
        printf 'mail_from = "%s"\n' "$TILO_WORKSPACES_MAIL_FROM"
      fi
      if [ -n "${TILO_WORKSPACES_MAIL_TEMPLATES:-}" ]; then
        printf 'mail_templates = "%s"\n' "$TILO_WORKSPACES_MAIL_TEMPLATES"
      fi
    done
  } > "$config"
fi

exec tilo-runtime
