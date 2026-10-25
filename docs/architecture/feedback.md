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
masked before review output can enter the outbox. The detectors recognise
email addresses, phone numbers, card numbers, IBANs and secrets; a name or
postal address in the description is masked only when it was also declared as
context, so the reporter hides anything else in the preview. Further redaction
or an attachment change requires fresh approval. The report itself is
attributed: its destination stores the reporter's person id.

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
The report's message is read out of the stored submission for each reader
(`chat::feedback::message`), never stored as sentences: the summary, then
each pick as its place, app, what it was and its role, the reporter's note on
it and the texts it showed, with hidden spans as their kind's placeholder
(`‹name›`), then the last actions and, when it was slow, the frame timings.
Its masked picture is the message's image (`report.png`). The report holds
its own reactions field, so members react to it, or +1 it, as to any message;
the timeline shows one message per report. Chat's `read-room` answers the
report as that message, so the Discord bridge mirrors a filed report
(`submit-feedback`) as a post with the picture uploaded beside it, and the
reactions on it as the bot's.
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

Feedback is opened about the window in front. Native desktops offer the
palette action `feedback.open` (`tilo-control actions invoke actions
feedback.open`); the browser desktop offers Give feedback in the Tilo menu when
the page holds a destination. Both name the focused window's app as the
report's `Subject` (`tilo_desktop::feedback::focused`) and bring back the one
open composer rather than a second. Only the app's id and its own display name
are used: tile labels can be window titles and are never read. The composer
shows what the report is about and offers "Include which app this is about"
only while it has a subject; consent adds that one public context field to the
review, and changing the subject discards an earlier review.

A report can also be about one control. `feedback.point` arms the next click
(the desktop says so; Escape lets it go): the click focuses the window under it
without reaching the app, and the shell asks that app over its shell-ui
connection what is at that point (`PointAt`). The app answers from its semantic
tree, the one assistive technology reads, with the deepest control a person
could name (a durable identity or an accessible name): its id, role and
accessible name, never what a field holds (`Pointed`). The subject then names the control too; with consent
its id and role are context and its name is reviewed as the person's own words,
since a row can be named after what they wrote. An app that cannot answer, or
does not in time, leaves the report about its window. Hosts that cannot
take a screenshot (`FeedbackHost::offers_screenshots`) do not show the
screenshot choice; a screenshot requested anyway is refused, not dropped.

A private window has no collaboration route, so its actions are offered to its
own device instead: the native desktop's `device-local` catalogue lists what each
open private window's door publishes and runs an invocation through that door as
the window's person (`tilo-control actions invoke device-local <id>`). Nothing is
published to a room or answered for another device. Typing the issue, including
the app, review, send and retry are the composer's ordinary field edits and
effects, the same ones its controls use.

`tilo_desktop::feedback::PrivateFeedback` binds an already-admitted host adapter
to the profile's durable store. Its outbox is a private package key pinned to
the exact destination. Native hosts install it through
`InProcessApps::install_feedback` and pump it on storage/transport wakes, even
while its window is closed. Browser tile hosts have the same custody slot and
wake pump; their launch configuration cannot supply a destination or adapter.
Pending private writes schedule a storage wake independently of draft records;
awaiting a destination receipt does not schedule automatic transport retries
on a timer, but every storage or transport wake re-sends the same approved
payload until the destination's receipt arrives. Approved reports, Sent ones
included, stay in the private outbox; once it holds 64 reports or 4 MB it
refuses new ones rather than evicting.
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
signed acknowledgment binds the owner, reporter, destination, submission
identity and approved digest. Screenshot receipts remain refused
until an actual owner content store supplies a durable content witness.

### Report a problem: picking parts of the screen

"Report a problem…" (the avatar menu, and the Report problem shortcut,
Super+R; Alt+Shift+R in a browser) dims the screen and lets the person point
at what is wrong. `tilo_desktop::feedback_picker::FeedbackPicker` is the one
overlay for native and browser hosts: the part under the pointer is outlined
with a spring-animated highlight, a click picks it, Shift-click or a dragged
rectangle picks several (up to `report::MAX_PICKS`), the wheel widens to the
enclosing part, Enter or Continue finishes and Escape leaves. Picks come from
the retained widget tree, never from pixels: `ui::pick` walks the same
accessibility nodes the screen reader uses and resolves each to its widget
path (`Name[i]/Child[j]`), role, label, app and, where the window has one, its
record.

`ui::pick::capture` collects each pick's texts and classifies them. A text a
widget took from the app's localization (`AccessNode::interface_name`) is
interface copy and travels as it is; every other text is content, and content
is Personal: the review hides it whole until the person reveals a span. The
report also carries the last 20 actions the person invoked
(`ui::pick::recent_invocations`, structured addresses and outcomes, not
sentences) and, when they tick "This was slow", recent frame timings.

