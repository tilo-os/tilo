# Critique of the qOS native app model — as demonstrated by qos-hello

Concrete findings from actually building + installing + launching a qOS-native app end-to-end.
Reviewed against the four goal pillars: **DX, security, native APIs/performance, install speed**.

## What already works

| Pillar | Evidence |
|---|---|
| Install speed | pack 7ms / verify 7ms / unpack 8ms for a 3 MB binary. Zero-overhead once bytes are local. |
| Native perf | Plain ELF, dynamically linked. `qos-runtime` SDK adds zero runtime indirection for a "hello" workload. |
| Graceful degradation | `qos-hello` runs cleanly with all services absent. `is_qos_native()==false`, every `try_connect()==Err(ServiceFdMissing)`. The Flatpak-portability promise holds. |
| Content addressing | BLAKE3 root hash IS the identity. `qos-pkg pack` is deterministic (same inputs → same PackageId). |
| Signing | ed25519 sig over `(manifest_hash, payload_root_hash, version, publisher_pubkey)`. Verify roundtrips. Byte-tamper detection works. |
| Sandbox structure | bwrap invocation constructed from manifest declarations. Seccomp deny-list applied via `--seccomp <fd>` (memfd-staged). Namespace isolation enforced. |

## Security gaps — priority-ordered

### Blocking (must fix before "uncompromising security" is honest)

1. **Landlock plan is derived but not enforced.** Launcher logs "shim wiring is a follow-up". The kernel FS-access ruleset is computed and thrown away. This is the difference between "we say the app can't read your Documents" and "the kernel prevents it."
   - **Fix**: implement the self-reinvocation shim (`qos-launcher __sandbox-exec <manifest> <entry>`) — the pattern the compositor already uses at `shell/src/sandbox/landlock_shim.rs`. Landlock rules install after bwrap's mount namespace is set up, before exec.

2. **`/usr` and `/lib64` are bind-mounted read-only into every dynamically-linked app's sandbox.** Necessary for glibc but includes all host binaries, setuid, `/etc/shadow`, `/etc/passwd`, everything under `/usr/share`. A malicious app has an enormous read surface. The demo manifest explicitly binds them because the launcher doesn't provide a minimal runtime.
   - **Fix short-term**: launcher grows a `--minimal-libs` flag that binds only `/lib64/ld-linux-x86-64.so.2` + `/lib64/libc.so.6` + `/lib64/libgcc_s.so.1` (or the equivalent /usr paths) rather than the whole tree. Explicit allowlist.
   - **Fix long-term**: real `qos-runtime` bundle at `/usr/lib/qos-runtime/<version>/` provides ONLY what apps declare in their runtime dep. Nothing else on host FS is bind-mounted.

3. **Publisher trust is self-trust.** `qos-pkg verify` succeeds because the publisher's own key signs the package. Anyone can pack + sign anything. There's no "trusted-root" walk.
   - **Fix**: `qos-pkg verify --require-attestor qos-curated/<ver>` should walk the `trust.attested_by[]` list, verify each claim signature against the attestor's known pubkey, refuse if none are trusted.
   - **UX fix**: install flow shows the actual publisher key on first install with a "trust this publisher going forward?" prompt.

4. **Attestation `claim` is opaque.** Currently accepted as any hex string. Not verified against any attestor pubkey.
   - **Fix**: `[[trust.attested_by]]` entries need a `signature` field. The attestor's pubkey lives in `~/.config/qos/trusted-attestors.toml`. Verify at pack, verify at install, refuse at launcher for `strict` policy.

### Non-blocking but worth addressing

5. **No revocation.** Compromised publisher key → all their previously-signed packages remain valid.
   - **Fix**: `~/.config/qos/revoked-keys.txt` checked at verify. Attestors can publish revocations.

6. **Payload can be modified after unpack.** Launcher trusts `--payload-root` without re-hashing. Deliberate for local dev but a real install should verify.
   - **Fix**: `qos-launcher run-installed --require-hash <id>` re-hashes the payload tree against the manifest's `payload_root_hash`.

7. **Seccomp deny-list vs allow-list.** Current policy blocks ~10 dangerous syscalls but everything else is allowed. `execve` of another binary, `open` on any bind-mounted path, network syscalls if net's shared. Reasonable default but worth tightening.
   - **Fix**: per-package `sandbox.seccomp = "qos-strict"` option that uses an allowlist. Opt-in from stricter apps.

