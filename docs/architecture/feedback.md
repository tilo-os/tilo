# Feedback

Feedback is an attributed report in a configured Chat channel. It uses the
channel's ordinary permission-checked action transport; it creates neither an
account connection nor a second messaging service. A report's resource is
`chat.feedback:<submission-id>`, beside its `m_feedback_<submission-id>` message.

## Implemented contracts

`tilo-feedback-review` supplies deterministic privacy review. Model proposals
can add redactions but cannot remove the deterministic floor. Invalid spans or
an oversized merged floor fail the review rather than silently losing masks.

The SDK exposes `tilo-feedback` as `ui::feedback`, supplying the portable draft, approved preview, capture boundary
and durable outbox. Drafts begin with no context. The host attaches context only
after an explicit user action. Semantic identifiers stay local; context sent in
a report has synthetic field keys. Credentials and declared personal values are
masked before review output can enter the outbox. Further redaction or an
attachment change requires fresh approval.

A screenshot is a one-shot request for a specific app surface and semantic
revision. A changed or protected surface, unknown sensitive geometry, invalid
pixel bounds or oversized image is refused. The host must classify all text
bounds, treating unclassified text as sensitive. Masking precedes PNG encoding;
only the resulting content-addressed PNG is eligible for the attachment store.
There is no desktop capture or automatic recording buffer.

The outbox uses the host's existing `DurableStore`. Sending waits for local
persistence. Retried reports preserve the same submission identity and payload,
including across restarts. A successful invocation is not delivery: the outbox
requires a receipt from the authenticated destination with matching identity,
destination and payload digest, and persists that receipt before showing Sent.
Failed or pending writes cannot produce success. Pending reports cannot be
evicted to make room.
If the store refuses its first write, the approved payload stays in memory with
its original identity. Retry persists that same payload before any transport
call; a refused write is never described as a durable queued report.

Chat exposes `submit-feedback` on text-channel resources, requiring Comment.
The action validates the channel's workspace, derives attribution from the
invoking envelope and refuses changed payloads or authors reusing an identity.
Concurrent first submissions survive conflicting CRDT root maps; concurrent
retries appear as one feedback message. Immutable report content remains
available separately from the ordinary message. The `triage` report action
requires Write and records attributed status changes, duplicate targets or an
address of an existing task. A task reference grants no access and creates no
external task automatically.
Opening a report reveals its exact message rather than just its channel.
System search indexes the reviewed report text with its typed `chat.feedback`
anchor and the same reveal action, so the result exposes the report’s existing
receipt and permission-checked triage actions. Retried reports have one report
entry; private drafts and captured context are not added to this index.
`read-feedback-receipt` uses Chat's registered semantic reader over the canonical
record at the SDK's stamped committed heads. The request binds to the published
report resource and accepts no caller-selected heads, destination or source.
`read-feedback-status` uses the same canonical basis and View permission to
return that receipt-shaped identity plus the latest 128 attributed triage entries.
Concurrent histories can exceed the per-writer limit; `earlierEntries` explicitly
counts older entries omitted from this bounded status read. It preserves
status changes, duplicate targets and typed task references without creating a
task or granting access to one. The SDK supplies the actual source record stamp;
a returning participant can read this history after reconnecting. As with the
receipt projection, the returned JSON alone is not a durability witness.

## Host integration still required

The SDK provides `feedback_composer::prepare`, a real shared resource component
with the same semantic actions as its themed, localized controls. It always
prepares an isolated ephemeral store and private component: raw draft text never
enters profile records, history or room synchronization. Both attachment choices
start off. Review shows masked issue/context and PNG before explicit approval;
redaction, screenshot removal and retry use published effects. Changed issue text
or consent requires a fresh review. The system-owned `feedback::Composer` freezes
an approved queued payload and resumes a matching pending outbox report after
restart. Hosts call `FeedbackServices::pump` on storage/transport wakes so the
current private presentation updates and announces progress without another
click; its local-state wake belongs to that component's retirement scope.
The native application catalogue and the browser desktop now open the shared
composer through its private preparation path. Its ephemeral draft owner never
enters the desktop's shared records, replication stores or document restoration;
the native mount has no collaboration manager. Closing discards an unapproved
review, while the approved outbox remains owned by the profile. A controller
permits only one mounted editor at a time.

`tilo_desktop::feedback::PrivateFeedback` binds an already-admitted host adapter
to the profile's durable store. Its outbox is a private package key pinned to
the exact destination. Native hosts install it through
`InProcessApps::install_feedback` and pump it on storage/transport wakes, even
while its window is closed. Browser tile hosts have the same custody slot and
wake pump; their launch configuration cannot supply a destination or adapter.
Pending private writes schedule a storage wake independently of draft records;
awaiting a destination receipt does not schedule automatic transport retries.
An unconfigured entry refuses review/send rather than choosing a public room.
The production startup still needs to install its authenticated adapter and
admitted destination; the source acceptance uses isolated private fixtures.

