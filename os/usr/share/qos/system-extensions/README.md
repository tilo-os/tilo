# qOS system-extension contract (version 1)

qOS is an immutable bootc/composefs image. Optional system extensions are
desired state for a future image deployment, never an in-place package-manager
transaction.

`catalog.d/` and `trust.d/` are image-owned inputs: release builds put only
pinned catalogue entries and public verification keys there. They are not
mutable configuration directories, and private signing keys must never enter
the image. `state.json` lives at `/var/lib/qos/system-extensions/state.json`.
The `system-extension` crate writes it atomically as `root:root` mode `0600`.
It verifies a manifest's Ed25519 signature, an exact base image digest,
optional exact kernel ABI/architecture, and the local artifact receipt before
an extension becomes active desired state.

There are two deliberately different activation classes:

* `local_model_runtime` is userspace-only. The constrained
  `qos-system-extension-userspace.service` creates a runtime eligibility
  marker. The per-user `qos-model-manager` independently validates the typed,
  signed local-model catalog and binds vLLM only to an allocated loopback
  endpoint. Settings reaches it through the launcher-owned `models@1` service
  descriptor; it receives no shell command, URL, or ambient app authority.
* `kernel_module` and `gpu_driver_stack` are boot-bound. The constrained
  `qos-system-extension-boot-stage.service` atomically creates a
  `reboot-required` marker but never touches `/boot` or loads a driver. The
  immutable deployment builder must call `ExtensionState::accepted_for` with
  the exact new deployment digest and kernel ABI before consuming it. The
  resulting deployment is rebooted and covered by ordinary qOS boot-health and
rollback handling.

## BootPack catalogue consumer

Unprivileged code does not translate PCI IDs to a driver pack. It may ask only
for `boot-pack-recommend`; the root-owned manager reads the local PCI sysfs
facts and verifies a separate image-owned PCI recommendation catalogue signed
by its own pinned public key. Each rule requires an exact vendor/device pair;
an exact subsystem pair wins over a generic device rule, and duplicate rules
are rejected. The response contains only exact BootPack catalogue ids. Settings
or the unprivileged model manager can then enqueue only
`boot-pack-stage <catalog-id>` on the fixed request socket. The catalog id is
an identifier, not a path or image reference. The root-owned
`qos-system-extension-manager` resolves only
`catalog.d/<catalog-id>.json` and `trust.d/<key-id>.json` from the signed base
image, verifies the manifest signature and exact running image/kernel
compatibility, and accepts only reboot-bound entries whose `artifact.source`
is a `bootc://registry/path@sha256:<digest>` reference matching the signed
artifact digest.

The staging manager repeats the PCI match, so an exact id from another machine
is not authorization to stage a pack here. The socket worker writes only that
id to a root-owned fixed queue and returns `queued`; it has no network. A
separate path-activated manager re-verifies the catalog and then runs the one
fixed operation `bootc switch --transport=registry` with that exact digest
reference. This stages an immutable deployment; it never hot-loads a driver,
runs package-manager commands, or reboots the machine. qOS never hot-installs a
driver. The non-secret status projection reports `queued`,
`staging`, `staged`, `failed`, or
`active`, plus `activation_class: boot_pack`, the exact extension id, staging
generation, a bounded error if any, and whether a reboot is still required. On
the next boot it is recomputed from the booted deployment digest, so qOS never
claims a staged driver is already active without evidence.

Settings presents those states as one accelerator-setup card. The model
manager asks for the root-probed recommendation again when the user chooses
setup, and stages only when exactly one signed BootPack matches. Model download
remains disabled while the BootPack is queued or staged. Once staging finishes,
Settings asks the user to restart through the normal system power UI; neither
Settings nor the model service has a restart verb. After boot, local model
setup becomes available only when the root-owned status projection says the
digest-pinned deployment is active.

`/var/lib/qos/local-models/staging` is root-only transfer workspace.
`verified` is root-owned and group-readable by the local-model runtime; only a
verified backend may promote bytes into it. No helper in this image fetches an
artifact or interprets a manifest as a command.

## Desktop request API

Settings must not run `qos-model-manager` with privilege or wait for its
synchronous install path. It can read the non-secret status projection at
`/run/qos/system-extension-api/status.json` and enqueue without waiting by
writing exactly one line to the group-connectable Unix socket
`/run/qos/system-extension-api/request.sock`, then closing the connection:

* `userspace-stage`
* `boot-stage`
* `boot-pack-recommend`
* `boot-pack-stage <catalog-id>`

The socket is `root:qos-identity` mode `0660` inside a root-owned non-writable
directory. Its `Accept=yes` systemd service runs in the background and accepts
no other words or fields. In particular there is no `restart` request: a boot
stage only writes `reboot-required`. A future deployment composer may restart
only after it has verified and staged a BootPack from the signed catalogue
against the exact running deployment digest and kernel ABI.