## DX gaps — priority-ordered

### Blocking (must fix before presenting)

1. **Manifest schema divergence.** `qos-pkg` writes `[package] id = "..."`. `qos-launcher` expects top-level `package_id = "..."`. User has to know this and hand-edit the manifest after `unpack`. Every friction point.
   - **Fix**: `qos-launcher` uses `qos_pkg::Manifest`, not `MinimalManifest`. Delete the shim.

2. **Exec bit dropped by unpack.** After `qos-pkg unpack`, `bin/qos-hello` is mode 644. Launcher's bwrap fails with a misleading "No such file or directory" — actually POSIX `EACCES` masquerading. User has to know to `chmod +x`.
   - **Fix**: qos-pkg records permission bits per blob; unpack restores them.

3. **User has to know to bind `/usr`, `/lib64`, `/etc/ld.so.cache` for dynamic binaries.** Not documented. The default sandbox is unusable for any real app.
   - **Fix**: launcher grows `--minimal-libs` flag that adds the dynamic-loader binds automatically. Documented as the recommended default until the runtime-bundle path lands.

### Non-blocking

4. **`qos-pkg pack` auto-generates a keypair silently.** No way to recover it. If you sign a v0.1 qpkg and want to sign v0.2 later, you can't (unless you saved the identity).
   - **Fix**: emit `--identity-out <path>` when auto-generating, or refuse to auto-generate unless `--identity <path>` is `<generated>`.

5. **No `qos-install <qpkg>` verb.** User orchestrates unpack + chmod + record manually.
   - **Fix**: one command that unpacks to `~/.local/share/qos-apps/<id>/`, sets exec bit, writes a symlink in `~/.local/share/qos-apps/bin/<cli-cmds>` if the manifest has `[cli]`.

6. **Launcher error messages are terse.** "TOML parse error: missing field `package_id`" doesn't hint that the launcher's shim schema differs from the qpkg schema — the fix from #1 removes this whole class.

7. **No way to inspect the effective sandbox.** `qos-pkg inspect` shows the manifest text; there's no human-readable "this app can read /X, can write /Y, can talk to services Z" summary.
   - **Fix**: `qos-pkg inspect --sandbox` renders the sandbox declarations in prose plus intersections with the workspace policy (when applicable).

8. **No unsandboxed dev-run mode.** Iterating on an app is slow because every launch goes through bwrap.
   - **Fix**: `qos-launcher run --dev` skips bwrap/Landlock/seccomp, prints "DEV MODE — SANDBOX BYPASSED" in red, refuses if the manifest declares `[trust]` (published packages must not dev-run).

## Install-speed observations

For the 3 MB `qos-hello`:

| Step | Wall-clock |
|---|---|
| pack | 7 ms |
| verify | 7 ms |
| unpack | 8 ms |
| launcher setup (bwrap argv + seccomp compile + fd plumbing) | ~40 ms |
| **install-to-run total (excluding build)** | **~65 ms** |

For a real 500 MB video editor:
- pack + unpack scale linearly (~1 s for 500 MB on NVMe)
- verify is O(blobs), not O(bytes) — stays ~10 ms because signature is over the root hash
- **launcher overhead stays constant** — bwrap doesn't care about payload size

Where the design promises "instant" but we haven't proven it:
- **Page-fault streaming** (qos-app-fs) — WIP anchor; not built. Without it, cold-launch is bounded by whole-payload fetch. With it, only bytes the app reads are transferred.
- **Content-addressed dedup across apps** — the store layer that would dedup libqos-runtime across all installed apps. Also WIP.

## What I'd fix now vs later

**Now** (before presenting):
- DX #1 (manifest schema unification)
- DX #2 (exec bit preservation)
- DX #3 (--minimal-libs launcher flag)

**Deferred (documented, not blocking demo):**
- Security #1 (Landlock enforcement) — needs shim binary, ~1 day of careful work
- Security #2 second bullet (real runtime bundle) — ~1 week
- Security #3 (trusted-root verify chain) — ~2 days
- DX #5 (qos-install verb) — ~1 day
- FUSE page-fault streaming — WIP anchor exists, needs fuser-0.17 rewrite

Everything else is a follow-up. The demo shows the pipeline works; the fixes are known and scoped.