`tilo_feedback::report::ReportDraft::review` runs the deterministic floor
first (credentials, addresses, numbers and the names of the people present
become typed placeholders such as `‹name›`), then, where the page has a model,
one pass through `model_view` that may only propose more spans to hide;
Personal values never reach a model that is not local. The composer shows the
exact outgoing report, each hidden span as a placeholder the person can show
or hide again (credentials stay hidden), a note per pick, an overall note and
"This was slow". Nothing leaves before Send, which queues the reviewed
submission in the same outbox as any other report.

In the browser the picture of each pick is copied from the frame presented
right after the picker closes, its content areas painted over on the device
(`ui::pick::sensitive_areas`; images are masked too), and the crops are stacked
into the one masked screenshot the report carries.

A persistent room keeps the channel reports go to with the room service's
`TILO_ROOMS_FEEDBACK_CHANNEL` (the relay's `--feedback-channel`): the relay
adds it after the room's channels on every start. The web entry names it for
its home workspace with `TILO_WEB_HOME_FEEDBACK` (the channel id, `c_<name>`),
and the page configures that destination after admission without holding up
the desktop; a desktop that mounted first takes the startup up on its next
frame.

The browser's existing `CertifiedPeer.configureFeedback({room, provider, channel})`
selects a destination, before or after the desktop mounts. `WasmPeer` requests admission from
its connected host, matches both roots/devices against retained certificates,
requires the reporter's actual current standing and the room's sandbox, and installs the trusted Rust startup
custody. The helper waits for this acknowledgment and never joins another room.
Sending invokes Chat's `submit-feedback` on the destination owner over the
existing remote-action transport. The owner runs it in an app worker of its own
as the reporter, under the standing the room's policy gives them now, so the
action's `comments` grant decides: a commenter can file a report without any
document Write. The invocation answers only once the owner's durable store holds
the change. Receipt requests are bounded and retries preserve
the approved submission identity; loss of session admission or stale policy fails
closed. Description-only capture is the production default.

Live configuration must name the already-authorized room UUID, exact provider
`record:so.tilo.chat:` and text channel ID, and select the already-certified
reporter's root/device. These values are not present in the public demo's verified
configuration. No destination, enrollment or grant is created by this feature.
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

This read path does not grant document authorship. The room-action service runs
document actions for the host's own person, and for anyone else the room admits
only actions on a `comments` field, each in a worker opened as that person with
their current standing, so the door refuses what their grants do not cover.
A comment filed this way succeeds only once the host acknowledges saving the
change (the session's durability barrier); publishing it, or the worker's copy,
is not a save. The host's own document actions still answer at publication. The signed HTTPS fixture retries one reviewed payload,
checks opted-in personal context redaction and current/stale source revisions,
reads a retained receipt, resumes an approved mutation after disconnecting before
publication, retries that same identity once, and reads its attributed triage
through the public status action after restarting the host. It also confirms
that an unpublished local-only report
cannot return a receipt. A second fixture admits a separate person as Commenter:
they file through the owner, get the signed receipt at once, file again without a
second report, and are refused triage, which needs Write. Raw document frames
still need Write (`OpClass::Splice`); filing never publishes one as the reporter.

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
rechecks that admission. The browser files through the owner's submit action,
which needs only the reporter's comment grant. Missing roles, changed routes and
restricted actions refuse.
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
fields. `NoCapture` reads nothing from any surface: a review carries the description
and, with consent, the subject's app identity; it offers no screenshot.
The canonical service refuses screenshot-bearing acknowledgments until a durable
content witness is integrated; Saved report metadata alone cannot confirm an
attachment.

These are source hooks, not an enabled public guest endpoint. The current
`RoomRecordsLink::view_document`/`RelayRecordsLink` interface carries verified
source bytes and a revision, not a canonical Saved witness or signed feedback
acknowledgment. The enrolled runtime must bind its existing action IO and serve
`CanonicalDestination::receipt` through that owner's authenticated service;
the browser bootstrap must register the resulting Rust startup custody. Do not
reinterpret the current semantic-read projection as this service, inject role
claims from public config, or grant Write to reporters who only comment. Live acceptance still needs the actual existing
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
cargo test --locked -p tilo-desktop --lib feedback::
cargo check --locked -p tilo-browser-runtime --target wasm32-unknown-unknown
cargo test --locked -p chat --lib feedback::
cargo clippy --locked -p tilo-feedback -p tilo-desktop -p chat --all-targets -- -D warnings
cargo check --locked -p shell --tests
```

Before enabling the journey, exercise the mounted native and browser production
paths with a synthetic report, reviewed opt-in context and masked attachment;
force storage/transport failures and restart; then verify one attributed durable
report, truthful success/retry, triage permissions and the linked existing task.
