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
  marker. The future `qos-local-model-service` binary independently validates
  the typed local-model catalog/request and binds only a loopback endpoint. It
  receives no shell command, URL, or ambient download authority.
* `kernel_module` and `gpu_driver_stack` are boot-bound. The constrained
  `qos-system-extension-boot-stage.service` atomically creates a
  `reboot-required` marker but never touches `/boot` or loads a driver. The
  immutable deployment builder must call `ExtensionState::accepted_for` with
  the exact new deployment digest and kernel ABI before consuming it. The
  resulting deployment is rebooted and covered by ordinary qOS boot-health and
  rollback handling.

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

The socket is `root:qos-identity` mode `0660` inside a root-owned non-writable
directory. Its `Accept=yes` systemd service runs in the background and accepts
no other words or fields. In particular there is no `restart` request: a boot
stage only writes `reboot-required`. A future deployment composer may restart
only after it has verified and staged a BootPack from the signed desired state
against the exact target deployment digest and kernel ABI.
