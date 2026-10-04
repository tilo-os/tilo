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

## Authenticated browser opening

`PeerHandle::open_semantic_live_then(route, publisher, frame, done)` and the WASM
`openSemanticLive(optionsJson, frame)` accept an explicit exact record and the
publisher supplied by an authenticated carrier subscription. Options name
`room`, `app`, `record` and the publisher's device hex. The payload cannot select
that publisher. The callback returns no frame while the session is disconnected,
before current room admission, without a currently valid publisher certificate,
or without local and publisher membership in the current exact-record MLS epoch.
Old cached decryption keys do not restore authority after removal or a governor
change. This reader never opens a new route or requests keys and never queues
semantic frames for later replay.

The reader validates canonical interaction bounds, a nonnil invocation and
nonzero sequence, and the actor against the root that certified the carrier
publisher. Runs and source app peers remain that authenticated publisher's
assertions; neither is a credential or independent proof of app mutation.
Root-signed revocations received during the session are additive and scoped by
root; a foreign root cannot revoke another root's certificate. Revocation
knowledge is only as current as received evidence, not a fresh global attestation.
Verified revocations and fail-closed overflow state persist in channel storage
version3; version2 membership and held edits migrate without requesting access.
A failed save blocks semantic reads and retries the same state. Present but
undecodable saved authority blocks semantic reads and ordinary channel saves for
the session, preserving the original bytes for explicit recovery. Invalid saved
revocation signatures also fail closed. The bounded
revocation store refuses semantic opening if its capacity is exceeded; exhaustion
is an availability limit, not permission to forget prior revocations.

The returned receipt identifies publisher, principal, exact record, current
membership epoch, source sequence/time, execution, peer and canonical interaction.
Browser integration must recheck its active subscription and exact mount generation
after awaiting it, clear on authority/lifetime changes, and bind target geometry
to a genuine presentation. This API does not activate a publisher, a cursor,
or panorama semantics; generic pointer decoding still refuses semantic frames.


A verified browser receipt includes `authorityGeneration`,
`authorityObservedAtMs` and `authorityExpiresAtMs` (nullable). Consumers compare
the generation (both APIs use lossless decimal strings) synchronously against
`peer.semanticGeneration()` before using
a retained receipt, and refuse it at the earliest certificate expiry or known
revocation deadline. Wall-clock rollback before the verification time also
invalidates the receipt. Source event time remains separate. Network authority,
room/session changes and storage readiness invalidate the local generation;
read-only status refreshes do not. These fields grant no new authority and
neither obtain keys nor renew a publisher lifetime.
