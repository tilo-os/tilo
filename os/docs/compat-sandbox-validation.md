# Compatibility sandbox validation

## Private temporary storage

Wine and Proton capsule plans create a private Bubblewrap tmpfs at `/tmp`.
Their Landlock rules must also allow that mount to be written. Previously,
the mount existed but writes were denied, preventing runtimes from creating
temporary files and local socket state. The planner now includes the private
mount in its writable rules; it does not bind the host's temporary directory.

The regression test parses the generated launcher-shim arguments for
both capsule layers. It requires writable `/tmp`, a private tmpfs rather than
a host bind, active Landlock, and the existing network restriction. It failed
before the fix; all 48 compatibility-service tests passed afterward on ARM.
The native shell integration check also passed with locked dependencies.

## Kernel-level enforcement

Run the reusable probe on the native build architecture:

```sh
cargo build -p launcher --bin tilo-launcher --locked
bash os/tests/kernel-landlock-private-tmp.sh \
  /path/to/kernel target/debug/tilo-launcher /path/to/new-results
```

The script builds a small initramfs containing the shipped launcher and a file
access probe. QEMU uses TCG, no network, and no attached disk. The init process
mounts a private tmpfs and runs the launcher with and without the writable rule.
It requires `/tmp` writes to succeed only with the rule, and unrelated writes
to remain denied in both cases. Its init mode refuses to run outside PID 1.

The checked-in probe passed on the built Tilo AArch64 7.2.5 kernel. Landlock
reported ABI 10 and both restrictions were fully enforced. The original
policy returned `EACCES` for temporary and unrelated files; the corrected rule
allowed the temporary file while retaining `EACCES` outside it.

Validated SHA-256 values:

- Kernel: `f88ab7ad6eae32417a437ff119c4ed8e8f8ddd5edaca920d82678fab1298ab7a`.
- ARM launcher (Rust 1.97.1): `5426235f69f27b802ce15083ed69a4225efd1e74565a2e602c288dbbf34f929e`.

The initial local build used Fedora's Rust 1.94.1 because its `cargo` executable
bypassed rustup. The preparation environment now places rustup's shims first
on `PATH`; `rustup show active-toolchain` confirms the repository's
`rust-toolchain.toml` selects 1.97.1. All 48 service tests, the shell check, and
the kernel probe were repeated successfully with that toolchain. The two
launcher builds are different artifacts; only the pinned build hash is listed
above.

This validates Landlock enforcement for the changed permission. It does not
replace complete Wine/FEX launches inside the final capsule sandbox, seccomp
acceptance, or final installer-image testing. WSL's ABI 3 kernel cannot validate
the Tilo ABI 5 requirement; that is why this check uses the built Tilo kernel.

## FEX syscall-policy comparison

The launcher library generated both `tilo-default` and `tilo-jit` BPF
programs for AArch64. A native helper installed each filter with
`PR_SET_NO_NEW_PRIVS` and `PR_SET_SECCOMP` before executing the test command.
Native `/usr/bin/true` passed under both filters. FEX executing the guest
`/usr/bin/true` exited 139 under `tilo-default` and 0 under `tilo-jit`.
`fex-tilo-seccomp-validation.log` retains the comparison. These profiles differ
in their simultaneous writable/executable-memory restrictions; the other
syscall restrictions remain active.

Both Wine architectures subsequently passed the tracked console smoke test
with `tilo-jit` installed before FEX and before the managed FEXServer. This
covered Wine version queries, creation of independent prefixes, `cmd.exe`
output, and wineserver cleanup/wait. The result is retained in
`fex-wine-tilo-jit-results/result.json`. Container capabilities remained dropped,
external networking disabled, and the normal Podman filter enabled as well.

This establishes a syscall-policy requirement for the translated runtime.
Trusted ARM runtime selection now chooses the JIT profile for translated
Wine, as described below. Catalogue-supplied settings cannot select the host
architecture, interpreter, or syscall profile. Complete capsule integration
is still required.

## Capsule descriptor handoff

Native ARM tests on 2026-09-16 found that an unprivileged Bubblewrap launch
could not reopen `/proc/<broker-pid>/fd/<n>` directory sources: it returned
`Permission denied`. Direct inherited `--bind-fd` bindings succeeded and
retained the original directories after their pathnames were replaced.
The executor now passes the anchored prefix and home descriptors directly.
It duplicates sources above all reserved child slots before `pre_exec`,
preventing descriptor remapping from overwriting another pending source.

