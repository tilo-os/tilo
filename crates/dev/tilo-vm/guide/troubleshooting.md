# Troubleshooting

Start with `tilo-vm doctor`: it checks each prerequisite and prints the fix.

**Cannot reach the VM over SSH.** `tilo-vm vm status`, then `tilo-vm vm
start`. If it is running but silent, look at it: `tilo-vm vm shot`,
`tilo-vm vm serial`. As a last resort, `tilo-vm vm reset`.

**The VM refuses its root SSH key.** SSH answers but says `Permission
denied (publickey)`. On UTM, Hyper-V and qemu the key is installed once
after the first boot; on libvirt it goes in at creation, so a VM created
before that needs the same step. The error prints the exact command;
`tilo-vm guide platforms` explains it.

**Protocol mismatch.** The VM's image and your `tilo-vm` disagree. If the VM
is older, `tilo-vm vm upgrade`; if `tilo-vm` is older, rebuild it with
`cargo install --locked --path crates/dev/tilo-vm`.

**No tilo-vmd / no tilo-control.** The VM runs a production image. Move it to
a dev image: `tilo-vm vm upgrade --image code.podesta.ai/qos/tilo-vm:<tag>`.

**Session is failed or inactive.** `tilo-vm logs`, then
`tilo-vm slot restart`. A bad build usually shows in the first lines;
`tilo-vm rollback` gets you back to a working generation.

**An interrupted transition.** `tilo-vm status` reports it. Rerun the swap,
or `tilo-vm slot restart` to re-apply the recorded generation.

**Session differs from a greetd session.** A slot session must look like a
production login: no ambient or effective capabilities, and the same SELinux
user domain. `tilo-vm doctor` shows the difference. The session unit drops
the ambient capability pam_systemd grants; if the check still fails, the dev
image's unit template or the expected domain (`tilo-vmd rpc --expect-domain`)
is out of date: `tilo-vm vm upgrade`.

**Which credential does what.** There are two, and neither stands in for
the other, although both use a forge token with package read access as the
secret:

| Operation | Needs | Where |
|---|---|---|
| `vm create` without `--image` (disk download), doctor's published-disk check | a forge token: `FORGEJO_TOKEN`, or an fgj or tea login for code.podesta.ai | this host |
| `swap --from ci:`/`sha:` when the VM has podman, `vm upgrade` | a podman registry login | in the VM: `tilo-vm vm ssh --raw -- podman login --authfile /etc/ostree/auth.json code.podesta.ai` |
| `swap --from ci:`/`sha:` when the VM has no podman | a podman registry login | this host: `podman login code.podesta.ai` |

`tilo-vm doctor` shows which of the host's two it found.

**401 or reqPackageAccess from code.podesta.ai on a pull.** A podman
registry login is missing where the pull ran; the error names the place and
the exact `podman login` line. `FORGEJO_TOKEN` does not help here.

**`vm create` stops before downloading.** It checks the hypervisor, an
`--image`, the published disk, the VM disk path and free space first, and
names the missing piece with its fix. A 401/403 on the disk means the forge
token is missing or lacks package read access. "No dev disk is published"
means CI has not published one (or that tag): ask a human to dispatch the
"Release / Dev VM disk" workflow (`.forgejo/workflows/dev-disk.yml`), or
dispatch it with fgj if you may, or ask a human for a shared VM
(`tilo-vm vm use <name>`); `tilo-vm guide platforms` has the details. An
interrupted download resumes when you rerun the command.

**No hypervisor.** `tilo-vm doctor` names what is missing (virsh,
virt-install, `qemu:///session`, `/dev/kvm`; UTM's `utmctl`; Hyper-V) and the
install line for your host.

**The VM cannot build.** Its image lacks cargo, rsync or python3 (`tilo-vm
status` shows where builds run). `tilo-vm vm upgrade` to a current dev image;
on a Linux host `--builder host` works meanwhile. A sync that fails with
exit 1 from flock means another build of the same worktree is running.

**Busy.** Another command holds the VM or your slot. VM-wide swaps, `gc`,
upgrades and `vm stop` take the whole VM; wait and retry.

**Receipts.** Every command that changes something or runs long leaves
`target/dev-runs/<id>/run.json` (status, doctor, guide, lists and logs do
not) with phase timings (sync, build, assemble, transfer, import, apply, session-ready,
download) and flags phases slower than the goals: warm build < 60s, swap
plus restart < 10s, cached boot < 30s.
