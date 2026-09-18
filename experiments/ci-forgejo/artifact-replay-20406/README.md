# Historical app artifact reuse rehearsal

On 2026-09-19, replayed retained Forgejo artifact **34748**, `tilo-apps-x86_64`,
from [successful main-push run 20406 (UI 7045)](https://code.podesta.ai/qos/qos/actions/runs/7045).
Fresh API metadata confirmed source `525aa7824c7a8476aa5ea006499ddd01a8b3c2ea`,
not expired, 328,814,609 expanded bytes. Downloaded ZIP is 130,844,973 bytes.
The isolated checkout starts at `87e99138d1f6b25c9a23121cad6f5c66137856ad`.
No native compilation, execution of payloads, publication, or live CI changes.

## Result

- All 67 ZIP entries are regular files with safe unique relative paths; original
  SHA256SUMS covers every other entry and matches.
- All 51 qpkgs pass Ed25519 signed-root verification, package identity, manifest,
  collection and every payload BLAKE3 hash, index SHA256, manifest identity/version
  and matching declared runtime. Checks mirror the documented format in
  `crates/packaging/pkg-cli/src/main.rs` and `pkg/src/{signed,blobs}.rs`.
- Runtime `tilo-runtime/2026.07` passes its publisher/signature, architecture,
  BLAKE3 root and all payload hashes/sizes. Signing preimage matches
  `crates/packaging/runtime-bundle/src/lib.rs`.
- Actual current `dist/apps/assemble-collection.sh` ran twice against these
  historical inputs. Every output file matched byte-for-byte SHA256, including
  compressed collection archive `4cd9ab079136b8bcc97fa9000ee92485911a2182b870eab2411586fb6f1d1e84`.
  The receipt records source, runtime, publisher keys, each package identity,
  assembler script hashes and every output hash.
- Measured checkout plus retained input/output files: 1,058,833,529 logical bytes
  (about 1 GiB), below the 2 GiB task budget. Only one output tree was retained;
  the second assembly replaced the first after its digests were captured.
- Five targeted tests cover path escapes, ZIP symlinks, checksum tampering,
  signed qpkg manifest/signature/payload tampering, and runtime signature,
  payload and unlisted-file rejection.

This proves **historical artifact integrity and current assembler compatibility**.
It does not prove native execution, current-source/toolkit ABI compatibility,
publisher trust/revocation policy, installation safety, or current app qualification.
The helper is an offline experiment verifier, not a replacement for Rust's
production package verifier. Historical publisher keys are reported, not trusted.
Artifact API metadata authenticates which retained run supplied the bytes; source
attribution is not an independent signed build attestation.

## Reproduce

Use a separate checkout and a downloaded artifact after independently checking
Forgejo source, expiry, size and run identity. Python 3.11+, Bash, GNU tar,
`blake3` and `cryptography` are required. The rehearsal used an isolated venv with
binary wheels only. Neither dependency is added to production jobs.

```sh
python tools/replay_app_artifact.py \
  --archive dist/artifact-replay/artifact.zip --work dist/replay-20406 \
  --source 525aa7824c7a8476aa5ea006499ddd01a8b3c2ea \
  --expected-size 328814609 --epoch 1789675035 \
  --run-id 20406 --artifact-id 34748
python -m unittest discover -s tools/tests -p test_replay_app_artifact.py -v
```

The epoch is the recorded historical runtime production timestamp, fixed for both
replays. Work must be a fresh directory below this checkout's `dist/`. The helper
checks a conservative footprint estimate; it does not impose a filesystem quota.
Large generated artifacts are intentionally uncommitted.

## Smallest next shipping slice

Attach immutable desired-state receipts to app planning: source SHA, architecture,
selected app closure, exact builder/toolchain identity, runtime content root and
package content identities. Validate their reuse in a private nonpublishing native
pilot before restoring continuous publication. This rehearsal supplies the missing
transport/assembly proof, not dependency-aware rebuild proof: the current producer
still compiles the complete runtime/app feature graph. Scheduling needs one active
publication plus one replaceable latest pending desired state, never cancellation
of active publication. Publication needs durable per-object receipts and an atomic
final index promotion so interrupted upload can resume without redoing compilation.
Keep OS promotion manual and macOS workflows disabled but retained.
