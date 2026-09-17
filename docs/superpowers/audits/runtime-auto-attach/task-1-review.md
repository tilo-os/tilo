### Spec Compliance

- ❌ Issues found: the shared bootstrap forces offline Cargo operation (`crates/runtime/runtime-portal/src/native.rs:206`), and rejects ordinary framework child launches after SDK adoption (`crates/runtime/runtime-portal/src/native.rs:50`, `crates/runtime/runtime-portal/src/lib.rs:142`). Both conflict with the requested automatic ordinary-app behavior.
- ⚠️ Browser privacy remains a separately owned boundary. The generic predicate at `ui/src/launch.rs:139` does not prove suppression of inherited collaboration, receipts, discovery, or presence; require the Browser local constructor and direct/spawned ambient-authority regression evidence before accepting that integration.
- ⚠️ Actual ordinary Cargo GUI launches, real declared action execution, visible-frame evidence, Linux execution, and final all-app proof remain Task 2/controller checks; SDK subprocess coverage at `shell/src/runtime_host/daemon_process_tests.rs:189` is useful transport/lifecycle evidence, not that proof.

### Strengths

- `shell/src/runtime_host/unix.rs:55` centralizes connection preparation for managed spawn and automatic exec, preserving one identity-handshake implementation.
- `shell/src/runtime_host/daemon.rs:183` uses a real bounded status exchange and startup lock; `shell/src/runtime_host/daemon.rs:245` acquires Session ownership before removing stale native endpoints. The production Session and existing McpServer remain the authority and adapter.
- `ui/src/launch.rs:638` and `ui/src/launch.rs:746` attach before live construction, with an injected attachment seam and detached diagnostic branches; no application allowlist is introduced.
- `shell/src/runtime_host/daemon_process_tests.rs:189` exercises distinct real SDK registrations, late join, common daemon ownership, and a surviving app after the first exits. `crates/runtime/runtime-portal/tests/native.rs:122` exercises serialized missing-host construction and visible failures, although its offline assertion must change.

### Issues

#### Critical (Must Fix)

- None identified in this task package.

#### Important (Should Fix)

- **Plan-mandated defect — forced offline first-launch build.** `crates/runtime/runtime-portal/src/native.rs:206` unconditionally adds `--offline`; `crates/runtime/runtime-portal/tests/native.rs:127` explicitly requires it. A user can successfully build an ordinary app while lacking shell-only dependencies in Cargo's cache, so the promised first plain `cargo run` then fails to bootstrap its host. The corrected spec requires normal Cargo dependency/cache/network settings. Remove the unconditional flag and revise the controlled-Cargo regression to require ordinary settings by default and preservation of explicitly selected offline settings (for example `CARGO_NET_OFFLINE`/Cargo configuration). This must not be described as intentional acceptable no-download behavior.

- **Ordinary app-launched children are mistaken for malformed launcher transfers.** `crates/runtime/runtime-portal/src/lib.rs:142` correctly restores CLOEXEC, but `crates/runtime/runtime-portal/src/native.rs:50` treats any retained `QOS_SVC_COLLAB_FD`/`QOS_SVC_IDENTITY_FD` environment value as current inherited authority. The SDK leaves those variables in the process environment; identity adoption similarly sets CLOEXEC (`crates/runtime/runtime-identity/src/lib.rs:1396`). File Explorer's `SystemRunner::spawn`/`spawn_arg` (`apps/file-explorer/src/files_app.rs:3976`, `apps/file-explorer/src/files_app.rs:3992`) uses ordinary `Command::spawn`, inheriting those values while the kernel closes their descriptors. The launched framework app therefore fails `ensure_attached` before becoming live, rather than obtaining its own runtime connections. Introduce a shared process/transfer provenance discriminator that distinguishes authority assigned to an ancestor from a transfer assigned to this process; ensure stale metadata cannot affect either bootstrap or downstream SDK adoption. Keep actual current malformed/partial transfers fail-closed and retain authoritative inherited connections. Do not solve this with per-app environment clearing or an allowlist. Add an isolated SDK-parent → ordinary Command child regression (including same-app child/resume marker behavior), plus existing malformed-transfer regressions.

#### Minor (Nice to Have)

- Existing validation output is not pristine: `.superpowers/sdd/2026-09-17-runtime-auto-attach/host-build.log:11` and `:21` contain unrelated UI warnings; `host-green.log:39` contains a linker unwind warning; `clippy-owned.log:931` ends with existing shell lint errors. These are disclosed limitations, not a reason to expand this task into unrelated cleanup or claim Clippy passed.

### Assessment

- **Task quality: Needs fixes.** Shared ownership and lifecycle architecture are appropriate, and focused tests cover meaningful process boundaries. The forced network restriction and ordinary descendant startup failure block the requested all-app behavior.
- **Review scope:** owned `c2d6045c8^..c2d6045c8`, 11 files; excluded intervening external commits and original-base branch drift. Read the supplied review package in one logical pass, retrieving portions omitted by tool-output truncation; no changed source files were separately re-read.
- **Focused external check:** named risk was ordinary descendant startup under SDK-owned CLOEXEC services; inspected only File Explorer's SystemRunner launch methods and runtime-identity's service adoption to establish the environment/descriptor mismatch. No application code changes, git mutations, suite reruns, native GUI launches, or subagents.
- **Evidence inspected:** portal-green.log reports 10 passing tests at line 17; ui-green.log reports 32 at line 72; host-green.log reports 33 at line 112; host-build.log records successful completion. These establish the reported checks, not coverage of the two missing behaviors above.
