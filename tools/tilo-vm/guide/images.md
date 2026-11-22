# Image-level changes

`swap` replaces components inside an account. Everything else lives in the VM's
image: `os/Containerfile` and `os/Containerfile.dev`, systemd units, `/etc`,
packages, the pieces `os/components.toml` does not list. For those, rebuild the
image and move the running VM onto it:

```sh
tilo-vm vm upgrade --from local        # this worktree's image, built on t
tilo-vm vm upgrade --from sha:1a2b3c4  # main's build of a commit, the same way
tilo-vm vm upgrade                     # back to the VM's channel's newest image
tilo-vm vm rollback-image              # back to the image before, if the new one is bad
```

A published image goes the way every device takes an update:
the VM pins it in its channel's place
(`tilo-update-channel pin`) and its own `tilo-verified-update` checks the
signature and stages it.

No new disk and no new VM: accounts, layers and in-VM build dirs live in `/var`,
which bootc keeps across images. Sessions restart with the reboot.

Accounts keep their layers across the change: they are your deployed work, and
only an explicit `tilo-vm rollback` or `tilo-vm gc` deletes any (gc never the
device account's current generation or one with a held layer). Each layer was
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
| a component `os/components.toml` lists (tilo-session, fs, an app, ...) | `tilo-vm swap <component>`: seconds, no reboot (`tilo-vm guide swapping`) |
| anything else, on main | `tilo-vm vm upgrade --from sha:<commit>` |
| anything else, on a branch | `tilo-vm vm upgrade --from local` |

Swap whatever it can swap. An image upgrade reboots the VM and restarts every
account's session, including other agents'.

## From a published build: `--from sha:<commit>`

Every image a device may follow is signed with the development key and
published in `registry.tilo.so/tilo/tilo-vm`:

| Tag | What |
| --- | --- |
| `<arch>-ci`, `<board>-ci` | the dev channel: main's newest guarded image |
| `main-<sha7>-ci-<arch>`, `main-<sha7>-<board>` | main's build of one commit |

`--from sha:<commit>` takes main's dev image of that commit; a commit main has
not published (a branch's, or one whose checks failed) is built on t from a
checkout of it with `--from local`. A pinned build lasts until the VM's
channel publishes something newer; `tilo-vm vm upgrade` without an image returns
to the channel at once. To see what exists:
`skopeo list-tags docker://registry.tilo.so/tilo/tilo-vm`.

## This tree's image and disk, from t

t builds every dev image and disk: main's published ones and yours. The checks
that publish main's build this worktree's state, uncommitted edits included, on
a build server of the image's architecture, and `tilo-vm` downloads the result:

```sh
tilo-vm vm upgrade --from local      # build the image for the VM's architecture, move the VM onto it
tilo-vm image build                  # just build it: target/tilo-vm/target/publish/os-dev/<arch>
tilo-vm image build --disk           # and a disk of it, for a new VM:
tilo-vm vm create dev --image target/tilo-vm/target/publish/os-dev-disk/x86_64/disk/disk.qcow2 --gl
tilo-vm image build --arch aarch64   # for an ARM VM or board, from any host
tilo-vm vm upgrade --image target/tilo-vm/target/publish/os-dev/x86_64   # one image build left
```

The image is t's `os-dev-image-<arch>` check, the disk `os-dev-disk-<arch>`:
the same build, from the same `os/image` pieces and `os/Containerfile.dev`,
that main's dev channel publishes. t keeps every step's answer by what it reads,
so after a one-crate change only that crate's binaries and the layers above them
build again, and a change under `os/` that no binary reads compiles nothing.
Nothing compiles here, and no podman, sudo or loop device is needed; only `t`
(CONTRIBUTING.md says how to set it up). The build's progress shows here, and
a failed one names its step: `t log os-dev-image-<arch>` has the whole log.

`--lean` builds the development loop's image and disk instead
(`os-dev-image-lean-<arch>`): no local model runtimes and no commit named, so
they build faster while working on the disk or the desktop. Their base carries
the OS image's clock rather than the commit's, so a VM on a newer published
image takes a lean image only with `--allow-older-base`; `--from local` builds
the full one. Disks are uncompressed (a 64 GiB sparse qcow2): build one once and
make every VM from it.

## Windows support (Proton) on a dev image

The Windows runners are not in the image: they are the `windows-compat`
download (`/usr/share/tilo/system-extensions/README.md`), a system extension of
about 2.5 GB a device installs from a signed manifest. On x86 it carries Proton;
on ARM it carries FEX and the x86 guest root, and no Proton. A manifest exists
only for an image CI resolved it for, keyed by that image's package set, so test it on a
published dev image, not on a disk of your own tree (its packages resolve anew
and match no manifest: the install answers "Not available for this version of
Tilo yet").

Every published dev image has its payload: `publish-os-dev-<arch>` publishes the
payload for its own commit first (`publish-windows-compat-<arch>`). The signed
manifest follows at the daily `publish-extension-manifests-<arch>` run; for a
dev image published since, sign it now:

```sh
t dispatch --repo tilo --ref main publish-extension-manifests-x86_64 --publish
```

It signs a manifest for every published image whose payload exists; it ends
red while another extension does not resolve, and still publishes the rest (its
log says `published tilo-extension-windows-compat ...`). Then, in a VM of that
image (`tilo-vm vm upgrade --image registry.tilo.so/tilo/tilo-vm:main-<sha7>-ci-x86_64`),
install it the way Settings does:

```sh
sudo systemctl start tilo-extension@install:windows-compat.service
sudo /usr/libexec/tilo-extension status:windows-compat   # installing -> installed
ls /usr/lib/tilo/windows-runners                          # proton on x86; x86 on ARM
```

An image that would change `/usr` when merged (an entry not owned by root,
unlabelled, writable by others, or a directory of the base with another mode or
label) is refused before it is linked: the status reads `failed` with "not
turned on, it would damage the system: <the entry>", and nothing is merged. An
extension that still keeps a start from reaching the graphical target is turned
off by the next start (`tilo-extension-trial.service`).

On a check server, `t test os-vm-x86_64` from a tree that differs from main
gives a VM to reach with `t vm ssh` (a VM of main's own result takes no
connections); move it onto the published image with `bootc switch` first.

## What `--from local` does

1. **Build** this worktree's dev image on t for the VM's architecture
   (`os-dev-image-<arch>`), into `target/tilo-vm/` of the worktree.
2. **Check** the image, from its own labels: a dev image
   (`sh.tilo.image.flavour=dev`), not sealed (a board image, whose UKI carries
   its device tree, says `sh.tilo.image.sealed=true`), and built FROM a
   production base no older than the VM's (`sh.tilo.base.created`, compared
   with `base_created` in the VM's `/usr/share/tilo-vm/base.json`);
   `--allow-older-base` goes back on purpose.
3. **Send only new layers.** The image's blobs it lacks are copied into an OCI
   layout on this host (`~/.cache/tilo-vm/images/<vm>/`), `rsync` mirrors it
   to `/var/lib/tilo-vm/image` in the VM over the VM's SSH (only new blobs
   cross), and bootc imports only the layers its store lacks.
4. **Switch and reboot**: `bootc switch --transport oci
   /var/lib/tilo-vm/image:tilo-vm` (then `bootc upgrade` on later runs) under
   the VM lock, a reboot, and a wait for a new boot and its device steps. The
   report names the booted digest, the image kept for rollback and each phase's
   time.

`--image <dir>` with what `image build` left does steps 2 to 4.

Why an OCI layout and not a registry: it needs nothing but the SSH every other
command uses, so it works the same under libvirt, UTM and Hyper-V; there is no
registry container to keep running, no port to forward into the VM and no
insecure-registry setting to add to the image.

```sh
tilo-vm vm upgrade --from local --no-reboot            # stage only; boots at the next restart
tilo-vm vm upgrade --from local --allow-older-base     # a branch older than the VM's image
```

## Going back

bootc keeps the image the VM ran before its last upgrade as the rollback
deployment. `tilo-vm vm rollback-image` makes it the default and reboots;
running it again returns to the newer one. `bootc status` in the VM
(`tilo-vm vm ssh -- bootc status`) shows both. An older image than that is
gone; rebuild it or upgrade to a published one:
`tilo-vm vm upgrade --image registry.tilo.so/tilo/tilo-vm:<tag>`.

## Storage

The host keeps one copy of the image per VM in
`~/.cache/tilo-vm/images/<vm>/`, and the VM one in `/var/lib/tilo-vm/image`;
both are what makes the next upgrade send only the difference. Deleting either
is safe: the next upgrade sends the whole image once.

## When it refuses

- **not named as a dev image** or **not a dev image**: a production image
  (`registry.tilo.so/tilo/tilo:*`, or `os-image-<arch>`'s layout) is sealed
  and has no sshd or device
  steps. Build the dev image FROM it: `--from local`.
- **is sealed**: the image carries a UKI (a board image); only
  `os/Containerfile.dev` output (which unseals its base) can go into a dev VM.
- **older base**: this worktree's commit predates the VM's base. Rebase,
  move onto a published image, or `--allow-older-base`.
- **t is missing**: `tilo-vm` builds images only on t; set it up
  (CONTRIBUTING.md) or move onto a published image.
- **busy**: a swap, gc or another upgrade holds the VM lock; wait and rerun.
