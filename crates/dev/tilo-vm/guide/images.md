# Image-level changes

`swap` replaces components inside a slot. Everything else lives in the VM's
image: `os/Containerfile` and `os/Containerfile.dev`, systemd units, `/etc`,
packages, the pieces `os/components.toml` does not list. For those, rebuild the
image and move the running VM onto it:

```sh
tilo-vm vm upgrade --from local        # build, send the new layers, bootc switch, reboot
tilo-vm vm upgrade --from sha:1a2b3c4  # or: a CI build of main, no OS build here
tilo-vm vm rollback-image              # back to the image before, if the new one is bad
```

No new disk and no new VM: slots, layers and in-VM build dirs live in `/var`,
which bootc keeps across images. Sessions restart with the reboot.

## Pick the fastest loop

| What changed | Loop |
|---|---|
| a component `os/components.toml` lists (shell, fs, an app, ...) | `tilo-vm swap <component>`: seconds, no reboot (`tilo-vm guide swapping`) |
| anything else, and CI has built the commit | `tilo-vm vm upgrade --from sha:<commit>` |
| anything else, not pushed yet | `tilo-vm vm upgrade --from local` |

Swap whatever it can swap. An image upgrade reboots the VM and restarts every
slot's session, including other agents'.

## From a CI build: `--from ci:<tag>` / `--from sha:<commit>`

Every main build pushes its OS image, unsigned, to
`code.tilo.so/tilo/tilo-ci:main-<sha7>-<arch>` (`sha:<commit>` is short for
`ci:main-<sha7>`). `vm upgrade` then takes, in order, the first of these that
exists:

1. the dev image CI published for that build,
   `code.tilo.so/tilo/tilo-vm:main-<sha7>-ci` (or `:main-<sha7>` from a signed
   release). The VM pulls it with its own registry login: nothing is built or
   sent from this host;
2. the integration image itself (then the signed `tilo:main-<sha7>`). It is
   pulled here and used as the base of `--from local`'s dev image build, which
   compiles only the dev layers, with this worktree's `os/Containerfile.dev`.
   The result is `localhost/tilo-vm:ci-<tag>`, sent and switched to like any
   local image.

Nothing on this host rebuilds the production image. To see which builds
exist: `skopeo list-tags docker://code.tilo.so/tilo/tilo-ci`.

## Building the base here

`--from local` builds the production base `localhost/tilo:latest` itself when
its inputs changed, with the same `os/Containerfile` and scripts CI uses. Local
builds link without LTO (`TILO_RUST_LTO=off`, recorded in the image's
`org.tilo.rust-lto` label) and keep the Rust target cache between builds, so
after a one-crate change only the binaries that depend on it are relinked, and
a change under `os/` that no binary reads does not recompile at all. Published
images use thin LTO.

## What `--from local` does

1. **Build**, with `os/dev/build-dev-image.sh` (`TILO_DEV_DISK=0`), which is
   also `./os/build.sh --dev --local`:
   - the production base `localhost/tilo:latest` is rebuilt only when its inputs
     changed: every file of its build context except the dev-image-only
     sources (`os/Containerfile.dev`, `os/dev-image/`, ...), hashed from git and
     compared with the base's `sh.tilo.base-inputs` label. The inputs for this
     machine (RPM snapshot, kernel, the non-production owner key, no browser)
     are resolved by `os/dev/local-inputs.sh`, which says what it chose and why;
   - the dev image `localhost/tilo-vm:latest` is rebuilt FROM it with podman's
     layer cache, so only the steps after your change run. Editing `/etc`, a
     unit or `os/dev-image/` does not recompile the dev binaries: that stage
     copies only what cargo reads.
2. **Check** the image: named `localhost/tilo-vm:<tag>` (never a production
   name), labelled `sh.tilo.image.flavour=dev`, and not sealed (no UKI under
   `/boot/EFI/Linux`). Its production base must not be older than the VM's
   (`base_created` in `/usr/share/tilo-vm/base.json`); `--allow-older-base`
   goes back on purpose.
3. **Send only new layers.** `skopeo` writes the image into an OCI layout on
   this host (`~/.cache/tilo-vm/images/<vm>/`, reusing every blob it already
   has), `rsync` mirrors it to `/var/lib/tilo-vm/image` in the VM over the
   VM's SSH (only new blobs cross), and bootc imports only the layers its store
   lacks.
4. **Switch and reboot**: `bootc switch --transport oci
   /var/lib/tilo-vm/image:tilo-vm` (then `bootc upgrade` on later runs) under
   the VM lock, a reboot, and a wait for a new boot, SSH and tilo-vmd. The
   report names the booted digest, the image kept for rollback and each phase's
   time.

Why an OCI layout and not a registry: it needs nothing but the SSH every other
command uses, so it works the same under libvirt, UTM and Hyper-V; there is no
registry container to keep running, no port to forward into the VM and no
insecure-registry setting to add to the image.

```sh
tilo-vm vm upgrade --from local --rebuild-base        # rebuild the base even if it looks current
tilo-vm vm upgrade --from local --base code.tilo.so/tilo/tilo:main-1a2b3c4   # a CI base, as it is
tilo-vm vm upgrade --from local:localhost/tilo-vm:mine   # an image already built; no build
tilo-vm vm upgrade --from local --no-reboot            # stage only; boots at the next restart
```

## Going back

bootc keeps the image the VM ran before its last upgrade as the rollback
deployment. `tilo-vm vm rollback-image` makes it the default and reboots;
running it again returns to the newer one. `bootc status` in the VM
(`tilo-vm vm ssh -- bootc status`) shows both. An older image than that is
gone; rebuild it or upgrade to a published one:
`tilo-vm vm upgrade --image code.tilo.so/tilo/tilo-vm:<tag>`.

## Storage

The host keeps one uncompressed copy of the image per VM in
`~/.cache/tilo-vm/images/<vm>/`, and the VM one in `/var/lib/tilo-vm/image`;
both are what makes the next upgrade send only the difference. Deleting either
is safe: the next upgrade sends the whole image once.

## When it refuses

- **not named as a dev image** or **not a dev image**: a production image
  (`localhost/tilo:latest`, `code.tilo.so/tilo/tilo:*`) is sealed and has no
  sshd or tilo-vmd. Build the dev image FROM it: `--from local`.
- **is sealed**: the image carries a UKI; only `os/Containerfile.dev` output
  (which unseals its base) can go into a dev VM.
- **older base**: your `localhost/tilo:latest` predates the VM's base.
  `--rebuild-base`, rebase, or `--allow-older-base`.
- **busy**: a swap, gc or another upgrade holds the VM lock; wait and rerun.
