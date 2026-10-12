# Exact admission fixtures

Frozen on 2026-09-21 from the real StatePeer and Ed25519 admission path. These are test identities only: root seed `[1;32]`, governor `[2;32]`, member `[3;32]`; clock UNIX epoch +2000 seconds; room `aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa`; app `test.app`; record `tab:1`. The original operation is member sequence7 and adds `literal-A` using that member's explicit Automerge actor. The initial document/room history and its original actor bytes were captured once and remain in the fixture; tests never regenerate them.

- `native-v4.postcard`: committed-shape peer basis before this member operation, with normal governance/covered record. No exact-admission index.
- `original-signed-op.postcard`: complete original canonical SignedOp including its signature.
- `original-app-document.postcard`: original canonical NeighbourMsg wrapper around that exact SignedOp.
- `operation-id.hex`: fixed BLAKE3 digest of the complete canonical SignedOp, not its signing preimage or later governor checkpoint.
- `native-v5.postcard`: the same basis after explicit native V5 writer selection and successful canonical admission of sequence7. This is serialized candidate evidence; a fixture does not simulate a host durability barrier.

The capture used a temporary ignored test invoking the existing `seeded` fixture, normal `publish_incremental`, `sign_op`, `receive_from_report` and `durable`; the generator was removed after capture. Normal tests read these literal files, verify original signature and bytes, compare the frozen digest, exercise explicit writer migration and restored duplicate lookup, and refuse malformed V5 envelopes. Changing these bytes requires an intentional fixture review, not an automatic encoder rewrite. The two peer exports were last re-encoded when durable documents stopped recording a per-route wire version; nothing else in them changed. The operation, its wrapper, its digest and the V5 export were re-captured when every signed change came to name its action and ops to name the app surface they are decided against: the same edit (`literal-A`) by the same actor on the unchanged V4 basis, now committed as a named field edit, with the same artifact, attestation, time and sequence, signed by the same member key, then admitted.

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `native-v4.postcard` | 2184 | `c707b77223e19c220d1c2c90900e76319ab36093f606a26eda826b5efcc9be9e` |
| `native-v5.postcard` | 2582 | `9f3a17d231dae63ff77ad071e0f9adb770798ae1d6ce0e24e96db8c7be53bf84` |
| `operation-id.hex` | 65 | `bb0b0e6678b443040059afd58d5c53de347735185bdc97426f29009ffeed5bde` |
| `original-app-document.postcard` | 531 | `d9a9c371bfa76ca35c89bcf368039422a22d0fe285eb5e007d47e746b0e35d7f` |
| `original-signed-op.postcard` | 468 | `087ec62d0403596e060846196375d003459ccd2eb463dea1af24e8fc2fcc32c4` |

Persisted byte fixtures are stored as `.hex` files: two lowercase hexadecimal
digits per byte, separated by spaces, with 16 bytes per LF line. Decode the
text before passing it to the production reader. The decoded bytes are the
original capture, including signatures, compression and malformed inputs;
regenerating them with a newer codec would erase the compatibility regression.
The byte counts and hashes above refer to decoded bytes; `operation-id.hex`
remains the digest itself.
