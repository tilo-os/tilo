# Identity on headless devices

A board without a screen, an agent host or a server joins your Tilo identity
from a shell. `tilo-identity` is another door to what first-use onboarding,
Settings > Devices & Sync and the shell's `identity.*` actions do, so a device
set up over SSH ends up exactly like one set up at a desktop.

Run every command as the account that owns the identity, never with `sudo`. A
step that needs an administrator asks for one itself: polkit in a desktop
session, `sudo` anywhere else. Every command takes `--json` and then prints one
JSON object, errors included (`{"error": "...", "next": "..."}`); `next` always
says what to do.

## Which case are you in?

```sh
tilo-identity status
```

- **"No identity"**, and you have no Tilo identity anywhere: create one here.
- **"No identity"**, but another device already has your identity: pair this
  device with it.
- An identity and a list of devices: you are done.

## Create a new identity

Nobody can recover an identity for you. Losing every device that holds it, and
its backup, loses it for good. `create` therefore asks you to acknowledge that,
and to decide about the backup, as onboarding does:

```sh
tilo-identity create --acknowledge-no-recovery-service \
  --backup ~/tilo-identity.backup --passphrase-file ~/pass.txt
```

- `--backup PATH` writes the passphrase-encrypted backup onboarding offers,
  checks that it opens again, and never replaces a file. Copy it off the device.
  Without `--passphrase-file`, the terminal asks for the passphrase (12
  characters or more) twice; `--passphrase-file -` reads it from stdin.
- `--no-backup-i-understand` creates the identity without one. Write it later
  with `tilo-identity backup --to PATH`.

The identity is kept where onboarding keeps it. Log in again afterwards so the
session runs as it.

## Pair a new device with an existing identity

Both devices must be on the same network segment: pairing finds the other
device with a UDP broadcast on port 23831. Their clocks must be roughly right.

On the **new** device:

```sh
tilo-identity pair --label "Orange Pi 3B"
```

It shows a six-digit code and waits up to 270 seconds. The account must be in
the `tilo-identity` group, as the account first-use onboarding runs in is; no
administrator is needed.

On a device that **already has** the identity:

```sh
tilo-identity approve --code 123456
```

`approve` finds the waiting device, refuses it if its code differs, and admits
it through the same privileged helper Settings uses. Name the device with
`--device <start of its key>` when the code alone is not enough for you. Then
log in again on the new device so its session runs as the identity, and check
either device with `tilo-identity status`.

Any of your devices can approve, the same way. When the device you approve on
cannot add devices by itself (a device you paired, rather than set up first),
your other devices ask you too: each shows "A device asks to be you", naming
the new device, the device that found it and its code. Approve it there when
the new device shows the same code; if more than one of your devices must
agree, each asks, and `approve` says how many still have to. A device you
removed is refused before anyone is asked. Home servers are added from the
device you set up first.

A device holding the key without a screen answers the same ask from a shell:

```sh
tilo-identity asking                  # the devices asking, their codes, who found them
tilo-identity approve --code 123456   # when the new device shows that code
tilo-identity decline --code 123456
```

`approve` answers a device asking here before it looks on the network, so the
one command approves either way.

## Devices

```sh
tilo-identity devices                         # every device of the identity
tilo-identity revoke <start of a key> --reason lost --yes
```

A revoked device can no longer act as the identity. You cannot revoke the
device you are using; do it from another one.

## What happens underneath

- `create` calls `provisioning::ensure_collaboration_identity`, as onboarding
  does, and `runtime_identity::export_portable_backup`, as onboarding's backup
  step does.
- `pair` runs `tilo-enroll pair-device --headless` as you, and reads the
  status file first-use onboarding reads.
- `approve` runs `tilo-enroll discover-device`, then
  `tilo-home-peer-control approve-device` through polkit or sudo, with the
  arguments the shell's `identity.device.pair.approve` action passes
  (`tilo_identity_host::device_admin`). The helper runs
  `tilo-enroll approve-device` as you, or as root when the machine keeps the
  identity's root key under `/var/lib/tilo`, writing your identity directory
  only as you.
- On a device without the whole key, `approve` takes the session's
  `identity.device.pair.discover` and `.approve` actions instead. The session
  proposes the certificate to your other devices over their private fabric
  (`tilo_identity_host::certification_ceremony`,
  `crates/session/shell/src/shell/device_certification.rs`), gathers the
  signature (FROST shares are combined there), and runs
  `tilo-enroll approve-device --approved-cert`. A device holding the key
  signs its answer with the identity's key, as every build expects; one
  without it signs with its own certified key and names its certificate. The
  joining device accepts either, the latter only from the device that minted
  its token while the identity certified it.
- `asking`, and `approve` or `decline` of a device asking, are the session's
  `identity.device.pair.asking` and `.answer` actions; `.answer` signs through
  the same step as the "A device asks to be you" notification's Approve.
- `devices` and `revoke` are the shell's `identity.devices.list` and
  `identity.device.revoke` steps.

## Troubleshooting

| Message | What to do |
|---|---|
| `no new device was found nearby` | Start `tilo-identity pair` on the new device first; check that both are on one network segment. |
| `no trusted Tilo device approved nearby pairing` | Run `tilo-identity approve` on the other device while `pair` waits. |
| `this device cannot approve new devices` | Approve from the device that created the identity. |
| `none of your devices online now can approve it` | Turn on the device you set up first, then pair again. |
| `It was not approved in time` | Approve on your other device within about three minutes, or pair again. |
| `this device's key ... is missing` | Make sure your account is in the `tilo-identity` group (`id -nG`), then log in again. |

See also [Device sync, remote control and serving mesh](device-mesh.md) and
[Identity signing boundary](identity-security.md).
