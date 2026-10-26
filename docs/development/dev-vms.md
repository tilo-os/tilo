# Try a change in a development VM

Use a dev VM when the change needs the Linux OS or its services, rather than
just a [desktop window](run-desktop.md). Each **slot** has its own user, session
and component layers over the image. A component swap creates a generation you
can roll back.

The canonical entry point is `tilo device guide quickstart`
([guide source](../../crates/dev/tilo-vm/guide/quickstart.md)). The examples
below use the standalone `tilo-vm` executable. If it isn't installed, install
the tool from your checkout first:

```sh
cargo install --locked --path crates/dev/tilo-vm --bin tilo-vm
```

`tilo-vm rollback` rolls back slot layers, not the local operating system.

## Select a VM

If you already have a registered development VM, select it:

```sh
tilo-vm vm list
tilo-vm vm use dev
tilo-vm status
```

Replace `dev` with the registered name. `vm use` selects it for this worktree in
`.tilo-vm`. The status report names the selected VM and slot; the default slot
is based on your branch. Use `--slot NAME` explicitly when several tasks share a
branch.

If you need your own VM:

```sh
tilo-vm doctor
tilo-vm vm create dev
tilo-vm vm start dev
```

Creation downloads and checks a published disk; it does not build an image. It
needs package-read credentials and the platform's hypervisor tools. `doctor`
diagnoses prerequisites. See `tilo-vm guide platforms` for Linux's rootless
libvirt session, macOS UTM and Windows adapters; do not substitute a generic
QEMU recipe.

## Put your component into a slot

Creating a slot starts its session. `--identity` provisions a throwaway
development identity, bypassing first-login setup only in that test slot.

```sh
tilo-vm slot create --headless --identity
tilo-vm swap shell
tilo-vm control actions list
tilo-vm shot -o target/after-swap.png
tilo-vm rollback
```

Worktree swaps normally build in the VM; registered remote builders can supply
device builds. `--profile dev` selects a faster compile and slower executable.
Use `tilo-vm guide swapping` for component names, app swaps, build placement and
matched runtime requirements. Do not deploy binaries with SSH/SCP.

For DRM and kernel-input tests, create a **headful** slot instead:

```sh
tilo-vm slot create display-check --headful --identity
tilo-vm swap shell --slot display-check
tilo-vm shot --slot display-check -o target/display-check.png
```

Headful slots have their own virtual display/seat; they do not automatically
appear in the hypervisor window. `tilo-vm guide driving` covers seat claims,
input and the distinction between compositor snapshots and hypervisor shots.

## Changed the image, not a component?

Units, packages, Containerfiles and boot configuration can require an image
upgrade, which reboots **every** session. Coordinate with other users first. Use
a published, reviewed dev-image reference to avoid a local image build:

```sh
tilo-vm vm upgrade --image registry.tilo.so/tilo/tilo-vm:REVIEWED_TAG
tilo-vm vm rollback-image
```

Replace `REVIEWED_TAG` with the published artifact's tag or use its digest.
`--from sha:COMMIT` can fall back to building a dev layer locally if only a base
image exists; it is not a guaranteed download-only route. See
`tilo-vm guide images` for image compatibility and retained layers, and
[the image checks](../../t.toml) for remote build inputs. Image publication is
explicit; do not assume a push produced one.

## Inspect and finish

`tilo-vm logs` reports the session journal; `slot show` lists generations and
file hashes. `tilo-vm guide troubleshooting` explains failures. Remove only your
disposable slot with `tilo-vm slot rm`; release any seat you claimed first. Stop
or remove a VM only if you own it and nobody else needs it.
