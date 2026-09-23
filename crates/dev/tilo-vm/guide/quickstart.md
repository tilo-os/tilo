# Quickstart

`tilo-vm` puts your build of a Tilo component into a shared dev VM without a
new VM or a reboot. You get your own **slot** in the VM: a Linux user, a
session, and a stack of layers over the VM's `/usr`.

## Get a VM

Agents usually share one a human already runs. `tilo-vm status` says which
VM you use and why; with none selected, ask a human which one to use (they
set it with `tilo-vm vm use <name> --global` or in `vms.toml`). Or make your own:

```sh
export FORGEJO_TOKEN=...      # package read access; an fgj or tea login works too
tilo-vm vm create dev        # downloads the published dev disk, creates, registers
tilo-vm vm start
```

## Use it

```sh
tilo-vm                      # where am I: VM, slot, generation, session, next steps
tilo-vm doctor               # check ssh, rsync, the VM, its daemon; prints fixes
tilo-vm slot create --headless
tilo-vm swap shell           # sync this worktree, build in the VM, restart your session
tilo-vm shot -o target/shot.png
tilo-vm control actions list
tilo-vm rollback             # back to the previous generation
```

- The VM comes from `TILO_VM`, then `.tilo-vm` in the worktree, then the
  default in `vms.toml`.
- Your slot is named after your branch. Pass a name to any slot command to
  use another one.
- Every command takes `--json` and writes a receipt under `target/dev-runs/`.
- Commands are idempotent: rerunning one that succeeded changes nothing.

Never deploy with raw `ssh`/`scp` into the VM. It bypasses generations, so
nothing can roll it back, and other agents share that VM.

Next: `tilo-vm guide concepts`, `tilo-vm guide swapping`,
`tilo-vm guide fedora-loop` if you used `fedora44-vm-iterate.sh`.