A host must supply its existing durable profile store, an admitted
`Destination { room, provider, channel }`, authenticated action transport,
content storage, and an explicit focused-surface capture adapter. The composer
must show the masked preview and offer description, context/screenshot opt-in,
redaction, approval, durable send/retry status and a link to the resulting report.
Use shared toolkit controls, themed/localized presentation and semantic actions.

The destination receipt adapter must check the **canonical**
`RecordStore::save_status(record)` at the time it reads the canonical document.
Use `chat::feedback::receipt_from_record` with the retained canonical owner,
record id and admitted destination. It checks current durability and the store's
Chat package/room namespace before reading the report. Named records also need
their matching room stamp; Chat's package-primary document uses its actual store
namespace rather than a fabricated record stamp. MemoryOnly, pending saves
and failures are not receipts. The UI's cached Saved indicator is not a receipt
witness: it can describe earlier heads. Retain the authenticated source identity
through this adapter; never derive it from report payloads or URLs. The submitting
host feeds the receipt to `Outbox::confirm_durable_receipt` with that identity.
Persist masked screenshot bytes before allowing its immutable payload to send.

The browser room-action server can serve an explicitly selected record's
registered semantic readers through an exact-target catalogue lookup. Document
operations keep their existing catalogue revision and invocation path. It mounts the compiled app over the host's exact
retained, signature-verified source, qualifies the existing record binding,
and rechecks current certified Read before disclosure and after projection.
Only actual registered Read/Immediate readers with their exact result contract
are admitted; generic effects remain excluded. The SDK-stamped output survives
the response. A governor-created primary uses the empty wire record name and its
internal SDK primary identity; windowless creation receives the actual selected
room rather than inventing a starter workspace.

This read path does not grant document authorship or broaden the room-action
service's existing host-person invocation policy. A browser's existing
ClientLocal Chat action still submits through its certified participant and
normal peer publication. The signed HTTPS fixture retries one reviewed payload,
checks opted-in personal context redaction and current/stale source revisions,
reads a retained receipt, resumes an approved mutation after disconnecting before
publication, retries that same identity once, and reads its attributed triage
through the public status action after restarting the host. It also confirms
that an unpublished local-only report
cannot return a receipt. Host-side document actions retain their existing peer
publication limitation: publish/local cache completion alone is not a durable
commit acknowledgment. A separate writer correction requires qualification.
No anonymous/public-guest submission journey is established by these tests.
The existing browser frame transport signs document frames as `OpClass::Splice`,
which maps to Write under governed admission. The app's Comment requirement alone
does not establish that transport authority. An admitted demo participant and its
actual overrides must be qualified; never infer a broader document grant from
permission to submit a report.

Private component paints propagate declared capture protection through the
existing `SecretPaint` boundary. The browser binds protection to the scene only
when that GPU output actually presents, so an unpublished update cannot clear
protection of old visible pixels. The presented-source capture refuses protected
or unknown protection before copying pixels. This is a protection prerequisite,
not a complete feedback screenshot adapter: complete classified text geometry
and explicit focused-surface consent remain required.

For the qualified registered-read path, invoke `read-feedback-receipt` through the
existing admitted action transport. Check the returned SDK record stamp against the retained Chat
record, and the receipt identity, digest and destination against the outbox.
This projection alone is **not** a durability witness. Accept it only from the
authenticated destination owner's existing durable action service, after that
owner's commit completed. A local app dispatch, a replica's Saved indicator or
caller-supplied JSON must not be promoted to delivery confirmation.

This integration uses the existing public demo deployment. It does not authorize
publishing, changing access policy or connecting personal resources/accounts.

## Focused verification

Run with the configured cargo drop-in:

```sh
cargo test --locked -p tilo-feedback-review --lib
cargo test --locked -p tilo-feedback --lib
cargo test --locked -p tilo-app --lib feedback_composer::tests
cargo test --locked -p shell --lib shell::in_process::feedback_tests::
cargo check --locked -p tilo-browser-runtime --target wasm32-unknown-unknown
cargo test --locked -p chat --lib feedback::
cargo clippy --locked -p tilo-feedback -p chat --all-targets -- -D warnings
cargo check --locked -p shell --tests
```

Before enabling the journey, exercise the mounted native and browser production
paths with a synthetic report, reviewed opt-in context and masked attachment;
force storage/transport failures and restart; then verify one attributed durable
report, truthful success/retry, triage permissions and the linked existing task.
