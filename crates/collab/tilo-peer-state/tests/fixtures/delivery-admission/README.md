# Exact admission fixtures

Frozen on 2026-09-21 from the real StatePeer and Ed25519 admission path. These are test identities only: root seed `[1;32]`, governor `[2;32]`, member `[3;32]`; clock UNIX epoch +2000 seconds; room `aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa`; app `test.app`; record `tab:1`. The original operation is member sequence7 and adds `literal-A` using that member's explicit Automerge actor. The initial document/room history and its original actor bytes were captured once and remain in the fixture; tests never regenerate them.

- `native-v4.postcard`: committed-shape peer basis before this member operation, with normal governance/covered record. No exact-admission index.
- `original-signed-op.postcard`: complete original canonical SignedOp including its signature.
- `original-app-document.postcard`: original canonical NeighbourMsg wrapper around that exact SignedOp.
- `operation-id.hex`: fixed BLAKE3 digest of the complete canonical SignedOp, not its signing preimage or later governor checkpoint.
- `native-v5.postcard`: the same basis after explicit native V5 writer selection and successful canonical admission of sequence7. This is serialized candidate evidence; a fixture does not simulate a host durability barrier.

The capture used a temporary ignored test invoking the existing `seeded` fixture, normal `publish_incremental`, `sign_op`, `receive_from_report` and `durable`; the generator was removed after capture. Normal tests read these literal files, verify original signature and bytes, compare the frozen digest, exercise explicit writer migration and restored duplicate lookup, and refuse malformed V5 envelopes. Changing these bytes requires an intentional fixture review, not an automatic encoder rewrite. The two peer exports were last re-encoded when durable documents stopped recording a per-route wire version; nothing else in them changed. The operation, its wrapper, its digest and the V5 export were re-captured when signed ops began carrying a claim per change: the same payload, artifact, attestation, time and sequence, re-signed over the new envelope by the same member key, admitted on the unchanged V4 basis.

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `native-v4.postcard` | 2184 | `c707b77223e19c220d1c2c90900e76319ab36093f606a26eda826b5efcc9be9e` |
| `native-v5.postcard` | 2398 | `84a66f0c1729d23f01e87bce7b6488ab5751981fa7841d4118e7ce4c984a5893` |
| `operation-id.hex` | 65 | `61601c5559a64fa6a398144198d801e867e79bb53f33a41c221b7b3b685e00c9` |
| `original-app-document.postcard` | 352 | `cf5d81e7060758413e2ec60866bc8133d2d367446f418af36f523d64078ad1fb` |
| `original-signed-op.postcard` | 289 | `270f55e092b59ed5d65d1ca7ebb114a107448aa924f727eeea6b56040117c310` |

Persisted byte fixtures are stored as `.hex` files: two lowercase hexadecimal
digits per byte, separated by spaces, with 16 bytes per LF line. Decode the
text before passing it to the production reader. The decoded bytes are the
original capture, including signatures, compression and malformed inputs;
regenerating them with a newer codec would erase the compatibility regression.
The byte counts and hashes above refer to decoded bytes; `operation-id.hex`
remains the digest itself.
