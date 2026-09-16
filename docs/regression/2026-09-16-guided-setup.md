# Guided setup implementation evidence

This records incremental evidence for the Windows/Linux migration application.
It is not a claim that the full migration application is finished.

## Foundation model — f6ab9c6e7

- macOS host baseline before changes: `cargo test -p imager --lib
  --no-default-features`, 133 passed.
- After changes: focused setup model suite, 16 passed; full Imager library suite,
  149 passed. The implementer's report is committed with that change.
- Independent task review approved the capability gating, folder-mode semantics,
  storage accounting, and lexical path validation without actionable findings.
- Native Linux verification: Fedora 44 ARM64 Parallels VM, `uname -m` reports
  `aarch64`. Compiled the current shared worktree's
  `apps/imager/src/setup/model.rs` using the guest user's Rust compiler with
  `--edition=2024 --test`; ran the produced guest executable. All 16 tests passed.

The model requires capability evidence rather than enabling a route based on
the host name, counts both copied and synchronized local files, and rejects
overflow and lexical source/destination overlaps. Actual filesystem identity,
case folding, mount safety, and authorization remain execution responsibilities.

## Outstanding end-state evidence

- Native Windows build, UI, folder picker, package, and retained-Windows boot.
- Linux interactive wizard, desktop-session installation, and complete boot flow.
- Internal installer bundle and same-disk protected-partition protocol.
- Host-side storage preparation, encryption handling, and recovery across crashes.
- Real copied files, resumable verification, conflicts, recoverable deletions,
  and two-way connections across boot sessions.
- First-login continuation, boot selection, default switching, signed release
  availability, and representative physical-hardware verification.

No Windows VM is currently listed by Parallels. The bundled Imager release
catalog is `unreleased` with no payload entries. These are current verification
and distribution gaps, not reasons to replace the requested native app with a
mock or to claim completion from model tests.
