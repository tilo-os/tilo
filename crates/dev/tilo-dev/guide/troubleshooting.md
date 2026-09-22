# Troubleshooting

Start with `tilo-dev doctor`: it checks each prerequisite and prints the fix.

**Cannot reach the VM over SSH.** `tilo-dev vm status`, then `tilo-dev vm
start`. If it is running but silent, look at it: `tilo-dev vm shot`,
`tilo-dev vm serial`. As a last resort, `tilo-dev vm reset`.

**Protocol mismatch.** The VM's image and your `tilo-dev` disagree. If the VM
is older, `tilo-dev vm upgrade`; if `tilo-dev` is older, rebuild it with
`cargo install --locked --path crates/dev/tilo-dev`.

**No tilo-devd / no tilo-control.** The VM runs a production image. Move it to
a dev image: `tilo-dev vm upgrade --image code.sandbox.example/qos/tilo-dev:<tag>`.

**Session is failed or inactive.** `tilo-dev logs`, then
`tilo-dev slot restart`. A bad build usually shows in the first lines;
`tilo-dev rollback` gets you back to a working generation.

**An interrupted transition.** `tilo-dev status` reports it. Rerun the swap,
or `tilo-dev slot restart` to re-apply the recorded generation.

**Session differs from a greetd session.** A slot session must look like a
production login: no ambient or effective capabilities, and the same SELinux
user domain. `tilo-dev doctor` shows the difference. The session unit drops
the ambient capability pam_systemd grants; if the check still fails, the dev
image's unit template or the expected domain (`tilo-devd rpc --expect-domain`)
is out of date: `tilo-dev vm upgrade`.

**401 or reqPackageAccess from code.sandbox.example.** Log in with a forge token that
has package read access: `podman login code.sandbox.example` on the host, and for
`vm upgrade` inside the VM too (the error names which one).

**Busy.** Another command holds the VM or your slot. VM-wide swaps, `gc` and
upgrades take the whole VM; wait and retry.

**Receipts.** Every run leaves `target/dev-runs/<id>/run.json` with phase
timings (build, assemble, transfer, import, apply, session-ready) and flags
phases slower than the goals: warm build < 60s, swap plus restart < 10s,
cached boot < 30s.