The executor test also exposed an existing seccomp handoff bug. The
filter memfd was positioned at EOF after writing; Bubblewrap read an empty
program and `PR_SET_SECCOMP` returned `EINVAL`. Rewinding before inheritance
resolved this failure without changing the syscall policy.

Run the integration regression explicitly on Linux with Bubblewrap and
unprivileged user namespaces available:

```sh
cargo test -p compat-service --locked \
  executor_retains_directories_after_path_replacement -- --ignored --nocapture
```

This passed on native ARM with Rust 1.97.1, both without a filter and with
the `tilo-default` filter loaded by Bubblewrap. The normal service suite
also passed all 48 tests; the environment-dependent regression is excluded
from the default suite and was run separately. The shell integration check
passed. These tests validate descriptor transfer, mounts, and filter
loading; they do not replace the full capsule/Landlock runtime acceptance.

## Native translated-runtime supervisor

`tilo-windows-runtime` is a native helper intended to run after entering the
capsule. Its allow-listed modes are `wine32`, `wine64`, and `proton`; they
select fixed guest entry points rather than a catalogue-supplied interpreter.
The guest root is fixed at `/opt/tilo/x86`. The helper does not create or relax
the sandbox and must not replace the outer namespace or Landlock setup.

Each invocation starts its own foreground FEXServer, waits up to 20 seconds
for the upstream readiness pipe to close, and verifies the server is still
alive before running guest code. It monitors unexpected server exit while
the guest runs. Wine modes additionally wait for the corresponding guest
wineserver to finish before terminating FEXServer. Guest exit codes and
signal status are propagated. Cleanup sends SIGTERM to the owned process,
waits for a bounded grace period, then kills and reaps it if necessary.

Six native unit tests cover readiness EOF, timeout, unexpected protocol data,
early server exit, mode validation, and exit/signal status translation.
The service suite passes 55 tests, with the environment-dependent Bubblewrap
regression still run separately. Strict Clippy also passes with Rust 1.97.1.

`os/tests/managed-fex-wine-smoke.py` exercises the helper with fresh
32-bit and 64-bit Wine prefixes. For each architecture it requires an exact
console marker and propagation of guest exit code 23 on a second invocation.
Unlike the earlier Wine fixture, it starts no external FEXServer: the native
Tilo helper must own readiness, monitoring, Wine-server wait, and teardown.
All four checks passed on native ARM on 2026-09-16; the result and logs are
retained under `managed-fex-runtime-tracked-results` in the preparation tree.
Run it in the same offline, capability-free container described by the Wine
fixture, with the guest filesystem at `/opt/tilo/x86`, the helper at `/helper`,
and empty writable output directory `/results`:

```sh
python3 /test.py --helper /helper --output-dir /results
```

The OS recipe builds and installs the helper at
`/usr/libexec/tilo-windows-runtime`.

## Trusted host architecture selection

The runtime registry selects native Wine on x86-64 and the managed FEX helper
on AArch64. ARM Wine uses the JIT syscall profile; native Wine retains the
default profile. Proton retains its existing nested-runtime policy. Unknown
host architectures fail closed. The Windows application's declared bitness
selects only the fixed Wine mode, not the host interpreter or sandbox policy.

ARM plans mount the guest filesystem read-only at `/opt/tilo/x86`, bind native
FEX executables, and expose Wine's guest resources at their expected paths.
Proton uses the shipped emulator and graphics-provider manifests to hand off
to native ARM pressure-vessel. The manifests preserve the host/guest library
separation and clear FEX_ROOTFS inside the nested runtime.

On 2026-09-16, all 58 service tests and strict Clippy passed with Rust 1.97.1.
The architecture matrix covers both Windows bitnesses, both compatibility
layers, and both supported host architectures, including retained network
restrictions. The managed Proton integration test additionally ran the real
Tilo supervisor and shipped manifests with networking disabled and all
capabilities dropped. UMU verified the pinned runtime offline, skipped updates,
and Proton wrote the expected Windows console marker into a fresh prefix.

Reproduce with `os/tests/managed-fex-proton-smoke.py` in the disposable container
layout documented in that test:

```sh
python3 /test.py --helper /helper --runtime /runtime --proton /proton \
  --output-dir /results
```

