# rig-core 0.44.0

Source: the checksum-verified crates.io `rig-core-0.44.0.crate` from
https://github.com/0xPlaygrounds/rig, upstream revision
`2d1071e0e0c602f994e9d6cfd8cf5faea6f53650` (tag `v0.44.0`).
Archive SHA-256:
`6c60db6e3da7681abbdfa9637c1a76dbd5a713f935f0a6753d72c5d2770ed4c6`.
The upstream MIT licence is preserved byte-for-byte in `LICENSE`.

Upstream checked on 2026-10-15: the latest release is 0.44.0; Git HEAD
`45a1f0178aea6a6dbccd7f653b39cfe751912e20` still overwrites Chat replay
names and keeps only the first normalized name. No newer fixed release or
HEAD implementation was available.

Local runtime changes are in the Chat decoder, completion writer and stream-event contract:

- Accumulate function/custom tool-name fragments independently per open Chat
  call. Empty names do nothing; a restatement equal to the accumulated name
  does not duplicate it. Other nonempty fragments append verbatim. No declared
  tool names, prefix matching or completion guesses participate.
- Preserve the accumulated name in the native call item after ordinary field
  merging. The Chat raw-document reassembler also uses the same name helper
  when rebuilding replay history; otherwise late empty names overwrite the
  completed name in `CompletionResponse::raw`. Generic DTO merging and other
  providers remain unchanged.
- Give the normalized call writer its complete name only at call closure.
  A crate-private `Out::chat_argument_preview(index)` opens an unnamed
  presentation-only Start and streams argument fragments immediately. No
  provisional name is exposed or guessed; End carries the assembled name.
  Other providers keep their first-name-only writer behavior.
- Permit unnamed tool Starts in the stream-event transcript validator and
  document that contract. Such previews are not executable calls. The gateway
  already accepts an empty opening name, coalesces preview arguments, and
  uses the final name only after a successful provider finish. Its existing
  256-byte coalescing threshold still applies to previews without an id.
  Gated HTTP tests withhold the terminal frame until a preview is observed,
  proving liveness rather than merely event order in a buffered reply.

Wire regression coverage lives in the gateway's `rig_stream_tests.rs` and in
this crate's Chat tests. See `vendor/PATCHES.md`. The manifests and dependencies
are unchanged. The archive's standalone `Cargo.lock` and Cargo cache marker
are omitted; the runtime uses the repository's committed workspace lock.

Remove this patch when upstream ships equivalent Chat-name accumulation and
consistent normalized/replay/event names. Rig remains the sole wire decoder.
