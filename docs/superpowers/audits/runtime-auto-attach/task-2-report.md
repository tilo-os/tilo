# Task 2 — independent native auto-attach proof and delivery docs

Owned proof tooling and documentation were committed as `e01fc4809`
(`test: prove native runtime auto attachment`). The report and retained target
evidence are coordination artifacts and are not part of that commit.

## Outcome

The retained macOS run at `target/runtime-proof/runtime-auto-attach/native-3`
passes the native behavior gate. It used literal ordinary launches:

```text
cargo run -p word-processor
cargo run -p file-explorer
cargo run -p photos -- --open /Users/xaver/dev/qos-macos-shell/target/runtime-proof/runtime-auto-attach/native-3/source.png
cargo run -p calculator
```

The first app started daemon PID 28409 before the MCP proxy was opened. The
proxy was then explicitly launched as
`target/debug/qos-runtime-mcp --profile /tmp/qos-auto-1omj8pct --connect` (PID
28548). No manifest, `--run-app`, manually supplied service descriptor,
`QOS_RUNTIME_BINARY`, Browser launch, or Keychain interaction was used.

Writer, Files, and Photos registered first. Calculator launched late and joined
the same daemon. The existing MCP adapter reported four distinct real peers:

| App | Peer | Actual PID |
| --- | ---: | ---: |
| `dev.qos.word-processor` | 1 | 28361 |
| `dev.qos.file-explorer` | 2 | 28451 |
| `dev.qos.photos` | 3 | 28526 |
| `dev.qos.calculator` | 4 | 28549 |

Calculator's freshly resolved declared `add-function` operation was invoked
with `sin(x) + 0.271828`. Invocation 1 moved from `accepted` to the canonical
terminal state `succeeded`; the complete request/reply sequence is in the
40-line `transcript.jsonl`. Writer PID 28361 was then terminated and reaped.
Files, Photos, Calculator, daemon PID 28409, and MCP PID 28548 remained live,
and MCP discovery contained only the three expected surviving apps.

The proof finally closed the MCP proxy with status 0, terminated and reaped the
remaining exact app PIDs, terminated the exact daemon PID, and checked all six
known PIDs independently. `cleanup.json` records `remaining: []`. The isolated
short `/tmp` runtime profile was removed; retained application state lives only
under the evidence directory. `proof.json` was written after that cleanup gate.

## Provenance and retained evidence

The host was built with
`cargo build -p shell --no-default-features --bin qos-runtime-mcp`. The exact
relevant input snapshot captured before the provenance build is
`host-build-source-hashes.json`. The runner hashed Cargo metadata and all Rust,
Cap'n Proto, and manifest inputs under runtime-portal, shell, ui, and the four
app packages before and after the native run; `source_changed` is empty. This
binds the proof to the active dirty source tree rather than claiming it came
from a clean commit. The run's recorded HEAD was
`9ca3b22ed5a33911e19ea06515a5188436b104da`; `metadata.json` retains its full
dirty status and isolated environment.

Exact exercised executable hashes:

| Executable | SHA-256 |
| --- | --- |
| `qos-runtime-mcp` | `30f265b5d6354c279acf06c581321de3fee945f2ed91c34d531c4fdbf840dcd2` |
| `word-processor` | `7fb4c08a282f1691d10dc7fdbb66edcc0601d096dda521290e0bb1524d560d0d` |
| `file-explorer` | `84f01bb18813c009236471aaad452fdfbab9dc86cff5d96c41caff7570241e40` |
| `photos` | `f299d83eb46e641ae311bfb5b5c88eb860702c934f45353f563fc73df41011c3` |
| `calculator` | `e1ada694524c4454ca0e1d107414eb00371109dde3a52d50780667e42f4c3077` |

The proof isolated `QOS_RUNTIME_PROFILE`, `QOS_USER_HOME`, Writer recovery, and
all XDG config/data/state/cache/runtime directories. It retained exact commands,
environment, source and binary hashes, app logs, daemon log, registrations,
action contract/admission/terminal output, process snapshots, and cleanup.
Private runtime identity/profile contents were not copied.

`native-1` and `native-2` remain as failed harness attempts. Both reached four
distinct peers and a successful Calculator terminal action, but correctly
withheld `proof.json`: the first used process-group escalation after the target
had exited, and the second mistook unreaped direct-child zombies for live PIDs.
The harness was changed to signal captured exact PIDs, wait/reap its direct
children, and separately verify every retained PID. A real subprocess regression
now covers SIGTERM, reap, and exact PID absence. Both failed attempts were
independently checked to have left no processes; they are not acceptance runs.

## Verification

