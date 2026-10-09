# Messaging integration research

Current source status (2026-10-09): private Messages, shared Contacts routing,
account-scoped encrypted history and the native Tilo transport are on main.
Default native startup invokes Tilo, Signal and SMS enrollment paths with original
protected account review. Signal linking and text delivery are fixture-qualified,
not real-account qualified. Its production publication restriction remains;
accepted installed-runtime and live-account qualification are still prerequisites.
WhatsApp transport/encrypted-store adapters are present, but default startup and
first-account linking were reverted with #3468; the existing reland #3526 is pending.
No real WhatsApp or Signal linking, sending or receiving is claimed here.
Direct text is the qualified first scope; media, calls and external group UI
remain separate work.
The inventory and feasibility sections below preserve the initial research. Their
base-specific findings are historical; they do not override the subsequent
accepted product direction or imply that live accounts are configured.

## Pinned Signal compatibility acceptance

signal-cli 0.14.8 `listAccounts` returns only `{number}` rows. Enrollment and
sender/receiver activation must separately query `listIdentities` with both
`account` and `number` set to the explicitly selected owned account. That local
query yields the canonical `uuid`, number and verified identity key; the number
must appear exactly once in `listAccounts`. No `trust` or public `getUserStatus`
lookup establishes account ownership. The original credential authority is
rechecked around pairing completion and again at protected acceptance.

