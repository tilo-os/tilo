# Validation status before measurement

- The isolated-main adapter diff was reviewed and frozen before any Cargo command.
- Scoped rustfmt and git diff --check passed.
- Recorder synthetic failure test preserved exit42, both stdout/stderr, byte count and SHA256; no truncation.
- Parser:14 synthetic assertions pass, plus successful parsing of the previously audited shell/UI control logs. Aggregation:8 synthetic assertions pass, including withholding ratios if all three baseline/candidate attempts did not complete.
- adapter-pure-tests-v1 reached the15-minute build cap (exit124); no tests ran. Its complete raw build log/result are retained. No source or profile change followed.
- adapter-pure-tests-v2 reused compiled dependencies and passed all4 added pure tests (3 shell,1 UI), no ignored tests and no GPU warmup. It completed in6m00s. Reported8,788,420KiB peak is build/test process RSS, NOT benchmark application memory; compiler work through sccache may also occur in its server process.
- All14 selected adapter/source/profile/dependency inputs matched the frozen manifest after validation. Git status contains exactly the four allowed adapter files.
- ordinary-release-build-v1 failed with a compiler internal error and malformed dependency archives; 163 of 1,745 scanned archives had invalid, zeroed headers.
- ordinary-release-build-direct-v1 used a fresh target and bypassed sccache, but failed after 41 seconds with Input/output error. The kernel confirmed ongoing writes failing on vda/vda3 despite 143 GiB guest free space. No runtime benchmark was executed.
- After user-authorized sccache cleanup, a flushed 1 MiB write matched a direct disk reread and kernel warnings were absent during the check. All 14 frozen hashes still matched. ordinary-release-build-after-cleanup-v1 used another fresh target and hit its 15-minute cap (exit 124) without new storage errors. The observed Btrfs write-error counter stayed at 20,769.
- ordinary-release-build-after-cleanup-v2 succeeded (exit 0) in 12m43s, continuing the same normal release build using v1's successful dependencies. All 14 input hashes matched before and after. All 942 new rlib archive headers are valid. Both final main executables were flushed and their direct-I/O reread SHA256 values matched ordinary reads. Build log byte count/SHA256 match the result metadata; no compiler processes remained before runtime.
- Btrfs write_io_errs stayed at 20,769 through the successful build and binary audit. No new storage errors were logged after cleanup. An unrelated USB port-resume warning occurred at19:01; this is not being presented as a completely warning-free kernel.
- The build's 11,178,332 KiB peak RSS is compiler/build-process memory, NOT application memory.
- All failed attempts and their complete logs remain preserved. Test binaries are not used as performance executables.

The original working branch has not been committed, pushed, merged or reset. Current-main performance results are not available yet.

## Final measurement status (supersedes the preceding pending note)

- All21 predeclared runs completed with valid logs and expected sample counts. All file byte counts and SHA256 values matched their recorded metadata. No runtime attempts were replaced or discarded.
- All4 binary hashes,14 frozen main input hashes and38 original worktree input hashes still matched after measurement. No Cargo/rustc/benchmark processes remained.
- Btrfs write_io_errs remained20,769. No source or benchmark-harness edits were made during build/measurement.
- A reporting-only correction was applied after measurement: unavailable worst-event time becomes null instead of0; the frozen mean-time ratio is suppressed because branch means are integer-microsecond truncated. The original generated summary is preserved; comparison-summary-v2.json is the corrected version. Summary fixtures and all14 parser assertions pass.
- Remote main advanced from the tested31e5afdde to84329100b (11 commits /53 files) while the task ran. The new tip was fetched but not checked out over the benchmark or original worktrees and has not been benchmarked here.
- Fetch reported one existing empty loose Git object10f6023ceb985a5b2fa9c87d6beec41a00857a30, dated18:40:23+02:00 before cache cleanup. A connectivity-only check of HEAD and origin/main passed. The empty object was left untouched; full repository integrity/repair is not claimed.
- Full results, limitations and individual attempt links are in RESULTS.md. Performance goals remain unmet; no commit/push.
