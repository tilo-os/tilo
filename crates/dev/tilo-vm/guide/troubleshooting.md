# Troubleshooting

Start with `tilo-vm doctor`: it checks each prerequisite and prints the fix.

**What `vm status` (and `status`) say.** Each case has its own fix:

| It says | Meaning | Next |
|---|---|---|
| shut off | the hypervisor reports the VM is not running | `tilo-vm vm start <name>` |
| running, but SSH does not answer | still booting, or stuck | `tilo-vm vm serial <name>`, `tilo-vm vm shot <name>`, `tilo-vm doctor`; last resort `tilo-vm vm reset` |
| SSH answers but refuses the root key | the key was never installed | the printed install command (`tilo-vm guide platforms`) |
| reachable over SSH, but no tilo-vmd | not a Tilo dev image | see "No tilo-vmd" below |
| its tilo-vmd speaks protocol N, this tilo-vm speaks M | an incompatible daemon | see "Protocol mismatch" below |

With `--json` the report carries `vm.state` (`ready`, `off`, `no-ssh`,
`key-refused`, `no-daemon`, `incompatible`) and, when it is not ready, the
same top-level `error`, `hint` and `see` as any failed command; the exit
status is then non-zero.

**The VM refuses its root SSH key.** SSH answers but says `Permission
denied (publickey)`. On UTM, Hyper-V and qemu the key is installed once
after the first boot; on libvirt it goes in at creation, so a VM created
before that needs the same step. The error prints the exact command;
`tilo-vm guide platforms` explains it.

**Protocol mismatch.** The VM's tilo-vmd and your `tilo-vm` disagree; both
protocol numbers are in the message. If `tilo-vm` is older, update your
checkout and rebuild it the way the docs build it: `cargo build -p tilo-vm`,
then run `target/debug/tilo-vm` (any cargo drop-in works).
If the VM is older, it depends on the VM: one running a bootc dev image moves
onto a current one with `tilo-vm vm upgrade`; one that is not (no `bootc`)
cannot be upgraded in place, so create a VM from a dev image
(`tilo-vm vm create <name>`) or ask whoever set it up. The hint checks which
it is.

**No tilo-vmd / no tilo-control.** The VM runs a production image (or any
non-Tilo image). SSH still works: `vm start` returns as soon as SSH answers,
`vm status` says "reachable over SSH, but no tilo-vmd", and `vm ssh` works.
If it has bootc, move it to a dev image:
`tilo-vm vm upgrade --image registry.tilo.so/tilo/tilo-vm:<tag>`, or one built
here: `tilo-vm vm upgrade --from local` (`tilo-vm guide images`).

**"Not running a bootc dev image".** `vm upgrade` and `vm rollback-image`
check for bootc before anything else. Without it they cannot move the VM:
create a VM from a dev image (`tilo-vm vm create <name>`, the published dev
disk), or ask whoever set this VM up.

**`vm ssh` refuses a command.** Reading is always fine (`systemctl status`,
`journalctl`, `cat`, `ls`, `command -v`, pipelines of those). `vm ssh`
refuses only writes behind tilo-vm's back: copies, moves, `tee`, `dd` or
redirections into `/usr`, `/etc`, `/boot` or tilo-vm's state
(`/var/lib/tilo-vm`, `/var/lib/extensions`), package installs,
`systemctl enable|start|restart|stop` of tilo units, `systemd-sysext`
merges, `bootc switch|usr-overlay` and `tilo-vmd` subcommands. Change
components with `tilo-vm swap`, the image with `tilo-vm vm upgrade`.
`--raw` is the documented escape hatch for the rare change by hand; it
leaves a receipt.

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

**Which credential does what.** Only dev disk downloads need one:

| Operation | Needs | Where |
|---|---|---|
| `vm create` without `--image` (disk download), doctor's published-disk check | a forge token: `FORGEJO_TOKEN`, or an fgj or tea login for code.tilo.so | this host |
| `swap --from ci:`/`sha:`, `vm upgrade` | nothing: images on registry.tilo.so are public | in the VM, or this host when the VM has no podman |

`tilo-vm doctor` shows whether the host has a forge token.

**401 on a pull.** Tilo images on registry.tilo.so need no login, so the
registry refusing a pull means the reference is not a published Tilo image:
check its name and tag.

**`vm create` stops before downloading.** It checks the hypervisor, an
`--image`, the published disk, the VM disk path and free space first, and
names the missing piece with its fix. A 401/403 on the disk means the forge
token is missing or lacks package read access. "No dev disk is published"
means CI has not published one (or that tag). "Has no qcow2 disk for
<arch>" means the tag exists but your architecture's lane has not published
yet, or failed: each architecture publishes on its own. Wait for the main
"Release / OS" run, ask a human to dispatch the "Release / Dev VM disk"
workflow (`.forgejo/workflows/dev-disk.yml`) for an existing image, or
dispatch it with fgj if you may, or ask a human for a shared VM
(`tilo-vm vm use <name>`); `tilo-vm guide platforms` has the details. An
interrupted download resumes when you rerun the command.

**No hypervisor.** `tilo-vm doctor` names what is missing (virsh, qemu-img,
passt, `/dev/kvm`, a `qemu:///session` whose `virsh capabilities` lists a KVM
guest; UTM's `utmctl`; Hyper-V) and the install line for your host.
virt-install is not used. If `virsh --connect qemu:///session capabilities`
fails, doctor quotes virsh's error: it means libvirt's QEMU driver
(`virtqemud`, or `libvirtd`) does not start for your user. Libvirt's storage
driver is never needed.

**The VM cannot build.** Its image lacks cargo, rsync or python3 (`tilo-vm
status` shows where builds run). `tilo-vm vm upgrade` to a current dev image;
on a Linux host `--builder host` works meanwhile. A sync that fails with
exit 1 from flock means another build of the same worktree is running.

**Busy.** Another command holds the VM or your slot. VM-wide swaps, `gc`,
upgrades and `vm stop` take the whole VM; wait and retry.

**A VM that needs a fresh boot.** `tilo-vm vm restart` shuts it down under
the VM lock, waits until it is off, starts it and waits for tilo-vmd.

**Receipts.** Every command that changes something or runs long leaves
`target/dev-runs/<id>/run.json` (status, doctor, guide, lists, logs,
`slot show`, `slot exec` and `vm ssh` do not; `vm ssh --raw` does, since it
may change the VM behind tilo-vm) with phase timings (sync, build, assemble, transfer, import, apply, session-ready,
download) and flags phases slower than the goals: warm build < 60s, swap
plus restart < 10s, cached boot < 30s.
