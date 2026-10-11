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
Each report also exports `reply`, using the existing attributed Chat message
action with that report's exact message as its thread parent. It requires the
ordinary message Write permission and grants no new publication authority.

## Host integration

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
The relay supplies read-only feedback admission and acknowledgment on its existing
`RemoteActionFrame` transport. `RelayRecordsLink` carries the existing certified
caller and governor Read policy into `RoomActions`; it uses
`Rooms::resource_view_document` and the retained store, never the app worker's
cache as durability evidence. Selected manifests, files and directory ancestry
must pass durability barriers. Registered Chat projection runs in a confined
worker; only the purpose-specific receipt digest reaches the existing owner
signer. `read-feedback-destination` confirms an existing text channel and
`read-feedback-receipt` can require the exact attributed reporter root. A
signed acknowledgment binds the owner, reporter, destination, publication verb,
submission identity and approved digest. `ReviewedAttachments` stores only a
masked PNG through the existing content store and validates its full bounded
decode, hash and dimensions. The content backend must supply a current durability
barrier; memory, regenerable and unqualified backends cannot attest custody.
`CanonicalDestination::with_attachments` binds the canonical owner's existing
store, checked before and after receipt signing. The reporter's local copy or
saved report metadata is insufficient. Losing the bytes during signing prevents
acknowledgment. This does not install capture, create content transport or grant
access: the production relay/browser/native description-only bindings continue
to refuse screenshot receipts until their actual owner content custody is wired
and qualified. Chat reports declare an exact opening view for the existing
browser/native resource-view path; reading a status does not navigate.

The browser's existing `CertifiedPeer.configureFeedback({room, provider, channel})`
selects a destination before desktop mounting. `WasmPeer` requests admission from
its connected host, matches both roots/devices against retained certificates,
requires actual current role/capabilities, and installs the trusted Rust startup
custody. The helper waits for this acknowledgment and never joins another room.
Sending queues the registered ClientLocal action on the existing Chat record and
uses the peer's ordinary Write publication. It does not invoke a document write
on the host as another person. Receipt requests are bounded and retries preserve
the approved submission identity; loss of session admission or stale policy fails
closed. Description-only capture is the production default.

Live configuration must name the already-authorized room UUID, exact provider
`record:so.tilo.chat:` and text channel ID, and select the already-certified
reporter's root/device. These values are not present in the public demo's verified
configuration. No destination, enrollment or grant is created by this feature.

The remaining image path is missing implementation, independently of that live
configuration. The native SDK's `RecordContent` wrapper preserves its backing
store's current durability result, but a served content lane is not a destination
witness. `DeviceBackend` and the browser `FilesBackend` still lack an attestation
from their storage owner; `DocumentBlobs` exposes presence and reads, not a
durability barrier. The relay's `RoomRecordsLink` exposes retained documents and
current Read policy, but no owner-scoped document-media binding. Complete the
image path through the existing record-content broker and scoped content store,
including owner persistence and current authorization before receipt signing;
do not substitute the reporter's cache or a new upload endpoint. The production
native/browser `NoCapture` bindings also require an explicit focused-surface
producer with inspected privacy bounds and frame identity. The pure masking
permit and private preview are already present; they are not that producer.
Implementing these adapters needs no live capture or enrollment. Exercising them
against an actual person's surface, reporter identity and destination does, and
those selections and permissions remain prerequisites for deployed acceptance.

Native startup reads optional `feedback-destination.json` under the existing
`org.tilo.Shell/apps` private profile. Its closed JSON contains only `room`,
`provider` and `channel`; it contains no role, capability, identity or key. Missing
configuration leaves feedback unconfigured. The compositor binds this selection
to its original `ShellNetwork`. `feedback_custody` reads the committed enrolled
room's governor and checks current selected reporter custody without a legacy
identity fallback. The correlated owner response supplies admission; every use
also compares current native role and capabilities, so a stale answer cannot
survive policy change, revocation, selection change or an uncertain owner.

The producer calls `InProcessApps::install_feedback_startup` and retains the
private outbox. Its Chat publisher opens the existing record inside the
original SDK committed-read bridge and invokes the registered local
`submit-feedback` door as the original application person. The ordinary native
broker model pump commits and publishes the edit without painting or pixels;
no direct peer snapshot or replacement network bypasses that custody. Its
presentation remains ephemeral inside the original checked namespace, so model
service teardown cannot replace the user's saved Chat window view. Startup cannot
create a missing Chat record. Cold restart requires fresh owner admission and
current selected custody again.
`NoCapture` remains mandatory until a durable screenshot content witness exists.
Source qualification does not establish deployed native or browser acceptance.
Tests use isolated certified sessions, the real native broker and retained storage,
including cold application and owner restarts.

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

### Authenticated startup and canonical acknowledgments

`tilo_desktop::feedback::transport` supplies the production adapter and owner
receipt primitive. Enrollment is a non-deserializable host input containing the
already-admitted destination, certified owner and reporter `Caller`s, current
permission context and actual publication requirement. Every send and receipt
rechecks that admission. Browser Splice still requires Write; a Comment action
does not supply it. Missing roles, changed routes and restricted actions refuse.
Read-only admission can confirm a previously durable report through its exact
receipt reader; it cannot resend the report or perform new publication.

`SessionTransport` bridges existing session IO: queue the registered
`submit-feedback` action with the retained provider and reporter, then read a
bounded `SignedReceipt` from the pinned canonical owner. A dispatch acceptance,
ordinary receipt projection or caller-supplied JSON cannot confirm delivery.
The profile stores the destination, owner/reporter public identities and
publication requirement under exclusive custody before transport can run. A
restart restores the same approved outbox; another owner/device/route cannot
silently adopt it. No capability or private signing key is persisted here.