FEX/guest production packaging, complete native helper bindings, and the full
outer capsule/Landlock launch remain acceptance requirements. The standalone
container test does not establish those gates or graphical application support.

## Private capsule identity and generated namespace probes

The generated ARM Proton namespace exposed a failure hidden by the earlier
whole-container test: UMU calls `getpwuid()` while preparing the prefix, but
the capsule had no account database. UMU printed a traceback yet exited zero;
the new fixture correctly rejected the run because Windows wrote no marker.

The executor now creates private, read-only passwd and group files from its
effective UID/GID. Both use the fixed name `tilo` and the capsule's private home.
No host account database or shadow file is mounted. Execution rejects a planned
session UID that differs from the executor's UID. The two anonymous descriptors
occupy slots 36 and 37; sources are duplicated above all reserved slots before
remapping. The plan grants Landlock read access and fixes `USER` and `LOGNAME`.

On 2026-09-16, 60 service tests and strict Clippy passed. The real Bubblewrap
descriptor regression passed with both its tested filter configurations:
`getent` resolved the synthetic UID/GID, writes to either file failed, and no
shadow database was visible.

Three ignored `generated_arm_*_namespace_executes_windows_console` tests now
exercise the Wine32, Wine64, and Proton namespace plans. They require
the fixed production artifact paths, a matching PE `cmd.exe` supplied through
`TILO_ARM_CAPSULE_CMD_EXE`, writable `/run/user/<uid>`, Wine resource mountpoint
directories, and a dummy render node inside a disposable container. They create
a non-listening Unix socket only to satisfy the Wayland bind, so they do not
establish graphics acceptance. Run each test separately with a 240-second outer
timeout and container teardown.

These diagnostic fixtures remove only the executable Landlock shim because
the WSL kernel lacks the required ABI. Generated mounts, clean environment,
offline network namespace, seccomp and descriptor transfer remain intact.
Their results must never substitute for full Tilo-kernel capsule acceptance.
The initial Wine64 run timed out during prefix initialization. The managed
C-drive fix described below resolves that specific failure.

The generated Proton namespace passed on 2026-09-16 after adding three exact
native helper bindings: `readlink`, `getopt`, and `true`. The ARM handoff failed
without `readlink`; pressure-vessel's nested Bubblewrap probe failed without
`true`. `getopt` is required by the pinned entrypoint on native x86 hosts.
Each helper is read-only and automatically included in the plan's Landlock
read paths. No whole executable directory was exposed.

The successful test verified the immutable Steam runtime offline, initialized
a fresh Proton prefix, and required Windows to create the expected marker.
It retained the generated private runtime lock, identity files, descriptor
handoff and network namespace. Missing locale-generation inputs, session-bus
IPC fallback, and the diagnostic container's machine-id warning remain visible
in the log; this console result does not establish those acceptance gates.

## Managed C-drive initialization

The generated Wine namespace revealed an architecture-independent prefix bug:
Tilo precreated `dosdevices` for its managed `Q:` mapping, preventing Wine's
fresh-prefix branch from creating `C:`. Windows system-directory lookups then
failed despite `drive_c/windows/system32` existing. A controlled diagnostic
with only `C: -> ../drive_c` added passed; the unchanged case timed out.

Capsule preparation now creates the anchored `drive_c` directory and requires
that exact relative `C:` link. Missing links in older prefixes are repaired
without changing stored content. Replaced links, regular files or directories
at `C:` are rejected, and a symlinked `drive_c` directory is not followed.
The private `C:` and grant-only `Q:` mappings remain separate from the removed
default `Z:` mapping.

After removing the temporary diagnostic mapping hook and rebuilding, the
generated Wine32, Wine64 and Proton ARM namespaces all passed on 2026-09-16
(18.45, 27.01 and 29.29 seconds respectively). Each required a Windows
file marker; Wine retained the production JIT filter. The service suite passed
63 tests and strict Clippy. Logs are `capsule-wine32-c-drive-fixed.log`,
`capsule-wine64-c-drive-fixed.log`, and `capsule-proton-c-drive-fixed.log` in
the preparation tree. The test-only `TILO_ARM_CAPSULE_WINEDEBUG` override can
enable diagnostic channels without changing production defaults.

These remain namespace-only console checks. Full Landlock, graphics, audio,
IPC and final-image acceptance requirements above are unchanged.
