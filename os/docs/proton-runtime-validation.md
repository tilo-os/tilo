# Pinned Proton runtime preparation

The default `UMU-Proton-10.0-4` tool manifest requires Steam tool app ID
`1628350`: the x86 Steam Linux Runtime 3.0 (sniper). UMU 1.4.0 supports
native AArch64 launchers, but that does not change the architecture of this
Proton release or its required runtime.

`os/external-assets/github-assets.tsv` pins the runtime archive at the immutable
build URL for `3.0.20260805.254768`. Its SHA-256 is
`e264f0639ab775338311036f207b35cebe99bc417b016b53931ebca8b30b3d94`
and its size is 193,105,964 bytes. The checksum matches the publisher's
`SHA256SUMS` in that same versioned directory. Do not replace the URL with
`latest-public-beta` in the build manifest.

## Local ARM verification

On 2026-09-16, preparation downloaded and verified the pinned UMU, Proton,
and sniper archives, then extracted them using Python's `tarfile` data filter.
The runtime's `VERSIONS.txt` identifies depot/sniper
`3.0.20260805.254768` and both pressure-vessel builds `0.20260805.0`.
From the extracted `SteamLinuxRuntime_sniper` directory, this command passed:

```sh
pressure-vessel-arm64/bin/pv-verify .
```

The verifier checked both the depot's `mtree.txt.gz` and the platform's
`usr-mtree.txt.gz`. The ARM verifier and `pressure-vessel-wrap --help` executed
natively. This verifies archive contents and native utility startup, not a
successful Proton launch.

The bundled `_v2-entry-point` has an explicit emulation path: with a machine
architecture plus valid emulator and graphics-provider manifest paths, it
selects native pressure-vessel and its architecture-specific execution helper.
For `aarch64-linux-gnu`, this selects `pressure-vessel-arm64`. The qOS
integration must implement and test that contract, including x86 child
processes, graphics providers, and the emulator server.

### Version-matched emulation contract

The same official release directory publishes
`sources/steam-runtime-tools_0.20260805.0.tar.xz`, SHA-256
`03014797d44f540b393c7b0f5b123510e00346bcdd00a6e145ccc69c51377e31`.
Preparation verified and extracted this source archive to inspect
`docs/steam-runtime-emulator.json.5.md` and the corresponding implementation.
This provides the version-matched contract even when the upstream GitLab
documentation cannot be accessed.

The `emulator_v0` manifest explicitly supports non-transparent emulation
without `binfmt_misc`. It declares emulated and required native architectures,
native shared-library dependencies, and separate arguments/environment for
host helpers, container helpers, and the main command. Its emulator executable
must be an ELF binary, not a shell-script wrapper. Container configuration
must switch FEX from the host guest rootfs to the runtime's filesystem.

The optional server has a defined lifecycle protocol: write `READY=1` and
close stdout after initialization, then exit when stdin reaches EOF. This
must not be confused with the separate FEXServer readiness pipe used by the
existing Wine smoke harness. qOS must either supply the required adapter or
manage the server outside that optional manifest field with equivalent
readiness and teardown guarantees.

## Remaining integration requirements

- The OS image currently installs UMU and Proton but does not yet provision
  this Steam runtime into capsules. Adding its checksum pin alone does not
  make offline Proton launches work.
- UMU's `UMU_RUNTIME_UPDATE=0` prevents updates to an installed runtime; it
  does not suppress initial installation. Provisioning must satisfy UMU's
  integrity checks and installation marker contract without granting an
  offline capsule network access.
- Runtime content should remain immutable, with any required installation
  markers and pressure-vessel variable state confined to capsule-owned
  writable storage.
- Validate the complete nested runtime under qOS Landlock, syscall policy,
  network grants, and graphics selection before changing production launch
  plans. Native utility startup is not acceptance for the Windows runtime.
