# Quickstart

`tilo-vm` puts your build of a Tilo component into a shared dev VM without a
new VM or a reboot. You get your own **slot** in the VM: a Linux user, a
session, and a stack of layers over the VM's `/usr`.

## Get a VM

Agents usually share one a human already runs. `tilo-vm status` says which
VM you use and why; with none selected, ask a human which one to use (they
set it with `tilo-vm vm use <name> --global` or in `vms.toml`). Or make your own:

```sh
tilo-vm doctor               # hypervisor, KVM, tokens, published disk: each with its fix
tilo-vm vm create dev        # checks everything first, then downloads the published disk
tilo-vm vm start
```

On Linux the VM lives in libvirt's rootless `qemu:///session`, so a human can
watch it in virt-manager (File > Add Connection > QEMU/KVM user session).
`vm create` needs a forge token (`FORGEJO_TOKEN`, or an fgj or tea login) to
download the disk; `tilo-vm guide platforms` covers the rest.

A board or another real machine that runs a Tilo dev image is registered
instead: `tilo-vm vm add <name> --ssh root@<address>` (`tilo-vm guide devices`).

## Use it

```sh
tilo-vm                      # where am I: VM, slot, generation, session, next steps
tilo-vm slot create --headless
tilo-vm swap shell           # sync this worktree, build in the VM, restart your session
tilo-vm shot -o target/shot.png
tilo-vm control actions list
tilo-vm slot exec -- sha256sum /usr/bin/shell   # what your slot sees (guide swapping)
tilo-vm rollback             # back to the previous generation
tilo-vm rollback --forward   # and forward again
```

## Headful: your shell on a real display path

```sh
tilo-vm slot create --headful       # own vkms display and seat: real DRM and libinput
tilo-vm swap shell                  # your build, session restarted
tilo-vm shot -o target/headful.png  # the slot compositor's own frame
tilo-vm input key super             # real keyboard events on the slot's seat

tilo-vm seat claim                  # move the session onto seat0, the hypervisor window
tilo-vm vm shot -o target/seat0.png # the hypervisor's picture of seat0
tilo-vm seat release                # give seat0 back to greetd
```

What each picture is:

- `tilo-vm shot` asks the slot's compositor to render its current scene into
  a PNG (`tilo-control snapshot`). It is the same for a headless slot, a
  headful slot on its vkms seat, and a slot holding seat0; it never reads a
  display.
- `tilo-vm vm shot` is the hypervisor's screenshot of the VM's real display,
  seat0: greetd's greeter, or the slot that claimed it. Headless and vkms
  slots never appear there. It works without SSH.

## Rules

- The VM comes from `TILO_VM`, then `.tilo-vm` in the worktree, then the
  default in `vms.toml`.
- Your slot is named after your branch, unless `TILO_VM_SLOT` or
  `slot = "<name>"` in the worktree's `.tilo-vm` names another.
- Every command that works on a slot accepts `--slot <name>`: `status`,
  `swap`, `input`, `control`, `rollback`, `logs`, `shot`,
  `seat claim|release`, `slot create|show|exec|restart|rm` and
  `slot apps install`. All but
  `status`, `swap`, `input` and `control` also take the name as a
  positional argument (`tilo-vm logs review-a`).
- After a command on a slot other than your default one, its `next:` hints
  carry the same `--slot`.
- Every command takes `--json`; a failure is one object,
  `{"error","hint","see"}`, at the top level (also inside `status` reports).
  Commands that change something or run long (create, start, restart, swap,
  rollback, seat, gc, `vm ssh --raw`, ...) write a receipt under
  `target/dev-runs/`; looking around (status, doctor, guide, lists, logs,
  `slot show`, `slot exec`, `vm ssh` without `--raw`) does not.
- Commands are idempotent: rerunning one that succeeded changes nothing.

Never deploy with raw `ssh`/`scp` into the VM. It bypasses generations, so
nothing can roll it back, and other agents share that VM. `tilo-vm vm ssh`
is for looking around; it refuses writes into `/usr`, `/etc`, `/boot` and
tilo-vm's state unless you pass `--raw`.

Changed a Containerfile, a unit, `/etc` or the package list instead of a
component? `tilo-vm vm upgrade --from local` rebuilds the image and moves the
running VM onto it; `tilo-vm image build` on another machine builds it there
instead, for `tilo-vm vm upgrade --image
target/tilo-vm/image` (`tilo-vm image disk` then writes a disk from it for `vm create --image`); once CI
has built the commit, `tilo-vm vm upgrade --from sha:<commit>` moves it onto
CI's build without building the OS here (`tilo-vm guide images`).

Next: `tilo-vm guide concepts`, `tilo-vm guide swapping`,
`tilo-vm guide driving`, `tilo-vm guide images`, `tilo-vm guide fedora-loop`
if you used `fedora44-vm-iterate.sh`.
