# Coming from the Fedora loop

`dist/dev/fedora44-vm-iterate.sh` builds debug binaries in a Fedora 44 distrobox
(`tilo-app-fedora44`, target dir `target/fedora44-dev`) and copies them over SSH
into a mutable Fedora 44 desktop VM, where Tilo runs as the "Tilo (my build)"
developer session (`tilo-session-devel`) of user `quickuser`. You log out and in
by hand in the VM's UI.

Slots test the Tilo OS image itself instead: the VM runs the dev image, every
change is a generation you can roll back, and the session restarts for you
through the production login path (PAM, then `tilo-session`).

| Fedora loop                                                           | tilo-vm                                                                                                                                                                     |
| --------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `bootstrap` (dependency image and distrobox)                          | `tilo-vm vm create <name>`, `tilo-vm vm start`: the dev image carries the toolchain and sccache                                                                             |
| `build shell` (debug, `--no-default-features --features dev-session`) | part of `tilo-vm swap shell`: built in the VM from your synced worktree, release with the manifest's features and `dev-overrides`; `--profile dev` for debug-speed compiles |
| `deploy shell` / `cycle shell` (shell and its launcher together)      | `tilo-vm swap shell`, and `tilo-vm swap launcher` when you changed the launcher too                                                                                         |
| `build host` / `deploy tilo-launcher tilo-unlock-helper`              | `tilo-vm swap launcher`, `tilo-vm swap unlock-helper`                                                                                                                       |
| `deploy PACKAGE`                                                      | `tilo-vm swap <component>` (`python3 os/scripts/install-components.py --list` names them)                                                                                   |
| `deploy-apps STAGE` (a staged app store and runtime)                  | one app from your worktree: `tilo-vm swap app:<dir>`; the runtime with every thin app: `tilo-vm swap apps+runtime` (or `--from ci:<tag>`)                                   |
| `handoff` (log out and in through the VM UI)                          | nothing: the swap logs the slot out and in; `tilo-vm seat claim` shows it on the VM's display                                                                               |
| `verify` (launch attestation: path and sha256 of the running shell)   | `tilo-vm status` (generation, layer hash per component, session state) and `tilo-vm logs`                                                                                   |
| `TILO_VM_HOST`, `TILO_VM_SSH_PORT`, `TILO_VM_USER`                    | `ssh = "user@host:port"` for the VM in `vms.toml`                                                                                                                           |
| undo: redeploy an older binary                                        | `tilo-vm rollback`                                                                                                                                                          |

Differences to expect:

- Builds are release builds with `dev-overrides`, not debug builds, unless you
  pass `--profile dev`; the shared sccache in the VM keeps them warm.
- Your changes live in your slot (user `tds-<slot>`), not in a shared login, so
  other agents on the same VM are unaffected.
- The session is started by the production path, not by `tilo-session-dev`, so
  there is no "Tilo (my build)" entry to pick.

The Fedora loop remains the way to test Tilo as a session on a stock Fedora 44
desktop (see docs/development/host-session.md), which slots do not cover: use it
for that, and slots for the Tilo OS image.
