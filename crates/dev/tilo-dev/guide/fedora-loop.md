# Coming from the Fedora loop

`dist/dev/fedora44-vm-iterate.sh` copies a debug binary into a mutable
Fedora 44 VM and asks you to log out by hand. Slots replace it: every change
is a generation you can roll back, the session restarts for you through the
production login path, and the VM runs the Tilo image itself.

| Fedora loop | tilo-dev |
|---|---|
| `bootstrap` (distrobox with the toolchain) | `tilo-dev vm create <name>`, `tilo-dev vm start`: the dev image carries the toolchain and sccache |
| `build shell` | part of `tilo-dev swap shell`: built in the VM from your synced worktree |
| `deploy shell` / `cycle shell` | `tilo-dev swap shell` |
| `deploy PACKAGE` | `tilo-dev swap <component>` (`install-components.py --list` names them) |
| `deploy-apps STAGE` | `tilo-dev swap app:<dir>` (one app, from your worktree); the runtime with every thin app: `tilo-dev swap apps+runtime --from ci:<tag>` |
| `handoff` (log out in the VM UI) | nothing: the swap logs the slot out and in; `tilo-dev seat claim` shows it on the VM's display |
| `verify` | `tilo-dev status` (generation, layer hash, session state, greetd parity) and `tilo-dev logs` |
| `TILO_VM_HOST`, `TILO_VM_SSH_PORT`, `TILO_VM_USER` | `ssh = "user@host:port"` for the VM in `vms.toml` |
| undo: redeploy an older binary | `tilo-dev rollback` |

Differences to expect:

- Builds are release builds with the manifest's features plus
  `dev-overrides`, not debug builds; the shared sccache keeps them warm.
- Your changes live in your slot (user `tds-<slot>`), not in a shared login,
  so other agents on the same VM are unaffected.
- The Fedora loop's files stay; use them only for what slots cannot do yet.
