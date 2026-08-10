# perf scripts

Small, composable measurement + optimisation runners. Each script is
executable stand-alone; the higher-level flow is described below.

## Measure

- `binary-sizes.sh [target-dir]` — walk `target/release/` (or the
  given dir) for shell + terminal + file-explorer + qos-relay + fs +
  qos-install, emit a `{name: bytes}` JSON blob on stdout.

- `qemu-boot-measure.sh <qcow2> <label>` — headless QEMU boot of a
  qcow2 through the my-distrobox-44 toolbox's qemu-system-x86_64
  + OVMF. Snapshots the base so consecutive runs are hermetic; polls
  the serial console for the multi-user.target reach; extracts the
  kernel timestamp on the reach line into a JSON `{state, boot_ms}`
  blob. Powers the guest down cleanly.

- `qemu-install-boot-verify.sh [--disk PATH]` — assert the produced
  qcow2's ESP carries a *complete* bootloader (GRUB's shim +
  grubx64.efi + grub.cfg, or systemd-boot's loader + loader.conf) and
  that the disk boots to `multi-user.target`. An ESP with neither is
  what an install that silently did not plant a loader leaves behind.
  Reads the ESP with `qemu-img
  convert` + `sfdisk` + mtools (no NBD, no host mount) so it runs
  rootless everywhere; then boots a throwaway backing-file snapshot
  under OVMF and emits `boot_ms` in the same units as
  `qemu-boot-measure.sh`. Emits JSON on stdout for CI.

- `compare.sh <base.json> <cur.json>` — diff two runs (either output
  format) and print a percentage-change table.

## Optimise (three-step PGO + optional BOLT)

1. `pgo-instrument.sh` — build shell + terminal + file-explorer +
   qos-install with `-C profile-generate` into `target/pgo-instrumented/`.
   Run the resulting binaries; they drop `.profraw` files into
   `target/pgo-profraw/`.
2. `pgo-merge.sh` — merge every `target/pgo-profraw/*.profraw` into
   `os/pgo/qos.profdata`. **Commit that file** — pinning the profile
   bytes in-tree is what makes the second-phase build reproducible
   under the frozen RPM repo.
3. `pgo-rebuild.sh` — read `os/pgo/qos.profdata` and rebuild with
   `-C profile-use`. Output lands at `target/pgo-release/`.
4. (optional) `bolt-optimize.sh` — post-link `llvm-bolt` on the PGO'd
   binaries using the same profile signal. Output at `target/bolted/`.

## What's blocked

- Rebuilding the bootc qcow2 to measure the boot-time delta against
  the baseline requires pulling the digest-pinned frozen RPM repo,
  which lives on a private registry (`code.podesta.ai/qos/rpms`). To
  run locally: `podman login code.podesta.ai` first. In CI the
  `.forgejo/workflows/build-image.yml` job already has the token; the
  rebuilt image + qcow2 flow through the release channel.

## Baseline captured (pre-service-prune host qcow2, 2 vCPU / 2 GB / KVM)

- **Boot to multi-user.target**: 14,531 ms
- **State reached**: reached-target (systemd initialisation completed)
- **Note**: this baseline was captured against the qcow2 built before
  the qos-relay SELinux fix (DynamicUser=yes → policy denial on the
  StateDirectory rename); the audit log shows the crash loop but the
  target reach still succeeds because qos-relay is on `WantedBy=` not
  the critical chain.
