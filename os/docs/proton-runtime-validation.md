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
For `aarch64-linux-gnu`, this selects `pressure-vessel-arm64`. The Tilo
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
existing Wine smoke harness. Tilo must either supply the required adapter or
manage the server outside that optional manifest field with equivalent
readiness and teardown guarantees.

## Native ARM Proton console acceptance

On 2026-09-16, `os/tests/fex-proton-smoke.py` passed with the pinned Proton
and sniper inputs, unmodified FEX commit
`91bbce5a96b9e1eaa32f95f440ea8b46bbfbdf20`, and these local images:

- Native host: `localhost/tilo-fex-graphics-host:arm64-dev`, image ID
  `5819150611346d43c72daa5a733e207b093ef89645d727d545c4f289ce0ff8a7`.
- Guest graphics filesystem exported from
  `localhost/tilo-fex-graphics-guest:fedora44-dev`, image ID
  `a2e62822ac3d9498f75d38c1005fa09c089460cb1f3de9c4fee2364e8a75db14`.

The test runs native ARM pressure-vessel with an emulator manifest pointing
directly to the FEX ELF executable. It imports native dependencies plus both
x86 graphics architectures, sets `PRESSURE_VESSEL_ARCHITECTURES` explicitly,
and switches `FEX_ROOTFS` to empty inside the assembled runtime. A managed
FEXServer must report readiness and remain alive through the test.

Proton creates a fresh prefix and runs Windows `cmd.exe`, which must write
`TILO_PROTON_CONSOLE_PASS` into `C:\tilo-console.txt`. The harness reads that file
from the prefix and requires the exact marker, exit 0, no timeout, and a live
server. The checked-in harness met all four conditions. Its result and logs
are retained in the preparation directory's `fex-proton-smoke-results`.

The container used `--network=none --init --cap-drop=ALL --userns=keep-id`,
the normal Podman seccomp policy, and private executable `/tmp` and `/dev/shm`.
Runtime, Proton, and guest graphics content were bound read-only. No host
binfmt registration was added or changed.

### Runtime lock and writable state

This pressure-vessel release opens the source platform's `files/.ref` with
`O_RDWR | O_CREAT`, including when `--copy-runtime` is selected. Merely
pre-creating that file in a read-only runtime does not work. The passing
fixture binds a separate private writable file over `files/.ref`, while
keeping all runtime content read-only. It uses `--copy-runtime` with a private
`--variable-dir` for runtime assembly. This avoids making shared runtime
content writable by the application. Cross-filesystem hard-link attempts
fall back to copying in this fixture.

To run the harness in an equivalent disposable container, mount the verified
runtime at `/runtime`, Proton at `/proton`, guest rootfs at `/rootfs`, the
test at `/test.py`, and an empty writable results directory at `/results`.
Bind the private lock file read-write over
`/runtime/sniper_platform_3.0.20260805.254768/files/.ref`, then run:

```sh
python3 /test.py --runtime /runtime --rootfs /rootfs \
  --proton /proton --output-dir /results
```

The direct mode validates the Proton console runner inside sniper on
ARM. It supplies UMU's non-Steam identity (`UMU_ID=umu-0`) directly. The
separate offline mode below also exercises the UMU entry point. Neither mode
validates game graphics, physical GPU drivers, or the complete Tilo capsule
policy. The known FEX graphics failures remain unresolved.

## Offline UMU entry point acceptance

The checked-in harness also passed on 2026-09-16 with the same isolated
container and `--umu /umu/umu_run.py`, where `/umu` is the read-only extracted
UMU 1.4.0 zipapp artifact. Results are retained in
`fex-umu-offline-smoke-results`. This mode executes guest Python through FEX;
the unmodified runtime entry point then selects native ARM pressure-vessel
using the emulator and graphics manifests. No replacement verifier or global
binfmt handler is required.

Before invoking UMU, the harness prepares a fresh per-user `umu/steamrt3`
directory with copied top-level metadata/scripts, symlinks to the read-only
runtime directories, and the upstream `umu -> _v2-entry-point` link. It does
not pre-create an installation-success marker. UMU itself detects the offline
network, runs the bundled x86 verifier through FEX, verifies the platform,
writes its marker, and skips updates because `UMU_RUNTIME_UPDATE=0`.
The harness requires all three corresponding log messages, the Windows
marker file, exit 0, no timeout, and a live FEX server. All passed.

The offline fixture has no D-Bus session. The Steam launcher service reports
that it cannot create its IPC server and uses its upstream fallback to run
the command. Session IPC acceptance remains a separate requirement. The
fixture also reports a missing valid machine ID; this did not prevent the
console command, but does not establish behavior for applications requiring
that identity.

## Remaining integration requirements

- The OS recipe now installs this runtime under
  `/usr/lib/tilo/windows-runners/steam-runtime/SteamLinuxRuntime_sniper` and
  runs the host-native `pv-verify` during artifact preparation. The ARM
  extraction and verification commands passed in the local build container;
  this is not a completed final OS image build.
- UMU's `UMU_RUNTIME_UPDATE=0` prevents updates to an installed runtime; it
  does not suppress initial installation. Provisioning must satisfy UMU's
  integrity checks and installation marker contract without granting an
  offline capsule network access. The smoke fixture demonstrates this.
- Runtime content should remain immutable, with any required installation
  markers and pressure-vessel variable state confined to capsule-owned
  writable storage.
- Validate the complete nested runtime under Tilo Landlock, syscall policy,
  network grants, and graphics selection before changing production launch
  plans. The offline console test does not establish those integration checks.
  On x86 the check failed: pressure-vessel starts Bubblewrap of its own, whose
  new mounts a capsule's Landlock refuses, so Proton never started in a
  capsule. x86 capsules now start Proton's own script directly on the
  system's libraries (`docs/windows-compatibility.md`, "Opening a program with
  Proton"). The same refusal withdraws the ARM route rather than awaiting this
  check: Proton's x86 program files reach the translator only through this
  runtime's container, so an ARM computer refuses a Proton route when it is
  planned and keeps Wine. The runtime and its manifests still ship as payload
  and are exercised by the disposable-container checks above.

## Withdrawn capsule mount

An earlier ARM plan mounted the pinned runtime's top-level files and
directories read-only under `/tmp/tilo-runtime/umu/steamrt3`, created the
upstream `umu` entry-point symlink, fixed `UMU_FOLDERS_PATH`, and handed the
executor a private memfd over the platform's `files/.ref` lock. Nothing of
that reaches a capsule plan now: the nested container those mounts prepared
for is what a capsule refuses, so an ARM computer plans no Proton route and
the executor carries no runtime lock descriptor. The runtime and its manifests
stay in the extension as payload for the disposable-container checks above.