- `python3 -m unittest scripts.tests.test_runtime_auto_attach_proof`: 6 passed.
  These focused tests cover exact ordinary command construction, native status
  validation, distinct registrations, source-change detection, exact-PID
  signalling, and real child reap/absence.
- `python3 -m py_compile scripts/runtime-auto-attach-proof.py scripts/tests/test_runtime_auto_attach_proof.py`: passed.
- `cargo build -p shell --no-default-features --bin qos-runtime-mcp`: passed.
  It retained two unrelated existing ui warnings (`unused_mut` in `record.rs`
  and dead `next_wake` in `animation_schedule.rs`).
- `python3 scripts/runtime-auto-attach-proof.py --output target/runtime-proof/runtime-auto-attach/native-3 --host-source-snapshot target/runtime-proof/runtime-auto-attach/host-build-source-hashes.json`: passed.
- `git diff --check` on the owned files: passed before commit.

The harness unit tests do not substitute for native acceptance; `native-3` is
the native evidence. Task 1's 13 runtime-portal, 32 launch, and 36 runtime-host
tests remain the focused implementation evidence from reviewed commits
`c2d6045c8` and `6dce082e8`.

## Documentation and limits

`docs/runtime-mcp.md` now describes ordinary zero-configuration joining,
first-app daemon startup, persistent profiles, source-checkout host bootstrap,
inherited qOS precedence, explicit assistant `--connect` configuration,
retained manifest mode, lifecycle, and Unix support. The todo records this
native gate as complete while leaving controller-owned whole-change review and
final acceptance open.

This proof establishes background native runtime behavior. It does not qualify
visible frames, presentation performance, Photos History acceptance, Browser
privacy behavior, native behavior for all 47 source-audited GUI entry points,
or Windows. Photos source was still under separate History development, so the
proof records exact build inputs/hashes and makes no History acceptance claim.

## Self-review

The final owned diff was checked against every Task 2 brief item. The runner
does not modify runtime/app implementation, does not infer liveness from a PID
file, does not replay uncertain mutations, starts `--connect` only after the
first app's native status reply, and writes the success marker only after exact
cleanup. Documentation distinguishes automatic app attachment from explicit
assistant setup and from legacy manifest ownership. No remaining Task 2 defect
was found; controller-owned final review remains intentionally open.

## Fix round 1 — reusable preparation and enforced profile removal

Fix commit: `9c6de88b3` (`fix: make runtime proof preparation reproducible`).

Independent review retained Spec Compliance PASS and required two P2 quality
fixes:

1. **“The documented proof command requires an artifact it never explains how
   to create.”** The runner now supports
   `--capture-host-source-snapshot PATH`, which writes the exact JSON schema
   consumed by `--host-source-snapshot`. The guide gives the required order:
   capture the source snapshot, build `qos-runtime-mcp`, then run the proof
   without source edits or another build between those steps.
2. **“A proof can pass even when the temporary runtime profile was not
   removed.”** Profile deletion no longer suppresses filesystem errors. The
   runner verifies the path and symlink are absent, records
   `profile_removed` and any `profile_cleanup_error` in `cleanup.json`, and
   refuses to write `proof.json` when removal failed.

Both changes were developed from focused failing regressions. Before the fix,
the public snapshot CLI test exited 2 because `--output` and
`--host-source-snapshot` were mandatory and the requested mode did not exist;
the profile test errored because `remove_profile` did not exist. After the fix:

```text
python3 -m unittest \
  scripts.tests.test_runtime_auto_attach_proof.RuntimeAutoAttachProofTests.test_supported_cli_captures_host_source_snapshot \
  scripts.tests.test_runtime_auto_attach_proof.RuntimeAutoAttachProofTests.test_profile_cleanup_rejects_a_remaining_directory
..
Ran 2 tests in 0.140s
OK
```

The full focused suite, Python compilation, diff checks, a real supported CLI
snapshot capture, and a real temporary-directory removal check are recorded in
the final fix verification. No native GUI run was repeated: `native-3` remains
valid, runtime/app implementation did not change, and review independently
confirmed that retained profile was absent.

Final fix verification:

```text
python3 -m py_compile scripts/runtime-auto-attach-proof.py \
  scripts/tests/test_runtime_auto_attach_proof.py
# exit 0

python3 -m unittest scripts.tests.test_runtime_auto_attach_proof
........
Ran 8 tests in 0.237s
OK

git diff --check -- scripts/runtime-auto-attach-proof.py \
  scripts/tests/test_runtime_auto_attach_proof.py docs/runtime-mcp.md
# exit 0

# A fresh temporary profile containing a marker was passed to remove_profile.
# The helper returned, both profile and marker were absent, and the empty test
# parent was removed.
real profile removal: passed
```
