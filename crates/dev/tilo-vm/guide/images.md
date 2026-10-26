# Image-level changes

`swap` replaces components inside a slot. Everything else lives in the VM's
image: `os/Containerfile` and `os/Containerfile.dev`, systemd units, `/etc`,
packages, the pieces `os/components.toml` does not list. For those, rebuild the
image and move the running VM onto it:

```sh
tilo-vm vm upgrade --ref my-branch     # t builds and publishes the branch, the VM pins it, reboot
tilo-vm vm upgrade --from sha:1a2b3c4  # main's build of a commit, the same way
tilo-vm vm upgrade                     # back to the VM's channel's newest image
tilo-vm vm rollback-image              # back to the image before, if the new one is bad
```

A published image goes the way every device takes an update
(docs/development/devices.md): the VM pins it in its channel's place
(`tilo-update-channel pin`) and its own `tilo-verified-update` checks the
signature and stages it.

No new disk and no new VM: slots, layers and in-VM build dirs live in `/var`,
which bootc keeps across images. Sessions restart with the reboot.

Slots keep their layers across the change: they are your deployed work, and
only an explicit `tilo-vm rollback` or `tilo-vm gc` deletes any (gc never the
device slot's current generation or one with a held layer). Each layer was
built against the image before, though, so the first boot of a new image
(`tilo-vm-reconcile-image.service` records every booted image) checks each one:
built against the new base, it merges; otherwise it still merges when the
interface versions it was built for match the new image's
`system-interfaces.toml`; otherwise it is held, kept but not merged, so the
image's own component runs, and the desktop tells you which layers are held and
why. `vm upgrade` and `tilo-vm status` say the same. A new swap of the component
builds against the new image; swapping a held layer again brings it back. A
machine whose previous image was never recorded counts as no change.

bootc fetches only the layers of the new image it does not hold yet, and needs
room for those alone: what an earlier attempt downloaded before it was cut
short is reused. When even that does not fit, the error says how much is
needed and how much is free.

## Pick the fastest loop

| What changed | Loop |
|---|---|
| a component `os/components.toml` lists (shell, fs, an app, ...) | `tilo-vm swap <component>`: seconds, no reboot (`tilo-vm guide swapping`) |
| anything else, on a pushed branch or main | `tilo-vm vm upgrade --ref <branch>` or `--from sha:<commit>` |
| anything else, where t cannot publish the branch yet | for now: `tilo-vm vm upgrade --from local`, or build on another machine (below) and `vm upgrade --image target/tilo-vm/image` |

Swap whatever it can swap. An image upgrade reboots the VM and restarts every
slot's session, including other agents'.

## From a published build: `--ref <branch>`, `--from sha:<commit>`

Every image a device may follow is signed with the development key: main's by
`publish-os-dev-<arch>` and `publish-os-board-<board>` in
`registry.tilo.so/tilo/tilo-vm`, a branch's by `publish-os-ref-<target>` in the
branches' own `registry.tilo.so/tilo/tilo-vm-ref`:

| Tag | What |
| --- | --- |
| `<arch>-ci`, `<board>-ci` | the dev channel: main's newest gated image |
| `main-<sha7>-ci-<arch>`, `main-<sha7>-<board>` | main's build of one commit |
| `tilo-vm-ref:ref-<branch>-<sha7>-<target>` | a branch's build, published when dispatched |

`--ref <branch>` runs `t dispatch publish-os-ref-<target> --ref <branch>
--publish` for the VM's target (its board, else its architecture) and moves the
VM onto the result. `--from sha:<commit>` takes the build of that commit: main's,
else a branch's found by listing the tags. A pinned build lasts until the VM's
channel publishes something newer; `tilo-vm vm upgrade` without an image returns
to the channel at once. To see what exists:
`skopeo list-tags docker://registry.tilo.so/tilo/tilo-vm`.

## Building somewhere else: `tilo-vm image build`

The local builds below (`image build`, `--from local`, `--image <layout>`) are the
temporary path for an image t cannot yet publish from a branch: they send an
unsigned image over SSH instead of the device's signed update path, and go away
once every branch can be published (docs/development/devices.md).

`tilo-vm image build` runs the same build as `--from local` without a VM and
leaves the dev image as an OCI layout, `target/tilo-vm/image`, with its
`.facts` and `.digest` beside it. `vm upgrade --image <that layout>` moves a VM
onto it. `tilo-vm image disk` writes a dev disk from it,
`target/tilo-vm/disk.qcow2` (`image build --disk` does both in one go), for
`vm create --image`.

A laptop does not have to compile the OS: run `image build` on a build
machine, in a copy of the worktree inside the dev build container, and bring
`target/tilo-vm` back:

```sh
# on the build machine, in the synced worktree:
export SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)   # the commit time dates the image (needed without git)
tilo-vm image build
# back here, after syncing target/tilo-vm:
tilo-vm vm upgrade --image target/tilo-vm/image       # new layers into the VM, reboot

# no VM yet, and no published disk to create one from:
tilo-vm image disk                                    # here: loop device, sudo
tilo-vm vm create dev --image target/tilo-vm/disk.qcow2 && tilo-vm vm start
```

- Sync the worktree over, uncommitted edits included, and mirror
  `target/tilo-vm` back incrementally (rsync or similar): blobs the laptop has
  stay, only new layers cross.
- On the build machine the build is incremental like a local one: the base is
  rebuilt only when its inputs changed (outside a git checkout its files are
  fingerprinted directly), podman's layer cache and the compile's build
  directory and sccache under os/output (os/image/compile.sh runs outside the
  image build) keep everything else, and a failed step reruns on its own.
- The nested podman keeps its image store in whatever directory the build
  container binds at `/var/lib/containers`; a persistent directory there is the
  whole cache.
- The disk is written where loop devices are, not in a rootless build
  container. A new disk replaces `target/tilo-vm/disk.qcow2`,
  which backs any VM made from it: keep a VM current with `vm upgrade
  --image`, and write a disk only for a new VM.

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
tilo-vm vm upgrade --from local --base registry.tilo.so/tilo/tilo:main-1a2b3c4   # a CI base, as it is
tilo-vm vm upgrade --from local:localhost/tilo-vm:mine   # an image already built; no build
tilo-vm vm upgrade --from local --no-reboot            # stage only; boots at the next restart
```

## Going back

bootc keeps the image the VM ran before its last upgrade as the rollback
deployment. `tilo-vm vm rollback-image` makes it the default and reboots;
running it again returns to the newer one. `bootc status` in the VM
(`tilo-vm vm ssh -- bootc status`) shows both. An older image than that is
gone; rebuild it or upgrade to a published one:
`tilo-vm vm upgrade --image registry.tilo.so/tilo/tilo-vm:<tag>`.

## Storage

The host keeps one uncompressed copy of the image per VM in
`~/.cache/tilo-vm/images/<vm>/`, and the VM one in `/var/lib/tilo-vm/image`;
both are what makes the next upgrade send only the difference. Deleting either
is safe: the next upgrade sends the whole image once.

## When it refuses

- **not named as a dev image** or **not a dev image**: a production image
  (`localhost/tilo:latest`, `registry.tilo.so/tilo/tilo:*`) is sealed and has no
  sshd or tilo-vmd. Build the dev image FROM it: `--from local`.
- **is sealed**: the image carries a UKI; only `os/Containerfile.dev` output
  (which unseals its base) can go into a dev VM.
- **older base**: your `localhost/tilo:latest` predates the VM's base.
  `--rebuild-base`, rebase, or `--allow-older-base`.
- **busy**: a swap, gc or another upgrade holds the VM lock; wait and rerun.
