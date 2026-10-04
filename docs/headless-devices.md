# Identity on headless devices

A board without a screen, an agent host or a server joins your Tilo identity
from a shell. `tilo-identity` is the command-line door to the same code that
first-use onboarding and Settings > Devices & Sync run, so a device set up over
SSH ends up exactly like one set up at a desktop: same identity, same
workspaces, and recorded on both sides so the two devices find each other
again after a reboot.

Run every command as the account that owns the identity, never with `sudo`.
Steps that need root (the device key, the system device lists) ask for `sudo`
themselves, or for polkit in a graphical session. Every command takes `--json`
and then prints one JSON object, errors included
(`{"error": "...", "next": "..."}`); the `next` field always says what to do.

## Which case are you in?

```sh
tilo-identity status
```

- **"No identity"** and you have no Tilo identity anywhere: create one here
  (below).
- **"No identity"** but another device already has your identity: pair this
  device with it (below).
- An identity and a list of devices: you are done; the list shows which of
  your devices are online now.

## Create a new identity

There is no escrow or reset service. Losing every device that holds the
identity, and its backup, loses the identity for good. `create` therefore asks
for that acknowledgement and for a decision about the backup, exactly as
onboarding does:

```sh
tilo-identity create --acknowledge-no-recovery-service \
  --backup ~/tilo-identity-backup.tilo-identity --passphrase-file ~/pass.txt \
  --restart-session
```

- `--backup PATH` writes the passphrase-encrypted recovery file onboarding's
  backup step writes, checks that it opens again, and never overwrites a file.
  Copy it off the device. Without `--passphrase-file`, the passphrase (at least
  12 characters) is asked twice on the terminal; `--passphrase-file -` reads it
  from stdin.
- `--no-backup-i-understand` creates the identity without one. Write it later
  with `tilo-identity backup --to PATH`.

`--restart-session` restarts the headless board session so it runs as the new
identity; without it, run `tilo-identity restart-session` later (on a desktop,
log out and back in instead). `status` says when the running session does not
run as the identity yet.

The identity is stored like onboarding stores it: in the desktop Secret
Service when one answers, otherwise in a private file under
`~/.config/tilo/identity/secrets/` (shown as "Private session file" by
`status`).

## Pair a new device with an existing identity

Both devices must be on the same network segment: pairing finds the other
device with a UDP broadcast on port 23831, and their clocks must be roughly
right.

On the **new** device:

```sh
tilo-identity pair --label "Orange Pi 3B" --restart-session
```

It prints a six-digit code and waits up to 270 seconds. `--restart-session`
restarts the headless board session afterwards so it runs as the joined
identity (leave it out on a desktop and log in again instead).

On a device that **already has** the identity, as its owner:

```sh
tilo-identity approve --code 123456
```

`approve` finds the waiting device, refuses it if its code differs, certifies
it, and sends it the identity's device list, home servers and workspace rooms.
Both devices then record each other as direct peers, so they reconnect on
their own later, with or without a home server. Several devices waiting? Pick
one with `--device <id prefix>`.

Check the result on either device:

```sh
tilo-identity status
```

The other device appears as `online in N room(s)` within a few seconds of both
sessions running. The first device's workspaces appear on the new device,
first as "From another device" and then under their own names, with the apps
open in them; later changes on either device follow.

Apps that are open on one device are also opened on the other when
"Allow Continuity from this device" and "App continuations" are switched on in
Settings > Devices on both. Without a screen, the same switches are
`{"continuity": {"enabled": true, "app_continuations": true}}` in
`~/.config/tilo/settings.json`, read when the session starts.

## Restarting the session

```sh
tilo-identity restart-session
```

It stops the board compositor in your own user manager, restarts
`tilo-board-headless-session@$USER`, and waits until the new compositor
reports in, so `status` afterwards shows what it runs as.

## Devices

```sh
tilo-identity devices                         # everything certified for the identity
tilo-identity revoke <id prefix> --reason lost --yes
```

A revoked device can no longer act as the identity. You cannot revoke the
device you are using; do it from another one. Revoking needs the identity's
root key, so run it on a device that can approve devices (`status` shows
`can approve devices`); elsewhere it names the device to use.

What revocation reaches, and when:

- The device you revoke from refuses the revoked device's signatures at once,
  stops dialing it, and drops it from `/var/lib/tilo/personal-device-peers.json`
  within seconds (`tilo-direct-peer-prune.path` applies the revocation that
  `revoke` records in `/var/lib/tilo/device-revocations`).
- Your other devices learn the revocation when they next exchange identities
  with a device that holds it, which happens whenever they connect. From then on
  they refuse the revoked device, remember the revocation in their own fleet,
  and drop it from their direct peers the same way.
- A revocation is permanent and covers everything the revoked key signs from
  then on, whatever time the signature claims.

## What happens underneath

- `create` calls `provisioning::ensure_collaboration_identity`, the function
  onboarding uses, and `runtime_identity::export_portable_backup`, the
  function onboarding's backup step uses.
- `pair` runs `sudo tilo-enroll pair-device --headless`. `approve` runs
  `tilo-enroll discover-device`, then the polkit/sudo helper
  `tilo-home-peer-control approve-device`, the same path Settings uses. The
  helper runs `tilo-enroll approve-device` as you (it signs with your
  identity), then records the new device in
  `/var/lib/tilo/personal-device-peers.json`, which only root may change. It
  accepts only a certificate that names the device you chose, is not a home
  server, and is signed by the same identity that certified this machine's own
  device key.
- The new device receives the approver's room list
  (`~/.local/state/tilo/personal-rooms.json`, kept by the session) and stores
  it as `/var/lib/tilo/personal-rooms.json`, plus every other ordinary device
  of the identity as a direct peer. The running session reads both files every
  two seconds; no restart is needed for an already enrolled session.
- The session publishes what it sees of your devices to
  `$XDG_RUNTIME_DIR/tilo-session/personal-devices.json`, which `status` reads.

## Troubleshooting

| Message | What to do |
|---|---|
| `no device is waiting to pair on this network` | Start `tilo-identity pair` on the new device first; check both are on one network segment. |
| `no approving Tilo device answered` | Run `tilo-identity approve` while `pair` waits. |
| `... needs the identity's root key, which this device does not hold; do this on '<device>'` | Approving, revoking, certifying and backing up stay with the devices holding the root; run the command on the named device. |
| `this device's key ... is missing` | `sudo systemctl start tilo-device-key-init.service`; make sure your account is in the `tilo-identity` group, then log in again. |
| `online state unknown (session not running)` | Start or restart the Tilo session (`tilo-board-headless-session@$USER` on a board). |
| A device shows `not a direct peer` | It was enrolled before devices recorded each other; pair it again. |
| `the running session has no identity yet` | `tilo-identity restart-session`. |

## Known limits

- Without internet access the two devices find each other only after one of
  them has connected to the other: device ids are resolved through iroh's
  public address lookup, and LAN-only mDNS discovery is off by default.
- A device enrolled by another device holds only the identity's public record;
  apps that need to sign as the identity (authored launches) do not start on
  it yet, so a continued app may stay "Available elsewhere" there.

See also [Device sync, remote control and serving mesh](device-mesh.md) and
[Identity signing boundary](identity-security.md).
