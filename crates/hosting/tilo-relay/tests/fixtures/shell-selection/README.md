# Public shell selection fixtures

These are unchanged public test outputs from the real `SessionOwner` producer,
exported by `qos-session-peer/tests/publication.rs` test
`actual_shell_receive_publication_preserves_foreign_intent_and_recovers_baseline_tail`
with `QOS_PUBLICATION_FIXTURE_DIR` set. They were captured from that test's
fixture-export mode for reproducible store tests.

`manifest.json` records public authority/certificates, historical test time,
room/routes, and exact BLAKE3 hashes, lengths, channels and kinds of the signed
Postcard message bodies. `baseline/` is a full room/application checkpoint;
`tail/` is a room delta plus the touched application snapshot, then a room-only
rename; `checkpoint2/` is the next full checkpoint. The public peer exports and
original signed foreign increment permit exact-state convergence assertions.
No private identity keys or live user data are included.

Tests wrap these bodies in synthetic retained transport frames. A zero outer
Envelope signature is not independent authentication: the fixture import models
bytes already admitted through authenticated ingress, with locally pinned
shell authority. The pure helpers still verify signed inner messages and exact
room/application receipts. This fixture does not authorize arbitrary disk bytes.
