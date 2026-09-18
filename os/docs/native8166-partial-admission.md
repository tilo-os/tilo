# Run 8166 standalone binary reuse: proposed admission

Status: opt-in implementation available, retained artifacts not yet admitted.
Existing `validate_partial` continues rejecting reconstructed partials. The producer
accepts `--reconstructed-partial-admission JSON SHA256` only as a separate path. Nothing here labels run 8166 as the original run 7489.

## Retained evidence and exact union

Source: `4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc`.
Producer helpers: `9d7a6b259c9b75b6c5031472bfed05b6461c1893`.
Producer run: 8166, database ID 21554, attempt 1.
Producer reconstruction SHA256:
`79e92a97f1f24ad52466fad5fc574aa9c5629f9ae06af1be2340bbf0c4ba9e04`.

Four receipt byte hashes independently identify the proposed inputs:

| Producer | Count | Receipt SHA256 |
| --- | ---: | --- |
| 7489 original | 17 | `c81b8a9308ef7afb850271b499df49cb7afb162b2df86397e422450df6a084c1` |
| 7489 delta | 5 | `c3bb9e84d824190aa7f4c237254c636b991adaf9b40f79ef89e21c1f2581ce44` |
| 8166 release | 8 | `3bf7b8ea240aeea5992e2242a26ad00fa29c88af6928c3b3e417ddb57ee2cfe9` |
| 8166 Windows | 2 | `a03268cc7b841b916b427674ac5ee52016c2d3aa6a6f407e67bbe033eec0ed10` |

Reading those receipts gives exactly 32 distinct names, equal to frozen
`native.REQUIRED` plus `browser`. There are no differing overlaps. Executing
helper9d's existing pure `derive_recipe` on that inventory produces empty
`release` and `windows` missing-target lists. The thin-app build, hardening,
runtime bundle and app-store phases remain. This check does not validate the
registry payloads or admit the new receipts.

## Opt-in implementation boundary

1. Keep legacy `validate_partial` and its exact old purpose/run checks unchanged.
   Add a separate `validate_reconstructed_partial` entry point. Call it only when
   an independently hash-pinned admission document is explicitly supplied;
   unknown purposes still fail. Do not edit the retained receipt's
   `resume_admitted: false` or its reconstructed purpose.
2. Admission binds the two new receipt byte hashes, source/helper revisions,
   producer run/attempt, canonical reconstruction hash, exact expected binary
   inventories, and the capture-script hashes reviewed for each checkpoint.
   Receipt claims such as `payload_hashes_verified` are evidence, not authority.
3. Re-read exact registry manifest/config identities and mounted metadata; verify
   metadata hash against the approved receipt. Check exact metadata purpose,
   producer identity, architecture, false installable/complete/build-success
   flags and unchanged reconstruction hash. Require metadata inventory equality
   with the receipt and approved inventory. Check the embedded original
   reconstruction itself, not just a matching untrusted hash field.
4. Verify its reconstruction provenance against helper9d and frozen source4583:
   original recipe/driver hashes; reproducible derived recipe/driver with the
   recorded path/seed parameters; helper/producer code hashes; original two
   checkpoint receipt hashes and original 22-name reused inventory. Reconstruct
   the embedded seed union from the independently validated 7489 receipts.
   The frozen source inputs (builder/base/RPM/WPE/epoch) must equal the current
   reconstruction input contract. Reject extra producer checkpoints or altered
   missing-target sets. This is a depth-one chain for this admission, not an
   unrestricted recursive importer.
5. Reuse existing payload checks: exact file set, executable regular files,
   aarch64 ELF header, native file_record secret scan, exact size/hash, no
   symlinks, copied-byte readback, and equality for any overlapping name. Admit
   only the eight release names or two Windows names selected by that receipt.
6. Feed the validated 32-name union to the existing recipe derivation and seed
   context. Record all four original receipts/hashes plus the new admission hash
   in the next reconstruction. Keep the final complete-builder inventory and
   separate consumer reconstruction approval unchanged.

The four-input count fits the existing producer limit of four checkpoints.
Adding the thin shared library is a different contract: its receipt has a
separate purpose, it is not a standalone binary, and it must not be inserted into
this union or renamed to satisfy `native.REQUIRED`.

## Cache integration boundary

Skipping the ten completed standalone targets does not by itself restore the
thin-app Cargo cache. The existing recipe unconditionally runs
`cargo clean --profile thin-app`; a separately admitted cache consumer must
explicitly address that seam, mount the exact matching cache path, and preserve
source/toolchain/profile provenance. Do not remove the clean instruction merely
because binary checkpoints were admitted.

## Focused verification required before enabling

Use mounted registry fixtures to test valid 17+5+8+2 union and actual recipe
selection. Reject missing admission, substituted receipt/hash/run/helper/source,
changed embedded reconstruction/seed union/derived recipe, cross-purpose thin
library receipt, wrong ELF architecture, differing overlap and modified copied
bytes. Confirm the legacy importer still rejects every reconstructed purpose.
The new admission path must be wired into the actual producer with an explicit
CLI option; a standalone unused validator would not save compilation time.
