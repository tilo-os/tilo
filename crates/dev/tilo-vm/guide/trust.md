# Trust: signed layers and the development key

A layer of the `vm` slot, or of a laptop's `device` slot, changes the
machine's own `/usr`. tilo-vm never merges one itself. It reaches `/usr` the
way a downloaded system extension does on a production device: as a **signed
layer**, verified by the system extension activation
(`tilo-extension-activate.service`), the production verifier, at every boot and
after every change. `systemd-sysext.service` stays masked on the dev image,
as on a production one. Your slots' own layers (headless, headful) never touch
`/usr` and need no signature.

The pieces:

- **The development key**, `keys/dev-trust.key` beside `vms.toml` (an Ed25519
  key, mode 0600; its public half is `keys/dev-trust.pub`). It never leaves
  this host. tilo-vm makes it the first time it needs it.
- **Enrollment**: the machine trusts that key once its public half is the
  extension authority `dev` in `/etc/tilo/extension-authorities.d/dev.pub`.
  Production devices ship that directory empty; adding a key is the owner's
  decision, and `tilo-security-audit` lists every enrolled authority, by
  fingerprint, under development posture.
- **Signing**: before a swap, rollback, userspace apply or restart of the `vm`
  or `device` slot, tilo-vm asks the machine which of those slots' layers wait
  for a signature. For each, tilo-vmd seals the files with fs-verity and writes
  a manifest of exactly those bytes; tilo-vm checks the manifest describes the
  layer it asked for (its content hash is the layer's name) and signs it.
- **Activation**: the activation checks the signature against the enrolled
  authorities, then every file (by its fs-verity digest, unread), link and
  executable bit, and merges only what passes. A layer changed on disk after
  it was signed, an unsigned one, or one signed by a key the machine has not
  enrolled is refused and named in `/run/tilo/signed-layers.json` and the
  journal; the swap fails and says why.

Signing adds one fs-verity pass and one read over a new layer, on all cores,
to a swap into those slots, and nothing to a swap into your own slot.

```sh
tilo-vm trust status          # this host's key, what the machine trusts, unsigned layers
tilo-vm trust enroll          # enroll this host's key, sign every vm/device layer, merge them
```

## VMs tilo-vm runs

A VM that tilo-vm created takes this host's key at its first swap into the
`vm` slot, and says so. Nothing to do.

## A real machine: once, by its owner

A board or a laptop never enrolls a key by itself. The first swap into its
`vm` or `device` slot says so, with the command:

```sh
tilo-vm --target yoga trust enroll
```

It enrolls this host's key as `dev`, signs the layers the `vm` and `device`
slots already have (every generation, so rollbacks keep working), and merges
the current ones. Then restart the desktop onto them:

```sh
tilo-vm slot restart device
```

Run `trust enroll` again whenever you like: what is already in place stays.
Another build host enrolls its own key the same way; the machine trusts each
one it enrolled. To stop trusting a key, its owner removes its file from
`/etc/tilo/extension-authorities.d/` (`tilo-vm vm ssh yoga --raw -- rm
/etc/tilo/extension-authorities.d/dev.pub`); its layers are refused from the
next activation on.

## Moving a machine from unsigned layers

A dev image from before signed layers merged tilo-vm's layers with
`systemd-sysext` directly. Moving such a machine needs no reinstall and keeps
every slot and layer:

1. Update tilo-vm on this host (`cargo build -p tilo-vm`).
2. Move the machine onto a current dev image:
   `tilo-vm vm upgrade yoga --from sha:<commit>`. It reboots. Until step 3 its
   `vm` and `device` slots run the image as installed: their layers are not
   signed yet, and nothing merges what is not.
3. `tilo-vm --target yoga trust enroll`. tilo-vmd removes the links the old
   merge left in `/var/lib/extensions` (the activation refuses to merge while
   anything it did not verify is in the search path), and the layers come
   back, signed.
4. `tilo-vm slot restart device`.

## When a layer is refused

`tilo-vm vm ssh -- cat /run/tilo/signed-layers.json` names each refused layer
and why, and `journalctl -u tilo-extension-activate` says the same. A layer
whose bytes changed is not repaired: swap the component again, which brings a
fresh layer and signs it, or roll back. A key the machine no longer trusts:
`tilo-vm trust enroll`.