Sources: [ListAccountsCommand](https://github.com/AsamK/signal-cli/blob/v0.14.8/src/main/java/org/asamk/signal/commands/ListAccountsCommand.java),
[ListIdentitiesCommand](https://github.com/AsamK/signal-cli/blob/v0.14.8/src/main/java/org/asamk/signal/commands/ListIdentitiesCommand.java),
[ManagerImpl](https://github.com/AsamK/signal-cli/blob/v0.14.8/lib/src/main/java/org/asamk/signal/manager/internal/ManagerImpl.java),
[SignalAccount local self identity initialization](https://github.com/AsamK/signal-cli/blob/v0.14.8/lib/src/main/java/org/asamk/signal/manager/storage/SignalAccount.java).

Only direct plain text is projected. Sync, edit, story, call, receipt and typing
payloads are not flattened into a concurrent `dataMessage`; supported receipts
retain their separate journal-correlated path. Media, groups, disappearing text
and linked-device sent-history synchronization need their own semantics.
An interrupted unpublished enrollment offers an explicit return to account
selection, clears its private projection and retires the old owned attempt.
Starting again still requires confirmation and fresh protected review. Published
accounts, pending reviews and uncertain message sends cannot use this reset.
These are source and nonsecret fixture acceptance limits; accepted-runtime and
real-account qualification remain outstanding under the publication restriction.

## Verified repository building blocks

The inspected base is `aad1bd2997` on main, in an isolated task worktree. The root
checkout has unrelated work; it was not edited. Current Chat is a resource app
with workspace-scoped message state. Its participant identity is not a Signal,
WhatsApp or telephone identity. Contacts exposes address books and UUID contacts,
with independently identified repeatable phone and email fields. A name or phone
field does not prove account ownership or service membership.

The resource framework already supports private person-owned records through
`ViewCx::person` / `SurfaceCx::person`, with enrolled-device synchronization and
no collaborator invitation path. Tube's watch history exercises that production
path. This is an available isolation primitive, not a decision that all future
messaging records must use it. The record access policy must match each actual
conversation and sharing intent.

`accounts::PersonAccounts` provides canonical account metadata, separate from
secret storage. Account IDs, service types and enabled state can be reused rather
than creating a second account registry. `SecretVault` / `SecretStore` and the
framework secret service keep credentials separate. Shared Dropdown, TextInput,
Button and navigation widgets provide keyboard and accessibility behavior.
Typed device-only effects expose operations to automation as well as controls.
`bounded-command` provides deadlines, bounded output and cancellation of only
processes it owns; it is not authorization for a sandboxed app to execute a
protocol daemon or gain ambient network access.

## Contacts, identity and first-resource journey: read-only findings

These findings describe the inspected checkout, not an agreed product split.

- Contacts' `Contact` is a UUID-addressed person **or organization**, not an
  authenticated principal. Phone/email rows have their own UUIDs. There are no
  typed WhatsApp JIDs, Signal ACIs, verified Tilo root-key links, verification
  provenance, or number reassignment lifecycle fields in that model.
- Duplicate review strips ASCII nondigits from phone strings. That is a
  formatting heuristic, not country-aware E.164 parsing or ownership proof.
  Name/email/phone matches produce review suggestions; a destructive merge is
  separately confirmed. Reuse review behavior, never its heuristic as routing
  or permission authority. Shared household numbers and recycled numbers remain
  identity ambiguities the current schema does not resolve.
- Contacts publishes `contacts.book` and `contacts.contact` resources, lookup
  by UUID, search entries, open/patch/favorite/delete actions and reviewed import.
  It is not a framework `SharedRecord` with a cross-app typed contact resolver.
  A consumer needs the actual book/record context as well as the contact UUID;
  a displayed resource label or bare contact UUID is not a universal address.
- Tilo's authenticated human identity is an Ed25519 root public key. Root-signed
  device certificates, governed admission and revocation provide device proof.
  `UserIdentity.label` is explicitly advisory. These prove Tilo authority, not
  ownership of a WhatsApp identity or a telephone number. The inspected Contacts
  model has no verified bridge to them.
- Accounts are sender configuration, separate from recipients: `PersonAccounts`
  is owned by Settings, with Mail currently the sole additional editor. Other
  apps can read it through `shared_person`; device credentials remain separate.
  Messaging setup needs a sanctioned editor/provider integration, not direct
  file edits or a parallel accounts database.
- Conversation membership, Contacts groups and resource permissions are
  different authorities. Contacts groups are user-visible labels. Workspace
  joining uses signed invitations, recipient-root binding or explicit one-use
  bearer admission, device proof and pinned transport authority. The capability
  system evaluates subject × scope × verb, including app/resource restrictions;
  membership in an external group provides none of that evidence automatically.
- File sharing already supports person-bound record grants and endpoint-bound
  bearer links. `NamedShareEnvelopeV1` signs the host endpoint and recipient;
  its durable inbox checks the certified human. `FileShareLinkV2` carries a
  routed, issuer-bound link with a secret fragment. `ShareSession::authorize`
  checks actor, record, certification, current grant, revocation and required
  view/edit permission on each operation. A copied link is not whole-workspace
  membership, and revocation is enforced by the authority.

For the possible journey “an existing group shares its first Tilo resource,” the
reusable starting point is an explicit **record-scoped** share through existing
FS grants/links, without prematurely granting the whole group a workspace. The
product discussion still has to decide where this is presented, who is invited,
which recipients have verified Tilo identities, how unknown recipients redeem,
and whether a persistent shared place is created later. This is a capability
available in the stack, not a settled UI workflow. Gaps include a typed cross-app
contact/endpoint resolver, user-approved identity-link evidence, group-to-grant
review, join/leave and resource-revocation behavior, and a deliberate transition
from record sharing to broader workspace collaboration. No speculative schema,
automatic group ACL mirroring or identity merge has been implemented.

## Protocol feasibility checked 2026-10-05

### WhatsApp

[whatsapp-rust](https://github.com/oxidezap/whatsapp-rust) is an MIT-licensed,
unofficial Rust WhatsApp Web client. Its upstream README describes QR and pairing
code authentication, encrypted direct and group messages, media, receipts and
history, with replaceable storage and runtime components. Upstream currently
advertises version 0.7. At the initial inventory this was an integration
candidate; the pinned SDK adapter and its encrypted persistence are described
below. Its unofficial status and account compatibility must be visible during
setup.

Before integration, pin and audit a release and its transitive licenses, crypto
versions, session persistence and revocation behavior. Tilo's direct crypto
generation must stay consistent with Iroh. A SQLite backend is not by itself a
credential encryption policy. Preserve WhatsApp's canonical recipient IDs and
protocol-specific trust state instead of deriving identity from a displayed
phone number or profile name.

### Signal

The upstream [signal-cli JSON-RPC manual](https://github.com/AsamK/signal-cli/blob/master/man/signal-cli-jsonrpc.5.adoc)
describes line-delimited requests with unique correlation IDs, receive
notifications, and a multi-account mode requiring an explicit account parameter.
Phone-numberless accounts use their ACI; linking results can have a null number.
A send response contains a timestamp. It does not establish recipient delivery.
Manual receive subscriptions wrap the envelope inside a result object. HTTP
receive replay retains only 1000 events in memory and may replay retained events
when the requested cursor is unknown, so it cannot substitute for durable local
deduplication or complete history recovery. Linking uses a temporary URI and a
separate completion operation.

An adapter should support both account forms, correlate responses exactly, check
account identity on events, and process notifications while a request is pending.
An interrupted send must be reconciled before retry; a JSON-RPC request ID is not
proof of protocol-level send idempotency. Use a broker-owned fixed endpoint with
a narrow typed interface, not a user-supplied arbitrary socket or command.

Upstream [signal-cli](https://github.com/AsamK/signal-cli) is unofficial and GPLv3.
Distribution review includes its Java/native dependencies and corresponding
source obligations. No code has been copied, process launched, account linked,
or security/trust setting changed. Version upkeep and packaging across Tilo's
architectures remain acceptance work.

### SMS

[ModemManager Messaging](https://www.freedesktop.org/software/ModemManager/doc/1.4.0/ModemManager/gdbus-org.freedesktop.ModemManager1.Modem.Messaging.html)
creates a message object and reports added objects, including incomplete incoming
multipart messages. The adapter must inspect message state before presenting a
complete incoming message. The [SMS interface](https://www.freedesktop.org/software/ModemManager/doc/1.24.2/ModemManager/gdbus-org.freedesktop.ModemManager1.Sms.html)
queues a message when Send is called; delivery reports are distinct properties.
Creation or a successful Send call alone is not recipient delivery. Long texts
may be split by the modem. A real local modem/SIM or an authenticated phone
companion is required; neither is configured for this task. Preserve modem and
SIM/subscription identity on every operation. SMS is not end-to-end encrypted.
An Android companion also needs the platform's
[default-handler permission workflow](https://developer.android.com/guide/topics/permissions/default-handlers).

## Safeguards independent of the product split

A route must identify service, sending account and canonical recipient together.
An explicit contact link must retain address-book resource identity plus contact
UUID. Never merge identities by name, formatted telephone number or a received
profile. Keep multiple accounts and multiple contact fields distinct.

Route changes must not move an unsent draft or queued operation implicitly.
Freeze the complete destination and account authorization generation at submission.
Never silently fall back from encrypted transport to SMS. Account revocation,
unknown recipients and unsupported attachments refuse submission. History retains
the actual route used even when today's selected route changes.

External sends require device-local durable ownership, operation IDs and protocol
IDs. A replicated record is not an outbox every device may drain. Persist the
submission boundary before invoking an adapter, and treat interruption after that
boundary as uncertain. Do not retry blindly. Distinguish accepted, acknowledged,
delivered, read, failed and uncertain using actual protocol evidence. Incoming
and receipt deduplication includes account and protocol identity. Undo of a local
record does not promise remote unsend. Secret keys, pairing URIs and message bodies
must not appear in diagnostics or shared workspace records.

These are requirements for future production-path tests, not tests already passed:
wrong-account responses; revoked linking; numberless Signal accounts; duplicate
notifications; partial SMS messages; route switching with unsent drafts; crashes
around submission; uncertain sends; receipt ordering; device ownership transfer;
and isolation from unauthorized collaborators. Live acceptance requires separately
authorized accounts and devices. No real messages have been sent.

## Design discussion and remaining work

The [ZapFast reference](https://zapfast.rocks/) informs visual investigation only.
No source, image, sound or other asset was copied. Independently reproduce useful
interactions and retain compatible asset licenses. Aim for comfortable legibility,
modern visual quality, accessibility and performance on older devices; the final
navigation and conversation organization depend on the user's ongoing discussion.

Resolve the product relationship and concrete journeys before implementing app
structure or resource scope. Then implement the approved interfaces, Contacts
integration, explicit route selection, account setup, private/shared policies,
broker boundaries, real adapters, durable send/receive processing and honest
availability states. Native Tilo resources may belong in either product; no
compatibility-only or personal-versus-work boundary has been settled.

## Implemented backend milestone

The current priority is Tilo users who configure external messaging accounts.
The product split and UI/history model remain open; an elaborate non-Tilo group
onboarding flow is not a prerequisite for protocol work.

`tilo-messaging-protocol` encodes explicit-account single-recipient Signal
JSON-RPC sends, checks exact response IDs, and decodes direct/subscription text
notifications with account-scoped, parsed sender UUIDs. It refuses unsupported
rich, group or ephemeral content instead of silently projecting it into permanent
plain text. Its stream adapter bounds frame allocation, preserves interleaved
incoming events, and requires a journal before any write. Interruptions and
incorrect correlation retire the stream and report uncertainty, never delivery.

The production encrypted journal reuses `tilo-secret-vault`, `private-fs` and
`tilo-advisory-lock`. A host-supplied vault key seals exact route/payload and
service-specific acknowledgment references. It offers no plaintext key fallback.
An exclusive lock prevents concurrent writers; atomic publication includes file
and directory durability. Reopening interrupted reservations marks them uncertain;
operation IDs are never automatically evicted/reused. Wrong device, account,
linking generation, encryption key or corrupt ciphertext fails without resetting
existing operations. A failed persistence call blocks further writes until reopen.
The host must protect the directory and key from unauthorized rollback/deletion;
AEAD authenticity alone is not rollback detection or a backup-restore policy.

The checked Signal send entry point reads the canonical `AccountStore`, verifies
service/account/device/link generation and live linked/revoked/unavailable state,
then journals and writes. It never uses an active/default-account fallback. A contributed workspace account
requires a separate credential delegation; store visibility alone is refused.
The broker must supply the binding from its trusted adapter/device authority,
perform its capability checks, and hold the relevant authority during submission;
UI action parameters cannot establish that authority. Account providers still need
Settings/editor integration. No accounts are inferred from contact names.

WhatsApp contracts distinguish message-class server ACKs from recipient receipts,
refuse wrong account/chat/message IDs, retain LID versus phone-JID boundaries,
and avoid converting self-read or feature-incapable receipts into delivery.
They are independently based on the upstream
[ServerAck event](https://github.com/oxidezap/whatsapp-rust/blob/main/wacore/src/types/events.rs)
and [receipt semantics](https://github.com/oxidezap/whatsapp-rust/blob/main/wacore/src/types/presence.rs).
The [send implementation](https://github.com/oxidezap/whatsapp-rust/blob/main/src/client/messaging.rs)
exposes controlled stanza/message IDs. SDK integration must pin a release, keep
keys under host custody, disable plaintext debug taps/PII logging and optional
TLS-bypass features, and audit chosen storage/runtime features. The pinned SDK
binding below is not yet a fully provisioned, functioning WhatsApp client.

SMS contracts prepare explicit international text recipients and delivery-report
requests, retaining numbers exactly rather than guessing/normalizing. Raw
`ModemManager` [state/PDU values](https://github.com/linux-mobile-broadband/ModemManager/blob/main/include/ModemManager-enums.h)
keep incomplete multipart reception, network submission and delivery reports
separate. A report's message reference alone wraps/reuses and cannot establish
which historical message was delivered. A real adapter must pin modem and actual
SIM identity before creation and again before send; a SIM switch refuses the send.
D-Bus message bodies, not argv, carry message text. Short codes/national-format
numbers, carrier report correlation and phone-companion support remain explicit
unsupported cases, not fallback routes. No modem/SIM was accessed for this work.

The backend tests execute the real encrypted filesystem/journal and checked
routing-to-stream composition, including restart duplicate refusal. All accounts,
keys and streams are fixtures. Remaining runtime work: capability-controlled
broker/endpoint provisioning, external SDK/D-Bus bindings and linking, reconciliation/receipts, durable
incoming deduplication and complete rich/group/ephemeral semantics. No app receives
ambient socket or network access. Authorized live acceptance and approved product
interfaces are still required; no account was linked or real message sent.

The native Signal runtime now consumes an authenticated Unix stream provided
by the supervisor, retains its encrypted journal and applies one total monotonic
I/O deadline across partial writes and frame reads. Its private receive buffer
wipes on drop. A timeout after reservation poisons the connection, persists
uncertainty and refuses another send; reconnecting does not permit replay of
the operation ID. Real Unix socket tests exercise requests, interleaved events,
responses, deadline expiry, revocation refusal and encrypted restart handling.
They use a fixture peer, not a logged-in Signal service.

The host explicitly binds the canonical Tilo account ID to a verified Signal
phone number or ACI. Wire requests/notifications use that Signal identity;
the frozen route, journal and incoming deduplication identity use Tilo's account
ID. Changing this mapping requires a new host linking generation. It is never
inferred from a display name or a Contacts heuristic.

The runtime OS-keychain path uses `tilo-secrets` through `tilo-secret-vault`
to look up an existing messaging-journal key. It refuses non-OS custody,
missing/locked keys and wrong store scope; it does not create a persistent grant,
provision a key or fall back to local files. Tests use an explicitly provisioned
isolated memory store and verify that losing the key leaves ciphertext unchanged.
Actual OS-keychain acceptance was not executed. `signal-cli` is not installed
on the laptop, so live daemon/version/linking acceptance is also unexecuted.
No personal account login or live send was attempted.

The native WhatsApp binding now calls the pinned MIT `whatsapp-rust` 0.7.0
`send_message_with_options` API with an exact stanza ID. It consumes an already
linked, account-owned SDK client supplied by the supervisor; it does not
instantiate default plaintext SQLite storage, HTTP/WebSocket endpoints or a
login. Default SDK features are disabled; only its Tokio runtime is enabled.
PII tracing, plaintext debug taps, TLS/certificate bypasses and plugin hooks are
not enabled. The library enables this binding by default so ordinary affected
package CI compiles and tests it; codec-only callers may opt out.

The binding checks canonical routing/current link authority and actual SDK
connected/logged-in state, freezes exact direct-text SDK inputs in the encrypted
journal, calls the real SDK once within a deadline and correlates its returned
recipient/stanza ID. Successful SDK write completion persists `Submitted`, never
`Acknowledged` or delivered. Errors/timeouts retire the send binding and persist
uncertainty. Server ACK projection uses the actual SDK event type and the existing
account/chat/class checks. Durable server/recipient receipt reconciliation is not
yet implemented. The host must prevent a linked SDK client's authority/account
from changing during the operation; editable account config cannot prove it.

SDK-enabled tests construct the actual unlinked SDK with a memory-only fixture
backend and network-forbidding HTTP/transport implementations, proving no key
grant, journal reservation or transport call occurs on an unavailable send.
They also exercise real typed JID/message/options and server-ACK projection,
and durable `Submitted` versus acknowledgment/restart semantics. They do not
exercise a logged-in encrypted WhatsApp exchange. A host service with encrypted
SDK identity/ratchet storage, linking and durable receive processing remains
unimplemented; this is a real send API binding, not full WhatsApp support.

Dependency resolution used a precise update of the single new consumer:
`$CARGO update -p tilo-messaging-protocol --precise 0.1.0`. The pinned SDK's
minimums required updates to async-trait, bytes, thiserror/thiserror-impl, Tokio
and zlib-rs; remaining existing versions were retained. Hakari regenerated the
native feature union. No lockfile was deleted or regenerated from scratch.


## Encrypted WhatsApp SDK state (implemented)

The SDK backend now implements its actual SignalStore, AppSyncStore,
ProtocolStore, DeviceStore and MsgSecretStore interfaces over one private,
account/device/link-generation-bound authenticated log. SDK identity, noise,
ratchet, prekey, message-secret, retry and pending-inbound state is encrypted
through the existing secret vault; production lookup requires an existing
OS-keychain key and never provisions a key or falls back to plaintext storage.
No upstream SQLite backend or second crypto implementation is introduced.

Writes run on the blocking pool, encode only changed rows, share one exclusive
transaction lock, and flush before publishing results. Batch writes, token
field merges and retry consumption are atomic. Compaction atomically replaces
an encrypted snapshot. Quotas fail closed rather than discarding ratchets;
corruption, truncation, wrong ownership/key and competing writers refuse open.
An uncertain write poisons the live store. The supervisor must prevent
privileged rollback/deletion: encryption cannot prove a backup is current.
Database debug dumps are refused rather than exporting private state.

Focused tests exercise actual SDK trait objects with generated fixture device
keys, isolated files and no network/account login. They prove encrypted restart
recovery, pending-message sender scoping, consumed-prekey non-resurrection,
atomic retry take, concurrent token preservation/expiry, quotas and corruption
refusal. This completes a storage prerequisite, not a connected WhatsApp
client: linking, host lifecycle, inbound application commit hook, receipt
reconciliation and app integration still require implementation.

## Accepted product direction

Messages is a new app for Tilo-native direct messaging and WhatsApp/Signal/SMS.
Existing Chat remains the persistent group-space app. Both read shared Contacts
identity infrastructure. A person opens with the current service/account beside
their name; switching it changes to that route's separate history. Neither
matching display names nor matching group names merge identity or membership.
Uncertain identity associations require explicit confirmation. Native Tilo
supports richer resource sharing; external recipients can follow a shared
resource link into the Tilo browser app. Creating a native collaboration space
from a group is deferred and does not block Messages.


## Shared Contacts and Messages surface (implemented)

The new `messages` app reads the person-owned `ContactDirectory` and the existing
`PersonAccounts` record through portable platform APIs. It has no account-editor
or credential authority. Contacts projects stable UUIDs and labels into the
shared directory; phone/email fields never become service addresses implicitly.
Explicit association actions refuse conflicting recipients, and routing reads
refuse ambiguity after concurrent confirmations. Removed identities stay
unavailable: restoration after a Contacts undo still needs an explicit reviewed
identity-reactivation policy rather than silently restoring external routes.

The person's name has a shared dropdown carrying exact service/account/recipient
values, with the selected route also visible below it. Separate route keys select
separate history viewports; the surface accurately reports unavailable history
until the host supplies a broker. It neither writes external message plaintext
into a workspace CRDT nor simulates sends or delivery. Shared navigation docks
the person list on wide windows and presents one pane on narrow windows, using
the theme's scaled breakpoint. Samples exist only in capture fixtures. Existing
Chat remains unchanged. Web/app/search registries include Messages via their
existing generator.

Remaining acceptance work includes host broker transport/history injection,
account setup/linking, native direct-message collaboration and rich-resource
sharing, supervisor injection of the SMS transport, durable inbound/receipt processing and the
external browser resource-link journey. These are implementation gaps, not
successful live-service tests. No real account login, message send, keyring
provisioning or modem/SIM mutation was used for validation.

### Native SMS transport

The Linux adapter now calls ModemManager's actual D-Bus Create and Send methods
on a supervisor-supplied connection. It pins the daemon's unique owner, modem,
SIM object and ICCID, checks them before creation and sending, and verifies that
the created SMS belongs to that modem and has the exact recipient and text.
[ModemManager's initializer](https://github.com/linux-mobile-broadband/ModemManager/blob/main/src/mm-base-sms.c)
initializes newly created text SMS as Unknown state and Submit PDU; the adapter
requires these values and refuses received, stored or already sent objects.
The encrypted journal reserves the operation before Create. An interrupted or
failed mutation is uncertain and is never automatically retried; successful
Send records Submitted, never Delivered.

Tests run the production adapter against a private D-Bus daemon and fixture
interfaces, without touching a modem or a personal account. The host must still
provide the authorized account/SIM lease. ModemManager has no atomic Send
conditional on ICCID, so physical SIM replacement during that method remains a
hardware acceptance gap. Inbound SMS and delivery reports still need the host's
durable event projection.

### Revision-aware Contacts dependency

Messages consumes the exact directory and `routing` implementation from
Contacts PR [2910](https://code.tilo.so/tilo/tilo/pulls/2910), commit
`0ca97cf64228fbf638683b4c88fa72b993ae9894`. This dependency is included for
compilation and journey testing; schema, projection and account editing remain
with its owner. Integrate Contacts first, then reconcile Messages against that
landed commit so the shared-schema diff belongs to one PR. Both remain held
for this request; no merge or deployment is requested.

Dropdown and history keys now retain service, sender account, association id,
provider scope, recipient and identity revision. Messages uses the shared
`routing::routes` and `resolve_account` functions rather than a private resolver.
Unversioned compatibility endpoints produce no transport route. A revoked
association, changed revision, wrong provider namespace or disconnected sender
invalidates an old explicit selection; the app never switches to a default.

Sender metadata uses the existing `PersonAccount.config.messaging_scope` field
for an explicit provider namespace. Missing metadata stays unavailable; neither
Contacts hints nor service names guess it. This is display/selection provenance,
not credential authority. Setup must supply that metadata, and the host adapter
must independently verify current provider identity and its device grant before
I/O. The destructive ConfirmAccount effect forwards the full reviewed identity
to the shared action. Legacy recipient-only confirmation refuses. A verified
provider candidate and its visual review remain unavailable until the host
supplies identity evidence; no manually typed address fabricates that evidence.

## Durable WhatsApp recipient receipts (implemented)

The SDK runtime now admits actual typed `Receipt` events against the host-held
subscription client, current account/link binding, and the exact immutable
journaled operation and direct recipient. Unknown operations, other chats or
senders, revoked bindings, and another client are refused. Phone and LID JIDs are
not aliased. Device-qualified recipient JIDs require a separately verified SDK
mapping before admission; this increment deliberately does not invent that mapping.

Recipient delivered/read evidence persists independently of submission and server
acknowledgment, advances monotonically, and survives restart. Sent-only and self
receipts do not create recipient evidence. A failed encrypted snapshot write
makes evidence unavailable instead of exposing optimistic delivery. Late receipts
can reconcile an uncertain operation without resending it. Existing journals
without the optional receipt field remain readable under the original key/format.

Fixture tests use the real pinned SDK event types and encrypted journal, without
network access or linked accounts. The host still must own the event subscription,
retain credential authority, validate current Contacts identity evidence, and
inject history into Messages. This is not a claim of live account acceptance.

## Private history consumer and host loan (implemented)

Messages consumes a read-only `tilo_messaging::HistoryClient` through existing
`HostContext::lend` / `ViewCx::take_lent`, without credential stores, keys,
adapter connections or a new ambient socket. The service must bind the admitted
principal and account grant and revalidate current provider identity at every
read. The consumer does not register or provision that native broker itself;
standalone and unconfigured hosts continue to report history unavailable.

Requests and responses retain the owner's exact shared Contacts `routing::Route`
(person, association, service, sender, scope, recipient, revision). Pages are
bounded to 128 unique messages and 256 KiB of text/identifier payload. Another
identity component, duplicates, oversized payloads and invalid incoming delivery
states are refused before projection. The `ReadHistory` semantic action is a
cancellable device-only read, and automatic reads on route selection use the same
consumer. It changes no account/security state and reports no credential access.
Sending stays separately unavailable; send text is declared a secret argument so
the action host keeps it out of logs, history and confirmation text.

History exists only in transient view state; no external plaintext is persisted
in the Messages workspace document. Navigation clears the page immediately.
Worker completions revalidate the selection and Contacts resolver, and discard
obsolete generations, revoked identities and changed routes. Submitted and
server-accepted states remain visually distinct from recipient-delivered/read.
Reopening does not retain a host loan: the mounting host must grant it afresh.

Production headless tests use real component construction, shared Settings-owned
account metadata synchronization and semantic action dispatch with an explicitly
fixture-only host. They prove visibility, document privacy, response and request
forgery refusal, late-result revocation/navigation and loan withdrawal. Separate
`history` / `history-narrow` capture scenarios contain sample conversation data;
the live app still starts empty. Narrow/wide dark/light/high-contrast geometry is
verified using the shared OS-themed controls. Full-history pagination, arrival
subscriptions, native broker registration and actual linked-account acceptance
remain unfinished; fixture-host validation is not live transport acceptance.

The device-only `ListPeople` and `ListRoutes` read actions expose stable person
IDs and exact current dropdown choices to automation. Matching names remain
separate; revoked links no longer yield recipients. These are metadata choices,
not linked-device or sending grants. Effect debug output is redacted, including
send text and embedded route arguments.

The history capability also carries the host's verified person-root principal.
Messages compares that exact root to the component's current `cx.me()` before
retaining a loan. A capability for another persona stays unavailable without a
host read, even when copied metadata would otherwise match every route field.
The principal is not inferred from a display name or Contacts target UUID.

Native adapter ownership now also requires the host-verified, canonical person-root public
key alongside device/service/account/link generation. Journal and WhatsApp store encrypted
owner records refuse another person even when every other ID and the supplied key match.
Route admission checks the live adapter's person root before transport I/O. Old experimental
records with no person-root field are refused without resetting or overwriting ciphertext;
any future migration requires an explicit ownership-verifying workflow. This binds existing
authority and does not create a credential delegation grant.

## Hosted Signal send and private inbox (implementation increment)

`signal_broker::SignalBroker` binds a certified person root, device, exact Tilo
account ID and linking generation to two supervisor-owned signal-cli streams.
Read-only `listAccounts` verifies the explicitly selected provider account and
its canonical own ACI; the namespace is `signal:<own ACI>`. Before sending,
`listIdentities` must report the selected recipient ACI as `TRUSTED_VERIFIED`
and match the reviewed `signal-identity:<33-byte fingerprint>` revision. These
probes neither link accounts nor change trust. The broker separately requires
current host authorization, canonical Contacts resolution and an enabled,
owned account. Editable account metadata never creates this service authority.

Messages can receive `MessagingClient` through the existing `HostContext::lend`
door. `ApplicationHost::register_service` confines registration to the host's
exact person and named package; shell `InProcessApps::install_signal_receiving`
also checks the frozen device and retains the independent inbox connection's
shutdown guard. Only Messages receives this service. Historical snapshots and
capture views never acquire live sending or receive subscriptions.

Sending uses a stable operation ID, an exact-route request and the normal
sensitive-action confirmation. Encrypted history reserves the operation before
adapter I/O; the native submission journal independently reserves before the
provider write. Repeated or ambiguous operations are never automatically
replayed. A submission response can report acceptance or uncertainty, never
recipient delivery/read. The UI revalidates Contacts and account selection
before publishing worker results, and keeps plaintext outside its CRDT.

The independent receive connection persists bounded raw notifications under the
existing host-held vault key before projecting confirmed direct-message ACIs.
Unknown or ambiguous events remain encrypted for later reconciliation. A
payload-free observer wakes the selected view, which reads a newly authorized,
route-scoped page. Inbound reads do not hold the sender worker while waiting for
network input. Dropping the inbox guard closes only its own connection.

Production setup remains incomplete: an authorized profile/credential-grant
publisher must implement `SignalAuthority`, obtain the supervisor-owned handles
and call the registration API. New account linking and inherited standalone
handle handoff are not implemented. No ambient socket discovery, daemon launch,
key creation, login or security-policy change is substituted for that setup.
Live
provider acceptance, hardware SMS and actual daemon trust-race behavior require
separate authorized acceptance tests; synthetic daemon fixtures prove the wire,
reservation, refusal, encryption, restart and idle refresh paths only.

This increment integrates main's unified SDK redesign: Messages uses `NavShell`
with its shared header and responsive Back, and preserves the redesigned
Contacts editor while retaining its canonical identity-directory projection.

## Account-owned WhatsApp and SMS host wiring

`ExternalBroker` now binds the real `WhatsappRuntime` and `SmsRuntime` to the
same private Messages client and encrypted history used by Signal. The shell
registers these by exact service/account under the frozen person/device and
rejects duplicate registrations. Every read and send revalidates the current
Contacts association, account, scope and identity revision; there is no service
fallback. The adapter's own journal still reserves before native mutation.
Native successful submissions are persisted as Submitted, never Delivered or
Read. Restart retains operation reservations and private text remains encrypted.

`open_system` can assemble private history with an existing OS keychain key and
host-supplied native adapter. It does not provision keys, grants or connections.
Production must still supply the account supervisor and an `ExternalAuthority`
publisher that retains the current certified caller/account/SIM lease throughout
I/O. WhatsApp/SMS inbound messages and production event subscription forwarding,
interactive account linking, and standalone capability handoff remain unfinished.
The focused broker fixture executes both service paths and proves stale exact
routes are rejected before adapter calls, Submitted evidence, revocation,
encryption and no replay after restart. It does not prove provider or hardware
acceptance.

## Private WhatsApp recipient evidence in Messages

`ExternalBroker::observe_whatsapp_receipt` now accepts receipts only through the
host API. It resolves the original operation's frozen private-history route,
revalidates current account/Contacts/provider identity, and delegates evidence
admission to the real `WhatsappRuntime`. That runtime checks the exact SDK client
instance, journal operation, direct-chat sender and receipt message IDs. Group,
self and Sent-only observations supply no recipient evidence. Foreign sources,
wrong recipients/operations and revoked routes are refused.

Delivered/Read evidence is durable and monotonic in both native journal and
encrypted private history. A Read receipt can resolve a previously uncertain
send without replaying it. The original message time stays unchanged. A
payload-free wake refreshes the selected Messages view; plaintext never enters
the shared app record. This does not promote SDK send completion or server ACK
to recipient delivery. Optional history fields preserve reads of earlier data;
unsupported recipient evidence in another service's store fails closed.

The real-SDK, zero-network fixtures prove source and route refusals, private
restart persistence, no replay, monotonic evidence and visible Messages refresh.
Live provider acceptance, actual subscription bootstrap, inbound rich messages,
account linking and full live acceptance remain unfinished.


## Private Signal recipient evidence in Messages

Signal delivery/read notifications now travel through the same account-owned
receive path and private history as direct messages. The bounded codec follows
signal-cli's documented receipt fields ([upstream schema](https://github.com/AsamK/signal-cli/blob/master/src/main/java/org/asamk/signal/json/JsonReceiptMessage.java)):
explicit receipt type, canonical sender ACI, exact receiving account and positive
original-message timestamps. Viewed, ambiguous, malformed and unbounded receipt
batches are refused rather than treated as delivery.

The encrypted native journal correlates each timestamp to exactly one previously
acknowledged operation on the original service/account/recipient route. Unknown
or colliding timestamps cannot select a message. Every operation must also match
its complete canonical Contacts route and current authority before durable,
monotonic evidence is admitted. Unmatched notifications stay encrypted for later
reconciliation. Server acceptance alone still supplies no recipient evidence.

Native socket fixtures exercise interleaved receipt/ACK processing, encrypted
restart persistence, foreign sender/unknown timestamp refusal and independent
idle receipt arrival refreshing the actual selected Messages conversation.
No receipt sends or retries a message, and no plaintext enters the shared app
record. The fixtures do not prove live linked-account/provider acceptance.
Production authority/supervisor bootstrap, linking UI, WhatsApp/SMS inbound
subscription forwarding, rich content/groups/calls and complete user journeys
remain unfinished.


## Verified WhatsApp direct-text inbox admission

The account-owned `ExternalBroker::observe_whatsapp_message` host API now
projects actual decrypted SDK `InboundMessage` records into private history.
The exact SDK instance, current account/link binding, canonical sender/chat and
complete reviewed Contacts route must agree. The new `ExternalAuthority`
inbound resolver refuses by default and cannot derive grants from display names
or alternate phone/LID identities.

The initial parser accepts bounded plain and minimal extended text. It refuses
edits, disappearing/view-once wrappers, mixed/media payloads, multicast/group/
self/broadcast sources, invalid timestamps and oversized identities. These
formats require their own retention and interaction semantics; none is silently
flattened into ordinary stored text. History deduplicates on the unambiguous
(chat, sender, stanza ID) tuple: identical redelivery is a no-op, conflicting
contents or time refuse, and another chat reusing the ID stays separate.

Durable encrypted insertion precedes the payload-free view wake. Actual
Messages/SDK fixtures prove visible arrival, restart persistence, revocation,
source refusal, collision separation and zero network calls. No text enters the
shared record. This API does not register the production SDK subscription or
claim at-least-once delivery: the supervisor must connect it to the SDK's
[durability hook](https://github.com/oxidezap/whatsapp-rust/blob/v0.7.0/src/types/durability_hook.rs)
before acknowledging accepted events. Unconfigured, unknown and unsupported
traffic is refused; full rich messaging and production setup remain incomplete.


Ordinary direct texts may also carry bounded SDK reporting secrets and device
metadata. The receive projection permits only the pinned transport metadata
fields, zeroizes temporary secret copies, and returns only text/identity/time.
Retention/sharing controls and threaded addons still refuse. SDK message-field
equality treats an absent field as equal to a present empty field, so admission
also checks every decoded unsupported payload's presence explicitly. Exhaustive
schema destructuring requires a review when the decoded SDK schema changes.
The pinned upstream generator discards unknown protobuf fields; this admission
layer does not claim to detect wire fields the SDK discarded. Future protocol
extensions therefore require an SDK/schema review rather than a full-support
claim based on this text projection.

SDK CI run 72368 measured the desktop module at 92,265,562 bytes, 56,410 above
its existing budget. Isolated remote builds at the exact CI main baseline
`60f484b76c` and reconciled feature measured 91,671,919 and 92,272,605 bytes:
about 0.66% feature growth. A separate audited raise receipt follows the
repository's standard measurement plus 1% headroom rounded to 64 KiB; policy
and the generated module's size checks pass. These were remote WASM compilation
and module measurements, not successful complete bundles: both full packaging
attempts stopped at `npm ci` because this builder lacks npm. The generated
modules were retrieved by a remote Cargo drop-in after successful narrow checks, without local
Cargo builds or changing builder provisioning.

## Pinned SMS text inbox admission

`SmsRuntime::read_received` and the host-only
`ExternalBroker::observe_sms_message` now read an explicitly selected native
object under its complete reviewed Contacts route. The adapter rechecks the
pinned daemon unique owner, modem membership, SIM path and ICCID before and after
reading. It admits complete 3GPP deliver text only, with the exact E.164 sender,
bounded nonblank text, empty binary data and a positive RFC3339 provider timestamp.
Flash class 0, partial PDUs, status reports, CDMA teleservices and invalid time
are refused. Repeated metadata/body reads detect changing objects; the host must
still hold its SIM lease. A physical hot-swap inside a native method cannot be
made atomic through this D-Bus interface.

The native ID includes daemon owner, host epoch, modem and SMS object. Identical
redelivery is a no-op; conflicting body/time on that identity refuses. It is not
a cross-daemon deduplication guarantee when ModemManager recreates stored SMS
under fresh object identities. Current account/contact authority is checked
again before encrypted insertion; only new durable records wake the app.
Neither reader nor host bridge creates, sends, stores, deletes or acknowledges a
modem message. The app never receives the bus handle or accepts an inbound DTO.

Private D-Bus fixtures exercise the real adapter and broker, exact-route history,
revocation, encrypted restart, duplicate/conflict handling, incomplete/report/
flash/binary/wrong-sender/bad-time refusal, changing SIM/body refusal and arrival
through the actual selected Messages view without shared-record plaintext.
Production initial enumeration and Added/State subscriptions remain unwired.
Unknown senders, short codes and CDMA need separate reviewed association and
message semantics; no normalization or invented timestamp supplies them.

Primary API references: the upstream [SMS interface](https://raw.githubusercontent.com/linux-mobile-broadband/ModemManager/main/introspection/org.freedesktop.ModemManager1.Sms.xml)
and [Messaging interface](https://raw.githubusercontent.com/linux-mobile-broadband/ModemManager/main/introspection/org.freedesktop.ModemManager1.Modem.Messaging.xml).
The latter documents that Added can precede completion of multipart SMS; an
Added event alone therefore cannot produce an ordinary inbox row.

The matched WASM dependency audit found Messages and its portable messaging
client as runtime additions. The three additional third-party names in the
combined Cargo tree (async-channel, buffa and smoothutf8) resolve exclusively
through workspace-hack into the host-built tilo-resource-macros proc macro.
They are not WASM runtime transport dependencies. Native protocol/WhatsApp
packages are absent; zbus was already in the baseline graph. This explains the
combined host/target tree without claiming every emitted byte is irreducible.
The exact new SDK CI run must still pass before browser qualification is complete.

## SDK durability hook and atomic direct-text batches

`WhatsappInboxHook` is now installable through the actual SDK builder's
`with_inbound_durability_hook_arc` before connecting. The host assembles its
already authorized encrypted backend/client/broker, binds the hook once to the
exact owner and SDK, and retains the supervisor Arc while receiving. A weak
binding avoids an ownership cycle and fails after supervisor loss; relinking
requires a fresh assembly. No grant, key, login, network connection or ACK is
created by constructing/binding this hook.

The host verifies every message and current Contacts route in a bounded batch,
then stages all new history rows and performs one encrypted atomic write.
Invalid/unsupported/conflicting members reject the entire batch before insertion.
A persistence failure makes the store unavailable and the hook fails. Identical
redelivery is idempotent, and the successful durable batch produces one
payload-free view wake. The hook returns success only after durable admission;
the SDK owns subsequent acknowledgements. Placeholder/PDO recoveries refuse
because they use the SDK's separate acknowledgement path.

This connects the direct-text durability contract, not full protocol support.
It retains the SDK's documented pending-buffer/crash/replay limitations and
requires current host-held authority and a backend implementing durable pending
inbound operations. The encrypted backend implements those operations; the real
SDK builder checks them before accepting the hook. Unsupported traffic cannot
be acknowledged merely to unblock later traffic; rich-content/retention handlers,
full startup and supervisor installation of the receipt subscription remain open.

The plain remote development image lacked npm, while the declared SDK CI image
supplies it. Packaging now uses the remote Cargo wrapper's documented per-invocation
build-image override to the repository-resolved immutable CI builder, without global config
changes, software installation or local builds. The prior matched 0.66% size
measurement remains historical evidence; qualification requires the completed
current bundle and terminal SDK CI result.

Complete release browser packaging subsequently passed in the official CI
builder `registry.tilo.so/tilo/tilo-builder@sha256:b86785bc1f142208ca38d48401fc044b196e11389ca02ccdcfd622cebf486648`.
The retrieved module measures 92,387,046 bytes, within the audited 93,192,192-byte
budget. No dependency installation, global runner/config change or laptop build
was needed. This resolves the missing-npm packaging blocker; current-head SDK
CI and live native-account acceptance are separate qualification requirements.

## Bounded WhatsApp recipient subscription

`external_broker::WhatsappReceiptSubscription::attach` registers a receipt-only
SDK handler on the exact client owned by the account adapter. The supervisor
retains and drains the subscription with `next`; dropping it unregisters and
closes its queue. Each event reuses the existing operation, native journal,
Contacts route, current caller/account grant and monotonic encrypted-history
checks. Unknown operation IDs create nothing; repeated IDs are no-ops.

The queue retains at most 64 receipt events, each with at most 64 bounded IDs
and bounded native address/type metadata. Overflow, invalid admission, lost
supervisor, revoked identities or persistence failure make the stream fail
observably. The supervisor must reconcile and replace a failed stream; the
implementation does not silently drop receipts, reconnect, send, replay, or
claim a durable provider event log. Already persisted individual operations in
a multi-ID receipt remain valid evidence even if a later operation refuses.

Fixture tests feed the registered production handler at the SDK event boundary,
using real account-owned SDK clients and encrypted history. They do not connect
to the provider: the SDK's internal dispatch bus is private. Live subscriptions,
full bootstrap, setup/linking and upstream crash/replay acceptance remain open.


Executed remote verification for this increment: 17 broker/private Messages
fixtures pass (run `09a4cbff-d7f5-4b99-8a51-1429dddd9be5`), including the
registered receipt handler's exact routing, duplicates, source ownership,
supervisor loss, overflow, revoked identity and actual persistence failure.
Strict all-target protocol lint and the required shell test-target compile pass.
No personal account, real provider connection or message send was used.

## Account lifecycle, native bootstrap and hosted setup state

`tilo-messaging::AccountState` is a non-secret host report, separate from sending
and credential authority. The exact account router forwards it without choosing
another account. Existing hosts that report no lifecycle use Unknown, not a
fabricated connection state. Messages shows linking, locked, offline, relink and
failed states beside the selected service/account and exposes the same values
through `MessagesEffect::ListAccountStates`. These values remain local view data;
external message contents, credentials and pairing codes never enter the CRDT.

`supervisor::AccountSupervisor` freezes the entire principal/device/service/
account/link-generation owner. Metadata or a state label cannot publish Ready.
Verified native installation retains the private history observer; non-ready
states refuse sending/history while the underlying broker still checks current
Contacts and caller/account authority before I/O. The shell's
`InProcessApps::install_messaging_supervisor` lends this lifecycle-aware service
during setup, with existing exact persona/device and duplicate-account checks.

The opaque `supervisor::whatsapp::WhatsappBootstrap` constructs the real SDK,
encrypted backend and durability hook together before a connection. Its
`open_system` uses existing OS-keychain custody only; `build` supports explicit
host-owned encrypted custody and private fixtures. Native network implementations
are supplied by the owning host; neither constructor discovers an account,
connects, logs in, creates a grant or provisions an OS key. The SDK's supported
`into_client` path retains its standard major-sync worker instead of dropping the
sole sync receiver. Installing the pair binds durable inbox admission, registers
bounded recipient receipts, owns their drain task and registers native lifecycle
events. Weak callbacks avoid client/supervisor cycles. A start barrier prevents
publication races; teardown aborts only its own receipt task and unregisters its
handlers. An unlogged/unconnected client never publishes Ready.

Signal setup can retain the existing verified receive worker as part of the same
supervisor publication. Worker termination wakes the private view, reports failed
state and refuses new operations; teardown retains its existing owned interrupt
and join behavior. This reuses the native Signal adapter and receiver rather than
starting another daemon or implementing another transport.

Executed remote validation for the batch: 22 portable Messages/router tests,
19 private native broker fixtures and six native Signal hosted regressions pass.
The final two real SDK bootstrap/actual Messages lifecycle and semantic-action
fixtures also pass with the SDK major-sync worker enabled
(`28ede494-2a0d-4f8f-8e18-2013a1e8baf7`). Strict all-target protocol/Messages/
portable-client lint and required shell test-target compilation pass. Private
fixture network calls remain zero. These are not live provider/keychain tests.

This closes the native assembly/lifecycle publication path, not complete account
setup. Production still needs the certified-caller/current-grant publisher and
host startup that supplies its owned handles/transports and calls these APIs.
Explicit pairing-code/QR setup UI, secret/account provisioning policy, SMS initial
enumeration and Added/State subscriptions, rich content and live linked-provider
acceptance remain open. Linking and health labels do not substitute for those
operations or authorize an app to initiate personal-account login.

## Current personal-device admission and native I/O revocation

The existing action broker now projects an opaque `CertifiedPersonContext` from
its actual signed admission verifier. It requires the person's own derived room,
the exact signed user/device keys, a live execution and the original registered
policy clock; agents, host-scoped and foreign-device admissions are refused.
Revalidation uses the existing current certificate/admission check rather than a
new clock, caller label or serialized permission. This exposes existing admission
to a native host; it creates no device enrollment or credential grant.

Native `Authorization` can retain a process-owned account/credential read lease
through adapter I/O and atomic inbox persistence. Signal, WhatsApp and SMS sends
recheck current authority after native submission before promoting the private
history state. If authority disappears during I/O, the outcome remains uncertain
and the durable reservation prevents replay, including after restart. This does
not revoke a native message that was already submitted. WhatsApp batch admission
retains each authorization through its encrypted atomic write.

Signal supervisor installation now retains its verified receive worker and
publishes its client/health in one locked transition. Its actual Ready observer
can already observe receiver failure; there is no interval with a published Ready
client and a receiver awaiting assignment.

Executed remote fixtures: two final signed personal-context tests pass
(`71cf5e15-9b05-4aea-91a2-a441af2e1742`), five existing certified local-host custody
regressions pass, four focused authority fixtures pass, and eight actual Signal
stream/hosted Messages tests pass. The latter include in-flight revocation,
encrypted restart with no replay and failure inside the actual Ready notification.
No real provider account, message, security setting or enrollment was used.

Twenty final private native broker regressions also pass
(`20de1f09-51b4-4f5a-ad5c-aaa24d979a65`), as do strict action-broker/protocol
all-target lint and required shell test-target compilation. Prior-head CI run
72712 identified one explicit shell enum-match lint; the single unsupported
service is now named explicitly rather than suppressing the lint. That prior
run is a failed qualification, not a baseline failure or a green current-head CI.

The concrete production publisher must still bind this signed device identity
to the host's SDK profile device id and current credential/SIM custody, then read
current committed Contacts and account state at the native operation boundary.
Those device namespaces are different; equating their strings or using editable
metadata as custody would be incorrect. Startup and explicit account setup remain
open; the opaque projection and retained lease alone do not complete them.

## Owned SMS inbox subscriptions and native lifecycle

The exact native SMS runtime now provides an opaque read-only inbox source.
The broker subscribes to the pinned daemon's modem `Added` signal and SMS
`PropertiesChanged` signals before initial enumeration. The streams are drained
continuously into coalesced wakes while native reads are in progress; each wake
rescans retained native objects rather than keeping plaintext in an event queue.
A 30-second bounded rescan also checks current daemon/SIM identity. Neither the
bus, modem nor another account is discovered or rebound automatically.

Complete known-recipient messages use current reviewed Contacts authority and
the existing exact read-only adapter/atomic encrypted history. Partial messages
wait for completion; duplicate delivery remains a no-op. Unsupported PDU/flash
semantics are not converted to ordinary durable messages. Unknown recipients
cannot create a Contacts association by appearing on the modem. This still needs
an explicit unknown-sender/short-code policy to provide those journeys.

Enumeration refuses native lists above 512 entries or malformed paths instead
of silently truncating. The full scan has a deadline. Exceeding either limit,
losing original daemon/SIM custody, or failing private persistence stops the owned
worker and reports Failed through the supervisor. These bounds are operational
limits, not a claim of full carrier/hardware acceptance. Retained modem objects
can disappear before observation; no upstream at-least-once guarantee is claimed.

`AccountSupervisor::install_sms_receiving` retains subscriptions and the receive
worker before Ready publication, reusing the same owned receiver lifecycle as
Signal. Teardown aborts only its tasks and drops its match subscriptions; the
host's bus connection remains usable. There are no Create, Send, Store, Delete,
ACK, SIM unlock, OS grant or personal-account operations on this receive path.

Fifteen actual private D-Bus/runtime/broker/Messages fixtures pass on the final
source (`53fe7861-168a-4abe-9e82-b65dfabfdaa6`), including initial encrypted persistence,
Added-to-partial-to-complete observation, unknown-recipient refusal, duplicate
projection, daemon replacement, SIM change, actual storage failure, excessive
inbox refusal and the real native Added event waking the selected Messages view
without shared plaintext. Eight actual Signal stream/hosted regressions pass;
strict default and standalone SMS all-target lint and required shell test-target
compilation pass. Twenty private native broker regressions also pass
(`d1f4878e-30a2-4f43-b941-28f3f156242c`). The locked manifest change adds only the already locked
`futures-util` dependency edge; no package/version resolution was regenerated.

This closes native SMS enumeration/Added/State observation and supervisor
installation. Production current-account/credential publication and host startup,
explicit account setup/linking, rich content and linked-provider/hardware
acceptance remain open. The subsystem tests do not declare the full app complete.

### Native signed-account startup and existing credential permissions

The shell now has an executable Signal account startup path in
`shell::in_process::messaging`. It binds the existing opaque signed personal
admission to the native host's current cryptographic custody and separately to
the SDK profile device identifier. A foreign current certificate, another root,
changed device key, retired execution or unavailable selected identity refuses.
The two device identifiers are deliberately not treated as interchangeable.

`CredentialBroker::request_existing` checks the actual stored exact application,
item and origin permission with a deny-only decision callback. Startup cannot
create a permission, reuse a consumed one-shot decision or accept a serialized
grant as proof. The native host freezes that credential binding to the exact
account owner and retains the same process-owned broker that its permission
surface revokes. Every provider-authority call and lifecycle query checks that
current admission and grant; a revoked account reports locked without requiring
a send attempt. Contacts and provider fingerprint checks remain required through
the supplied current provider authority.

`InProcessApps::start_native_signal_account` uses the existing OS-keychain-only
runtime/history constructors and independently owned sender and receive streams.
It discovers no sockets, spawns no daemon and never logs in. Ungranted startup
does not call the assembly factory, touch private account storage or issue a
native RPC; it lends the locked lifecycle through the normal Messages mount.
The verified receive worker is retained before ready publication. Assembly
failure reports failed and never fabricates a usable adapter.

The native startup fixtures use synthetic root/device signatures through the
production admission verifier, an isolated encrypted credential vault with
explicit fixture-only decisions, actual Unix-stream Signal account RPCs,
encrypted journal/history and the actual Messages host mount. They cover the
zero-I/O production denied path, consumed one-shot permission, existing session
permission, current revocation, wrong profile/cryptographic device/account,
another item/origin/application, provider-read refusal after revoke and failed
verification. The fixtures create no real account, OS credential or permission.

This executable assembly is not yet automatically called by the default
`InProcessApps::native` startup. The default native broker now supplies the original signed personal execution
through the socket-scoped source described below. The remaining account setup
dispatcher must supply the shared credential broker, current Contacts/account
authority and supervisor-owned daemon handles. A separately opened vault/file snapshot would miss
in-process revocation and is not a substitute. WhatsApp linking/recovery and
network setup, rich content and linked-provider/hardware acceptance remain open.

The startup result now retains an opaque native account setup handle, while the
app receives only its ordinary messaging client. After an explicit grant, the
host can activate that same account: an already open Messages component keeps
its original service loan and sees the verified account become ready. An atomic
activation reservation refuses concurrent or duplicate assembly before any RPC;
only failed initial assembly has an explicit retry door. Recovery checks the
same retained admission and actual grant again and never creates a new grant,
changes the owner/link generation or retries an uncertain message. Native
custody selection follows the admitted person's own room, including a host with
another default identity.

### Signed native WhatsApp assembly and current network grants

The native host can now prepare a WhatsApp account under the same current signed
personal admission and exact existing credential grant as Signal. It registers
one lifecycle-aware Messages loan before assembly. Its retained setup handle
reserves activation, rejects a second concurrent factory before I/O and releases
a cancelled or failed initial reservation into an explicit recovery state.
Retired admission, another profile/service/account or a revoked item/app/origin
grant cannot construct the SDK or reach Contacts/provider metadata. No key,
permission or endpoint is provisioned by this factory.

Registration returns an owned `Send` native worker job; no UI record owner or
mutable app host is retained across SDK construction. The production factory
uses existing OS-keychain custody for the actual SDK's
encrypted backend, journal and private history; installs its actual durability
hook and owned receipt/lifecycle subscriptions; and preserves the SDK's truthful
unlinked/offline state. It accepts the hosting service's authorized network
providers and additionally checks current signed admission and credential grants
before and after HTTP exchanges, connection creation and each transport write.
A revoked transport can still be disconnected. An in-flight write cannot be
reported successful after revocation, nor can a subsequent write reach that
provider. This check does not create egress permission or replace the provider's
endpoint policy. Idle receive teardown and streaming media support still require
qualification; this increment does not claim full transport coverage.

These are native assembly entry points, not a connection from default desktop
startup. The native broker now publishes its original personal authority. The current
credential/current Contacts producer, default account setup dispatch and native
network provider integration remain required. The factories neither connect the SDK nor log in during construction.


### Default native broker personal admission and mounted text journey

The actual native broker now retains one policy-clock origin at construction.
Its native certificate adapter supplies that original clock to the production
signed-person admission verifier; repeated requests cannot replace an unavailable
or retired origin. The existing native certificate and device-fleet checks remain
in force.

After the broker binds its socket, it publishes a process-owned personal source
scoped to that exact socket and the actual compositor lifecycle. Default
`InProcessApps::native` resolves it lazily for account setup, signs an own-person
join with the existing native device custody and obtains the opaque context through
the real broker verifier. No editable person/account label, ambient socket or
new identity key supplies admission. A locked, onboarding or ended compositor
cannot admit setup. Locking retires the original execution; unlocking requires
a fresh execution and does not revive the old account authority. Startup also
refuses another socket, person or SDK device profile.

The mounted text fixture launches the normal native Messages component with
isolated, explicitly synthetic Contacts/account records and credential decisions.
It verifies independent Signal account streams, receives text while idle, selects
the exact route through semantic actions, displays private history and sends an
explicitly confirmed reply. A repeated operation produces one protocol write;
locking the actual session blocks further sends. The shared state dump contains
neither incoming nor outgoing message plaintext. Fixture teardown closes only
its owned streams. No personal account is linked or contacted.

This connects default native startup to the broker's original personal authority;
it does not automatically configure an account. The native source currently
requires the existing ShellNetwork HostIdentity; lack of that source remains
unavailable rather than provisioning identity. The production account setup
dispatcher still needs the shared credential permission owner, current account
and Contacts authority and owned daemon/network startup.
The synthetic mounted journey proves executable text delivery, not live-provider acceptance.


### Private Signal linking in the existing Messages app

Messages exposes account selection, begin, finish and cancel through its existing
semantic action door and shared controls. Begin and finish are sensitive actions
requiring explicit confirmation. The private provisioning capability is lent to
the exact admitted person, service/account and linking generation. Its separate
owned multi-account stream issues signal-cli's documented `startLink` and
`finishLink` RPCs; the expected canonical ACI is frozen before I/O. An account
with no phone number is accepted when the verified ACI matches. Wrong accounts,
stale attempts, malformed QR material, expired codes and revoked authority are
refused. Interrupting a request retires that generation and shuts down only its
owned stream; uncertain completion is never automatically replayed.

Pairing payloads stay outside shared records and exported action results. Owned
payload and QR matrix buffers are zeroized on replacement/drop; this does not
claim that the QR library zeroizes its transient allocations. The existing shared
QR control marks private material through the host's normal screenshot, recording
and sharing exclusion and exposes a protected generic accessible description.
The app removes the presentation on completion, cancellation or retirement of
the original session. Late job completion cannot publish into a changed account
selection or a removed/disabled canonical account.

Link completion reports verified identity, not delivery readiness. The native
supervisor reports Ready only after the separately owned message sender and
receiver are installed and verified. Mounted fixtures exercise confirmed linking,
private rendering and export exclusion, activation through the original open
Messages loan, idle receive/display, an exact-route confirmed reply, duplicate
refusal and actual session-lock revocation without contacting a real provider.

The native dispatcher still needs to create this capability through the shared
credential permission owner and authenticated daemon custody. New-account
identity enrollment, daemon packaging/startup, authorized initial key
provisioning and WhatsApp QR/network setup remain unfinished. Requiring a
previously verified intended ACI is appropriate for this retained-account link
capability; it does not implement enrollment of an unknown account. Rich content,
groups, calls, real-provider compatibility, hardware and speed acceptance remain
unqualified.

Protocol reference: [signal-cli JSON-RPC documentation](https://github.com/AsamK/signal-cli/blob/master/man/signal-cli-jsonrpc.5.adoc).


### Original native credential owner and exact permission revocation

The native app host now retains one deferred credential owner for its person and
SDK device. Preparing the host performs no vault I/O or permission decision.
Passwords mounts through the existing native component resolver and receives
that matching owner; Messages receives only its restricted messaging client.
Another person/device loan closes the vault view, and an installed owner cannot
be replaced. Standalone Passwords keeps its existing private storage policy.

Native credential requests first validate the original socket-scoped signed
execution, person, SDK device and account generation, then use this same owner.
They never reopen a separate vault snapshot. Native grants bind the exact app,
vault item, canonical origin, person, device, service, account and linking
generation. Legacy unscoped permissions cannot authorize this door. The user
decision callback receives the frozen scope; background checks never ask it.
Persistent scoped grants remain encrypted. Session grants have stable opaque
permission IDs and disappear with their original broker.

Passwords' protected permission view shows the service, account, generation and
lifetime. Its exported rows reveal only opaque IDs and generic labels. The
confirmed semantic revoke action and existing control invoke the original
broker's revoker, including session permissions. Stale permission IDs cannot
revoke a replacement; another account or scope remains untouched. Editing or
deleting a vault item updates the same owner, and deletion removes its session
permissions. The open view refreshes against a nonblocking metadata revision;
this does not claim a standalone idle wake subscription.

The native text fixture now binds its credential through the actual native
host gateway. After idle receive and one confirmed reply, it normally mounts
Passwords, confirms that its loan is the original broker, invokes confirmed
revocation, observes the original open Messages loan become locked, and proves
that another operation cannot write to the protocol. A separate default-source
fixture refuses foreign requests, invalid generations and retired executions
before any vault open. These use isolated synthetic stores, admissions and owned
streams, with no personal credential or live-provider message.

This completes the shared credential ownership and native revocation wiring.
The basic usable default-client gate is still open: the production dispatcher
needs canonical account enrollment/explicit scoped permission decisions and owned authenticated
Signal daemon or authorized WhatsApp network startup. Factories and injected
streams are not a configured default client. Existing retained-account linking
still requires a previously verified ACI. WhatsApp private pairing, rich content,
groups, calls and real-provider/hardware/performance acceptance remain unfinished.

### Current canonical records on the original native owner

The native host now supplies current Contacts/account authority through the
original SDK record owner. Native-backed cross-app person records share their profile-scoped
owner registry, so they see unflushed edits in the actual document instead of
opening another writer or falling back to an empty replica. Independent profiles
remain independent, and sharing an owner adds no resource editing permission.

The native factory validates the original signed socket-bound execution and the
matching restricted credential loan before opening existing canonical records.
It never seeds an account, creates a replacement permission, or treats public
metadata as credential evidence. Every query checks exact service, account,
scope, linking generation and recipient association against the current records.
Ambiguous incoming recipients are refused; names are never identity evidence.

UI-thread queries read that owner directly. Protocol workers use a bounded queue
and the compositor's existing wake path; the native event loop services it even
without a Messages window. Requests carry the original execution and credential
loan, which are checked before and after reading. Changes while queued therefore
take effect before the reply. Missing records, queue saturation, an unobserved
request deadline or a closed original owner return unavailable. No copied file
or stale authorization snapshot supplies a successful fallback.

Synthetic native tests exercise account/association changes, queued permission
and session revocation, timeout, owner shutdown and the mounted receive/reply
journey through this actual factory. This closes the current-record producer gap;
it does not configure a Signal daemon, WhatsApp network provider or personal
account. Production enrollment, explicit scoped permission decisions and owned
authenticated provider startup still need implementation and qualification.

### Owned native Signal activation

The native startup entry prepares the original signed account and restricted
Messages loan on the UI owner, then verifies supplied provider handles and opens
private history on the existing bounded session worker pool. The host retains at
most four activation receipts and pumps them through its existing compositor
loop, including when Messages is closed. Preparing or completing a worker does
not publish Ready: the UI owner must accept both independently verified streams
after checking the original execution, exact credential grant, current canonical
account and provider self-identity scope again.

The account probe reads the original record owner without inventing a Contact
recipient. Disabled accounts, changed linking generations or scopes and closed
owners refuse readiness; missing account-probe implementations fail unavailable.
Public state distinguishes unavailable metadata from revoked permission.

Dropping a receipt cancels publication and interrupts only clones of its supplied
owned native streams. Its reservation remains held until that worker releases
all half-assembled state, preventing an overlapping retry. Panics in this owned
helper are isolated and become generic failures, with no automatic replay.
Accepted adapters remain alive when their completed receipt is dropped.

Remote synthetic qualification covers 32 native messaging tests, nine supervisor
tests and eight hosted-message integrations, strict protocol/shell all-target
linting and the shell tests compile check. These prove the actual native host
loop and original current-record producer, cancellation, queue limits, late
revocation and owner shutdown. Canonical enrollment, a production dispatcher
invoking configured providers and packaged authenticated Signal daemon/private
profile custody remain open. No personal login, live message, device reboot or
deployment was performed.

### Idle private pairing expiry

Private setup capabilities report the time until their presentation must be
rechecked. Signal reports its retained pending attempt's existing deadline;
missing or unreadable pending state requests an immediate check. This does not
extend the 120-second provider lifetime or grant another attempt.

Messages supplies that deadline to the shared ModelView. The normal host wake
scheduler reconciles its projection at expiry, retires the private QR subtree
and clears the app's retained private presentation without another input. No
animation loop or timer service was added. Completion and cancellation remove
the pairing wake while preserving unrelated focus or animation deadlines.

Focused tests exercise the real shared frame host's deadline reconciliation,
the provider's expiry/finish refusal and the actual native protected linking UI
registration and retirement. Live launch remains empty and unavailable services
remain unavailable.

### Packaged Signal runtime and unpublished first-account evidence

Signal runtime availability is a Tilo packaging responsibility. The pinned
[signal-cli 0.14.8 requirements](https://github.com/AsamK/signal-cli/blob/v0.14.8/README.md)
require Java 25 and libsignal-client. `os/external-assets/signal-runtime.json`
pins the official JVM distribution, a matching Temurin 25 JRE, Signal's license
and its corresponding source archive by SHA-256. The normal external-asset
inventory contains those exact inputs. The JRE's legal notices and release
provenance remain in the payload. This upstream native payload is qualified
only for Linux x86_64; ARM preparation refuses rather than substituting a host
library or reporting a usable account.

The explicit scoped Signal release helper stages plain verified bytes,
signs them with the ordinary runtime bundle tooling, and seals
`so.tilo.runtime.signal` with the existing thin runtime-package script. It is
an optional provider package, outside Messages' network-disabled sandbox. The
helper does not install an executable into the user's host, introduce PATH
discovery or add Java to every application's runtime. Publication uses the
existing protected `release-apps.yml` workflow and normal registry admission.

Run this fixture inside an isolated native build container, using Cargo or
any Cargo drop-in supplied by that environment:

```sh
cargo build --locked -p runtime-bundle -p tilo-app-package-cli
cargo run --locked -p runtime-bundle --example signal_runtime_fixture -- /existing/authorized/fixture-identity.json
```

The fixture checks official archive digests, builds and verifies an ordinarily
signed runtime, runs the exact pinned CLI with a fresh empty profile, starts
only its own daemon process group, checks an empty `listAccounts` reply through
its private Unix socket, and terminates/reaps that daemon. It also runs the
ordinary runtime-package sealer and tests that the production verifier rejects
a replaced JVM. An existing explicitly authorized fixture identity must be
supplied; this tool creates no signing identity and installs no publisher trust.
The previously executed proof used a temporary synthetic test signer and tested
the thin `signal-0.1.0-x86_64.tapp` artifact and its content-addressed objects.
That proof established the normal bundle/package verifier's structure and
tamper rejection. Upstream archives were checked against official release
SHA-256 metadata; upstream GPG/signature verification was not executed. No
authenticated release was published or installed, and no new publisher became
trusted by the OS or registry. No registration, pairing, real messages or personal profile is
used. Local actual-archive staging encountered a temporary filesystem quota;
the native runtime execution proof is remote, not a laptop Java prerequisite.

`SignalEnrollment` reuses the private bounded linking state machine while
remaining distinct from existing-account relinking. Existing relinking still
freezes and requires the previously verified ACI. First enrollment requires a
canonical provider ACI, then returns noncloneable, nonserializable evidence for
explicit self-identity review. Acceptance consumes that evidence and rechecks
its original authority and phase. Cancellation, lock/unlock retirement and
replay refuse; completion remains offline, with no MessagingClient, route or
credential grant. The native host's preparation creates a fresh provisional
account ID bound to its actual principal/device and original signed personal
session. It never opens credential custody or writes a canonical account.

### Original canonical publication after self-identity review

Native enrollment now retains the original personal-session lease alongside
the protocol evidence. Its verified receipt has no public constructor, clone
or serialization. The host's reviewed publication consumes that receipt and
rechecks the same original principal, SDK device and live admission.

Publication invokes `PersonAccounts::publish_signal_account` on the original
Settings-owned person record. Its new `open_original_record` door propagates
writer errors and refuses replica owners; it never initializes a fallback
shadow when the original writer cannot be observed. Existing unflushed Settings
edits remain visible. Messages receives no account editor privilege.

The canonical action stores only the new UUID, reviewed name, own Signal ACI
scope and linking generation. An occupied UUID or previously configured own ACI
refuses without replacing state, including disabled accounts. Identical display
names remain separate identities. Malformed identifiers, empty or oversized
names and zero generations refuse. This operation creates no credential,
permission, provider or ready messaging loan.

Remote production-path tests cover canonical action validation, original-writer
visibility and replica refusal, native fixture pairing followed by reviewed
publication while preserving existing Settings edits, duplicate self-identity
refusal and lock/unlock retirement before canonical owner initialization.
The native publication door is implemented; the empty-app enrollment UI and
protected provider custody still have to invoke it through the original host
loop. The complete startup/pair/receive/reply acceptance journey remains open.

### Protected fresh permission review

The original per-person/device CredentialOwner now holds at most eight private
permission proposals, each expiring after two minutes. A proposal contains only
the exact app/item/origin and frozen person/device/service/account/generation;
it retains the original native execution check rather than a copied admission
boolean. It creates no permission, returns no credential and is never serialized
into a record or a public action descriptor. Foreign or retired admission refuses
before custody initialization. Changed vault state invalidates the old review.

The native host's `propose_native_messaging_permission` validates the existing
original credential binding and freezes that same signed personal-session lease.
Messages receives no CredentialOwner or decision authority. Native startup still
requires the existing exact grant and the original current account/contact and
provider-identity checks; permission approval never installs a ready adapter.

Passwords presents the request in its existing capture-protected window, with
the app, item, purpose, exact origin, service/account/link generation and original
person/device. Shared themed buttons open a shared confirmation dialog and let
the person choose this session or until revoked. The same approval handler is
published as a sensitive confirmed effect. Declining creates no access. Public
rows use only opaque request IDs and generic localized labels; private review
metadata and credential values never enter the agent surface or shared CRDT.

Confirmation consumes one original proposal. Expiry, replay, changed custody
or retired admission refuses; retirement during the decision compensates by
revoking only its newly created exact permission ID. Authority and frame-wake
callbacks run outside vault/request/wake mutexes. Original host wakes reconcile
new requests and decisions; the existing widget wake deadline retires idle
expired presentations without a polling service. An open protected review also
rechecks retained original admission on each existing frame.

Production-path fixtures cover sensitive semantic and actual pointer/modal
confirmation, public metadata privacy, durable exact-scope permission reopening,
queue capacity, denial, expiry and late retirement. The actual native mount test
proposes through the original source, approves through the original Passwords
semantic door and verifies that no ready messaging transport is published.
This completes the protected decision producer/review door, not encrypted Signal
profile custody, packaged provider startup or its empty-app enrollment controller.

The smallest remaining production path is a confirmed empty-app enrollment
action that owns the packaged provider and its protected private profile,
reviews the resulting self identity, invokes the implemented original Settings
publication door, uses the implemented protected fresh scoped decision door, and passes
independently verified send/receive handles into the existing owned activation
queue. The runtime fixture and evidence producer do not complete that UI,
profile-custody or dispatcher integration. Package availability and completed
linking must never be shown as ready until activation succeeds.

### Default first-account host and quiescent profile checkpoint

The ordinary native Messages mount now lends an initially empty private host
registry and a separate unpublished-enrollment capability. Its semantic actions
start Signal linking, finish the displayed attempt and propose the reviewed
self-identity's credential request. The QR and self-review remain local view
state; the replicated Messages record contains neither profile nor private
conversation text. An already-open empty client observes the account registered
by the original host after protected approval.

Linking uses an owned, normally installed `so.tilo.signal-provider` package.
The signed launcher rechecks original authority immediately before spawn and
pins the actual private PID-namespace init. Socket connection validates private
ownership, the actual peer's namespace and current authority before protocol
bytes. No ambient daemon, host Java or unsigned fallback is discovered. The
scoped `signal_provider_only` release mode seals the executable companion
and exports it alongside the existing runtime package using the supplied normal
release identity. The source manifest remains offline; it does not grant live
network access, install a package or admit a signer.

Finishing enrollment first stops its owned service. Only a clean namespace exit
can produce the host's nonserializable checkpoint witness. The host collects the
newest bounded owner-only profile through directory descriptors; links, public
files, extra sockets, hard links and oversized trees refuse. Forced shutdown
leaves the interrupted marker and cannot resume an older profile. Collection
replaces deleted files as well as changed files. Encryption uses the original
credential custody and runs on session workers. Existing explicitly granted
native accounts also start through this same signed provider factory on mount.

The synthetic empty native app journey exercises the original signed session
source, first-account actions, canonical Settings publication, protected
Passwords proposal and explicit approval, private received text, an attributed
reply and protected revocation refusing further writes. Its provider peer and
writer-free profile are fixtures, not a trusted launch artifact. Separate
launcher tests exercise actual private namespace teardown, authenticated peer
selection, graceful shutdown, forced refusal and original-host retirement.
No personal login, message or credential was used. Accepted release artifacts
for this executable and its pinned runtime, reviewed live networking and actual
linked-provider qualification remain required before claiming a live client.

The pinned Signal 0.14.8 contract needs an explicit adaptation. `finishLink`
returns the linked number, not the ACI. Stock `listAccounts` also returns only
numbers. The owned provider packages a narrow local command extension exposing
`getSelfNumber()` and `getSelfACI()` from its managers; it performs no remote
lookup or trust change. Completion queries this evidence after the finish reply,
requires an unambiguous account, and checks the original authority again. The
runtime keeps the frozen ACI separate from the phone number selected on the
multi-account wire. Manual receiving subscribes explicitly, validates the
returned subscription ID, and requires it on incoming frames and receipts.

Before an owned provider starts, the native host asks the existing protected
portal surface for temporary access to seven fixed Signal hosts. Unconfirmed
requests, denial and permanent choices produce no consent. The consent remains
bound to the original personal admission and cancellation, creates no persisted
capability, and supplies only named hosts to the existing filtered relay. The
namespace remains isolated. A narrow patch prepared from the checksum-verified
upstream source configures Signal's existing HTTP-proxy API to use that local
relay, refusing any foreign proxy. The source manifest stays offline without
this reviewed native evidence. A missing provider is acquired through the same
normal registry installer used by app search, on a worker with original checks
before and after; installation cannot grant account access or open a window.

Runtime-bundle signing and app-package signing use the existing distinct release
identities. The scoped release helper requires the existing accepted runtime
publisher and has no generated-key fallback. The build compiles and tests the
local identity and proxy adaptations against the pinned upstream classes, and
includes the modified source. Those synthetic manager/configuration tests do
not qualify a linked runtime or publish an accepted artifact. Accepted provider
and runtime release artifacts, a normal registry delivery, and qualification
under the production host remain required for live operation. WhatsApp, SMS and
private Tilo transport are not claimed operational by this Signal milestone.

The normal release workflow now has an explicit Signal-only mode. It requires
an exact main dispatch, `publish=true`, `architectures=x86_64` and no collection,
engine or coordinator qualification mode. It compiles only the provider and
two release tools, compiles the local adaptations against the verified upstream
archive, runs the identity/proxy fixtures, verifies the accepted runtime key,
and probes the signed JRE with an empty owned daemon. The helper never creates
a signing identity. Both thin packages and their objects are verified before
publisher OIDC is acquired and normal registry admission is requested. The
former optional OS-image targets have been replaced by this route; Java's
build-only compiler no longer changes the default OS RPM snapshot set.

After the tested source lands, the existing protected delivery entrypoint is:

```sh
fgj actions workflow run release-apps.yml --ref main \
  -f signal_provider_only=true -f apps=messages \
  -f architectures=x86_64 -f publish=true
```

This is a normal accepted-authority release, not a fixture-signature workaround.
Its registry request still needs normal admission. No release dispatch or live
account qualification is implied by documenting the entrypoint.

### Isolated follow-up transport qualification

Provider readiness is separate from the Messages UI and from one another:

| Provider | Qualified path | Next production gap |
| --- | --- | --- |
| Signal | Default empty app, synthetic first-account linking, original Passwords review, owned processes, encrypted profiles/history, receive/display/reply and revoked-write refusal | Accepted existing-authority runtime publication/installation and real-account qualification remain; production publication is restricted. Pinned 0.14.8 ordinary-text metadata requires the codec correction described below. |
| WhatsApp | SDK transport, encrypted SDK state, original grant on state transactions, supplied protected native account activation | Default startup/linking reland #3526 is pending after #3468 was reverted. No real-account compatibility proof. |
| SMS | Default first-SIM chooser, protected lease/review, private ModemManager receive/display/reply and revoked-write refusal in controlled fixtures | Physical SIM/modem/carrier qualification remains; submitted-only is not carrier delivery. |
| Native Tilo | Default original-identity enrollment, protected review, dedicated certified private transport, two-peer text, encrypted restart and revocation | Controlled native-peer proof is separate from WhatsApp/Signal compatibility; external group/media/call UI is not qualified. |

The Signal plain-text codec now recognizes the pinned
[signal-cli 0.14.8 JsonDataMessage schema](https://github.com/AsamK/signal-cli/blob/v0.14.8/src/main/java/org/asamk/signal/json/JsonDataMessage.java).
Ordinary events serialize false expiration-update, end-session and profile-update
flags plus a Boolean profile-key-presence flag. Rejecting those fields as unknown
also rejected ordinary direct text. Schema-derived fixtures reproduce that
failure and cover the correction through the production decoder and stream runtime.
Control flags set true, malformed values, unknown fields, rich/group/ephemeral
events and wrong-account traffic remain refused. The profile-key-presence flag
creates no identity, account or permission authority. This source proof does not
qualify an installed runtime or a real Signal account.

The SMS fixture `sms_runtime::tests::actual_sms_arrival_reply_and_revocation_keep_private_history_out_of_shared_state`
uses an owned private `dbus-daemon` and synthetic ModemManager objects. It receives
through the production inbox observer into the real Messages accessibility tree,
replies through the production D-Bus adapter and confirms that revocation prevents
both further modem mutations and encrypted history writes. The native Send result
is `submitted`, not carrier delivery. Text remains absent from shared app state.
The fixture supplies an account and authority; it does not qualify default account
setup, protected SIM permission review, physical SIM hot-swap or a real modem.

Run it remotely with plain Cargo arguments:

```sh
cargo test --locked -p tilo-messaging-protocol --lib sms_runtime::tests::actual_sms_arrival_reply_and_revocation_keep_private_history_out_of_shared_state -- --exact
```

The pending WhatsApp reland owns default first-account startup and its isolated
SDK-peer qualification, retaining the original reviewed self identity and grant.
Native Tilo now has independent personal-owner/private-transport fixtures proving
separate encrypted histories and collaborator exclusion. Neither proof supplies
WhatsApp/Signal compatibility, workspace Chat authority, a new trusted publisher
or real-account enrollment.

The native WhatsApp bootstrap retains the original account grant in the encrypted
SDK store. It checks that grant under the shared transaction lock before reads or
writes, including through clones made before the guard was attached. A grant
cannot be replaced on that store. The fixture
`whatsapp_store::tests::revoked_original_grant_stops_actual_sdk_ratchet_and_pending_writes`
uses the real SDK Backend traits to prove that revoked session, prekey and pending
inbound writes leave the ciphertext unchanged, revoked reads fail, and already
revoked opening performs no custody I/O. It uses supplied fixture custody and
qualifies no real account, default enrollment or external network connection.

The same production constructor now retains that original grant before opening
private SDK state. Custody lookup can block; a grant revoked while it responds is
rechecked before directory, lock or encrypted-header creation. The production
constructor fixture
`whatsapp_store::tests::revocation_during_custody_lookup_prevents_store_initialization`
failed against the prior post-initialization guard and passes with no store path
created. Initialization also rechecks before a missing header is published and
before returning the backend. These boundary checks do not claim cancellation of
an already executing filesystem write. No real keychain or account is used.

Fresh Signal activation uses the original approved account key for its private
profile, history and send journal. Their existing vault formats authenticate
separate purpose identities. It no longer asks for an unrelated, unprovisioned
messaging key from the OS keychain; no second credential is created. Each native
assembly worker checks the original account grant before and after obtaining
that existing key. Wrong-key existing state fails closed, without reset or a
fallback. The full empty-app first-account fixture now invokes production native
broker assembly on its isolated provider streams, rather than its supplied-key
adapter. It failed before this custody repair and passes after; protected
revocation retains the protocol-write assertion and additionally proves both
private ciphertext files remain unchanged. This still uses synthetic provider
peers and requires the accepted signed runtime for actual service qualification.
