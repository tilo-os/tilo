# Troubleshooting

Start with `tilo-vm doctor`: it checks each prerequisite and prints the fix.

**Cannot reach the VM over SSH.** `tilo-vm vm status`, then `tilo-vm vm
start`. If it is running but silent, look at it: `tilo-vm vm shot`,
`tilo-vm vm serial`. As a last resort, `tilo-vm vm reset`.

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

**401 or reqPackageAccess from code.podesta.ai.** Log in with a forge token that
has package read access: `podman login code.podesta.ai` on the host; inside
the VM (for `vm upgrade` and CI layers pulled there)
`tilo-vm vm ssh --raw -- podman login --authfile /etc/ostree/auth.json code.podesta.ai`.
The error names which one.

**`vm create` cannot download.** It needs a forge token: `FORGEJO_TOKEN`, or an
fgj or tea login for code.podesta.ai (`tilo-vm doctor` says which it found).
"No dev disk published" means CI has not published that tag; pick another
with `--tag`, or build a disk with `os/dev/build-dev-image.sh` and pass
`--image`. An interrupted download resumes when you rerun the command.

**The VM cannot build.** Its image lacks cargo, rsync or python3 (`tilo-vm
status` shows where builds run). `tilo-vm vm upgrade` to a current dev image;
on a Linux host `--builder host` works meanwhile. A sync that fails with
exit 1 from flock means another build of the same worktree is running.

**Busy.** Another command holds the VM or your slot. VM-wide swaps, `gc`,
upgrades and `vm stop` take the whole VM; wait and retry.

**Receipts.** Every run leaves `target/dev-runs/<id>/run.json` with phase
timings (sync, build, assemble, transfer, import, apply, session-ready,
download) and flags phases slower than the goals: warm build < 60s, swap
plus restart < 10s, cached boot < 30s.
