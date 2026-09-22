# Quickstart

`tilo-dev` puts your build of a Tilo component into a shared dev VM without a
new VM or a reboot. You get your own **slot** in the VM: a Linux user, a
session, and a stack of layers over the VM's `/usr`.

```sh
tilo-dev                      # where am I: VM, slot, generation, session, next steps
tilo-dev doctor               # check ssh, hypervisor tools, VM, daemon; prints fixes
tilo-dev slot create --headless
tilo-dev swap shell           # build shell from this worktree, restart your session
tilo-dev shot -o target/shot.png
tilo-dev control actions list
tilo-dev rollback             # back to the previous generation
```

- The VM comes from `TILO_DEV_VM`, then `.tilo-dev` in the worktree, then the
  default in `vms.toml`. `tilo-dev status` says which one and why.
- Your slot is named after your branch. Pass a name to any slot command to
  use another one.
- Every command takes `--json` and writes a receipt under `target/dev-runs/`.
- Commands are idempotent: rerunning one that succeeded changes nothing.

Never deploy with raw `ssh`/`scp` into the VM. It bypasses generations, so
nothing can roll it back, and other agents share that VM.

Next: `tilo-dev guide concepts`, `tilo-dev guide swapping`.
