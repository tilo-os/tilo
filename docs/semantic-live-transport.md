# Semantic LIVE transport primitives

Semantic actions do not imply mouse movement. `OpsFrame::Semantic` carries a
version-1 envelope on the existing ordered, encrypted record stream. It names
the exact record, host-issued execution UUID, source peer, sequence and source
wall-clock timestamp, plus bounded canonical `SemanticInteraction` JSON. The
wire codec checks envelope bounds and JSON shape; an authenticated consumer
must validate the action schema before use. This is not a second action door.

The variant is appended to the existing postcard enum. Old presence and preview
encodings retain their tags; old receivers reject the unknown variant. The
legacy pointer store and generic JavaScript LIVE decoder deliberately refuse
semantic envelopes. An envelope cannot create a cursor, change focus or extend
a pointer's idle lifetime. No semantic publisher or visual consumer is enabled
by these primitives alone.

`SealedLiveFrame::open_in_record(room, record, open)` checks the explicit record
digest and its canonical room/app/record channel before decryption. The browser
peer uses it with the exact route whose cipher it selected. A record claim
encrypted with some other held record key is refused.

`ChannelKeyAccess::seal_existing_live` seals under an already-held current key
for an exact room/channel. Unlike `access`, it never creates a channel, requests
keys, or changes a removed member's rejoin state. Missing, waiting, removed and
wrong-room keys return errors. The caller must independently validate current
publisher admission and the source record route.

## Integration boundary

An already-certified native `RelayCoeditSession` can use its existing
`live_join` and `publish_live` APIs. This does not require a new browser publisher
grant. A broker adapter must bind the source to current room enrollment, emit
only actual invocation lifecycle updates, and seal only with existing keys.
No invocation retry, observer refresh, query, camera movement or heartbeat
constitutes new semantic work.

A consumer must bind the transport-stamped publisher to its current valid,
non-revoked certificate, membership lifetime and principal; validate the
canonical interaction; enforce invocation sequence and terminal-state rules;
and expire missing/disconnected activity. Execution UUIDs are correlation under
that publisher, not credentials or replacement device identities. The source
peer, actual app/record/resource and presentation generation must remain
together when resolving SDK locus geometry. Missing geometry stays missing.

The stock standalone MCP endpoint currently selects its profile's private room.
Joined-room live transport is a separate audience. Integration needs an
explicit, currently admitted room binding; it must not share private-room keys
or issue new membership to make a demo work. These primitives change neither
endpoint scope nor room admission.

The [panorama contract](panorama-capture.md) remains read-only and allowlisted.
Its current receipts describe genuine LIVE pointers, not semantic envelopes.
Authenticated semantic receipt/locus integration and the two-run Writer
acceptance case are still required before a director can call such markers
real agent work. Synthetic codec, key-lifecycle and pointer-isolation tests
establish no real-model activity, concurrency or frame-rate result.
