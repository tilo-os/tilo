# Exact admission fixtures

Frozen on 2026-09-21 from the real StatePeer and Ed25519 admission path. These are test identities only: root seed `[1;32]`, governor `[2;32]`, member `[3;32]`; clock UNIX epoch +2000 seconds; room `aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa`; app `test.app`; record `tab:1`. The original operation is member sequence 1 and adds `literal-A` using that member's explicit Automerge actor. The initial document/room history and its original actor bytes were captured once and remain in the fixture; tests never regenerate them.

- `native-v4.postcard`: committed-shape peer basis before this member operation, with normal governance/covered record. No exact-admission index.
- `original-signed-op.postcard`: complete original canonical SignedOp including its signature.
- `original-app-document.postcard`: original canonical NeighbourMsg wrapper around that exact SignedOp.
- `operation-id.hex`: fixed BLAKE3 digest of the complete canonical SignedOp, not its signing preimage or later governor checkpoint.
- `native-v5.postcard`: the same basis after explicit native V5 writer selection and successful canonical admission of sequence 1. This is serialized candidate evidence; a fixture does not simulate a host durability barrier.

The capture used a temporary ignored test invoking the existing `seeded` fixture, normal `publish_incremental`, `sign_op`, `receive_from_report` and `durable`; the generator was removed after capture. Normal tests read these literal files, verify original signature and bytes, compare the frozen digest, exercise explicit writer migration and restored duplicate lookup, and refuse malformed V5 envelopes. Changing these bytes requires an intentional fixture review, not an automatic encoder rewrite. The two peer exports were last re-encoded when durable documents stopped recording a per-route wire version; nothing else in them changed. The operation, its wrapper, its digest and the V5 export were re-captured when every signed change came to name its action and ops to name the app surface they are decided against: the same edit (`literal-A`) by the same actor on the unchanged V4 basis, now committed as a named field edit, with the same artifact, attestation, time and sequence, signed by the same member key, then admitted. The V5 export's exact index gained its trailing list of compaction marks (none here) when compactions began folding evidence into per-writer marks; its one entry is unchanged. It was split into the settled and the recent entries (here the one entry, settled, and none recent) when governors began keeping the settled part's encoding between commits; only an empty map and the index length changed. Both peer exports were re-encoded when a room came to name its owner by root key instead of pinning one governing device: the base layout version went from 3 to 4 and the pinned device certificate became that device's root key; nothing else changed. All five were re-captured from the `seeded` fixture when certificates stopped carrying the host and read-all flags: the same edit (`literal-A`) by the same member key, now the member's first sequence.

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `native-v4.postcard` | 1715 | `3c11646b5c634336310a3b1dcf95d987e4f1b4a54777bd525b3f3b2326b2e04c` |
| `native-v5.postcard` | 2050 | `1ef822142d74d6d74a774e23fe0dbf9abc5fbaf7dcf3622583bbde090aad1400` |
| `operation-id.hex` | 65 | `a15b0b84d953c5df62b8f8c3cb8cea085888feee097341f563d8b59c39178c02` |
| `original-app-document.postcard` | 404 | `5ccff2d12bb43e336589a6b69a1fdc13a86667211d3256b7dcbdd659ba185c58` |
| `original-signed-op.postcard` | 341 | `a8ce8ed6c7842408fa014c02953ba6d6f74ef9176a886199df3d8a7f5b841523` |

Persisted byte fixtures are stored as `.hex` files: two lowercase hexadecimal
digits per byte, separated by spaces, with 16 bytes per LF line. Decode the
text before passing it to the production reader. The decoded bytes are the
original capture, including signatures, compression and malformed inputs;
regenerating them with a newer codec would erase the compatibility regression.
The byte counts and hashes above refer to decoded bytes; `operation-id.hex`
remains the digest itself.
