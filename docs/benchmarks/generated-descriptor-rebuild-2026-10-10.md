# Generated descriptor invalidation, 2026-10-10

An unchanged second Cargo invocation rebuilt `waproto` 0.7.0 because
`buffa-build` 0.9.2 watched `OUT_DIR/whatsapp.local.desc`. The build script
created that file 57,000,591 ns after Cargo recorded the build's start. Its
write-if-changed guard stopped later repeats, but could not prevent the first
repeat. The focused workflow's discovery followed by exact tests exposed this:
run 91518 took 5m34s for discovery, then 3m36s for the first exact invocation;
later invocations took roughly 1.3–1.8s. This does not imply every single-invocation
CI shard pays a second compilation, or establish compiler-cache misses.

The repair vendors only buffa-build and suppresses a precompiled descriptor's
rerun directive only when its supplied path and canonical path are both within
the caller's actual Cargo OUT_DIR. Missing environment, failed canonicalization,
external files, external symlinks into OUT_DIR, and symlinks escaping OUT_DIR
remain tracked. The registry waproto source is unchanged, including its four
original input directives, source hashes, descriptor transformation, and guards.
Memory-limit environment tracking is unchanged.

Vendoring waproto to switch descriptor APIs would introduce a new binary schema
asset, which repository policy forbids. The selected patch avoids that policy
conflict without changing the binary-content guard or introducing an unused API.

## Qualification

One isolated remote environment, two Cargo jobs, base
`82c220d220e4cf58c378ab082526576ee377b686`:

| Observation | Result |
| --- | --- |
| Original code, first / unchanged second `check -p waproto` | 46.83s / 8.63s; second reports generated descriptor changed after build start |
| Final staged source, first / unchanged second check | 10.58s / 0.64s; first observes the genuine buffa-build source update, second is fresh with no build-script execution |
| Generated Rust and tags, original versus repaired | Byte-identical SHA-256 hashes |
| Final patch, consistent real proto/descriptor message rename | Checksum-identical task-owned waproto source; build script reruns from `src/whatsapp.desc` in 22.65s and generated Rust changes |
| Final patch, deliberately mismatched source hash | Existing hash guard rejects the build with exit 101 |
| Final patch, public file-input consumer, unchanged / edited descriptor | 0.40s fresh / 0.57s rebuild; dirty reason identifies `input.desc`; new message appears in generated Rust |
| Required shell test-target compile check | Passed in 6m49s with two remote Cargo jobs |

The committed `tilo-messaging-protocol` integration target
`generated_descriptor` invokes the public file API through a normal workspace
consumer. It compares every generated output on unchanged input, checks a real
schema rename changes Rust, and captures actual production Cargo directives in
subprocesses. Seven real path cases cover internal output, external input, sibling directory
prefix, missing
OUT_DIR, failed OUT_DIR canonicalization, and both symlink directions. Run it with:

```sh
$CARGO test --locked -p tilo-messaging-protocol --test generated_descriptor -j2
```

All original upstream assertions are retained. Cargo 1.97 panics when directly
selecting the excluded build-only dependency; the existing normal consumer makes
the regression run in exact CI without expanding workspace membership.

The vendor comes from the checksum-verified published archive. Its provenance
records the single exportable source patch. The lock changes only the patched
package's source/checksum and the normal consumer's dev edge; versions and
transitive packages remain unchanged. Root patch changes and `vendor/` are
explicit global inputs in the affected-package selector, so full workspace
selection is expected under the existing policy.
