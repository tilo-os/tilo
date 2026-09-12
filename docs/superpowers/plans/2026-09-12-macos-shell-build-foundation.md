# macOS Shell Build Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the target-configuration and identity-publication blockers so the shared runtime can be tested natively on macOS and Fedora, as the first stage of the full shell port.

**Architecture:** Keep one source tree and OS-specific implementation boundaries. Preserve Linux hardening and identity backup guarantees while selecting valid Darwin build flags. Later subsystem plans implement the app host, platform services and shell runner; this foundation is not completion of the user's goal.

**Tech Stack:** Rust 1.97.1, Cargo resolver 2, native C/C++ via cc-rs, macOS arm64, Fedora 44 arm64 in Parallels.

**Spec:** `docs/superpowers/specs/2026-09-12-macos-shell-port-design.md`

## Global Constraints

- The shell runs on macOS, apps run inside, Fedora headless execution remains working, and all tests must pass on their owning hosts before the goal is complete.
- Apps retain shared source; target-specific providers implement host differences.
- Preserve the current Linux ELF hardening, default features and test assertions.
- Do not hide a shared test failure by gating it away or add fake-success providers.
- Use `/Users/xaver/dev/qos-macos-shell`, branch `codex/macos-shell`, for all source changes.
- Fedora's independent clone is `/home/parallels/qos-macos-shell`; update only by fast-forward from the same branch in `/media/psf/Home/dev/qos`.
- Mac build output: `/Users/xaver/dev/qos/target/macos-shell`. Fedora build output: `/home/parallels/qos-target`.
- Tests and benchmarks must use temporary profiles, not the user's workspace/identity.

## Full objective roadmap

1. This plan: native build flags and portable identity file publication.
2. Shared UI/runtime dependency closure: native builds and tests on both hosts, with platform APIs below SDK boundaries.
3. Shared component hosting: actual app factories, app-scoped state/lifecycle/wakes, and shell integration.
4. Mac shell runner and services: main-thread winit/Metal, real input, identity, collaboration, file/clipboard and accessibility.
5. Cross-host regression: complete test inventory, full suites, Fedora headless CLI/action/screenshot scenarios and Mac real-window scenarios.
6. Final branch review and requirement-by-requirement completion evidence.

Every later stage needs a concrete plan after its dependency audit. Keep this roadmap and the active goal open until all stages are verified.

## Task 1: Select native build flags by operating system

**Files:**
- Modify: `.cargo/config.toml`
- Verification subjects: existing `atomic-file` tests and a disposable Rust/C fixture built through cc-rs.

**Interfaces:**
- Consumes: Cargo's target cfg selection and cc-rs `CFLAGS_<target>` / `CXXFLAGS_<target>` lookup.
- Produces: the same Cargo commands work on Darwin and Linux without an environment override that drops hardening.

- [ ] Run `CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell cargo test -p atomic-file --locked` and capture the actual Darwin linker failure before editing.
- [ ] Scope existing architecture blocks to `cfg(all(target_os = "linux", target_arch = "x86_64"))` and `cfg(all(target_os = "linux", target_arch = "aarch64"))`. Keep each existing Linux rustflag unchanged.
- [ ] Add a Darwin Rust flag block with valid native options:

```toml
[target.'cfg(target_os = "macos")']
rustflags = ["-C", "force-frame-pointers=yes", "--remap-path-prefix=/build=."]
```

- [ ] Replace global forced `CFLAGS`/`CXXFLAGS` with explicit target variables. Preserve the existing x86_64 Linux values; add aarch64 Linux values equal to the former generic Linux flags. Add Darwin arm64/x86_64 C and C++ values `-fstack-protector-strong`. Keep `force = true` and explain why GNU assembler/ELF switches must not reach Darwin. Use full target strings with underscores, as existing cc-rs configuration does.
- [ ] Run the atomic-file tests again. Build and execute a disposable Cargo package whose `build.rs` calls `cc::Build::new().file("probe.c").compile("probe")`, whose C function returns 42 and whose Rust test calls it. Copy the real Cargo config into this isolated fixture so real target selection and cc-rs consume it. Record the fixture and commands in the task report rather than adding a config-text assertion test to the repository.
- [ ] Commit only `.cargo/config.toml` with `fix(build): scope native flags to Linux and macOS targets`.
- [ ] The controller fast-forwards Fedora and repeats atomic-file plus the C fixture. Verify the Linux executable's GNU_STACK is non-executable and GNU_RELRO is present with `readelf -lW`; x86 shadow-stack flag values are retained unchanged, with native x86 validation in later CI.

## Task 2: Publish identity backups without Linux-only syscalls on Darwin

**Files:**
- Modify: `crates/runtime/runtime-identity/src/backup.rs`
- Tests: the same file's existing test module.

**Interfaces:**
- Consumes: `publish_new_file(temp: &Path, destination: &Path) -> io::Result<()>` and `write_new_backup` durability wrapper.
- Produces: publication remains atomic and refuses an existing file on both systems; successful publication consumes the staging name.

- [ ] Run the existing runtime-identity test build on Mac after Task 1 and record the Linux syscall compile failure.
- [ ] Add a real filesystem race test: create two staging files with distinct bytes in one `tempfile::TempDir`; synchronize two threads using `Arc<Barrier>`; call `publish_new_file` concurrently for one destination; assert exactly one succeeds, destination equals the winning complete payload, the winning staging name is removed and the loser's bytes remain unchanged. Derive expected byte arrays literally in the test.
- [ ] Keep Linux's existing `renameat2(RENAME_NOREPLACE)` path under `cfg(target_os = "linux")`. Extract/use the existing hard-link publication behavior for other supported Unix targets:

```rust
fn publish_with_link(temp: &Path, destination: &Path) -> io::Result<()> {
    fs::hard_link(temp, destination)?;
    fs::remove_file(temp)
}
```

  Linux's ENOSYS/EINVAL/EOPNOTSUPP fallback calls the same helper. Gate CString/imports used only by Linux. Preserve same-directory staging and existing file/directory synchronization; do not replace with exists-then-rename.
- [ ] Run `cargo test -p runtime-identity --locked` on Mac and Fedora, including backup corruption/symlink/overwrite and the new race test. Diagnose any additional runtime build errors from evidence and report them instead of declaring an incomplete package green.
- [ ] Format only affected files and run the package's clippy checks. Commit as `fix(identity): preserve exclusive backup publication on macOS`.

## Verification record

The controller records exact commits, host commands, output paths and results in this plan's SDD ledger. Per-task review covers spec compliance and code quality. Full shell, app and Fedora headless verification belongs to the remaining roadmap; passing these foundation tests cannot close the active goal.
