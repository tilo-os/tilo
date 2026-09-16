# Compatibility sandbox validation

## Private temporary storage

Wine and Proton capsule plans create a private Bubblewrap tmpfs at `/tmp`.
Their Landlock rules must also allow that mount to be written. Previously,
the mount existed but writes were denied, preventing runtimes from creating
temporary files and local socket state. The planner now includes the private
mount in its writable rules; it does not bind the host's temporary directory.

The regression test parses the actual generated launcher-shim arguments for
both capsule layers. It requires writable `/tmp`, a private tmpfs rather than
a host bind, active Landlock, and the existing network restriction. It failed
before the fix; all 48 compatibility-service tests passed afterward on ARM.
The native shell integration check also passed with locked dependencies.

## Kernel-level enforcement

Run the reusable probe on the native build architecture:

```sh
cargo build -p launcher --bin qos-launcher --locked
bash os/tests/kernel-landlock-private-tmp.sh \
  /path/to/kernel target/debug/qos-launcher /path/to/new-results
```

The script builds a small initramfs containing the real launcher and a file
access probe. QEMU uses TCG, no network, and no attached disk. The init process
mounts a private tmpfs and runs the launcher with and without the writable rule.
It requires `/tmp` writes to succeed only with the rule, and unrelated writes
to remain denied in both cases. Its init mode refuses to run outside PID 1.

The checked-in probe passed on the built qOS AArch64 7.2.5 kernel. Landlock
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
the qOS ABI 5 requirement; that is why this check uses the built qOS kernel.

## FEX syscall-policy comparison

The actual launcher library generated both `qos-default` and `qos-jit` BPF
programs for AArch64. A native helper installed each filter with
`PR_SET_NO_NEW_PRIVS` and `PR_SET_SECCOMP` before executing the test command.
Native `/usr/bin/true` passed under both filters. FEX executing the guest
`/usr/bin/true` exited 139 under `qos-default` and 0 under `qos-jit`.
`fex-qos-seccomp-validation.log` retains the comparison. These profiles differ
in their simultaneous writable/executable-memory restrictions; the other
syscall restrictions remain active.

Both Wine architectures subsequently passed the tracked console smoke test
with `qos-jit` installed before FEX and before the managed FEXServer. This
covered Wine version queries, creation of independent prefixes, `cmd.exe`
output, and wineserver cleanup/wait. The result is retained in
`fex-wine-qos-jit-results/result.json`. Container capabilities remained dropped,
external networking disabled, and the normal Podman filter enabled as well.

This establishes a syscall-policy requirement for the translated runtime.
Production Wine policy has not been weakened or changed to JIT based on this
diagnostic. A trusted runtime selection and complete capsule integration are
still required; catalogue-supplied settings must not grant a weaker filter.

## Capsule descriptor handoff

Native ARM tests on 2026-09-16 found that an unprivileged Bubblewrap launch
could not reopen `/proc/<broker-pid>/fd/<n>` directory sources: it returned
`Permission denied`. Direct inherited `--bind-fd` bindings succeeded and
retained the original directories after their pathnames were replaced.
The executor now passes the anchored prefix and home descriptors directly.
It duplicates sources above all reserved child slots before `pre_exec`,
preventing descriptor remapping from overwriting another pending source.

The real executor test also exposed an existing seccomp handoff bug. The
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
the actual `qos-default` filter loaded by Bubblewrap. The normal service suite
also passed all 48 tests; the environment-dependent regression is excluded
from the default suite and was run separately. The shell integration check
passed. These tests validate descriptor transfer, real mounts, and filter
loading; they do not replace the full capsule/Landlock runtime acceptance.

## Native translated-runtime supervisor

`qos-windows-runtime` is a native helper intended to run after entering the
capsule. Its allow-listed modes are `wine32`, `wine64`, and `proton`; they
select fixed guest entry points rather than a catalogue-supplied interpreter.
The guest root is fixed at `/opt/qos/x86`. The helper does not create or relax
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

`os/tests/managed-fex-wine-smoke.py` exercises the real helper with fresh
32-bit and 64-bit Wine prefixes. For each architecture it requires an exact
console marker and propagation of guest exit code 23 on a second invocation.
Unlike the earlier Wine fixture, it starts no external FEXServer: the native
qOS helper must own readiness, monitoring, Wine-server wait, and teardown.
All four checks passed on native ARM on 2026-09-16; the result and logs are
retained under `managed-fex-runtime-tracked-results` in the preparation tree.
Run it in the same offline, capability-free container described by the Wine
fixture, with the guest filesystem at `/opt/qos/x86`, the helper at `/helper`,
and empty writable output directory `/results`:

```sh
python3 /test.py --helper /helper --output-dir /results
```

The OS recipe builds and installs the helper at
`/usr/libexec/qos-windows-runtime`. Trusted host-architecture selection,
FEX/guest packaging, Proton manifest configuration, and end-to-end capsule
acceptance remain integration work; merely installing this helper does not
complete ARM runtime support.
