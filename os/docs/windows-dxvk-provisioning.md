# Managed Wine DXVK provisioning

The ARM Wine helper installs the verified upstream DXVK payload into each
private Wine prefix. Proton retains its own runtime and DXVK management.
Upstream DLLs are native Windows libraries: unlike Fedora's specially marked
DXVK packages, they must not simply replace files in Wine's builtin directories.

## Immutable inputs and private destinations

The helper requires `WINEPREFIX=/run/tilo/capsule/prefix`. It reads the verified
payload and provenance from `/opt/tilo/x86/usr/share/tilo/dxvk`, and uses the
immutable `usr/lib/tilo/windows-guest/installed-packages.tsv` inventory to detect
guest runtime changes. These files are produced by the guest image build.

| Managed mode | Payload | Prefix destination |
|---|---|---|
| `wine32` | `x32` | `drive_c/windows/system32` |
| `wine64` | `x64` | `drive_c/windows/system32` |
| `wine64` | `x32` | `drive_c/windows/syswow64` |

Both variants install `d3d8`, `d3d9`, `d3d10core`, `d3d11`, and `dxgi`.
Only the application invocation gets the fixed native DXVK overrides. Wineboot
uses the separate `wineboot.exe=b` override; Wine server commands remove DLL
overrides. Proton's environment is left unchanged. No global process environment
is modified.

## Initialization, concurrency, and filesystem safety

Every managed Wine launch holds a shared lock on the retained prefix directory
inode until its application, Wine server wait, and FEX server cleanup finish.
Ready prefixes permit concurrent launches. Initialization or a changed payload
requires an exclusive, nonblocking lock. A busy prefix returns an error so the
caller can retry after its applications exit; it never kills those applications.
The lease explicitly unlocks its own open-file description when its lifetime
ends, including preparation error paths. A duplicated descriptor cannot extend
that lifetime, while independently opened leases from other launches remain
locked.

Under exclusive ownership, the helper invokes the architecture-matching
Wine builtin `wineboot.exe` through the existing FEX supervisor, using `-i` for
initialization and `-u` for an existing prefix update. It then waits for
the matching Wine server. Both preparation commands have 120-second deadlines.
The server wait is deliberately absent from the already-prepared fast path:
`wineserver -w` waits for server termination, including other clients.

Exclusive preparation runs inside a native child-subreaper scope. If Wineboot
or its server wait fails, the helper first attempts the matching `wineserver -k`
and `-w`, each with a ten-second deadline. Before returning from preparation,
it kills and reaps remaining owned descendants, including detached Wine workers,
using process descriptors. The existing FEX server is excluded. This cleanup
also runs after successful preparation, and never runs on the shared ready-prefix
path. The helper verifies that it is single-threaded and that process-descriptor
operations work before starting initialization. It restores the previous
subreaper setting after cleanup.

The exclusive prefix lock remains held until all initialization descendants
have been reaped. Exceptionally, a kernel-stuck process or unavailable cleanup
operation can extend this wait beyond the command deadlines; the helper retains
the lock rather than admitting another writer. Capsule teardown remains the
external recovery mechanism in that case.

Wineboot is selected by its fixed Windows name with builtin-only loading.
An absolute Unix path failed with status 139 in the managed C-only prefix;
Wine's ordinary automatic initialization succeeded. The helper preserves the
absence of a `Z:` drive and removes `WINEDLLPATH` and `WINEBUILDDIR` from the
Wineboot command, leaving only the runtime's immutable builtin search paths.

Directory traversal uses retained descriptors and rejects symlinks. DLLs are
written into exclusively created temporary files and renamed over destination
entries, so existing symlinks or hardlinks never cause their targets to be
truncated. Up to 128 temporary-name collisions are skipped without deleting
foreign leftovers. FIFO leaves cannot block validation. Present registry files
must be regular, singly linked files with the expected Wine architecture.
Selected linked DLL entries are removed before Wineboot without following their
targets; directory and device entries are rejected.