The owner constructs `CanonicalDestination` with its exact retained
`CanonicalRecord`, `chat::feedback::canonical_feedback_projection` and existing
purpose-specific `ReceiptSigner` service. It checks current canonical Saved
status, record namespace, submission digest and attributed reporter before
asking that service to sign. It checks current status/content and signer again
after the callback. Signing requests are opaque and only this validated path
creates them; private keys stay in the signing service. The versioned domain
`tilo-feedback-canonical-receipt-v1` binds the destination, owner, reporter,
publication class and receipt. Clients verify the pinned owner device signature
and exact approved ID/digest/destination before confirming the outbox.

Native startup consumes this custody through
`InProcessApps::install_feedback_startup(authenticated_reporter, startup)`;
another profile person/device and replacement are refused. The browser's Rust
host hook `register_feedback_startup(peer, startup)` requires an existing
certified peer, rejects replacement and is consumed by actual desktop mounting.
Peer close retires unused custody. URL/JSON launch options have no authority
fields. `NoCapture` explicitly supports description-only review until a qualified
focused-surface capture/context adapter is supplied; opt-in capture then refuses.
The canonical service refuses screenshot-bearing acknowledgments until a durable
owner-scoped media reader is bound; Saved report metadata alone cannot confirm
an attachment. The append-only `DocumentBlobs.durability`
query carries a backend's current scoped witness over the existing launch
capability. `DeviceBackend` delegates it without consulting its read cache or
`has()`. Older servers and unqualified backends answer `Unsupported`; neither
that query nor metadata transaction completion qualifies the filesystem daemon
or relay as a durable image owner. Their persistence barriers and receipt
binding remain implementation work coordinated with the document-media owner.

The page's `FilesBackend` reissues an atomic write when reviewed content is
stored again, even if its address already exists in the synchronous mirror.
This retries a failed OPFS write and replaces corrupt bytes without counting
the replacement twice against quota. Completion of that synchronous call
still cannot attest OPFS commit: browser media custody needs fresh persisted
readback and a per-address witness, not mirror presence or a cleared global
write-failure indicator.

The asynchronous content path now awaits that exact write's closed stream,
then reads committed OPFS directly under the existing profile writer lock and
checks the requested hash. Readback bounds allocation before loading a file.
Missing bytes, corruption, oversized content and read failures cannot attest
custody, even when the synchronous mirror still holds the approved bytes.
`ReviewedAttachments::persist_async` and `verify_async` use this owner path;
the synchronous browser witness remains unsupported. The composer's existing
submit action now retains an immutable approval while the exact image write
is pending. Existing private-storage wakes poll this owned future without
holding a UI borrow across an await. Failed custody permits retry of the same
image, identity and payload; it cannot queue a report. Successful custody
rechecks current host admission before the ordinary outbox write. Transport
still waits for that separate outbox write's durable acknowledgment. Closing
the presentation discards its raw review without changing an already approved
pending payload. Masked PNG clones share immutable bytes.

`FocusedCapture` implements the existing capture interface over a host-supplied
focused renderer source. Construction starts no readback. An explicit request
checks complete classified geometry and protection before reading exactly one
app frame, then rechecks focus, revision and classification before masking.
Unclassified text is sensitive. Missing cached geometry, secret/protected
frames and changed inspections fail closed. Context also checks protection and
frame identity around its classified semantic read. This contract does not
provide the renderer's inspection/readback producer: live startup retains
`NoCapture` until the existing native/browser renderer can supply that complete
presented-frame contract. No synthetic provider is installed on the live path.
Classification and masking work are bounded before readback: at most 8,192
regions and eight million masked pixel visits, including overlaps. A producer
may conservatively merge regions; the consumer refuses excessive work rather
than acquiring pixels and blocking on repeated overlapping masks.

These are source hooks, not an enabled public guest endpoint. The current
`RoomRecordsLink::view_document`/`RelayRecordsLink` interface carries verified
source bytes and a revision, not a canonical Saved witness or signed feedback
acknowledgment. The enrolled runtime must bind its existing action IO and serve
`CanonicalDestination::receipt` through that owner's authenticated service;
the browser bootstrap must register the resulting Rust startup custody. Do not
reinterpret the current semantic-read projection as this service, inject role
claims from public config, broaden host-person invocation policy or grant Write
to make guest Splice pass. Live acceptance still needs the actual existing
destination, permitted reporter role and this owner-service binding.

The adapter's headless fixture dispatches the real Chat action, persists through
filesystem-backed canonical/profile stores, loses its reply, retries the same
reviewed payload across separate owner processes and reads one attributed
durable report after another restart. Negative cases cover owner/reporter
substitution, unsigned/projection acknowledgments, current undurable edits,
destination restrictions, role loss and startup replacement. This is source
qualification, not public guest or attachment-delivery qualification.

Run with the configured cargo drop-in:

```sh
cargo test --locked -p tilo-feedback-review --lib
cargo test --locked -p tilo-feedback --lib
cargo test --locked -p tilo-app --lib feedback_composer::tests
cargo test --locked -p shell --lib shell::in_process::feedback_tests::
cargo test --locked -p tilo-desktop --lib feedback::transport::tests::
cargo check --locked -p tilo-browser-runtime --target wasm32-unknown-unknown
cargo test --locked -p chat --lib feedback::
cargo clippy --locked -p tilo-feedback -p tilo-desktop -p chat --all-targets -- -D warnings
cargo check --locked -p shell --tests
```

Before enabling the journey, exercise the mounted native and browser production
paths with a synthetic report, reviewed opt-in context and masked attachment;
force storage/transport failures and restart; then verify one attributed durable
report, truthful success/retry, triage permissions and the linked existing task.
