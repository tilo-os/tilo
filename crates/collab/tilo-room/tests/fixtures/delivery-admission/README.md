# Exact admission fixtures

Frozen on 2026-09-21 from the real StatePeer and Ed25519 admission path. These are test identities only: root seed `[1;32]`, governor `[2;32]`, member `[3;32]`; clock UNIX epoch +2000 seconds; room `aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa`; app `test.app`; record `tab:1`. The original operation is member sequence7 and adds `literal-A` using that member's explicit Automerge actor. The initial document/room history and its original actor bytes were captured once and remain in the fixture; tests never regenerate them.

- `native-v4.postcard`: the peer basis before this member operation, with normal governance/covered record, in the one stored shape. No exact-admission index. (Named for the shape it was captured in.)
- `original-signed-op.postcard`: complete original canonical SignedOp including its signature.
- `original-app-document.postcard`: original canonical NeighbourMsg wrapper around that exact SignedOp.
- `operation-id.hex`: fixed BLAKE3 digest of the complete canonical SignedOp, not its signing preimage or later governor checkpoint.
- `native-v5.postcard`: the same basis after explicit exact-admission selection and successful canonical admission of sequence7, in the one stored shape. This is serialized candidate evidence; a fixture does not simulate a host durability barrier.

The capture used a temporary ignored test invoking the existing `seeded` fixture, normal `publish_incremental`, `sign_op`, `receive_from_report` and `durable`; the generator was removed after capture. Normal tests read these literal files, verify original signature and bytes, compare the frozen digest, exercise explicit writer migration and restored duplicate lookup, and refuse malformed V5 envelopes. Changing these bytes requires an intentional fixture review, not an automatic encoder rewrite. The two peer exports were last re-encoded when stored room state became one shape (version 8, exact evidence second); the rows, governance, documents and signed bytes in them are unchanged.

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `native-v4.postcard` | 2187 | `d4c89bd3ff05c643696acadab6f1e0db73d02cfcbd6977d9cac759010db677d2` |
| `native-v5.postcard` | 2403 | `4c3790ccd8e2b0b9171cdee1da6f331431acac330de873de1524d5b9e1558f25` |
| `operation-id.hex` | 65 | `197d527b60b3f79fef59b7e15ac798eca589dd976ff72e9d9c77088f93b6a121` |
| `original-app-document.postcard` | 351 | `261a80f19dac244483b7f4c488ba3104e03b6ed21b7c488325e859dd75bc4f4e` |
| `original-signed-op.postcard` | 288 | `ae959e85e7380f43a4539265e3be7c209f216b528358e6dcc0cada1b352d67b5` |