The generation marker is only a cache hint. Every launch verifies all selected
DLL hashes against immutable inputs and checks all three Wine registry headers.
A marker alone cannot establish readiness. This validates current prefix
contents; a mutable prefix cannot independently attest historical initialization.

## Validation

On 2026-09-16, the service suite passed 82 tests (seven environment-dependent
tests ignored), strict Clippy passed, and the native ARM helper built with the
pinned Rust toolchain. Regression tests cover concurrent shared leases, busy
upgrades, corrupt payloads, redirected directories, external leaf links,
incomplete initialization, false markers, FIFO leaves, and stale temporary files.
An isolated native regression also verifies reaping an adopted detached child,
preserving a preexisting child, restoring subreaper state, and keeping the prefix
lock unavailable throughout cleanup.
Thirty consecutive default-parallel runs of the combined 29-test DXVK and
subprocess group passed. The final helper, including explicit lease release,
passed the portable generated Wine32 namespace in 30.23 seconds and Wine64 in
50.34 seconds. The lease regression retains a duplicated descriptor
and verifies that lease destruction still releases its lock; the independent
shared-lease regression verifies that another launch continues to prevent an
upgrade.
Wine 11.0 initialization confirmed the expected `system.reg`, `user.reg`,
and `userdef.reg` architecture headers for both modes.
Controlled Wineboot tests confirmed that builtin-only selection ignored a native
substitute in the initialized prefix, including one forged with a `Wine builtin
DLL` DOS marker. The substituted bytes remained unchanged, the substitute's
diagnostic did not execute, and Wineboot completed normally. Selecting the native
substitute instead produced a different failure, confirming the selection test.

The generated ARM Wine namespace fixtures also passed with the current helper,
the original native FEX runtime, and the derived guest root containing the
verified DXVK inputs. Wine32 completed in 29.43 seconds and Wine64 in 49.03
seconds. The portable FEX runtime also passed both generated namespaces:
Wine32 in 30.10 seconds and Wine64 in 47.99 seconds. With the capsule working
directory fixed to its private `drive_c`, portable Wine64 passed in 48.12 seconds
without the previous unavailable-working-directory warning. The descendant
cleanup helper subsequently passed portable Wine32 in 30.55 seconds and Wine64
in 50.26 seconds, also without that warning. These are end-to-end fresh-prefix
console-test times, not isolated
DXVK provisioning overhead measurements. Each test required a Windows
file marker and retained the generated offline namespace and JIT syscall
filter. These fixtures omit the executable Landlock shim on the WSL kernel;
they do not establish full Tilo-kernel sandbox acceptance.

The standalone native DXVK load fixture produced these results:

| Runtime and prefix | Windows executable | Result |
|---|---|---|
| Wine32, Win32 prefix | PE32 | All five native DLLs and required exports verified |
| Wine64, Win64 prefix | PE64 | All five native DLLs and required exports verified |
| Wine64, Win64 prefix | PE32 | Dispatch failed before DLL evidence; unresolved |

For each successful row, the fixture checked the loaded Windows module path,
the expected exported function, Wine's native-load trace, and the installed
DLL hash against the immutable payload. Evidence is retained under
`managed-dxvk-load-v3.rBFihB/results` in the preparation directory. The failed
mixed-architecture row is not accepted merely because `syswow64` contains the
32-bit payload. Mixed-prefix dispatch still requires a fix and successful
execution evidence. Export lookup does not establish Direct3D device creation,
Vulkan translation, or rendering.

`os/tests/managed-fex-wine-smoke.py` requires a disposable writable `/run/tilo`
mount and the complete guest payload. It checks console output, exit codes,
and all installed DLL hashes, retaining its test prefixes inside the disposable
container. Full graphics, Tilo-kernel sandbox, and final-image acceptance remain
separate requirements.
